//! Headless test harness (enabled by the `testing` feature).
//!
//! Creates a [`MainWindow`] and [`Viewer`] (or, for tabs, the whole app) without
//! running the event loop, so integration tests can drive navigation, zoom,
//! layout and tab logic and read the resulting window state directly. Viewer methods set their state immediately
//! (the `scrolled` callback that a live ListView would fire is not needed), so
//! assertions reflect the intended behavior.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};

use slint::{Model, ModelRc};

use crate::render::{RenderControl, RenderRequest};
use crate::settings::Store;
use crate::{App, FileDrag, MainWindow, ViewSettings, Viewer, Workers};

/// A window + viewer pair for tests, plus convenience accessors.
pub struct Harness {
    pub window: MainWindow,
    pub viewer: Rc<Viewer>,
    // Kept so render requests the viewer sends have a live receiver; tests can
    // drain it to see which pages were requested and in what order.
    requests: Receiver<RenderRequest>,
    // Kept alive so thumbnail requests have a receiver.
    _thumb_requests: Receiver<i32>,
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

    /// Builds a harness with the given per-page sizes in points.
    pub fn with_pages(pages: Vec<(f32, f32)>) -> Self {
        let window = new_window();
        let (sender, requests) = mpsc::channel();
        let (thumb_sender, _thumb_requests) = mpsc::channel();
        let viewer = Viewer::new(
            &window,
            pages,
            1.0,
            sender,
            thumb_sender,
            RenderControl::inert(),
            &ViewSettings::default(),
        );
        Self { window, viewer, requests, _thumb_requests }
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
        while let Ok(request) = self.requests.try_recv() {
            requests.push((request.page, request.generation, request.prefetch));
        }
        requests
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

/// The 0-based pages in the row at the top of `window`'s view, worked out
/// from what it shows: the scroll offset and row height in continuous mode, or
/// the row on display in paged mode.
pub fn pages_at_top(window: &MainWindow) -> Vec<i32> {
    let row = if window.get_continuous() {
        // Must match the `+ 16px` gap in the `PageRowView` delegate.
        let row_height = window.get_row_height_pt() * window.get_density() + 16.0;
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
    receivers: RefCell<Vec<(Receiver<RenderRequest>, Receiver<i32>)>>,
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
        let window = new_window();
        let app = App::new(&window, store);
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
        let path = PathBuf::from(title);
        if let Some(index) = self.app.find(&path) {
            self.app.select(index);
            return self.app.tabs.borrow()[index].id;
        }
        let index = self
            .app
            .insert(path, title.into(), vec![(width, height); count], ModelRc::default(), |_| {
                let (pages, requests) = mpsc::channel();
                let (thumbnails, thumb_requests) = mpsc::channel();
                self.receivers.borrow_mut().push((requests, thumb_requests));
                Workers { pages, thumbnails, control: RenderControl::inert() }
            })
            .expect("the window is gone");
        self.app.tabs.borrow()[index].id
    }

    /// Opens a real file, exactly as the open button or the command line does.
    pub fn open_file(&self, path: &Path) {
        self.app.open(path.to_string_lossy().into_owned());
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

    /// Writes out what the app remembers, as it does when a tab closes, the
    /// autosave timer fires or the window closes.
    pub fn save(&self) {
        self.app.save();
    }

    /// Reopens the tabs saved last time, as a real run does on start.
    pub fn restore(&self) {
        self.app.restore_session();
    }

    /// The viewer of the tab at `index`.
    pub fn viewer(&self, index: usize) -> Rc<Viewer> {
        self.app.tabs.borrow()[index].viewer.clone()
    }

    /// The titles the tab strip shows, in order.
    pub fn titles(&self) -> Vec<String> {
        let model = self.window.get_tabs();
        (0..model.row_count()).filter_map(|i| model.row_data(i)).map(Into::into).collect()
    }

    pub fn active_tab(&self) -> i32 {
        self.window.get_active_tab()
    }
}
