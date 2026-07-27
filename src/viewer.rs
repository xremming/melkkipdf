//! UI-thread view state: layout, zoom, spread/scroll modes, and the row model.
//!
//! Everything here runs on the UI thread, so interior mutability via `RefCell`
//! is enough — no locking. Pages are grouped into rows (one page, or two side by
//! side for a spread); the model holds rows, and only rendered pages carry an
//! image. Zoom is applied through the window's shared `density` property so a
//! zoom change writes one value instead of every row.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use slint::{ComponentHandle, Image, Model, ModelRc, VecModel, Weak};

use crate::render::RenderRequest;
use crate::{MainWindow, PageEntry, PageRow};

/// How pages are grouped into rows.
#[derive(Clone, Copy, PartialEq)]
enum Spread {
    /// One page per row.
    None,
    /// Two pages per row starting at the first page: [0,1] [2,3] …
    Odd,
    /// First page alone (centered), then pairs: [0] [1,2] [3,4] …
    Even,
}

/// How pages are scaled to the viewport when a fit mode is active.
#[derive(Clone, Copy, PartialEq)]
enum FitMode {
    Free,
    Width,
    Page,
}

/// The page indices making up one row.
#[derive(Clone, Copy)]
struct RowSpec {
    left: usize,
    right: Option<usize>,
}

const ZOOM_STEP: f32 = 1.25;
const MIN_ZOOM: f32 = 0.1;
const MAX_ZOOM: f32 = 8.0;
/// Logical pixels per point when zoom is "100%". 96/72 renders a point at one CSS
/// pixel's worth of density, a comfortable default reading size.
const BASE_DENSITY: f32 = 96.0 / 72.0;
/// Room left for the scrollbar and a small gutter when fitting.
const FIT_GUTTER: f32 = 24.0;
/// Vertical gap added to each row's height. Must match the `+ 16px` in the
/// `PageRowView` delegate so scroll-offset math matches the on-screen layout.
const ROW_GAP: f32 = 16.0;
/// Maximum number of rendered page images retained at once.
const MAX_RETAINED: usize = 24;

/// The 0/1/2 index used by the toolbar's spread radios.
fn spread_index(spread: Spread) -> i32 {
    match spread {
        Spread::None => 0,
        Spread::Odd => 1,
        Spread::Even => 2,
    }
}

/// Groups `page_count` pages into rows according to the spread mode.
fn build_row_specs(page_count: usize, spread: Spread) -> Vec<RowSpec> {
    let pair_from = |start: usize, rows: &mut Vec<RowSpec>| {
        let mut i = start;
        while i < page_count {
            let right = (i + 1 < page_count).then_some(i + 1);
            rows.push(RowSpec { left: i, right });
            i += 2;
        }
    };

    let mut rows = Vec::new();
    match spread {
        Spread::None => {
            for i in 0..page_count {
                rows.push(RowSpec { left: i, right: None });
            }
        }
        Spread::Odd => pair_from(0, &mut rows),
        Spread::Even => {
            if page_count > 0 {
                // First page sits alone (centered), then pairs.
                rows.push(RowSpec { left: 0, right: None });
                pair_from(1, &mut rows);
            }
        }
    }
    rows
}

/// Builds the reverse map from page index to (row index, is-right-of-spread).
fn page_locations(specs: &[RowSpec], page_count: usize) -> Vec<(usize, bool)> {
    let mut locations = vec![(0usize, false); page_count];
    for (row, spec) in specs.iter().enumerate() {
        locations[spec.left] = (row, false);
        if let Some(right) = spec.right {
            locations[right] = (row, true);
        }
    }
    locations
}

/// The largest row's width and height in points, used as the fit reference so no
/// row overflows the viewport.
fn reference_dims(specs: &[RowSpec], pages_pt: &[(f32, f32)]) -> (f32, f32) {
    let mut width = 0.0f32;
    let mut height = 0.0f32;
    for spec in specs {
        let (left_w, left_h) = pages_pt[spec.left];
        let (right_w, right_h) = spec.right.map_or((0.0, 0.0), |r| pages_pt[r]);
        width = width.max(left_w + right_w);
        height = height.max(left_h.max(right_h));
    }
    (width, height)
}

