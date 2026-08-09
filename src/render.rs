//! Off-thread page rendering.
//!
//! MuPDF handles are not `Send`, so a single worker thread owns the `Document`
//! for its whole lifetime and does all rendering. The UI thread talks to it over
//! a channel and only ever receives finished, reference-counted RGB buffers,
//! which it hands to the viewer through the `page-rendered` callback.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use mupdf::{Colorspace, Cookie, Device, Document, Error, Matrix, Pixmap};
use slint::{Image, Rgb8Pixel, SharedPixelBuffer, Weak};

use crate::MainWindow;

/// Points to the cookie of the render currently in progress, so the UI thread
/// can abort it. The address is valid only while `active`, which the worker sets
/// and clears under the mutex around each render.
#[derive(Default)]
struct AbortSlot {
    cookie: usize,
    epoch: u64,
    active: bool,
    /// Set when `advance` actually aborted the in-progress render, so the worker
    /// knows to discard its half-drawn result (rather than discarding merely
    /// because the epoch moved on).
    aborted: bool,
}

/// Lets the viewer cancel a render whose page has scrolled off screen.
#[derive(Clone)]
pub struct RenderControl {
    abort: Arc<Mutex<AbortSlot>>,
}

impl RenderControl {
    /// Aborts an in-progress render from an epoch older than `epoch` (its page
    /// has scrolled off screen).
    pub fn advance(&self, epoch: u64) {
        let mut slot = self.abort.lock().unwrap();
        if slot.active && slot.epoch < epoch {
            // SAFETY: while `active`, the worker keeps the cookie alive and will
            // not drop it until it re-takes this lock, so the address is valid.
            // Setting the abort flag while the worker reads it mid-render is the
            // unsynchronized signaling MuPDF's cookie is explicitly designed for.
            unsafe {
                (*(slot.cookie as *mut Cookie)).abort();
            }
            slot.aborted = true;
        }
    }

    /// A control not attached to a worker, for tests.
    pub fn inert() -> Self {
        Self { abort: Arc::new(Mutex::new(AbortSlot::default())) }
    }
}

/// A request to render a specific page at a given scale.
#[derive(Clone, Copy)]
pub struct RenderRequest {
    pub page: i32,
    pub scale: f32,
    /// The view "epoch" this request belongs to. The viewer bumps it on every
    /// scroll/zoom, so the worker can drop requests from earlier views (pages
    /// that have since scrolled off screen).
    pub generation: u64,
    /// A low-priority prefetch (a neighbor of the visible pages). Rendered only
    /// after every visible page in the same epoch.
    pub prefetch: bool,
}

/// Number of rendered pages kept in memory. Sized to comfortably cover a page
/// plus the neighbors we will prerender in a later step.
const CACHE_CAPACITY: usize = 48;

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
/// The worker opens its own `Document` (the UI thread reads page sizes from a
/// separate handle), renders on demand, and delivers each finished page back to
/// the viewer via the window's `page-rendered` callback.
pub fn spawn(path: String, window: Weak<MainWindow>) -> (Sender<RenderRequest>, RenderControl) {
    let (sender, receiver) = mpsc::channel::<RenderRequest>();
    let abort = Arc::new(Mutex::new(AbortSlot::default()));
    let control = RenderControl { abort: abort.clone() };

    thread::spawn(move || {
        let document = match Document::open(&path) {
            Ok(document) => document,
            Err(err) => {
                push_status(&window, format!("Failed to open {path}: {err}"));
                return;
            }
        };

        let mut cache: LruCache<CacheKey, PageBuffer> = LruCache::new(CACHE_CAPACITY);

        // Requests waiting to be rendered. We render one page at a time and
        // re-check the channel after each, so a fresh scroll preempts a stale
        // backlog: only the newest generation (the current view) is ever
        // rendered, and pages that scrolled off screen are dropped or aborted.
        let mut pending: Vec<RenderRequest> = Vec::new();
        loop {
            if pending.is_empty() {
                match receiver.recv() {
                    Ok(request) => pending.push(request),
                    Err(_) => break, // channel closed: shut down
                }
            }
            while let Ok(more) = receiver.try_recv() {
                pending.push(more);
            }

            // Keep only the newest view; drop everything older (off screen).
            let Some(newest) = pending.iter().map(|r| r.generation).max() else {
                continue;
            };
            pending.retain(|request| request.generation == newest);
            // Visible pages before prefetch, then topmost first; drop duplicates.
            pending.sort_by_key(|request| (request.prefetch, request.page));
            pending.dedup_by_key(|request| cache_key(request));

            let request = pending.remove(0);
            let key = cache_key(&request);

            if let Some(buffer) = cache.get(&key) {
                push_page(&window, request.page, buffer);
                continue;
            }

            let (outcome, aborted) = render_abortable(&document, &request, &abort);

            // An aborted render is half-drawn: drop it (it will be re-requested
            // if the page is still wanted). A completed render is kept even if
            // the view has since moved on.
            if aborted {
                continue;
            }
            match outcome {
                Ok(buffer) => {
                    cache.put(key, buffer.clone());
                    push_page(&window, request.page, buffer);
                }
                Err(err) => {
                    let page = request.page + 1;
                    push_status(&window, format!("Failed to render page {page}: {err}"));
                }
            }
        }
    });

    (sender, control)
}

