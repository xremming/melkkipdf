//! Off-thread page rendering.
//!
//! MuPDF handles are not `Send`, so a single worker thread owns the `Document`
//! for its whole lifetime and does all rendering. The UI thread talks to it over
//! a channel and only ever receives finished, reference-counted RGB buffers,
//! which it hands to the viewer through the `page-rendered` callback.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;
use std::mem::ManuallyDrop;
use std::path::Path;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use mupdf::{ColorParams, Colorspace, Cookie, Device, Document, Error, Matrix, Pixmap, Rect};
use slint::{Image, Rgb8Pixel, Rgba8Pixel, SharedPixelBuffer, Weak};

use crate::MainWindow;
use crate::images;
use crate::search::Area;

/// Points to the cookie of the render currently in progress, so the UI thread
/// can abort it. The address is valid only while `active`, which the worker sets
/// and clears under the mutex around each render, through a [`Registration`].
#[derive(Default)]
struct AbortSlot {
    cookie: usize,
    epoch: u64,
    /// The page and scale being rendered, so a view that still wants them
    /// lets the render finish.
    page: i32,
    scale: f32,
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

/// Locks the slot. It holds only plain values that every writer leaves
/// consistent, so a panic elsewhere while it was held does not make it
/// unusable, and the UI thread should not panic over one.
fn lock(abort: &Mutex<AbortSlot>) -> MutexGuard<'_, AbortSlot> {
    abort.lock().unwrap_or_else(PoisonError::into_inner)
}

