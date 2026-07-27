//! Off-thread page rendering.
//!
//! MuPDF handles are not `Send`, so a single worker thread owns the `Document`
//! for its whole lifetime and does all rendering. The UI thread talks to it over
//! a channel and only ever receives finished, reference-counted RGBA buffers.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread;

use mupdf::{Colorspace, Document, Error, Matrix};
use slint::{Image, Rgb8Pixel, SharedPixelBuffer, Weak};

use crate::MainWindow;

/// A request to render a specific page at a given scale.
#[derive(Clone, Copy)]
pub struct RenderRequest {
    pub page: i32,
    pub scale: f32,
}

/// Number of rendered pages kept in memory. Sized to comfortably cover a page
/// plus the neighbors we will prerender in a later step.
const CACHE_CAPACITY: usize = 16;

/// A page rendered to a shared RGB buffer. Cloning is a cheap refcount bump, so
/// the cache and the UI can hold the same pixels without copying.
///
/// We render opaque on white (no alpha) rather than compositing the page over a
/// transparent background, so untouched "paper" areas are white instead of
/// showing the window surface through them.
type PageBuffer = SharedPixelBuffer<Rgb8Pixel>;

/// Cache key: page index plus the scale quantized to whole per-mille steps, so a
/// float scale can be hashed and compared exactly.
type CacheKey = (i32, u32);

fn cache_key(request: &RenderRequest) -> CacheKey {
    (request.page, (request.scale * 1000.0).round() as u32)
}

/// Spawns the render worker and returns a channel for sending it requests.
///
/// `page_count` is populated once the document is open so the UI can clamp
/// navigation without holding a (non-`Send`) MuPDF handle itself.
pub fn spawn(
    path: String,
    window: Weak<MainWindow>,
    page_count: Arc<AtomicI32>,
) -> Sender<RenderRequest> {
    let (sender, receiver) = mpsc::channel::<RenderRequest>();

    thread::spawn(move || {
        let document = match Document::open(&path) {
            Ok(document) => document,
            Err(err) => {
                push_status(&window, format!("Failed to open {path}: {err}"));
                return;
            }
        };

        let count = document.page_count().unwrap_or(0);
        page_count.store(count, Ordering::Relaxed);

        let mut cache: LruCache<CacheKey, PageBuffer> = LruCache::new(CACHE_CAPACITY);

        while let Ok(mut request) = receiver.recv() {
            // Coalesce: if several requests piled up while we were rendering (fast
            // paging), skip the intermediate ones and honor only the latest.
            while let Ok(newer) = receiver.try_recv() {
                request = newer;
            }

            let key = cache_key(&request);
            let buffer = match cache.get(&key) {
                Some(buffer) => buffer,
                None => match render_page(&document, request.page, request.scale) {
                    Ok(buffer) => {
                        cache.put(key, buffer.clone());
                        buffer
                    }
                    Err(err) => {
                        let page = request.page + 1;
                        push_status(&window, format!("Failed to render page {page}: {err}"));
                        continue;
                    }
                },
            };

            let status = format!("Page {} of {}", request.page + 1, count);
            let _ = window.upgrade_in_event_loop(move |window| {
                window.set_page_image(Image::from_rgb8(buffer));
                window.set_status(status.into());
            });
        }
    });

    sender
}

/// Renders one page to a tightly-packed RGBA buffer.
///
/// MuPDF rows can be padded, so we copy row by row using the pixmap stride rather
/// than assuming the samples are contiguous.
fn render_page(document: &Document, page: i32, scale: f32) -> Result<PageBuffer, Error> {
    let page = document.load_page(page)?;
    let matrix = Matrix::new_scale(scale, scale);
    // `alpha = false` makes MuPDF clear the page to white before drawing, giving
    // opaque paper; with alpha the background would be transparent and show the
    // window surface through it.
    let pixmap = page.to_pixmap(&matrix, &Colorspace::device_rgb(), false, true)?;

    let width = pixmap.width();
    let height = pixmap.height();
    let stride = pixmap.stride() as usize;
    let samples = pixmap.samples();

    let mut buffer = SharedPixelBuffer::<Rgb8Pixel>::new(width, height);
    let destination = buffer.make_mut_bytes();
    let row_bytes = width as usize * 3;
    for y in 0..height as usize {
        let source_row = &samples[y * stride..y * stride + row_bytes];
        let destination_row = &mut destination[y * row_bytes..y * row_bytes + row_bytes];
        destination_row.copy_from_slice(source_row);
    }

    Ok(buffer)
}

/// Pushes a status message to the UI thread, ignoring the error that arises only
/// once the event loop has shut down.
fn push_status(window: &Weak<MainWindow>, status: String) {
    let _ = window.upgrade_in_event_loop(move |window| {
        window.set_status(status.into());
    });
}

/// A small least-recently-used cache. Capacity is tiny, so a linear scan to find
/// the eviction victim is cheaper than the bookkeeping a heavier structure needs.
struct LruCache<K, V> {
    capacity: usize,
    tick: u64,
    entries: HashMap<K, (V, u64)>,
}

impl<K: Eq + Hash + Clone, V: Clone> LruCache<K, V> {
    fn new(capacity: usize) -> Self {
        Self { capacity, tick: 0, entries: HashMap::new() }
    }

    /// Returns a clone of the cached value, refreshing its recency.
    fn get(&mut self, key: &K) -> Option<V> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.entries.get_mut(key)?;
        entry.1 = tick;
        Some(entry.0.clone())
    }

    fn put(&mut self, key: K, value: V) {
        self.tick += 1;
        let tick = self.tick;
        self.entries.insert(key, (value, tick));
        if self.entries.len() > self.capacity {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, (_, tick))| *tick)
                .map(|(key, _)| key.clone())
            {
                self.entries.remove(&oldest);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LruCache;

    #[test]
    fn evicts_least_recently_used() {
        let mut cache: LruCache<i32, i32> = LruCache::new(2);
        cache.put(1, 10);
        cache.put(2, 20);

        // Touch key 1 so key 2 becomes the least-recently-used entry.
        assert_eq!(cache.get(&1), Some(10));

        // Inserting a third entry must evict key 2, not the just-touched key 1.
        cache.put(3, 30);
        assert_eq!(cache.get(&2), None);
        assert_eq!(cache.get(&1), Some(10));
        assert_eq!(cache.get(&3), Some(30));
    }

    #[test]
    fn overwrites_existing_key_without_growing() {
        let mut cache: LruCache<i32, i32> = LruCache::new(2);
        cache.put(1, 10);
        cache.put(1, 11);
        assert_eq!(cache.get(&1), Some(11));
        assert_eq!(cache.entries.len(), 1);
    }
}
