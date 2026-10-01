//! Headless test harness (enabled by the `testing` feature).
//!
//! Creates a [`MainWindow`] and [`Viewer`] (or, for tabs, the whole app) without
//! running the event loop, so integration tests can drive navigation, zoom,
//! layout and tab logic and read the resulting window state directly. Viewer
//! methods set their state immediately (the `scrolled` callback that a live
//! ListView would fire is not needed), so assertions reflect the intended
//! behavior.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::clipboard::Clipboard;
use crate::render::{RenderControl, WorkerMessage};
use crate::search::{PageText, PageTextBuilder};
use crate::semantic::{Vectorized, Vectorizer};
use crate::settings::Store;
use crate::{App, FileDrag, MainWindow, OutlineItem, PageLayout, ViewSettings, Viewer, Workers};

pub use crate::semantic::EmbeddingModel;

/// A window + viewer pair for tests, plus convenience accessors.
pub struct Harness {
    pub window: MainWindow,
    pub viewer: Rc<Viewer>,
    // Kept so render requests the viewer sends have a live receiver; tests can
    // drain it to see which pages were requested and in what order.
    requests: Receiver<WorkerMessage>,
    // Kept alive so thumbnail requests have a receiver.
    _thumb_requests: Receiver<i32>,
    // Where the viewer's vectorizer sends page vectors, which
    // [`Harness::finish_vectorizing`] hands on, there being no event loop
    // to do it.
    vectors: Receiver<Vectorized>,
}

/// Installs Slint's testing backend on this thread, once. Windows made without
/// it get the platform's real backend, which on macOS refuses to run anywhere
/// but the main thread and on a machine without a display cannot start at all.
pub fn install_backend() {
    thread_local! {
        static INSTALLED: Cell<bool> = const { Cell::new(false) };
    }
    if !INSTALLED.replace(true) {
        i_slint_backend_testing::init_no_event_loop();
    }
}

/// A window on the testing backend.
fn new_window() -> MainWindow {
    install_backend();
    MainWindow::new().expect("failed to create the window")
}

impl Harness {
    /// Builds a harness with `count` identical pages of `width`×`height` points.
    pub fn uniform(count: usize, width: f32, height: f32) -> Self {
        Self::with_pages(vec![(width, height); count])
    }

    /// Builds a harness with the given per-page sizes in points, searching
    /// by meaning with the word-counting model.
    pub fn with_pages(pages: Vec<(f32, f32)>) -> Self {
        Self::with_model(pages, EmbeddingModel::words())
    }

    /// Builds a harness with the given per-page sizes in points, searching
    /// by meaning with `model`.
    pub fn with_model(pages: Vec<(f32, f32)>, model: EmbeddingModel) -> Self {
        let window = new_window();
        let (sender, requests) = mpsc::channel();
        let (thumb_sender, _thumb_requests) = mpsc::channel();
        let (vector_sender, vectors) = mpsc::channel();
        let vectorizer = Vectorizer::new(0, Arc::new(model), vector_sender, window.as_weak());
        let workers = Workers {
            pages: sender,
            thumbnails: thumb_sender,
            control: RenderControl::inert(),
            vectorizer,
        };
        let viewer = Viewer::new(&window, pages, 1.0, workers, &ViewSettings::default());
        viewer.activate();
        Self { window, viewer, requests, _thumb_requests, vectors }
    }

    /// Waits for the viewer's vectorizer to finish each batch it has going
    /// and hands the vectors to the viewer, as the app would on the event
    /// loop, until no more are being made. Returns how many batches there
    /// were.
    pub fn finish_vectorizing(&self) -> usize {
        let mut batches = 0;
        while self.viewer.vectorizing() {
            self.take_vectorized_batch();
            batches += 1;
        }
        batches
    }

    /// Waits for the batch the viewer's vectorizer has going and hands its
    /// vectors to the viewer, which sets the next batch going if there is
    /// one.
    pub fn take_vectorized_batch(&self) {
        assert!(self.viewer.vectorizing(), "nothing is being vectorized");
        let vectorized = self.vectors.recv().expect("the vectorizer stopped without a word");
        self.viewer.on_vectorized(vectorized.first_page, vectorized.result);
    }