impl RenderControl {
    /// Aborts an in-progress render from an epoch older than `epoch`, unless
    /// the new view still wants its page at its scale, as one of the `wanted`
    /// pairs of page and scale.
    ///
    /// The viewer starts a new epoch on every scroll event, and most of them
    /// still want the page being rendered. Aborting it anyway would only have
    /// it requested again and started over, so on a slow scroll a heavy page
    /// would never finish.
    pub fn advance(&self, epoch: u64, wanted: &[(i32, f32)]) {
        let mut slot = lock(&self.abort);
        let still_wanted = wanted
            .iter()
            .any(|&(page, scale)| page == slot.page && scale_key(scale) == scale_key(slot.scale));
        if slot.active && slot.epoch < epoch && !still_wanted {
            // SAFETY: while `active`, the worker keeps the cookie alive and will
            // not drop it until it re-takes this lock, so the address is valid.
            // The worker holds the cookie borrowed for the render, so it must
            // not be borrowed mutably here. A `Cookie` is only a pointer to
            // MuPDF's C struct, so aborting through a bitwise copy of it
            // writes the same flag without touching the worker's value, and
            // `ManuallyDrop` keeps the copy from freeing the struct. Setting
            // the flag while the render reads it is the unsynchronized
            // signaling MuPDF's cookie is designed for.
            unsafe {
                let mut handle = ManuallyDrop::new(std::ptr::read(slot.cookie as *const Cookie));
                handle.abort();
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

/// What of a page a screenshot is of.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shot {
    /// The whole page, rendered at screenshot resolution.
    Page,
    /// The area, rendered at screenshot resolution.
    Area(Area),
    /// The embedded image drawn `ordinal`-th on the page (see
    /// [`crate::search::ImageSpot`]), at its own resolution.
    Image { ordinal: usize },
}

/// A request for a screenshot of a page, for the clipboard.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenshotRequest {
    pub page: i32,
    pub shot: Shot,
}

/// A document's worker delivering the screenshot asked for, or why it could
/// not take it.
pub struct Screenshot {
    pub request: ScreenshotRequest,
    pub result: Result<Picture, String>,
}

/// An image for the clipboard: `width`×`height` pixels of RGBA, with the
/// alpha straight rather than premultiplied, as the clipboard wants it.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// What a worker reads from a document before rendering any of it.
pub struct DocumentInfo {
    /// Every page's size in points, which lays out the whole document before
    /// any page is rendered.
    pub pages_pt: Vec<(f32, f32)>,
    /// The outline (bookmarks) as `(title, 0-based page, depth)`, depth-first.
    /// Empty when the document has none, or it cannot be read.
    pub outline: Vec<(String, i32, i32)>,
}

/// A document's worker reporting that it has read the document `doc`, or
/// why it could not.
pub struct Loaded {
    pub doc: i32,
    pub result: Result<DocumentInfo, String>,
}

/// Reads what the viewer needs to lay out and navigate `document`. Loading
/// every page takes a while for a long document, which is why it happens on
/// the worker rather than on the UI thread.
fn read_info(document: &Document) -> Result<DocumentInfo, Error> {
    fn flatten(outlines: &[mupdf::Outline], depth: i32, out: &mut Vec<(String, i32, i32)>) {
        for outline in outlines {
            let page = outline.dest.as_ref().map_or(-1, |dest| dest.loc.page_number as i32);
            out.push((outline.title.clone(), page, depth));
            flatten(&outline.down, depth + 1, out);
        }
    }

    let count = document.page_count()?;
    let mut pages_pt = Vec::with_capacity(count.max(0) as usize);
    for index in 0..count {
        let bounds = document.load_page(index)?.bounds()?;
        pages_pt.push((bounds.width(), bounds.height()));
    }
    let mut outline = Vec::new();
    if let Ok(outlines) = document.outlines() {
        flatten(&outlines, 0, &mut outline);
    }
    Ok(DocumentInfo { pages_pt, outline })
}

/// A message to a document's render worker.
pub enum WorkerMessage {
    /// Render a page and deliver it to the viewer.
    Render(RenderRequest),
    /// Render a page, or part of one, for the clipboard.
    Screenshot(ScreenshotRequest),
    /// Drop every cached page. Sent when the document's tab goes to the
    /// background, so tabs nobody is looking at do not hold on to pixels.
    ClearCache,
}

/// Bytes of rendered pages the worker keeps, so a page scrolled or zoomed
/// back to arrives without rendering it again. A budget in bytes rather than
/// pages, because one page can take anywhere from kilobytes to a hundred
/// megabytes depending on its size and the zoom.
const CACHE_BUDGET: usize = 256 * 1024 * 1024;

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
    (request.page, scale_key(request.scale))
}

/// A scale quantized to whole per-mille steps, so two scales that render the
/// same pixels compare equal.
pub fn scale_key(scale: f32) -> u32 {
    (scale * 1000.0).round() as u32
}

/// The largest width or height, in pixels, a page is rendered at. A rendered
/// page becomes one GPU texture, and a texture over the GPU's size limit fails
/// to upload, leaving the page blank. Practically every desktop GPU allows
/// 8192, and it also caps one page at under 200 MB. A page zoomed past it is
/// drawn from a smaller render and looks softer, but it shows.
pub const MAX_RENDER_PX: f32 = 8192.0;

/// `scale`, reduced where needed so a page of `width_pt`×`height_pt` points
/// renders within [`MAX_RENDER_PX`] on both sides.
pub fn capped_scale(width_pt: f32, height_pt: f32, scale: f32) -> f32 {
    let largest = width_pt.max(height_pt) * scale;
    if largest > MAX_RENDER_PX { scale * MAX_RENDER_PX / largest } else { scale }
}

/// The document's file name, which names it in messages to the reader.
fn display_name(path: &str) -> String {
    Path::new(path).file_name().map_or_else(|| path.into(), |name| name.to_string_lossy().into())
}

/// The bytes a rendered page takes in memory.
pub fn buffer_bytes(width: u32, height: u32) -> usize {
    width as usize * height as usize * 3
}

/// Spawns the render worker and returns a channel for sending it requests.
///
/// The worker opens the document, reads its page sizes and outline, and sends
/// them to `loaded`, calling the window's `document-loaded` callback so the
/// app takes them in. It then renders on demand and delivers each finished
/// page back to the viewer via the window's `page-rendered` callback. Every
/// page is tagged with `doc`, because each open tab has a worker of its own
/// and all of them report through the one window.
pub fn spawn(
    path: String,
    doc: i32,
    window: Weak<MainWindow>,
    loaded: Sender<Loaded>,
    screenshots: Sender<Screenshot>,
) -> (Sender<WorkerMessage>, RenderControl) {
    let (sender, receiver) = mpsc::channel::<WorkerMessage>();
    let abort = Arc::new(Mutex::new(AbortSlot::default()));
    let control = RenderControl { abort: abort.clone() };

    thread::spawn(move || {
        let name = display_name(&path);
        let report = |result: Result<DocumentInfo, String>| {
            let _ = loaded.send(Loaded { doc, result });
            let _ = window.upgrade_in_event_loop(|window| window.invoke_document_loaded());
        };
        let opened = Document::open(&path)
            .and_then(|document| read_info(&document).map(|info| (document, info)));
        let document = match opened {
            Ok((document, info)) => {
                report(Ok(info));
                document
            }
            Err(err) => {
                report(Err(err.to_string()));
                return;
            }
        };

        let mut cache: LruCache<CacheKey, PageBuffer> = LruCache::new(CACHE_BUDGET);
        // Renders that failed. A broken page fails the same way every time,
        // so asking again would only repeat the error, but a render too large
        // for memory may work at another zoom, hence the scale in the key.
        let mut failed: HashSet<CacheKey> = HashSet::new();

        // Requests waiting to be rendered. We render one page at a time and
        // re-check the channel after each, so a fresh scroll preempts a stale
        // backlog: only the newest generation (the current view) is ever
        // rendered, and pages that scrolled off screen are dropped or aborted.
        let mut pending: Vec<RenderRequest> = Vec::new();
        // Screenshots waiting to be taken. They come before the pages, since
        // the reader is waiting for each one, and they are never dropped
        // for the view having moved on.
        let mut shots: VecDeque<ScreenshotRequest> = VecDeque::new();
        loop {
            if pending.is_empty() && shots.is_empty() {
                match receiver.recv() {
                    Ok(message) => accept(message, &mut pending, &mut shots, &mut cache),
                    Err(_) => break, // channel closed: shut down
                }
            }
            while let Ok(message) = receiver.try_recv() {
                accept(message, &mut pending, &mut shots, &mut cache);
            }

            if let Some(request) = shots.pop_front() {
                let result = render_screenshot(&document, &request).map_err(|err| err.to_string());
                let _ = screenshots.send(Screenshot { request, result });
                let _ = window.upgrade_in_event_loop(|window| window.invoke_screenshot_taken());
                continue;
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

            if failed.contains(&key) {
                continue;
            }
            if let Some(buffer) = cache.get(&key) {
                push_page(&window, doc, &request, buffer);
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
                    let bytes = buffer_bytes(buffer.width(), buffer.height());
                    cache.put(key, buffer.clone(), bytes);
                    push_page(&window, doc, &request, buffer);
                }
                Err(err) => {
                    failed.insert(key);
                    let (doc, page) = (doc, request.page);
                    let _ = window.upgrade_in_event_loop(move |window| {
                        window.invoke_page_failed(doc, page);
                    });
                    let number = page + 1;
                    push_notice(
                        &window,
                        format!("Failed to render page {number} of {name}: {err}."),
                    );
                }
            }
        }
    });

    (sender, control)
}

/// Takes one message off the worker's channel: queues a render or a
/// screenshot, or empties the cache.
fn accept(
    message: WorkerMessage,
    pending: &mut Vec<RenderRequest>,
    shots: &mut VecDeque<ScreenshotRequest>,
    cache: &mut LruCache<CacheKey, PageBuffer>,
) {
    match message {
        WorkerMessage::Render(request) => pending.push(request),
        WorkerMessage::Screenshot(request) => shots.push_back(request),
        WorkerMessage::ClearCache => cache.clear(),
    }
}

/// Hands a finished page to the UI thread, with the scale it was asked for
/// at, so the viewer can tell a render for an old zoom from a current one.
fn push_page(window: &Weak<MainWindow>, doc: i32, request: &RenderRequest, buffer: PageBuffer) {
    let (page, scale) = (request.page, request.scale);
    let _ = window.upgrade_in_event_loop(move |window| {
        window.invoke_page_rendered(doc, page, scale, Image::from_rgb8(buffer));
    });
}

/// A cookie published in the slot for the length of one render. Dropping it
/// withdraws the cookie, so even a render that panics cannot leave the UI
/// thread holding the address of a cookie that no longer exists.
struct Registration<'a> {
    abort: &'a Mutex<AbortSlot>,
}