struct Inner {
    pages_pt: Vec<(f32, f32)>,
    zoom: f32,
    scale_factor: f32,
    view: Option<(f32, f32)>,
    fit: FitMode,
    retained: VecDeque<usize>,
    spread: Spread,
    continuous: bool,
    current_row: usize,
    specs: Vec<RowSpec>,
    page_loc: Vec<(usize, bool)>,
    ref_w_pt: f32,
    ref_h_pt: f32,
}

pub struct Viewer {
    inner: RefCell<Inner>,
    model: Rc<VecModel<PageRow>>,
    window: Weak<MainWindow>,
    sender: Sender<RenderRequest>,
}

impl Viewer {
    pub fn new(
        window: &MainWindow,
        pages_pt: Vec<(f32, f32)>,
        scale_factor: f32,
        sender: Sender<RenderRequest>,
    ) -> Rc<Self> {
        let model = Rc::new(VecModel::<PageRow>::default());
        window.set_rows(ModelRc::from(model.clone()));

        let viewer = Rc::new(Self {
            inner: RefCell::new(Inner {
                pages_pt,
                zoom: 1.0,
                scale_factor,
                view: None,
                fit: FitMode::Width,
                retained: VecDeque::new(),
                spread: Spread::None,
                continuous: true,
                current_row: 0,
                specs: Vec::new(),
                page_loc: Vec::new(),
                ref_w_pt: 0.0,
                ref_h_pt: 0.0,
            }),
            model,
            window: window.as_weak(),
            sender,
        });

        let page_count = viewer.inner.borrow().pages_pt.len() as i32;
        window.set_page_count(page_count);
        window.set_spread_mode(spread_index(Spread::None));

        viewer.build_layout();
        viewer.apply_density();
        viewer.update_status();
        viewer.update_current_page();
        viewer
    }

    /// Requests renders for every page in a row.
    pub fn request_render_row(&self, row: i32) {
        let inner = self.inner.borrow();
        let Some(spec) = inner.specs.get(row.max(0) as usize) else {
            return;
        };
        let scale = inner.zoom * BASE_DENSITY * inner.scale_factor;
        self.send(spec.left, scale);
        if let Some(right) = spec.right {
            self.send(right, scale);
        }
    }

    /// Installs a freshly rendered page image into its row, evicting the
    /// furthest-back images if we are over the retention budget.
    ///
    /// Borrows of `inner` are kept to short scopes: the model updates and the
    /// self-calls below (which borrow `inner` themselves) must not run while a
    /// borrow is held, or a re-entrant call panics.
    pub fn on_page_rendered(&self, page: i32, image: Image) {
        let index = page as usize;
        let (row_index, is_right, width_pt, height_pt, is_current_paged, evicted) = {
            let mut inner = self.inner.borrow_mut();
            let Some(&(row_index, is_right)) = inner.page_loc.get(index) else {
                return;
            };
            let (width_pt, height_pt) = inner.pages_pt[index];
            let is_current_paged = !inner.continuous && row_index == inner.current_row;

            inner.retained.push_back(index);
            let mut evicted = Vec::new();
            while inner.retained.len() > MAX_RETAINED {
                if let Some(candidate) = inner.retained.pop_front() {
                    if candidate != index && !inner.retained.contains(&candidate) {
                        evicted.push(candidate);
                    }
                }
            }
            (row_index, is_right, width_pt, height_pt, is_current_paged, evicted)
        };

        let entry = PageEntry { page, width_pt, height_pt, image };
        self.set_row_entry(row_index, is_right, entry);
        if is_current_paged {
            self.refresh_current_row();
        }
        for page in evicted {
            self.clear_page_image(page);
        }
    }