    /// Drains and returns the 0-based page indices the viewer has requested for
    /// rendering, in the order they were queued.
    pub fn take_render_requests(&self) -> Vec<i32> {
        self.take_render_requests_full().into_iter().map(|(page, ..)| page).collect()
    }

    /// Like [`take_render_requests`], but also returns each request's view epoch
    /// and whether it is a low-priority prefetch.
    pub fn take_render_requests_full(&self) -> Vec<(i32, u64, bool)> {
        let mut requests = Vec::new();
        while let Ok(message) = self.requests.try_recv() {
            if let WorkerMessage::Render(request) = message {
                requests.push((request.page, request.generation, request.prefetch));
            }
        }
        requests
    }

    /// Whether the viewer shows a rendered image for the 0-based `page`.
    pub fn page_rendered(&self, page: usize) -> bool {
        let model = self.window.get_rows();
        (0..model.row_count()).filter_map(|index| model.row_data(index)).any(|row| {
            [row.left, row.right]
                .iter()
                .any(|entry| entry.page == page as i32 && entry.image.size().width > 0)
        })
    }

    /// Gives the viewer the text of every page as its indexer would, each page
    /// a list of lines (see [`text_pages`]).
    pub fn index_text(&self, pages: &[&[&str]]) {
        self.index_text_from(0, pages);
    }

    /// Like [`Harness::index_text`], for the pages from `first_page` on, as
    /// one of the batches the indexer sends.
    pub fn index_text_from(&self, first_page: usize, pages: &[&[&str]]) {
        self.viewer.on_text_indexed(first_page, text_pages(pages));
    }