impl<'a> Registration<'a> {
    /// Publishes `cookie` as the one to abort for `request`. The registration
    /// must be dropped before the cookie is.
    fn new(abort: &'a Mutex<AbortSlot>, cookie: &Cookie, request: &RenderRequest) -> Self {
        let mut slot = lock(abort);
        slot.cookie = cookie as *const Cookie as usize;
        slot.epoch = request.generation;
        slot.page = request.page;
        slot.scale = request.scale;
        slot.active = true;
        slot.aborted = false;
        Self { abort }
    }

    /// Withdraws the cookie and says whether the render was aborted, under
    /// one lock, so no abort can land between the two.
    fn finish(self) -> bool {
        let mut slot = lock(self.abort);
        slot.active = false;
        slot.aborted
    }
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        lock(self.abort).active = false;
    }
}

/// Renders a page under an abort cookie, registering the cookie so the UI thread
/// can cancel the render if the page scrolls off screen.
fn render_abortable(
    document: &Document,
    request: &RenderRequest,
    abort: &Mutex<AbortSlot>,
) -> (Result<PageBuffer, Error>, bool) {
    // A fresh cookie starts un-aborted (mupdf-rs exposes no way to reset one).
    let cookie = match Cookie::new() {
        Ok(cookie) => cookie,
        Err(err) => return (Err(err), false),
    };
    let registration = Registration::new(abort, &cookie, request);
    let result = render_page(document, request.page, request.scale, &cookie);
    let aborted = registration.finish();
    (result, aborted)
}

