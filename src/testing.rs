//! Headless test harness (enabled by the `testing` feature).
//!
//! Creates a [`MainWindow`] and [`Viewer`] (or, for tabs, the whole app) without
//! running the event loop, so integration tests can drive navigation, zoom,
//! layout and tab logic and read the resulting window state directly. Viewer methods set their state immediately
//! (the `scrolled` callback that a live ListView would fire is not needed), so
//! assertions reflect the intended behavior.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};

use slint::{Model, ModelRc};

use crate::render::{RenderControl, RenderRequest};
use crate::{App, MainWindow, Tab, Viewer};

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

impl Harness {
    /// Builds a harness with `count` identical pages of `width`×`height` points.
    /// Returns `None` if no windowing backend is available in the test runner.
    pub fn uniform(count: usize, width: f32, height: f32) -> Option<Self> {
        Self::with_pages(vec![(width, height); count])
    }

    /// Builds a harness with the given per-page sizes in points.
    pub fn with_pages(pages: Vec<(f32, f32)>) -> Option<Self> {
        let window = MainWindow::new().ok()?;
        let (sender, requests) = mpsc::channel();
        let (thumb_sender, _thumb_requests) = mpsc::channel();
        let viewer = Viewer::new(&window, pages, 1.0, sender, thumb_sender, RenderControl::inert());
        Some(Self { window, viewer, requests, _thumb_requests })
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

/// A window driven through the app's tab handling, for tests of opening,
/// switching and closing tabs. Its documents have no file behind them: each is
/// a set of uniform pages whose renders go nowhere.
pub struct Tabs {
    pub window: MainWindow,
    app: Rc<App>,
    // Kept so each document's render requests have a live receiver.
    receivers: RefCell<Vec<(Receiver<RenderRequest>, Receiver<i32>)>>,
}

impl Tabs {
    /// An empty window. Returns `None` if no windowing backend is available.
    pub fn new() -> Option<Self> {
        let window = MainWindow::new().ok()?;
        let app = App::new(&window);
        Some(Self { window, app, receivers: RefCell::new(Vec::new()) })
    }

    /// Opens a document of `count` pages named `title` in a new tab, as opening
    /// a file does, and returns the id its renders are tagged with.
    pub fn open(&self, title: &str, count: usize) -> i32 {
        let (sender, requests) = mpsc::channel();
        let (thumb_sender, thumb_requests) = mpsc::channel();
        self.receivers.borrow_mut().push((requests, thumb_requests));
        let viewer = Viewer::new(
            &self.window,
            vec![(600.0, 800.0); count],
            1.0,
            sender,
            thumb_sender,
            RenderControl::inert(),
        );
        let id = self.app.allocate_id();
        self.app.add_tab(Tab {
            id,
            path: PathBuf::from(title),
            title: title.into(),
            viewer,
            outline: ModelRc::default(),
        });
        id
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
