//! Headless test harness (enabled by the `testing` feature).
//!
//! Creates a [`MainWindow`] and [`Viewer`] without running the event loop, so
//! integration tests can drive navigation, zoom, and layout logic and read the
//! resulting window state directly. Viewer methods set their state immediately
//! (the `scrolled` callback that a live ListView would fire is not needed), so
//! assertions reflect the intended behavior.

use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};

use slint::Model;

use crate::render::{RenderControl, RenderRequest};
use crate::{MainWindow, Viewer};

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