/// Renders one page to a tightly-packed RGB buffer, honoring the abort cookie.
/// The scale is capped by [`capped_scale`], and the viewer stretches the
/// smaller image over the page.
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
    let bounds = page.bounds()?;
    let scale = capped_scale(bounds.width(), bounds.height(), scale);
    let ctm = Matrix::new_scale(scale, scale);
    let bbox = bounds.transform(&ctm).round();

    let mut pixmap = Pixmap::new_with_rect(&Colorspace::device_rgb(), bbox, false)?;
    pixmap.clear_with(0xff)?; // white paper
    {
        let device = Device::from_pixmap(&pixmap)?;
        page.run_with_cookie(&device, &ctm, cookie)?;
        // Dropping the device flushes the drawing into the pixmap.
    }

    Ok(pixmap_to_buffer(&pixmap))
}

/// The resolution a screenshot is rendered at, whatever the window's: 300
/// dots per inch, the usual for print, so a whole page pastes sharp.
pub const SCREENSHOT_DPI: f32 = 300.0;

/// The fewest pixels a screenshot's longer side has. A small part of a page
/// at [`SCREENSHOT_DPI`] would be a few hundred pixels, too few to read once
/// pasted anywhere, so a small area is rendered larger.
pub const SCREENSHOT_MIN_PX: f32 = 1600.0;

/// Pixels per point a screenshot of `width_pt`×`height_pt` points is
/// rendered at: [`SCREENSHOT_DPI`], raised until the longer side reaches
/// [`SCREENSHOT_MIN_PX`] and capped as every render is.
pub fn screenshot_scale(width_pt: f32, height_pt: f32) -> f32 {
    let longer = width_pt.max(height_pt).max(1.0);
    let scale = (SCREENSHOT_DPI / 72.0).max(SCREENSHOT_MIN_PX / longer);
    capped_scale(width_pt, height_pt, scale)
}

/// Takes the screenshot asked for: a page or an area of it rendered at
/// screenshot resolution, or an image embedded in the page at its own. The
/// pixmap covers the area alone, so MuPDF draws only what falls in it, and
/// an area reaching past the page is cut at its edge.
fn render_screenshot(document: &Document, request: &ScreenshotRequest) -> Result<Picture, Error> {
    let page = document.load_page(request.page)?;
    let bounds = page.bounds()?;
    let area = match request.shot {
        Shot::Page => bounds,
        Shot::Area(area) => Rect::new(
            bounds.x0 + area.x,
            bounds.y0 + area.y,
            bounds.x0 + area.x + area.width,
            bounds.y0 + area.y + area.height,
        )
        .intersect(&bounds),
        Shot::Image { ordinal } => {
            let image = images::drawn_nth(&page, ordinal)?;
            let (width, height) = (image.width() as f32, image.height() as f32);
            let scale = (MAX_RENDER_PX / width.max(height)).min(1.0);
            return Ok(picture(&draw_image(&image, width * scale, height * scale)?));
        }
    };
    if area.is_empty() {
        return Err(Error::InvalidArgument("the area is off the page".into()));
    }
    let scale = screenshot_scale(area.width(), area.height());
    let ctm = Matrix::new_scale(scale, scale);
    let bbox = area.transform(&ctm).round();

    let mut pixmap = Pixmap::new_with_rect(&Colorspace::device_rgb(), bbox, false)?;
    pixmap.clear_with(0xff)?;
    {
        let device = Device::from_pixmap(&pixmap)?;
        page.run(&device, &ctm)?;
    }
    Ok(picture(&pixmap))
}

