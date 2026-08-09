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

use crate::render::{RenderControl, RenderRequest};
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
/// How far an arrow key scrolls when the page is taller than the viewport.
const SCROLL_STEP: f32 = 120.0;
/// Rows to prefetch on either side of the visible range while idle.
const PREFETCH_ROWS: usize = 4;
/// Maximum number of rendered page images retained at once.
const MAX_RETAINED: usize = 48;

/// The 0/1/2 index used by the toolbar's spread radios.
fn spread_index(spread: Spread) -> i32 {
    match spread {
        Spread::None => 0,
        Spread::Odd => 1,
        Spread::Even => 2,
    }
}

/// The on-screen height of one row (all rows are uniform) in logical pixels.
fn row_height_px(inner: &Inner) -> f32 {
    inner.ref_h_pt * BASE_DENSITY * inner.zoom + ROW_GAP
}

/// The largest valid continuous scroll offset in logical pixels.
fn max_scroll_px(inner: &Inner) -> f32 {
    let view_height = inner.view.map_or(0.0, |(_, h)| h);
    (inner.specs.len() as f32 * row_height_px(inner) - view_height).max(0.0)
}

/// Horizontal gap between the two pages of a spread, in logical pixels. Must
/// match the `spacing` of the paged content layout in the `.slint` file.
const SPREAD_SPACING: f32 = 4.0;

/// The current paged row's rendered content size (width, height) in logical
/// pixels: one page, or two side by side for a spread.
fn paged_content_size(inner: &Inner) -> (f32, f32) {
    let Some(spec) = inner.specs.get(inner.current_row) else {
        return (0.0, 0.0);
    };
    let density = BASE_DENSITY * inner.zoom;
    let (left_w, left_h) = inner.pages_pt[spec.left];
    let (right_w, right_h, spacing) = match spec.right {
        Some(right) => {
            let (w, h) = inner.pages_pt[right];
            (w, h, SPREAD_SPACING)
        }
        None => (0.0, 0.0, 0.0),
    };
    ((left_w + right_w) * density + spacing, left_h.max(right_h) * density)
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
    /// Current continuous scroll offset from the top, in logical pixels.
    scroll_px: f32,
    /// Scroll offset within the current page in paged mode (logical pixels).
    paged_scroll_x: f32,
    paged_scroll_y: f32,
    /// View epoch, bumped whenever the visible set changes, so the render worker
    /// can drop requests from earlier views.
    generation: u64,
    specs: Vec<RowSpec>,
    page_loc: Vec<(usize, bool)>,
    ref_w_pt: f32,
    ref_h_pt: f32,
    /// Rendered sidebar thumbnails, indexed by page (empty until rendered).
    thumb_images: Vec<Image>,
}

pub struct Viewer {
    inner: RefCell<Inner>,
    model: Rc<VecModel<PageRow>>,
    thumb_model: Rc<VecModel<PageRow>>,
    window: Weak<MainWindow>,
    sender: Sender<RenderRequest>,
    thumb_sender: Sender<i32>,
    control: RenderControl,
}