    /// Updates the remembered viewport size and refreshes the HiDPI scale factor,
    /// re-fitting or re-rendering as needed.
    pub fn set_viewport(&self, width: f32, height: f32) {
        let scale_factor = self
            .window
            .upgrade()
            .map(|window| window.window().scale_factor())
            .unwrap_or(1.0);

        let (fit_active, density_changed) = {
            let mut inner = self.inner.borrow_mut();
            inner.view = Some((width, height));
            let changed = (inner.scale_factor - scale_factor).abs() > 1e-3;
            inner.scale_factor = scale_factor;
            (inner.fit != FitMode::Free, changed)
        };

        if fit_active {
            self.apply_fit();
        } else if density_changed {
            self.rerender_loaded();
        }
    }

    pub fn zoom_in(&self) {
        let zoom = self.inner.borrow().zoom;
        self.set_zoom(zoom * ZOOM_STEP);
    }

    pub fn zoom_out(&self) {
        let zoom = self.inner.borrow().zoom;
        self.set_zoom(zoom / ZOOM_STEP);
    }

    pub fn zoom_reset(&self) {
        self.set_zoom(1.0);
    }

    pub fn fit_width(&self) {
        self.inner.borrow_mut().fit = FitMode::Width;
        self.apply_fit();
    }

    pub fn fit_page(&self) {
        self.inner.borrow_mut().fit = FitMode::Page;
        self.apply_fit();
    }

    /// Selects continuous scroll or one-row-per-screen. Paged mode starts at the
    /// row the continuous view was scrolled to, so switching keeps the position.
    pub fn set_continuous(&self, continuous: bool) {
        if self.inner.borrow().continuous == continuous {
            return;
        }
        self.inner.borrow_mut().continuous = continuous;
        if let Some(window) = self.window.upgrade() {
            window.set_continuous(continuous);
        }
        if !continuous {
            self.refresh_current_row();
            self.request_current_row();
        }
        self.reapply_scale();
        self.update_status();
        self.update_current_page();
    }

    pub fn toggle_continuous(&self) {
        let continuous = self.inner.borrow().continuous;
        self.set_continuous(!continuous);
    }

    /// Sets the spread mode (0 = single, 1 = odd, 2 = even) and rebuilds rows.
    pub fn set_spread(&self, mode: i32) {
        let spread = match mode {
            1 => Spread::Odd,
            2 => Spread::Even,
            _ => Spread::None,
        };
        self.inner.borrow_mut().spread = spread;
        if let Some(window) = self.window.upgrade() {
            window.set_spread_mode(spread_index(spread));
        }
        self.build_layout();
        self.reapply_scale();
        self.update_status();
        self.update_current_page();
    }

    /// Reports the continuous scroll offset (logical pixels from the top) so the
    /// current page can be tracked. Rows are uniform height, so the top row is an
    /// exact division.
    pub fn scrolled(&self, offset: f32) {
        {
            let mut inner = self.inner.borrow_mut();
            if inner.specs.is_empty() {
                return;
            }
            let row_height = inner.ref_h_pt * BASE_DENSITY * inner.zoom + ROW_GAP;
            if row_height <= 0.0 {
                return;
            }
            let row = (offset / row_height).round().max(0.0) as usize;
            inner.current_row = row.min(inner.specs.len() - 1);
        }
        self.update_current_page();
    }

    /// Jumps to a 1-based page typed into the toolbar field. Invalid input is
    /// ignored. In continuous mode this scrolls the ListView (rows are uniform
    /// height, so the target offset is exact); in paged mode it swaps the row.
    pub fn go_to_page(&self, text: &str) {
        let Ok(requested) = text.trim().parse::<i32>() else {
            return;
        };

        let (row, continuous, scroll_target) = {
            let mut inner = self.inner.borrow_mut();
            if inner.page_loc.is_empty() {
                return;
            }
            let page = (requested - 1).clamp(0, inner.page_loc.len() as i32 - 1) as usize;
            let row = inner.page_loc[page].0;
            inner.current_row = row;
            let row_height = inner.ref_h_pt * BASE_DENSITY * inner.zoom + ROW_GAP;
            (row, inner.continuous, -(row as f32) * row_height)
        };

        if continuous {
            if let Some(window) = self.window.upgrade() {
                window.set_scroll_y(scroll_target);
            }
            self.request_render_row(row as i32);
        } else {
            self.refresh_current_row();
            self.request_current_row();
        }
        self.update_current_page();
    }