/// Draws an embedded image into a pixmap of `width`×`height` pixels, as
/// MuPDF draws it on a page: through its colour space, with its soft mask
/// applied, which leaves the pixmap transparent where the mask hides it,
/// and a stencil mask painted black. The mask is applied the way the page
/// interpreter applies it, as a clip, since decoding alone leaves it out.
/// Drawing the image smaller than it is has MuPDF decode it smaller, which
/// keeps a huge scan affordable.
fn draw_image(image: &mupdf::Image, width: f32, height: f32) -> Result<Pixmap, Error> {
    let bbox = Rect::new(0.0, 0.0, width, height).round();
    let mut pixmap = Pixmap::new_with_rect(&Colorspace::device_rgb(), bbox, true)?;
    pixmap.clear()?;
    {
        let device = Device::from_pixmap(&pixmap)?;
        let ctm = Matrix::new(width, 0.0, 0.0, height, 0.0, 0.0);
        if image.color_space().is_some() {
            let mask = image.mask();
            if let Some(mask) = &mask {
                device.clip_image_mask(mask, &ctm)?;
            }
            device.fill_image(image, &ctm, 1.0, ColorParams::default())?;
            if mask.is_some() {
                device.pop_clip()?;
            }
        } else {
            let black = [0.0, 0.0, 0.0];
            device.fill_image_mask(
                image,
                &ctm,
                &Colorspace::device_rgb(),
                &black,
                1.0,
                ColorParams::default(),
            )?;
        }
    }
    Ok(pixmap)
}

/// A pixmap's pixels as a picture for the clipboard: an opaque RGB pixmap
/// gets a solid alpha, and one with alpha, which MuPDF keeps premultiplied,
/// has its colours divided out again.
fn picture(pixmap: &Pixmap) -> Picture {
    let (width, height) = (pixmap.width(), pixmap.height());
    let stride = pixmap.stride() as usize;
    let samples = pixmap.samples();
    let n = pixmap.n() as usize;
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height as usize {
        let row = &samples[y * stride..y * stride + width as usize * n];
        for pixel in row.chunks_exact(n) {
            if pixmap.alpha() {
                let alpha = pixel[3];
                let straight = |value: u8| {
                    if alpha == 0 { 0 } else { (value as u32 * 255 / alpha as u32).min(255) as u8 }
                };
                rgba.extend_from_slice(&[
                    straight(pixel[0]),
                    straight(pixel[1]),
                    straight(pixel[2]),
                    alpha,
                ]);
            } else {
                rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 0xff]);
            }
        }
    }
    Picture { width, height, rgba }
}

/// Target width, in pixels, for sidebar page thumbnails.
const THUMB_WIDTH: f32 = 150.0;

/// The longer side, in pixels, of an embedded image's preview in the sidebar.
const PREVIEW_PX: f32 = 160.0;

/// What the thumbnail worker is asked to render: a page's thumbnail, or the
/// preview of the image drawn `ordinal`-th on a page, both by 0-based page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThumbRequest {
    Page(i32),
    Preview { page: i32, ordinal: usize },
}