/// Hands a finished page to the UI thread.
fn push_page(window: &Weak<MainWindow>, page: i32, buffer: PageBuffer) {
    let _ = window.upgrade_in_event_loop(move |window| {
        window.invoke_page_rendered(page, Image::from_rgb8(buffer));
    });
}

/// Renders a page under an abort cookie, registering the cookie so the UI thread
/// can cancel the render if the page scrolls off screen.
fn render_abortable(
    document: &Document,
    request: &RenderRequest,
    abort: &Arc<Mutex<AbortSlot>>,
) -> (Result<PageBuffer, Error>, bool) {
    // A fresh cookie starts un-aborted (mupdf-rs exposes no way to reset one).
    let mut cookie = match Cookie::new() {
        Ok(cookie) => cookie,
        Err(err) => return (Err(err), false),
    };
    {
        let mut slot = abort.lock().unwrap();
        slot.cookie = &mut cookie as *mut Cookie as usize;
        slot.epoch = request.generation;
        slot.active = true;
        slot.aborted = false;
    }
    let result = render_page(document, request.page, request.scale, &cookie);
    let aborted = {
        let mut slot = abort.lock().unwrap();
        slot.active = false;
        slot.aborted
    };
    (result, aborted)
}

/// Renders one page to a tightly-packed RGB buffer, honoring the abort cookie.
///
/// `alpha = false` (a white background) is emulated by clearing the pixmap to
/// white before running the page, matching `to_pixmap(..., alpha=false)`.
fn render_page(
    document: &Document,
    page: i32,
    scale: f32,
    cookie: &Cookie,
) -> Result<PageBuffer, Error> {
    let page = document.load_page(page)?;
    let ctm = Matrix::new_scale(scale, scale);
    let bbox = page.bounds()?.transform(&ctm).round();

    let mut pixmap = Pixmap::new_with_rect(&Colorspace::device_rgb(), bbox, false)?;
    pixmap.clear_with(0xff)?; // white paper
    {
        let device = Device::from_pixmap(&pixmap)?;
        page.run_with_cookie(&device, &ctm, cookie)?;
        // Dropping the device flushes the drawing into the pixmap.
    }

    Ok(pixmap_to_buffer(&pixmap))
}

/// Target width, in pixels, for sidebar page thumbnails.
const THUMB_WIDTH: f32 = 150.0;

/// Spawns a separate worker that renders small page thumbnails on demand. It is
/// deliberately independent of the main render pipeline: thumbnails are cheap,
/// persistent (never aborted or epoch-dropped), and each page is rendered at
/// most once. Send it 0-based page indices; results arrive via the window's
/// `thumbnail-rendered` callback.
pub fn spawn_thumbnails(path: String, window: Weak<MainWindow>) -> Sender<i32> {
    let (sender, receiver) = mpsc::channel::<i32>();
    thread::spawn(move || {
        let Ok(document) = Document::open(&path) else {
            return;
        };
        let mut done: std::collections::HashSet<i32> = std::collections::HashSet::new();
        while let Ok(page) = receiver.recv() {
            if !done.insert(page) {
                continue;
            }
            if let Ok(buffer) = render_thumbnail(&document, page) {
                let _ = window.upgrade_in_event_loop(move |window| {
                    window.invoke_thumbnail_rendered(page, Image::from_rgb8(buffer));
                });
            }
        }
    });
    sender
}

/// Renders one page at thumbnail size.
fn render_thumbnail(document: &Document, page: i32) -> Result<PageBuffer, Error> {
    let page = document.load_page(page)?;
    let width = page.bounds()?.width().max(1.0);
    let scale = THUMB_WIDTH / width;
    let matrix = Matrix::new_scale(scale, scale);
    let pixmap = page.to_pixmap(&matrix, &Colorspace::device_rgb(), false, true)?;
    Ok(pixmap_to_buffer(&pixmap))
}

/// Copies a pixmap's RGB samples into a tightly-packed shared buffer. MuPDF rows
/// can be padded, so we copy row by row using the pixmap stride.
fn pixmap_to_buffer(pixmap: &Pixmap) -> PageBuffer {
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
    buffer
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
        if self.entries.len() > self.capacity
            && let Some(oldest) =
                self.entries.iter().min_by_key(|(_, (_, tick))| *tick).map(|(key, _)| key.clone())
        {
            self.entries.remove(&oldest);
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