    pub fn next_row(&self) {
        if !self.inner.borrow().continuous {
            let current = self.inner.borrow().current_row as i32;
            self.go_to_row(current + 1);
        }
    }

    pub fn prev_row(&self) {
        if !self.inner.borrow().continuous {
            let current = self.inner.borrow().current_row as i32;
            self.go_to_row(current - 1);
        }
    }

    fn set_zoom(&self, zoom: f32) {
        {
            let mut inner = self.inner.borrow_mut();
            inner.fit = FitMode::Free;
            inner.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        }
        self.apply_density();
        self.rerender_loaded();
        self.update_status();
    }

    fn apply_fit(&self) {
        let recomputed = {
            let inner = self.inner.borrow();
            let Some((view_w, view_h)) = inner.view else {
                return;
            };
            if inner.ref_w_pt <= 0.0 || inner.ref_h_pt <= 0.0 {
                return;
            }
            let width_zoom = (view_w - FIT_GUTTER).max(1.0) / (inner.ref_w_pt * BASE_DENSITY);
            match inner.fit {
                FitMode::Width => width_zoom,
                FitMode::Page => {
                    width_zoom.min((view_h - FIT_GUTTER).max(1.0) / (inner.ref_h_pt * BASE_DENSITY))
                }
                FitMode::Free => return,
            }
            .clamp(MIN_ZOOM, MAX_ZOOM)
        };

        self.inner.borrow_mut().zoom = recomputed;
        self.apply_density();
        self.rerender_loaded();
        self.update_status();
    }

    /// Re-applies the current zoom after a layout change: re-fit if a fit mode is
    /// active, otherwise just push the density.
    fn reapply_scale(&self) {
        if self.inner.borrow().fit != FitMode::Free {
            self.apply_fit();
        } else {
            self.apply_density();
        }
    }

    /// Rebuilds rows for the current spread mode and installs a fresh model.
    fn build_layout(&self) {
        {
            let mut inner = self.inner.borrow_mut();
            let count = inner.pages_pt.len();
            inner.specs = build_row_specs(count, inner.spread);
            inner.page_loc = page_locations(&inner.specs, count);
            let (ref_w, ref_h) = reference_dims(&inner.specs, &inner.pages_pt);
            inner.ref_w_pt = ref_w;
            inner.ref_h_pt = ref_h;
            inner.retained.clear();
            let last_row = inner.specs.len().saturating_sub(1);
            inner.current_row = inner.current_row.min(last_row);
        }

        let rows = self.build_model_rows();
        self.model.set_vec(rows);
        if let Some(window) = self.window.upgrade() {
            window.set_row_height_pt(self.inner.borrow().ref_h_pt);
        }
        self.refresh_current_row();
        self.request_current_row_if_paged();
        self.update_current_page();
    }

    fn build_model_rows(&self) -> Vec<PageRow> {
        let inner = self.inner.borrow();
        inner
            .specs
            .iter()
            .map(|spec| {
                let left = Self::empty_page(&inner, spec.left);
                match spec.right {
                    Some(right) => PageRow {
                        left,
                        right: Self::empty_page(&inner, right),
                        has_right: true,
                    },
                    None => PageRow { left, right: Self::placeholder(), has_right: false },
                }
            })
            .collect()
    }

    /// An unrendered slot carrying only the page's fixed size.
    fn empty_page(inner: &Inner, page: usize) -> PageEntry {
        let (width_pt, height_pt) = inner.pages_pt[page];
        PageEntry { page: page as i32, width_pt, height_pt, image: Image::default() }
    }