/// Spawns a separate worker that renders small page thumbnails and image
/// previews on demand. It is deliberately independent of the main render
/// pipeline: thumbnails are cheap, persistent (never aborted or
/// epoch-dropped), and each is rendered at most once. Results arrive via the
/// window's `thumbnail-rendered` and `preview-rendered` callbacks, tagged
/// with `doc` like the pages of [`spawn`].
pub fn spawn_thumbnails(path: String, doc: i32, window: Weak<MainWindow>) -> Sender<ThumbRequest> {
    let (sender, receiver) = mpsc::channel::<ThumbRequest>();
    thread::spawn(move || {
        let name = display_name(&path);
        // The page renderer opens the same file and tells the reader when it
        // cannot, so a failure here is only logged.
        let document = match Document::open(&path) {
            Ok(document) => document,
            Err(err) => {
                eprintln!("Failed to open {name} for thumbnails: {err}.");
                return;
            }
        };
        let mut done: HashSet<ThumbRequest> = HashSet::new();
        while let Ok(request) = receiver.recv() {
            if !done.insert(request) {
                continue;
            }
            match request {
                ThumbRequest::Page(page) => match render_thumbnail(&document, page) {
                    Ok(buffer) => {
                        let _ = window.upgrade_in_event_loop(move |window| {
                            window.invoke_thumbnail_rendered(doc, page, Image::from_rgb8(buffer));
                        });
                    }
                    Err(err) => {
                        let number = page + 1;
                        eprintln!(
                            "Failed to render the thumbnail of page {number} of {name}: {err}."
                        );
                        let _ = window.upgrade_in_event_loop(move |window| {
                            window.invoke_thumbnail_failed(doc, page);
                        });
                    }
                },
                ThumbRequest::Preview { page, ordinal } => {
                    match render_preview(&document, page, ordinal) {
                        Ok(buffer) => {
                            let _ = window.upgrade_in_event_loop(move |window| {
                                window.invoke_preview_rendered(
                                    doc,
                                    page,
                                    ordinal as i32,
                                    Image::from_rgba8(buffer),
                                );
                            });
                        }
                        Err(err) => {
                            let number = page + 1;
                            eprintln!(
                                "Failed to render a preview of image {} of page {number} of {name}: {err}.",
                                ordinal + 1
                            );
                        }
                    }
                }
            }
        }
    });
    sender
}

/// Renders the image drawn `ordinal`-th on `page` at preview size. The
/// alpha is left premultiplied, which is how Slint wants it.
fn render_preview(
    document: &Document,
    page: i32,
    ordinal: usize,
) -> Result<SharedPixelBuffer<Rgba8Pixel>, Error> {
    let page = document.load_page(page)?;
    let image = images::drawn_nth(&page, ordinal)?;
    let (width, height) = (image.width().max(1) as f32, image.height().max(1) as f32);
    let scale = (PREVIEW_PX / width.max(height)).min(1.0);
    let pixmap = draw_image(&image, (width * scale).max(1.0), (height * scale).max(1.0))?;
    let (width, height) = (pixmap.width(), pixmap.height());
    let stride = pixmap.stride() as usize;
    let samples = pixmap.samples();
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    let destination = buffer.make_mut_bytes();
    let row_bytes = width as usize * 4;
    for y in 0..height as usize {
        destination[y * row_bytes..(y + 1) * row_bytes]
            .copy_from_slice(&samples[y * stride..y * stride + row_bytes]);
    }
    Ok(buffer)
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

/// Tells the reader about a problem, from the UI thread, ignoring the error
/// that arises only once the event loop has shut down.
fn push_notice(window: &Weak<MainWindow>, message: String) {
    let _ = window.upgrade_in_event_loop(move |window| {
        window.invoke_notify(message.into());
    });
}

/// A least-recently-used cache holding values up to a total cost. The cache
/// holds tens of entries at most once pages are large enough for the budget
/// to matter, so a linear scan to find the eviction victim is cheaper than the
/// bookkeeping a heavier structure needs.
struct LruCache<K, V> {
    budget: usize,
    used: usize,
    tick: u64,
    entries: HashMap<K, Entry<V>>,
}

struct Entry<V> {
    value: V,
    cost: usize,
    tick: u64,
}

impl<K: Eq + Hash + Clone, V: Clone> LruCache<K, V> {
    fn new(budget: usize) -> Self {
        Self { budget, used: 0, tick: 0, entries: HashMap::new() }
    }

    /// Returns a clone of the cached value, refreshing its recency.
    fn get(&mut self, key: &K) -> Option<V> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.entries.get_mut(key)?;
        entry.tick = tick;
        Some(entry.value.clone())
    }

    /// Stores `value` at `cost`, then evicts the least recently used entries
    /// until the cache is within its budget again. The new entry itself is
    /// always kept, even when it alone is over the budget, since the page it
    /// holds is the one the viewer wants now.
    fn put(&mut self, key: K, value: V, cost: usize) {
        self.tick += 1;
        let entry = Entry { value, cost, tick: self.tick };
        if let Some(previous) = self.entries.insert(key, entry) {
            self.used -= previous.cost;
        }
        self.used += cost;
        while self.used > self.budget && self.entries.len() > 1 {
            let Some(oldest) =
                self.entries.iter().min_by_key(|(_, entry)| entry.tick).map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&oldest) {
                self.used -= evicted.cost;
            }
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.used = 0;
    }
}