    /// The hits the search outlines on the 0-based `page`, as `(y, current)`
    /// pairs.
    pub fn highlights(&self, page: usize) -> Vec<(f32, bool)> {
        let model = self.window.get_rows();
        (0..model.row_count())
            .filter_map(|index| model.row_data(index))
            .flat_map(|row| [row.left, row.right])
            .filter(|entry| entry.page == page as i32)
            .flat_map(|entry| {
                (0..entry.highlights.row_count())
                    .filter_map(|index| entry.highlights.row_data(index))
                    .map(|highlight| (highlight.y, highlight.current))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// The areas of the selected text on the 0-based `page`, as
    /// `(x, y, width, height)` in points.
    pub fn selection(&self, page: usize) -> Vec<(f32, f32, f32, f32)> {
        selection_areas(&self.window, page)
    }

    /// The 0-based pages of the listed search results, in order.
    pub fn result_pages(&self) -> Vec<i32> {
        let model = self.window.get_search_results();
        (0..model.row_count()).filter_map(|index| model.row_data(index)).map(|r| r.page).collect()
    }

    /// Delivers a rendered image for the 0-based `page`, as the worker does,
    /// rendered at the scale the viewer asks for now.
    pub fn deliver(&self, page: usize, image: slint::Image) {
        self.viewer.on_page_rendered(page, self.viewer.page_render_scale(page), image);
    }

    /// Sets the viewport size, as a window resize would. Returns `&self` so it
    /// can be chained after construction.
    pub fn viewport(&self, width: f32, height: f32) -> &Self {
        self.viewer.set_viewport(width, height);
        self
    }

    /// Scrolls the continuous list to `offset` (logical pixels from the top)
    /// as the reader would with the wheel or scrollbar: the list says the
    /// reader scrolled, then reports its new offset.
    pub fn scroll_by_user(&self, offset: f32) {
        self.viewer.user_scrolled();
        self.viewer.scrolled(offset);
    }

    pub fn current_page(&self) -> i32 {
        self.window.get_current_page()
    }

    pub fn page_count(&self) -> i32 {
        self.window.get_page_count()
    }

    /// The continuous scroll offset (negative as you scroll down).
    pub fn scroll_y(&self) -> f32 {
        self.window.get_scroll_y()
    }

    pub fn spread_mode(&self) -> i32 {
        self.window.get_spread_mode()
    }

    pub fn continuous(&self) -> bool {
        self.window.get_continuous()
    }

    pub fn density(&self) -> f32 {
        self.window.get_density()
    }

    /// The 0-based pages in the row at the top of the view (see
    /// [`pages_at_top`]).
    pub fn pages_at_top(&self) -> Vec<i32> {
        pages_at_top(&self.window)
    }

    /// Number of rows (one page each, or two for a spread).
    pub fn row_count(&self) -> usize {
        self.window.get_rows().row_count()
    }

    /// Each row as `(left page index, optional right page index)`.
    pub fn rows(&self) -> Vec<(i32, Option<i32>)> {
        let model = self.window.get_rows();
        (0..model.row_count())
            .filter_map(|index| model.row_data(index))
            .map(|row| (row.left.page, row.has_right.then_some(row.right.page)))
            .collect()
    }
}

/// Page text as the indexer would read it, each page a list of lines. Lines
/// start 72pt from the top and are 14pt tall and 20pt apart, and each
/// character is 7pt wide from 72pt in.
fn text_pages(pages: &[&[&str]]) -> Vec<PageText> {
    pages
        .iter()
        .map(|lines| {
            let mut builder = PageTextBuilder::default();
            for (index, line) in lines.iter().enumerate() {
                let top = 72.0 + index as f32 * 20.0;
                builder.start_line(top, top + 14.0);
                for (column, character) in line.chars().enumerate() {
                    let x = 72.0 + column as f32 * 7.0;
                    builder.push(character, x, x + 7.0);
                }
            }
            builder.finish()
        })
        .collect()
}

/// The areas of the selected text on the 0-based `page` of `window`'s rows,
/// as `(x, y, width, height)` in points.
pub fn selection_areas(window: &MainWindow, page: usize) -> Vec<(f32, f32, f32, f32)> {
    let model = window.get_rows();
    (0..model.row_count())
        .filter_map(|index| model.row_data(index))
        .flat_map(|row| [row.left, row.right])
        .filter(|entry| entry.page == page as i32)
        .flat_map(|entry| {
            (0..entry.selection.row_count())
                .filter_map(|index| entry.selection.row_data(index))
                .map(|area| (area.x, area.y, area.width, area.height))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The 0-based pages in the row at the top of `window`'s view, worked out
/// from what it shows: the scroll offset and row height in continuous mode, or
/// the row on display in paged mode.
pub fn pages_at_top(window: &MainWindow) -> Vec<i32> {
    let row = if window.get_continuous() {
        let row_gap = window.global::<PageLayout>().get_row_gap();
        let row_height = window.get_row_height_pt() * window.get_density() + row_gap;
        let index = (-window.get_scroll_y() / row_height + 1e-3).floor().max(0.0) as usize;
        window.get_rows().row_data(index)
    } else {
        Some(window.get_current_row_content())
    };
    row.map_or_else(Vec::new, |row| {
        let mut pages = vec![row.left.page];
        if row.has_right {
            pages.push(row.right.page);
        }
        pages
    })
}

/// A window driven through the app's tab handling, for tests of opening,
/// switching and closing tabs and of what is remembered between runs. Its
/// documents normally have no file behind them: each is a set of uniform pages
/// whose renders go nowhere.
pub struct Tabs {
    pub window: MainWindow,
    app: Rc<App>,
    // Kept so each document's render requests have a live receiver.
    receivers: RefCell<Vec<(Receiver<WorkerMessage>, Receiver<i32>)>>,
}

impl Default for Tabs {
    fn default() -> Self {
        Self::new()
    }
}

impl Tabs {
    /// An empty window that remembers nothing on disk.
    pub fn new() -> Self {
        Self::with_store(Store::in_memory())
    }

    /// An empty window that remembers settings in `file`, as a real run does
    /// in its state directory. Nothing is restored until [`Tabs::restore`].
    pub fn with_settings_file(file: PathBuf) -> Self {
        Self::with_store(Store::load(file))
    }

    fn with_store(store: Store) -> Self {
        Self::with_store_and_model(store, EmbeddingModel::words())
    }

    /// An empty window that remembers nothing on disk and searches by
    /// meaning with `model`.
    pub fn with_model(model: EmbeddingModel) -> Self {
        Self::with_store_and_model(Store::in_memory(), model)
    }

    fn with_store_and_model(store: Store, model: EmbeddingModel) -> Self {
        let window = new_window();
        let app = App::new(&window, store, Clipboard::detached(), model);
        Self { window, app, receivers: RefCell::new(Vec::new()) }
    }

    /// Opens a document of `count` pages named `title` in a new tab, as opening
    /// a file does, and returns the id its renders are tagged with. `title`
    /// doubles as the path the document is remembered under.
    pub fn open(&self, title: &str, count: usize) -> i32 {
        self.open_sized(title, count, 600.0, 800.0)
    }

    /// Like [`Tabs::open`], with pages of `width`×`height` points.
    pub fn open_sized(&self, title: &str, count: usize, width: f32, height: f32) -> i32 {
        self.open_pages(title, vec![(width, height); count])
    }

    /// Like [`Tabs::open`], with each page's width and height in points.
    pub fn open_pages(&self, title: &str, pages: Vec<(f32, f32)>) -> i32 {
        self.open_outlined(title, pages, &[])
    }

    /// Like [`Tabs::open_pages`], with an outline of `(title, 0-based page,
    /// depth)` entries in the order they are listed.
    pub fn open_outlined(
        &self,
        title: &str,
        pages: Vec<(f32, f32)>,
        outline: &[(&str, i32, i32)],
    ) -> i32 {
        let path = PathBuf::from(title);
        if let Some(index) = self.app.find(&path) {
            self.app.select(index);
            return self.app.tabs.borrow()[index].id;
        }
        let outline: Vec<OutlineItem> = outline
            .iter()
            .map(|&(title, page, depth)| OutlineItem { title: title.into(), page, depth })
            .collect();
        let outline = ModelRc::new(VecModel::from(outline));
        let index = self
            .app
            .insert(path, title.into(), pages, outline, |id| {
                let (pages, requests) = mpsc::channel();
                let (thumbnails, thumb_requests) = mpsc::channel();
                self.receivers.borrow_mut().push((requests, thumb_requests));
                let vectorizer = self.app.vectorizer(id);
                Workers { pages, thumbnails, control: RenderControl::inert(), vectorizer }
            })
            .expect("the window is gone");
        self.app.tabs.borrow()[index].id
    }

    /// Opens a real file, exactly as the open button or the command line does,
    /// and waits for its worker to read it.
    pub fn open_file(&self, path: &Path) {
        self.start_opening(path);
        self.finish_loading();
    }

    /// Starts opening a real file, leaving its tab loading until
    /// [`Tabs::finish_loading`].
    pub fn start_opening(&self, path: &Path) {
        self.app.open(path.to_string_lossy().into_owned());
    }

    /// Waits for every document still loading to be read, and takes each in
    /// as the event loop would once its worker reports back.
    pub fn finish_loading(&self) {
        let loading = || self.app.tabs.borrow().iter().any(|tab| tab.loading.is_some());
        while loading() {
            let loaded = self
                .app
                .loads
                .recv_timeout(Duration::from_secs(30))
                .expect("a document took too long to load");
            self.app.finish_load(loaded);
        }
    }

    /// Files dragged from another application arrive over the window.
    pub fn drag_files_over(&self) {
        self.app.file_drag(FileDrag::Hovered);
    }

    /// The drag leaves the window without dropping anything.
    pub fn drag_away(&self) {
        self.app.file_drag(FileDrag::Cancelled);
    }

    /// A file is dropped onto the window. It opens once the event loop gets
    /// to it, so advance the mock time before looking for its tab.
    pub fn drop_file(&self, path: &Path) {
        self.app.file_drag(FileDrag::Dropped(path.to_path_buf()));
    }

    /// Waits for every open document's indexer to read all of its text, and
    /// takes it in as the event loop would.
    pub fn finish_indexing(&self) {
        let pending = || {
            self.app.tabs.borrow().iter().any(|tab| {
                tab.indexer.is_some()
                    && tab.viewer.as_ref().is_some_and(|viewer| !viewer.text_indexed())
            })
        };
        while pending() {
            let indexed = self
                .app
                .indexes
                .recv_timeout(Duration::from_secs(30))
                .expect("a document took too long to index");
            self.app.take_indexed(indexed);
        }
    }

    /// Waits for every open document's vectorizer to finish each batch it
    /// has going, and takes the vectors in as the event loop would, until no
    /// more are being made.
    pub fn finish_vectorizing(&self) {
        let pending = || {
            self.app
                .tabs
                .borrow()
                .iter()
                .any(|tab| tab.viewer.as_ref().is_some_and(|viewer| viewer.vectorizing()))
        };
        while pending() {
            let vectorized = self
                .app
                .vectors
                .recv_timeout(Duration::from_secs(120))
                .expect("a document took too long to vectorize");
            self.app.take_vectorized(vectorized);
        }
    }

    /// The text last copied.
    pub fn copied(&self) -> String {
        self.app.clipboard.copied()
    }

    /// Writes out what the app remembers, as it does when a tab closes, the
    /// autosave timer fires or the window closes.
    pub fn save(&self) {
        self.app.save();
    }

    /// Reopens the tabs saved last time, as a real run does on start, and
    /// waits for their documents to load.
    pub fn restore(&self) {
        self.app.restore_session();
        self.finish_loading();
    }

    /// The status shown in place of the pages, while no document is shown.
    pub fn status(&self) -> String {
        self.window.get_status().into()
    }

    /// The viewer of the tab at `index`. Panics while its document is loading.
    pub fn viewer(&self, index: usize) -> Rc<Viewer> {
        self.app.tabs.borrow()[index].viewer.clone().expect("the document is still loading")
    }

    /// The titles the tab strip shows, in order.
    pub fn titles(&self) -> Vec<String> {
        let model = self.window.get_tabs();
        (0..model.row_count()).filter_map(|i| model.row_data(i)).map(Into::into).collect()
    }

    pub fn active_tab(&self) -> i32 {
        self.window.get_active_tab()
    }

    /// Feeds the tab at `index` the text of its pages, as its indexer would,
    /// one entry per page holding the page's lines.
    pub fn index_text(&self, index: usize, pages: &[&[&str]]) {
        self.viewer(index).on_text_indexed(0, text_pages(pages));
    }

    /// The 0-based pages of the flags on the page edge, in the order drawn,
    /// with what each one's tooltip says.
    pub fn flags(&self) -> Vec<(i32, String)> {
        self.window.get_bookmarks().iter().map(|flag| (flag.page, flag.label.into())).collect()
    }

    /// The hue in degrees of each flag, in the order drawn.
    pub fn flag_hues(&self) -> Vec<i32> {
        self.window.get_bookmarks().iter().map(|flag| flag.hue).collect()
    }

    /// The names and hues of the colours a flag's menu offers.
    pub fn palette(&self) -> Vec<(String, i32)> {
        self.window.get_palette().iter().map(|color| (color.name.into(), color.hue)).collect()
    }

    /// The shape of each flag, in the order drawn, as an index into the
    /// shapes the menu offers.
    pub fn flag_shapes(&self) -> Vec<i32> {
        self.window.get_bookmarks().iter().map(|flag| flag.shape).collect()
    }

    /// The names of the shapes a flag's menu offers, in order.
    pub fn shapes(&self) -> Vec<String> {
        self.window.get_shapes().iter().map(|shape| shape.name.into()).collect()
    }

    /// The 0-based page Tab flips back to, or `None` while there is
    /// none.
    pub fn return_page(&self) -> Option<i32> {
        Some(self.window.get_return_page()).filter(|&page| page >= 0)
    }
}