    /// A dummy entry for the unused right half of a single-page row.
    fn placeholder() -> PageEntry {
        PageEntry { page: -1, width_pt: 0.0, height_pt: 0.0, image: Image::default() }
    }

    /// Writes one page's entry into its row's left or right slot.
    fn set_row_entry(&self, row_index: usize, is_right: bool, entry: PageEntry) {
        let Some(mut row) = self.model.row_data(row_index) else {
            return;
        };
        if is_right {
            row.right = entry;
        } else {
            row.left = entry;
        }
        self.model.set_row_data(row_index, row);
    }

    /// Drops a page's image, keeping its geometry so layout is unchanged and it
    /// re-renders when scrolled back into view. Uses only short borrows so it is
    /// safe to call from anywhere.
    fn clear_page_image(&self, page: usize) {
        let (row_index, is_right, entry, is_current_paged) = {
            let inner = self.inner.borrow();
            let Some(&(row_index, is_right)) = inner.page_loc.get(page) else {
                return;
            };
            let entry = Self::empty_page(&inner, page);
            let is_current_paged = !inner.continuous && row_index == inner.current_row;
            (row_index, is_right, entry, is_current_paged)
        };
        self.set_row_entry(row_index, is_right, entry);
        if is_current_paged {
            self.refresh_current_row();
        }
    }

    fn go_to_row(&self, target: i32) {
        let changed = {
            let mut inner = self.inner.borrow_mut();
            let count = inner.specs.len() as i32;
            if count == 0 {
                return;
            }
            let clamped = target.clamp(0, count - 1) as usize;
            let changed = clamped != inner.current_row;
            inner.current_row = clamped;
            changed
        };
        if changed {
            self.refresh_current_row();
            self.request_current_row();
            self.update_status();
            self.update_current_page();
        }
    }

    /// Pushes the current row's data to the paged view.
    fn refresh_current_row(&self) {
        let index = self.inner.borrow().current_row;
        if let (Some(window), Some(row)) = (self.window.upgrade(), self.model.row_data(index)) {
            window.set_current_row_content(row);
        }
    }

    fn request_current_row(&self) {
        let row = self.inner.borrow().current_row as i32;
        self.request_render_row(row);
    }

    fn request_current_row_if_paged(&self) {
        if !self.inner.borrow().continuous {
            self.request_current_row();
        }
    }

    fn send(&self, page: usize, scale: f32) {
        let _ = self.sender.send(RenderRequest { page: page as i32, scale });
    }

    fn apply_density(&self) {
        let density = BASE_DENSITY * self.inner.borrow().zoom;
        if let Some(window) = self.window.upgrade() {
            window.set_density(density);
        }
    }

    /// Re-requests every page that currently holds an image, so displayed pages
    /// re-render crisply at the new scale. Driven from Rust (rather than a
    /// per-delegate change handler) to keep the delegates side-effect-free.
    fn rerender_loaded(&self) {
        let (pages, scale) = {
            let inner = self.inner.borrow();
            let scale = inner.zoom * BASE_DENSITY * inner.scale_factor;
            (inner.retained.iter().copied().collect::<Vec<_>>(), scale)
        };
        for page in pages {
            self.send(page, scale);
        }
        self.request_current_row_if_paged();
    }

    fn update_status(&self) {
        let inner = self.inner.borrow();
        let percent = (inner.zoom * 100.0).round() as i32;
        let spread = match inner.spread {
            Spread::None => "single",
            Spread::Odd => "odd spread",
            Spread::Even => "even spread",
        };
        let status = if inner.continuous {
            format!("{} pages · {percent}% · continuous · {spread}", inner.pages_pt.len())
        } else {
            format!(
                "{} pages · {percent}% · paged {}/{} · {spread}",
                inner.pages_pt.len(),
                inner.current_row + 1,
                inner.specs.len().max(1),
            )
        };
        if let Some(window) = self.window.upgrade() {
            window.set_status(status.into());
        }
    }