impl Viewer {
    pub fn new(
        window: &MainWindow,
        pages_pt: Vec<(f32, f32)>,
        scale_factor: f32,
        sender: Sender<RenderRequest>,
        thumb_sender: Sender<i32>,
        control: RenderControl,
    ) -> Rc<Self> {
        let page_count = pages_pt.len();
        let model = Rc::new(VecModel::<PageRow>::default());
        window.set_rows(ModelRc::from(model.clone()));
        let thumb_model = Rc::new(VecModel::<PageRow>::default());
        window.set_thumb_rows(ModelRc::from(thumb_model.clone()));

        let viewer = Rc::new(Self {
            inner: RefCell::new(Inner {
                pages_pt,
                zoom: 1.0,
                scale_factor,
                view: None,
                fit: FitMode::Page,
                retained: VecDeque::new(),
                spread: Spread::None,
                continuous: true,
                current_row: 0,
                scroll_px: 0.0,
                paged_scroll_x: 0.0,
                paged_scroll_y: 0.0,
                generation: 0,
                specs: Vec::new(),
                page_loc: Vec::new(),
                ref_w_pt: 0.0,
                ref_h_pt: 0.0,
                thumb_images: vec![Image::default(); page_count],
            }),
            model,
            thumb_model,
            window: window.as_weak(),
            sender,
            thumb_sender,
            control,
        });

        window.set_page_count(page_count as i32);
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
        self.send(spec.left, scale, false);
        if let Some(right) = spec.right {
            self.send(right, scale, false);
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
                if let Some(candidate) = inner.retained.pop_front()
                    && candidate != index
                    && !inner.retained.contains(&candidate)
                {
                    evicted.push(candidate);
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
        let scale_factor =
            self.window.upgrade().map(|window| window.window().scale_factor()).unwrap_or(1.0);

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
            self.rerender_view();
        }
        // Viewport size affects paged centering and scroll limits.
        self.push_paged_offsets();
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

    /// Selects continuous scroll or one-row-per-screen. Either direction keeps
    /// the reading position: paged mode tracks `current_row` (set on scroll), and
    /// returning to continuous scrolls the list back to that same row.
    pub fn set_continuous(&self, continuous: bool) {
        if self.inner.borrow().continuous == continuous {
            return;
        }
        self.inner.borrow_mut().continuous = continuous;
        if let Some(window) = self.window.upgrade() {
            window.set_continuous(continuous);
        }
        if continuous {
            // Align the continuous scroll offset to the row paged mode left off
            // on, otherwise the list snaps back to its previous position.
            let target_px = {
                let mut inner = self.inner.borrow_mut();
                let target = (inner.current_row as f32 * row_height_px(&inner))
                    .clamp(0.0, max_scroll_px(&inner));
                inner.scroll_px = target;
                target
            };
            if let Some(window) = self.window.upgrade() {
                window.set_scroll_y(-target_px);
            }
        } else {
            // A freshly shown page starts at its top.
            {
                let mut inner = self.inner.borrow_mut();
                inner.paged_scroll_x = 0.0;
                inner.paged_scroll_y = 0.0;
            }
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
            inner.scroll_px = offset.max(0.0);
            let row_height = row_height_px(&inner);
            if row_height <= 0.0 {
                return;
            }
            let row = (offset / row_height).round().max(0.0) as usize;
            inner.current_row = row.min(inner.specs.len() - 1);
        }
        self.update_current_page();
        self.request_visible(false);
    }

    /// Requests the visible rows (and a prefetch margin), top-first. On scroll,
    /// pass `force = false` to skip rows already rendered; on zoom (a new scale),
    /// pass `force = true` to re-render everything visible. Also covers eviction:
    /// a delegate's `init` fires once and can't re-request a page cleared later.
    fn request_visible(&self, force: bool) {
        self.advance_epoch();
        let (visible, prefetch, scale) = {
            let inner = self.inner.borrow();
            if !inner.continuous || inner.specs.is_empty() {
                return;
            }
            let row_height = row_height_px(&inner);
            let view_height = inner.view.map_or(0.0, |(_, h)| h);
            if row_height <= 0.0 {
                return;
            }
            let top = (inner.scroll_px / row_height).floor().max(0.0) as usize;
            let span = (view_height / row_height).ceil() as usize + 1;
            let last = inner.specs.len() - 1;
            let visible_end = (top + span).min(last);

            let rendered: std::collections::HashSet<usize> =
                inner.retained.iter().copied().collect();
            let needs = |row: usize| {
                if force {
                    return true;
                }
                let spec = &inner.specs[row];
                !rendered.contains(&spec.left)
                    || spec.right.is_some_and(|right| !rendered.contains(&right))
            };

            let visible: Vec<i32> =
                (top..=visible_end).filter(|&r| needs(r)).map(|r| r as i32).collect();

            // Prefetch a few rows on either side of the visible range.
            let below = (visible_end + PREFETCH_ROWS).min(last);
            let above = top.saturating_sub(PREFETCH_ROWS);
            let mut prefetch: Vec<i32> = Vec::new();
            for row in (visible_end + 1)..=below {
                if needs(row) {
                    prefetch.push(row as i32);
                }
            }
            for row in above..top {
                if needs(row) {
                    prefetch.push(row as i32);
                }
            }

            let scale = inner.zoom * BASE_DENSITY * inner.scale_factor;
            (visible, prefetch, scale)
        };
        for row in visible {
            self.send_row(row, scale, false);
        }
        for row in prefetch {
            self.send_row(row, scale, true);
        }
    }

    /// Sends render requests for a row's pages at an explicit scale.
    fn send_row(&self, row: i32, scale: f32, prefetch: bool) {
        let inner = self.inner.borrow();
        if let Some(spec) = inner.specs.get(row.max(0) as usize) {
            self.send(spec.left, scale, prefetch);
            if let Some(right) = spec.right {
                self.send(right, scale, prefetch);
            }
        }
    }

    /// Jumps to a 1-based page typed into the toolbar field. Invalid input is
    /// ignored.
    pub fn go_to_page(&self, text: &str) {
        if let Ok(requested) = text.trim().parse::<i32>() {
            self.nav_to_page(requested - 1);
        }
    }

    /// Navigates to a 0-based page (from the outline or a thumbnail click).
    pub fn nav_to_page(&self, page: i32) {
        let row = {
            let inner = self.inner.borrow();
            if inner.page_loc.is_empty() {
                return;
            }
            let page = page.clamp(0, inner.page_loc.len() as i32 - 1) as usize;
            inner.page_loc[page].0
        };
        self.scroll_to_row(row);
    }

    /// Installs a rendered thumbnail into the sidebar's thumbnail model.
    pub fn on_thumbnail_rendered(&self, page: i32, image: Image) {
        let index = page as usize;
        let (row, is_right, width_pt, height_pt) = {
            let mut inner = self.inner.borrow_mut();
            let Some(&(row, is_right)) = inner.page_loc.get(index) else {
                return;
            };
            if index < inner.thumb_images.len() {
                inner.thumb_images[index] = image.clone();
            }
            let (width_pt, height_pt) = inner.pages_pt[index];
            (row, is_right, width_pt, height_pt)
        };
        if let Some(mut page_row) = self.thumb_model.row_data(row) {
            let entry = PageEntry { page, width_pt, height_pt, image };
            if is_right {
                page_row.right = entry;
            } else {
                page_row.left = entry;
            }
            self.thumb_model.set_row_data(row, page_row);
        }
    }

    /// Requests thumbnails for a visible thumbnail row's pages. The thumbnail
    /// worker renders each page at most once, so re-requests are cheap.
    pub fn request_thumbnail_row(&self, row: i32) {
        let inner = self.inner.borrow();
        if let Some(spec) = inner.specs.get(row.max(0) as usize) {
            let _ = self.thumb_sender.send(spec.left as i32);
            if let Some(right) = spec.right {
                let _ = self.thumb_sender.send(right as i32);
            }
        }
    }

    /// Rebuilds the thumbnail model from the current spread specs, reusing any
    /// already-rendered thumbnails.
    fn build_thumb_model(&self) {
        let rows: Vec<PageRow> = {
            let inner = self.inner.borrow();
            inner
                .specs
                .iter()
                .map(|spec| {
                    let left = Self::thumb_entry(&inner, spec.left);
                    match spec.right {
                        Some(right) => PageRow {
                            left,
                            right: Self::thumb_entry(&inner, right),
                            has_right: true,
                        },
                        None => PageRow { left, right: Self::placeholder(), has_right: false },
                    }
                })
                .collect()
        };
        self.thumb_model.set_vec(rows);
    }

    /// A thumbnail slot carrying the page's size and (possibly empty) thumbnail.
    fn thumb_entry(inner: &Inner, page: usize) -> PageEntry {
        let (width_pt, height_pt) = inner.pages_pt[page];
        PageEntry {
            page: page as i32,
            width_pt,
            height_pt,
            image: inner.thumb_images[page].clone(),
        }
    }

    /// Arrow up/down (dir -1/+1). Continuous mode always scrolls; paged mode
    /// scrolls within a tall page and moves to the next page at the edge.
    pub fn nav_line(&self, dir: i32) {
        if self.inner.borrow().continuous {
            self.scroll_by(dir as f32 * SCROLL_STEP);
        } else {
            // A downward step (dir +1) carries a negative wheel delta.
            self.paged_scroll(0.0, -(dir as f32) * SCROLL_STEP, false);
        }
    }

    /// Page up/down (dir -1/+1): always to the start of the previous/next page.
    pub fn nav_page(&self, dir: i32) {
        self.page_jump(dir);
    }

    /// Home: start of the first page.
    pub fn nav_home(&self) {
        self.scroll_to_row(0);
    }

    /// End: the very bottom of the document, so the last page is fully visible.
    pub fn nav_end(&self) {
        let (continuous, target_px) = {
            let mut inner = self.inner.borrow_mut();
            if inner.specs.is_empty() {
                return;
            }
            inner.current_row = inner.specs.len() - 1;
            // Continuous scrolls to the maximum offset (document bottom); paged
            // just shows the last row.
            let target = if inner.continuous { max_scroll_px(&inner) } else { 0.0 };
            inner.scroll_px = target;
            (inner.continuous, target)
        };
        if continuous {
            if let Some(window) = self.window.upgrade() {
                window.set_scroll_y(-target_px);
            }
            self.request_current_row();
        } else {
            {
                let mut inner = self.inner.borrow_mut();
                inner.paged_scroll_x = 0.0;
                inner.paged_scroll_y = 0.0;
            }
            self.refresh_current_row();
            self.request_current_row();
            self.push_paged_offsets();
        }
        self.update_current_page();
    }

    /// Paged-mode wheel handling: scroll within the current page, and move to the
    /// previous/next page only once the top/bottom edge is reached. Shift makes a
    /// vertical wheel scroll horizontally.
    pub fn paged_scroll(&self, delta_x: f32, delta_y: f32, shift: bool) {
        // A downward/rightward wheel carries a negative delta; scrolling in that
        // direction increases the offset.
        let (horizontal, vertical) = if shift { (-delta_y, 0.0) } else { (-delta_x, -delta_y) };

        let jump = {
            let mut inner = self.inner.borrow_mut();
            let (content_w, content_h) = paged_content_size(&inner);
            let (view_w, view_h) = inner.view.unwrap_or((0.0, 0.0));
            let max_x = (content_w - view_w).max(0.0);
            let max_y = (content_h - view_h).max(0.0);

            inner.paged_scroll_x = (inner.paged_scroll_x + horizontal).clamp(0.0, max_x);

            if vertical > 0.0 && inner.paged_scroll_y >= max_y - 0.5 {
                1 // at the bottom, scrolling down: next page
            } else if vertical < 0.0 && inner.paged_scroll_y <= 0.5 {
                -1 // at the top, scrolling up: previous page
            } else {
                inner.paged_scroll_y = (inner.paged_scroll_y + vertical).clamp(0.0, max_y);
                0
            }
        };

        if jump != 0 {
            self.paged_step_page(jump);
        } else {
            self.push_paged_offsets();
        }
    }

    /// Moves to the adjacent page in paged mode, landing at the top when moving
    /// forward and at the bottom when moving back, so scrolling reads as one
    /// continuous flow across the page boundary.
    fn paged_step_page(&self, dir: i32) {
        let changed = {
            let mut inner = self.inner.borrow_mut();
            let last = inner.specs.len().saturating_sub(1) as i32;
            let target = (inner.current_row as i32 + dir).clamp(0, last) as usize;
            if target == inner.current_row {
                return; // at the first/last page already
            }
            inner.current_row = target;
            inner.paged_scroll_x = 0.0;
            let (_, content_h) = paged_content_size(&inner);
            let view_h = inner.view.map_or(0.0, |(_, h)| h);
            inner.paged_scroll_y = if dir < 0 { (content_h - view_h).max(0.0) } else { 0.0 };
            true
        };
        if changed {
            self.refresh_current_row();
            self.request_current_row();
            self.push_paged_offsets();
            self.update_current_page();
        }
    }

    /// Clamps the paged scroll offset to the current page and content size and
    /// publishes the resulting content geometry to the window.
    fn push_paged_offsets(&self) {
        let (offset_x, offset_y, content_w, content_h) = {
            let mut inner = self.inner.borrow_mut();
            let (content_w, content_h) = paged_content_size(&inner);
            let (view_w, view_h) = inner.view.unwrap_or((0.0, 0.0));
            let max_x = (content_w - view_w).max(0.0);
            let max_y = (content_h - view_h).max(0.0);
            inner.paged_scroll_x = inner.paged_scroll_x.clamp(0.0, max_x);
            inner.paged_scroll_y = inner.paged_scroll_y.clamp(0.0, max_y);
            // Center each axis when the content is smaller than the viewport,
            // otherwise offset by the scroll position.
            let offset_x = if content_w <= view_w {
                (view_w - content_w) / 2.0
            } else {
                -inner.paged_scroll_x
            };
            let offset_y = if content_h <= view_h {
                (view_h - content_h) / 2.0
            } else {
                -inner.paged_scroll_y
            };
            (offset_x, offset_y, content_w, content_h)
        };
        if let Some(window) = self.window.upgrade() {
            window.set_paged_offset_x(offset_x);
            window.set_paged_offset_y(offset_y);
            window.set_paged_content_w(content_w);
            window.set_paged_content_h(content_h);
        }
    }

    /// Moves to the previous/next page boundary from the current position.
    fn page_jump(&self, dir: i32) {
        let target = {
            let inner = self.inner.borrow();
            if inner.specs.is_empty() {
                return;
            }
            let last = inner.specs.len() as i32 - 1;
            let row = if inner.continuous {
                let row_height = row_height_px(&inner);
                let ratio = if row_height > 0.0 { inner.scroll_px / row_height } else { 0.0 };
                let floor = ratio.floor();
                if dir > 0 {
                    floor as i32 + 1
                } else if ratio - floor > 0.01 {
                    // Scrolled partway into a page: snap to that page's start.
                    floor as i32
                } else {
                    floor as i32 - 1
                }
            } else {
                inner.current_row as i32 + dir
            };
            row.clamp(0, last)
        };
        self.scroll_to_row(target as usize);
    }

    /// Adjusts the continuous scroll offset by a delta, clamped to the document.
    fn scroll_by(&self, delta_px: f32) {
        let target = {
            let mut inner = self.inner.borrow_mut();
            let target = (inner.scroll_px + delta_px).clamp(0.0, max_scroll_px(&inner));
            inner.scroll_px = target;
            target
        };
        if let Some(window) = self.window.upgrade() {
            window.set_scroll_y(-target);
        }
        self.update_current_page();
    }

    /// Moves so the given row is at the top: scrolls the ListView in continuous
    /// mode, or swaps the shown row in paged mode.
    fn scroll_to_row(&self, row: usize) {
        let (continuous, target_px) = {
            let mut inner = self.inner.borrow_mut();
            if inner.specs.is_empty() {
                return;
            }
            let row = row.min(inner.specs.len() - 1);
            inner.current_row = row;
            let target_px = row as f32 * row_height_px(&inner);
            inner.scroll_px = target_px;
            (inner.continuous, target_px)
        };
        if continuous {
            if let Some(window) = self.window.upgrade() {
                window.set_scroll_y(-target_px);
            }
            self.request_current_row();
        } else {
            // A new page starts scrolled to the top.
            {
                let mut inner = self.inner.borrow_mut();
                inner.paged_scroll_x = 0.0;
                inner.paged_scroll_y = 0.0;
            }
            self.refresh_current_row();
            self.request_current_row();
            self.push_paged_offsets();
        }
        self.update_current_page();
    }

    fn set_zoom(&self, zoom: f32) {
        {
            let mut inner = self.inner.borrow_mut();
            inner.fit = FitMode::Free;
            inner.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        }
        self.apply_density();
        self.rerender_view();
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
        self.rerender_view();
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
        self.push_paged_offsets();
        self.build_thumb_model();
    }

    fn build_model_rows(&self) -> Vec<PageRow> {
        let inner = self.inner.borrow();
        inner
            .specs
            .iter()
            .map(|spec| {
                let left = Self::empty_page(&inner, spec.left);
                match spec.right {
                    Some(right) => {
                        PageRow { left, right: Self::empty_page(&inner, right), has_right: true }
                    }
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

    /// Pushes the current row's data to the paged view.
    fn refresh_current_row(&self) {
        let index = self.inner.borrow().current_row;
        if let (Some(window), Some(row)) = (self.window.upgrade(), self.model.row_data(index)) {
            window.set_current_row_content(row);
        }
    }

    /// Requests the current row (high priority) plus a few neighbors on each
    /// side as prefetch. Used by paged mode and one-shot navigation.
    fn request_current_row(&self) {
        self.advance_epoch();
        let (current, neighbors, scale) = {
            let inner = self.inner.borrow();
            if inner.specs.is_empty() {
                return;
            }
            let current = inner.current_row;
            let last = inner.specs.len() - 1;
            let rendered: std::collections::HashSet<usize> =
                inner.retained.iter().copied().collect();
            let needs = |row: usize| {
                let spec = &inner.specs[row];
                !rendered.contains(&spec.left)
                    || spec.right.is_some_and(|right| !rendered.contains(&right))
            };
            let start = current.saturating_sub(PREFETCH_ROWS);
            let end = (current + PREFETCH_ROWS).min(last);
            let neighbors: Vec<i32> = (start..=end)
                .filter(|&row| row != current && needs(row))
                .map(|row| row as i32)
                .collect();
            let scale = inner.zoom * BASE_DENSITY * inner.scale_factor;
            (current as i32, neighbors, scale)
        };
        self.send_row(current, scale, false);
        for row in neighbors {
            self.send_row(row, scale, true);
        }
    }

    fn request_current_row_if_paged(&self) {
        if !self.inner.borrow().continuous {
            self.request_current_row();
        }
    }

    fn send(&self, page: usize, scale: f32, prefetch: bool) {
        let generation = self.inner.borrow().generation;
        let _ = self.sender.send(RenderRequest { page: page as i32, scale, generation, prefetch });
    }

    /// Marks a new view: bumped before issuing a fresh set of render requests so
    /// the worker drops (and aborts in-progress) renders from the previous view.
    fn advance_epoch(&self) {
        let epoch = {
            let mut inner = self.inner.borrow_mut();
            inner.generation += 1;
            inner.generation
        };
        self.control.advance(epoch);
    }

    fn apply_density(&self) {
        let density = BASE_DENSITY * self.inner.borrow().zoom;
        if let Some(window) = self.window.upgrade() {
            window.set_density(density);
        }
        // Zoom changes the paged content size; keep its offsets in range.
        self.push_paged_offsets();
    }

    /// Re-renders the current view at a new scale (after zoom/fit) or the initial
    /// view on load. Driven from Rust (rather than a per-delegate change handler)
    /// to keep the delegates side-effect-free. Requesting the visible range here
    /// — not just already-rendered pages — is also what renders the first page on
    /// startup, when nothing has been rendered yet.
    fn rerender_view(&self) {
        if self.inner.borrow().continuous {
            self.request_visible(true);
        } else {
            self.request_current_row();
        }
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

    /// Publishes the page number shown by the toolbar. Normally the page at the
    /// top of the view; at the very bottom of the document it reports the last
    /// page, so "End" reads as the last page even in spread mode (where the top
    /// row's left page would otherwise be one short).
    fn update_current_page(&self) {
        let page = {
            let inner = self.inner.borrow();
            let last_row = inner.specs.len().saturating_sub(1);
            let at_end = if inner.continuous {
                let max = max_scroll_px(&inner);
                max > 0.0 && inner.scroll_px >= max - 1.0
            } else {
                inner.current_row == last_row
            };
            if at_end {
                inner.pages_pt.len() as i32
            } else {
                inner.specs.get(inner.current_row).map_or(0, |spec| spec.left as i32 + 1)
            }
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
        let viewer = Viewer::new(
            &window,
            pages,
            1.0,
            sender,
            std::sync::mpsc::channel().0,
            crate::render::RenderControl::inert(),
        );

        // Switch to paged mode, then deliver renders — including enough to force
        // the eviction path, which also calls back into the viewer.
        viewer.toggle_continuous();
        for page in 0..5 {
            viewer.on_page_rendered(page, slint::Image::default());
        }
        viewer.nav_page(1);
        viewer.on_page_rendered(2, slint::Image::default());
    }

    /// go_to_page parses, clamps, and reports the resulting page.
    #[test]
    fn go_to_page_parses_and_clamps() {
        let Ok(window) = MainWindow::new() else {
            return;
        };
        let (sender, _receiver) = std::sync::mpsc::channel();
        let viewer = Viewer::new(
            &window,
            vec![(600.0, 800.0); 10],
            1.0,
            sender,
            std::sync::mpsc::channel().0,
            crate::render::RenderControl::inert(),
        );
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