#[cfg(test)]
mod tests {
    use mupdf::pdf::PdfDocument;
    use mupdf::{Cookie, Size};

    use super::{
        AbortSlot, LruCache, MAX_RENDER_PX, Registration, RenderControl, RenderRequest,
        SCREENSHOT_DPI, SCREENSHOT_MIN_PX, ScreenshotRequest, Shot, capped_scale, render_page,
        render_screenshot, screenshot_scale,
    };
    use crate::search::Area;

    #[test]
    fn a_registration_withdraws_its_cookie_even_on_a_panic() {
        let control = RenderControl::inert();
        let request = RenderRequest { page: 2, scale: 1.0, generation: 1, prefetch: false };
        let result = std::panic::catch_unwind(|| {
            let cookie = Cookie::new().unwrap();
            let _registration = Registration::new(&control.abort, &cookie, &request);
            assert!(control.abort.lock().unwrap().active);
            panic!("the render failed");
        });
        assert!(result.is_err());
        // The slot may be poisoned by the panic, but must say nothing is live,
        // so advancing never touches the dropped cookie.
        let active = control.abort.lock().unwrap_or_else(|err| err.into_inner()).active;
        assert!(!active);
        control.advance(2, &[]);
        assert!(!control.abort.lock().unwrap_or_else(|err| err.into_inner()).aborted);
    }

    /// A control whose slot says `page` is being rendered at `scale` for
    /// epoch 1, under the given cookie.
    fn rendering(cookie: &mut Cookie, page: i32, scale: f32) -> RenderControl {
        let control = RenderControl::inert();
        *control.abort.lock().unwrap() = AbortSlot {
            cookie: cookie as *mut Cookie as usize,
            epoch: 1,
            page,
            scale,
            active: true,
            aborted: false,
        };
        control
    }

    fn aborted(control: &RenderControl) -> bool {
        control.abort.lock().unwrap().aborted
    }

    #[test]
    fn a_render_still_wanted_by_the_new_view_carries_on() {
        let mut cookie = Cookie::new().unwrap();
        let control = rendering(&mut cookie, 4, 2.0);
        control.advance(2, &[(3, 2.0), (4, 2.0), (5, 2.0)]);
        assert!(!aborted(&control));
    }

    #[test]
    fn a_render_the_new_view_does_not_want_is_aborted() {
        let mut cookie = Cookie::new().unwrap();
        let control = rendering(&mut cookie, 4, 2.0);
        control.advance(2, &[(10, 2.0), (11, 2.0)]);
        assert!(aborted(&control));
    }

    #[test]
    fn a_render_at_an_old_scale_is_aborted() {
        let mut cookie = Cookie::new().unwrap();
        let control = rendering(&mut cookie, 4, 2.0);
        control.advance(2, &[(4, 2.5)]);
        assert!(aborted(&control));
    }

    #[test]
    fn a_scale_within_the_limit_is_kept() {
        assert_eq!(capped_scale(600.0, 800.0, 2.0), 2.0);
        assert_eq!(capped_scale(600.0, 800.0, MAX_RENDER_PX / 800.0), MAX_RENDER_PX / 800.0);
    }

    #[test]
    fn a_scale_past_the_limit_is_capped_by_the_longer_side() {
        assert_eq!(capped_scale(600.0, 800.0, 20.0), MAX_RENDER_PX / 800.0);
        assert_eq!(capped_scale(1600.0, 400.0, 20.0), MAX_RENDER_PX / 1600.0);
    }

    #[test]
    fn a_page_zoomed_past_the_limit_renders_within_it() {
        let mut document = PdfDocument::new();
        document.new_page(Size::A4).unwrap();
        let buffer = render_page(&document, 0, 30.0, &Cookie::new().unwrap()).unwrap();
        assert_eq!(buffer.height(), MAX_RENDER_PX as u32);
        assert!(buffer.width() < buffer.height());
    }