    /// Publishes the 1-based page number at the top of the view for the toolbar.
    fn update_current_page(&self) {
        let page = {
            let inner = self.inner.borrow();
            inner
                .specs
                .get(inner.current_row)
                .map_or(0, |spec| spec.left as i32 + 1)
        };
        if let Some(window) = self.window.upgrade() {
            window.set_current_page(page);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Spread, Viewer, build_row_specs, page_locations};
    use crate::MainWindow;
    use slint::ComponentHandle;

    fn as_pairs(specs: &[super::RowSpec]) -> Vec<(usize, Option<usize>)> {
        specs.iter().map(|s| (s.left, s.right)).collect()
    }

    /// Regression: a page arriving in paged mode used to re-enter a held borrow
    /// of `inner` via `refresh_current_row` and panic. Drive that exact path.
    /// Skips silently if no windowing backend is available in the test runner.
    #[test]
    fn paged_render_does_not_reenter_borrow() {
        let Ok(window) = MainWindow::new() else {
            return;
        };
        let (sender, _receiver) = std::sync::mpsc::channel();
        let pages = vec![(600.0, 800.0); 5];
        let viewer = Viewer::new(&window, pages, 1.0, sender);

        // Switch to paged mode, then deliver renders — including enough to force
        // the eviction path, which also calls back into the viewer.
        viewer.toggle_continuous();
        for page in 0..5 {
            viewer.on_page_rendered(page, slint::Image::default());
        }
        viewer.next_row();
        viewer.on_page_rendered(2, slint::Image::default());
    }

    /// go_to_page parses, clamps, and reports the resulting page.
    #[test]
    fn go_to_page_parses_and_clamps() {
        let Ok(window) = MainWindow::new() else {
            return;
        };
        let (sender, _receiver) = std::sync::mpsc::channel();
        let viewer = Viewer::new(&window, vec![(600.0, 800.0); 10], 1.0, sender);
        // Paged mode avoids touching the scroll offset property.
        viewer.set_continuous(false);

        viewer.go_to_page("4");
        assert_eq!(window.get_current_page(), 4);

        // Out of range clamps to the last page.
        viewer.go_to_page("999");
        assert_eq!(window.get_current_page(), 10);

        // Non-numeric input is ignored (page unchanged).
        viewer.go_to_page("abc");
        assert_eq!(window.get_current_page(), 10);
    }

    #[test]
    fn no_spreads_is_one_page_per_row() {
        let specs = build_row_specs(3, Spread::None);
        assert_eq!(as_pairs(&specs), vec![(0, None), (1, None), (2, None)]);
    }

    #[test]
    fn odd_spreads_pair_from_the_first_page() {
        let specs = build_row_specs(5, Spread::Odd);
        // [0,1] [2,3] [4]
        assert_eq!(as_pairs(&specs), vec![(0, Some(1)), (2, Some(3)), (4, None)]);
    }

    #[test]
    fn even_spreads_keep_the_first_page_alone() {
        let specs = build_row_specs(5, Spread::Even);
        // [0] [1,2] [3,4]
        assert_eq!(as_pairs(&specs), vec![(0, None), (1, Some(2)), (3, Some(4))]);
    }

    #[test]
    fn locations_map_pages_back_to_rows() {
        let specs = build_row_specs(5, Spread::Even);
        let loc = page_locations(&specs, 5);
        // page 0 -> row 0 left; page 2 -> row 1 right; page 3 -> row 2 left.
        assert_eq!(loc[0], (0, false));
        assert_eq!(loc[1], (1, false));
        assert_eq!(loc[2], (1, true));
        assert_eq!(loc[3], (2, false));
        assert_eq!(loc[4], (2, true));
    }

    #[test]
    fn handles_empty_and_single_page_documents() {
        assert!(build_row_specs(0, Spread::Even).is_empty());
        assert_eq!(as_pairs(&build_row_specs(1, Spread::Odd)), vec![(0, None)]);
        assert_eq!(as_pairs(&build_row_specs(1, Spread::Even)), vec![(0, None)]);
    }
}