    #[test]
    fn a_screenshot_is_rendered_at_print_resolution_or_large_enough_to_read() {
        assert_eq!(screenshot_scale(612.0, 792.0), SCREENSHOT_DPI / 72.0);
        assert_eq!(screenshot_scale(200.0, 100.0), SCREENSHOT_MIN_PX / 200.0);
        // A page too tall for the limit is capped as every render is.
        assert_eq!(screenshot_scale(100.0, 4000.0), MAX_RENDER_PX / 4000.0);
    }

    #[test]
    fn a_screenshot_of_part_of_a_page_covers_that_part_alone() {
        let mut document = PdfDocument::new();
        document.new_page(Size::A4).unwrap();
        let area = Area { x: 100.0, y: 100.0, width: 200.0, height: 100.0 };
        let request = ScreenshotRequest { page: 0, shot: Shot::Area(area) };
        let picture = render_screenshot(&document, &request).unwrap();
        assert_eq!((picture.width, picture.height), (1600, 800));
        assert_eq!(picture.rgba.len(), 1600 * 800 * 4);
        assert!(picture.rgba.iter().all(|&byte| byte == 0xff), "blank paper is opaque white");

        // An area reaching past the page is cut at its edge, and one off
        // the page is refused.
        let area = Area { x: 500.0, y: 0.0, width: 200.0, height: 100.0 };
        let request = ScreenshotRequest { page: 0, shot: Shot::Area(area) };
        let picture = render_screenshot(&document, &request).unwrap();
        let page_width = document.load_page(0).unwrap().bounds().unwrap().width();
        assert!((picture.width as f32 - (page_width - 500.0) * 16.0).abs() <= 1.0);
        assert_eq!(picture.height, 1600);
        let area = Area { x: 700.0, y: 0.0, width: 200.0, height: 100.0 };
        let request = ScreenshotRequest { page: 0, shot: Shot::Area(area) };
        assert!(render_screenshot(&document, &request).is_err());
        // An image a blank page does not have is refused too.
        let request = ScreenshotRequest { page: 0, shot: Shot::Image { ordinal: 0 } };
        assert!(render_screenshot(&document, &request).is_err());
    }

    #[test]
    fn evicts_least_recently_used() {
        let mut cache: LruCache<i32, i32> = LruCache::new(2);
        cache.put(1, 10, 1);
        cache.put(2, 20, 1);

        // Touch key 1 so key 2 becomes the least-recently-used entry.
        assert_eq!(cache.get(&1), Some(10));

        // Inserting a third entry must evict key 2, not the just-touched key 1.
        cache.put(3, 30, 1);
        assert_eq!(cache.get(&2), None);
        assert_eq!(cache.get(&1), Some(10));
        assert_eq!(cache.get(&3), Some(30));
    }

    #[test]
    fn overwrites_existing_key_without_growing() {
        let mut cache: LruCache<i32, i32> = LruCache::new(2);
        cache.put(1, 10, 1);
        cache.put(1, 11, 1);
        assert_eq!(cache.get(&1), Some(11));
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.used, 1);
    }

    #[test]
    fn evicts_by_cost_rather_than_count() {
        let mut cache: LruCache<i32, i32> = LruCache::new(100);
        for key in 0..10 {
            cache.put(key, key, 10);
        }
        assert_eq!(cache.entries.len(), 10);

        // One large entry pushes out as many small ones as it needs room for.
        cache.put(10, 10, 45);
        assert_eq!(cache.used, 95);
        assert_eq!(cache.entries.len(), 6);
        assert_eq!(cache.get(&4), None);
        assert_eq!(cache.get(&5), Some(5));
    }

    #[test]
    fn keeps_a_new_entry_larger_than_the_budget() {
        let mut cache: LruCache<i32, i32> = LruCache::new(100);
        cache.put(1, 1, 10);
        cache.put(2, 2, 500);
        assert_eq!(cache.get(&1), None);
        assert_eq!(cache.get(&2), Some(2));
    }

    #[test]
    fn clearing_frees_the_budget() {
        let mut cache: LruCache<i32, i32> = LruCache::new(100);
        cache.put(1, 1, 60);
        cache.clear();
        assert_eq!(cache.get(&1), None);
        cache.put(2, 2, 60);
        cache.put(3, 3, 40);
        assert_eq!(cache.get(&2), Some(2));
    }
}
