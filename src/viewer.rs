//! UI-thread view state: layout, zoom, spread/scroll modes, and the row model.
//!
//! Everything here runs on the UI thread, so interior mutability via `RefCell`
//! is enough — no locking. Pages are grouped into rows (one page, or two side by
//! side for a spread); the model holds rows, and only rendered pages carry an
//! image. Zoom is applied through the window's shared `density` property so a
//! zoom change writes one value instead of every row.
//!
//! Each open tab has a viewer of its own, but they all share one window, so only
//! the active viewer writes to it. The others keep updating their own state and
//! models, and [`Viewer::activate`] puts all of it back on screen.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;
use std::rc::{self, Rc};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, Image, Model, ModelRc, Timer, TimerMode, VecModel, Weak};

use crate::images::{ImageKey, ImageSpot};
use crate::links::{self, PageLink};
use crate::render::{
    RenderControl, RenderRequest, ScreenshotRequest, Shot, ThumbRequest, WorkerMessage,
    buffer_bytes, capped_scale, scale_key,
};
use crate::screenshot::{CLICK_PX, Capture};
use crate::search::{self, Area, Hit, PageText, Snippet};
use crate::selection::{TextPos, Unit};
use crate::semantic::{self, Vectorizer, Vectors};
use crate::{
    Highlight, ImageRow, MainWindow, PageEntry, PageLayout, PageRow, Screenshot, SearchResult,
};

/// How pages are grouped into rows.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Spread {
    /// One page per row.
    #[serde(rename = "single")]
    None,
    /// Two pages per row starting at the first page: [0,1] [2,3] …
    Odd,
    /// First page alone (centered), then pairs: [0] [1,2] [3,4] …
    Even,
}

/// How pages are scaled to the viewport when a fit mode is active.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FitMode {
    Free,
    Width,
    Page,
}

/// The per-document view choices remembered between runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewSettings {
    pub continuous: bool,
    pub spread: Spread,
    pub fit: FitMode,
    /// Only meaningful with [`FitMode::Free`]; a fit mode derives the zoom from
    /// the window size instead.
    pub zoom: f32,
    /// The 0-based page at the top of the view. A page rather than a scroll
    /// offset, since the offset depends on the window size, zoom and spread.
    pub page: usize,
}

impl Default for ViewSettings {
    fn default() -> Self {
        Self { continuous: true, spread: Spread::None, fit: FitMode::Page, zoom: 1.0, page: 0 }
    }
}

/// A page's rendered image, as far as budgeting and re-requesting it go.
#[derive(Clone, Copy)]
struct Retained {
    /// The bytes the image takes.
    bytes: usize,
    /// The scale it was rendered at, as a [`scale_key`].
    scale: u32,
}

/// The page indices making up one row.
#[derive(Clone, Copy)]
struct RowSpec {
    left: usize,
    right: Option<usize>,
}

impl RowSpec {
    /// The row's one or two pages, left to right.
    fn pages(self) -> impl Iterator<Item = usize> {
        std::iter::once(self.left).chain(self.right)
    }
}

/// An offset the viewer has just given the continuous list, and how often it
/// has had to give it again.
///
/// To reach an offset far from its current one, Slint's ListView divides it
/// by the row height it measured on its previous layout. When the rows have
/// just changed height (a new spread or zoom, or another tab's document) that
/// height is stale, so the list lands on the wrong row and reports that row's
/// offset back as if the reader had scrolled there. Having laid out the new
/// rows it knows their height, so setting the same offset again lands.
#[derive(Clone, Copy)]
struct Settling {
    target: f32,
    retries: u8,
}

/// Where the reader is, in terms that survive a relayout: the first page of
/// the row at the top of the view, and how far into that row the view starts,
/// as a fraction of the row's height. A scroll offset or a row index would not
/// do, since both mean a different page once the zoom or the spread changes.
#[derive(Clone, Copy)]
struct Place {
    page: usize,
    into_row: f32,
}

const ZOOM_STEP: f32 = 1.25;
const MIN_ZOOM: f32 = 0.1;
const MAX_ZOOM: f32 = 8.0;
/// Logical pixels per point when zoom is "100%". 96/72 renders a point at one CSS
/// pixel's worth of density, a comfortable default reading size.
const BASE_DENSITY: f32 = 96.0 / 72.0;
/// Width held back when fitting in continuous mode, whose scrollbar lies over
/// the right edge of the pages. Paged mode has no scrollbar and holds back
/// nothing.
const SCROLLBAR_GUTTER: f32 = 24.0;
/// How far an arrow key scrolls when the page is taller than the viewport.
const SCROLL_STEP: f32 = 120.0;
/// How many times the viewer puts the list back on an offset it set before
/// accepting where the list says it is (see [`Settling`]).
const MAX_SETTLE_RETRIES: u8 = 3;
/// How close, in logical pixels, a reported offset must be to count as the one
/// the viewer set.
const SETTLE_TOLERANCE: f32 = 0.5;
/// How close to a row's top, as a fraction of the row, counts as on it.
const ROW_SNAP: f32 = 1e-3;
/// How far into a row, as a fraction of the row, the view has to be for
/// paging back to go to that row's start rather than the row before.
const PARTWAY: f32 = 0.01;
/// The share of rows, smallest first, whose largest sets the size of a usual
/// row. The rest may be larger and still count as usual (see
/// [`OUTLIER_FACTOR`]).
const USUAL_SHARE: f32 = 0.9;
/// How much larger than the usual row, in width or height, a row can be and
/// still set the size every row is laid out and fitted to. A row past this,
/// such as a fold-out map in a book of scanned pages, is shrunk to that size
/// in continuous mode instead of making every other row as large as itself.
const OUTLIER_FACTOR: f32 = 1.25;
/// How often a search runs while the query is being typed. The first
/// keystroke searches at once, and the rest at most this often, always ending
/// with the latest query. Unlike waiting for typing to stop, a longer query
/// already shows hits for its first letters.
const SEARCH_INTERVAL: Duration = Duration::from_millis(100);
/// The most hits a search lists and outlines. Every hit is still counted.
const MAX_HITS: usize = 2000;
/// The most pages a search by meaning lists. Every page gets a score, but
/// past the first few the scores say little, and the list is for reading
/// down, not scrolling.
const MAX_RELATED: usize = 20;
/// The most pages one vectorizer job takes, so that a long document's
/// progress shows as it goes and the first pages can be searched before the
/// last are done.
const VECTORIZE_BATCH: usize = 32;
/// Rows to prefetch on either side of the visible range while idle.
const PREFETCH_ROWS: usize = 4;
/// Bytes of rendered page images a viewer keeps at once. A budget in bytes
/// rather than pages, because one page can take anywhere from kilobytes to a
/// hundred megabytes depending on its size and the zoom.
const RETAIN_BUDGET: usize = 256 * 1024 * 1024;
/// How long the viewport has to stay one size before pages are rendered for
/// it. A window being dragged to a new size reports every step on the way,
/// and each one would otherwise render the visible pages at a scale that is
/// thrown away a moment later.
const RESIZE_SETTLE: Duration = Duration::from_millis(150);
/// How far, in logical pixels, the wheel has to push past the top or bottom of
/// a page in paged mode to turn the page, once it is already scrolling (see
/// [`WHEEL_PAUSE`]). A trackpad's stream of small steps has to add up to this,
/// rather than turning a page per event.
///
/// Nothing ever holds page turns back. The window cannot tell a fling's
/// momentum from a new swipe, as winit reports both as the same kind of
/// event, so a rest after each turn also swallowed the next swipe.
const FLIP_OVERSCROLL: f32 = 60.0;
/// How long after a click another on the same place counts as the next of
/// a double or triple click.
const MULTI_CLICK: Duration = Duration::from_millis(400);
/// How often a drag held past the top or bottom of the view scrolls it, and
/// the most it scrolls each time in logical pixels. The further past the
/// edge the pointer is, up to that, the faster it goes.
const DRAG_SCROLL_INTERVAL: Duration = Duration::from_millis(50);
const DRAG_SCROLL_MAX: f32 = 40.0;
/// How long the wheel has to be still for its next event to start a new
/// scroll. The first push past the edge of a new scroll turns the page at
/// once, however small, so each notch of a mouse wheel turns one page even
/// though macOS reports a slow notch as a fraction of a line.
const WHEEL_PAUSE: Duration = Duration::from_millis(150);

impl Spread {
    /// The spread for the toolbar's 0/1/2 index; anything else is single pages.
    pub(crate) fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Odd,
            2 => Self::Even,
            _ => Self::None,
        }
    }

    /// The 0/1/2 index used by the toolbar's spread radios.
    fn index(self) -> i32 {
        match self {
            Self::None => 0,
            Self::Odd => 1,
            Self::Even => 2,
        }
    }
}

/// Physical pixels per point at the view's zoom, which the pages of a usual
/// row are rendered at. See [`row_render_scale`] for any one row.
fn render_scale(inner: &Inner) -> f32 {
    inner.zoom * BASE_DENSITY * inner.scale_factor
}

/// A row's width and height in points: its pages side by side.
fn row_dims(pages_pt: &[(f32, f32)], spec: &RowSpec) -> (f32, f32) {
    let (left_w, left_h) = pages_pt[spec.left];
    let (right_w, right_h) = spec.right.map_or((0.0, 0.0), |right| pages_pt[right]);
    (left_w + right_w, left_h.max(right_h))
}

/// How much smaller than its own size `row` is drawn in continuous mode: 1 for
/// a row within the reference size, and for a larger one whatever shrinks it
/// to fit, since every row there shares the reference height.
fn row_shrink(inner: &Inner, row: usize) -> f32 {
    let (width, height) = row_dims(&inner.pages_pt, &inner.specs[row]);
    let fit = |reference: f32, size: f32| if size > reference { reference / size } else { 1.0 };
    fit(inner.ref_w_pt, width).min(fit(inner.ref_h_pt, height))
}

/// The zoom the fit mode gives a box of `width_pt`×`height_pt` points in the
/// current view, or `None` without a fit mode, a viewport or a size. `pair`
/// says whether the box holds a spread, whose gap is not the pages' own.
fn fit_zoom(inner: &Inner, width_pt: f32, height_pt: f32, pair: bool) -> Option<f32> {
    let (view_w, view_h) = inner.view?;
    if width_pt <= 0.0 || height_pt <= 0.0 {
        return None;
    }
    // A fit should leave no more room than the layout itself needs: nothing
    // at all in paged mode, which shows one row alone, and in continuous mode
    // the scrollbar's width and the gap between rows, so a fitted row fills
    // the view exactly.
    let (gutter_w, gutter_h) =
        if inner.continuous { (SCROLLBAR_GUTTER, inner.row_gap) } else { (0.0, 0.0) };
    let spacing = if pair { inner.spread_spacing } else { 0.0 };
    let width_zoom = (view_w - gutter_w - spacing).max(1.0) / (width_pt * BASE_DENSITY);
    let zoom = match inner.fit {
        FitMode::Width => width_zoom,
        FitMode::Page => width_zoom.min((view_h - gutter_h).max(1.0) / (height_pt * BASE_DENSITY)),
        FitMode::Free => return None,
    };
    Some(zoom.clamp(MIN_ZOOM, MAX_ZOOM))
}

/// The zoom `row` is drawn at. In continuous mode that is the view's zoom,
/// shrunk for a row larger than the usual one. Paged mode shows each row
/// alone, so with a fit mode on each row gets the zoom that fits it.
fn row_zoom(inner: &Inner, row: usize) -> f32 {
    if inner.continuous {
        return inner.zoom * row_shrink(inner, row);
    }
    let spec = &inner.specs[row];
    let (width, height) = row_dims(&inner.pages_pt, spec);
    fit_zoom(inner, width, height, spec.right.is_some()).unwrap_or(inner.zoom)
}

/// Physical pixels per point that the pages of `row` are rendered at.
fn row_render_scale(inner: &Inner, row: usize) -> f32 {
    row_zoom(inner, row) * BASE_DENSITY * inner.scale_factor
}

/// The continuous row at `offset` (logical pixels from the top), and how far
/// into that row the offset is, as a fraction of the row's height. Every
/// question about the row at the top of the view goes through here, so the
/// page counter, paging and the remembered place always agree.
///
/// An offset put exactly on a row's top comes back a hair short of it after
/// the division, which would read as the very end of the row before and lose
/// a page, so a row starts [`ROW_SNAP`] before its top.
fn row_at(inner: &Inner, offset: f32) -> (usize, f32) {
    let last = inner.specs.len().saturating_sub(1);
    let row_height = row_height_px(inner);
    if row_height <= 0.0 {
        return (0, 0.0);
    }
    let rows = offset / row_height;
    let row = ((rows + ROW_SNAP).floor().max(0.0) as usize).min(last);
    (row, (rows - row as f32).clamp(0.0, 1.0))
}

/// The rows on screen: in continuous mode those the viewport spans from the
/// scroll offset, with one more below for a row sliding in, and in paged mode
/// the one row shown.
fn rows_in_view(inner: &Inner) -> RangeInclusive<usize> {
    let last = inner.specs.len().saturating_sub(1);
    let row_height = row_height_px(inner);
    if !inner.continuous || row_height <= 0.0 {
        let row = inner.current_row.min(last);
        return row..=row;
    }
    let view_height = inner.view.map_or(0.0, |(_, h)| h);
    let (top, _) = row_at(inner, inner.scroll_px);
    let span = (view_height / row_height).ceil() as usize + 1;
    top..=(top + span).min(last)
}

/// Whether every page of `row` has an image rendered at the current scale,
/// or failed to render at it and so is not worth asking for again. An image
/// from before a zoom still shows, stretched, but is asked for again.
fn row_rendered(inner: &Inner, row: usize) -> bool {
    let spec = &inner.specs[row];
    let current = scale_key(row_render_scale(inner, row));
    let done = |page: usize| {
        inner.retained.get(&page).is_some_and(|retained| retained.scale == current)
            || inner.failed.contains(&page)
    };
    done(spec.left) && spec.right.is_none_or(done)
}

/// How many rows to prefetch on each side of the `in_view` rows on screen:
/// [`PREFETCH_ROWS`], or fewer once the zoom makes pages so large that the
/// budget could not hold them next to the rows in view. Prefetching more
/// would only render pages to evict them again.
fn prefetch_rows(inner: &Inner, in_view: usize, scale: f32) -> usize {
    let scale = capped_scale(inner.ref_w_pt, inner.ref_h_pt, scale);
    let row_bytes = buffer_bytes(
        (inner.ref_w_pt * scale).ceil() as u32,
        (inner.ref_h_pt * scale).ceil() as u32,
    )
    .max(1);
    let spare = (RETAIN_BUDGET / row_bytes).saturating_sub(in_view);
    PREFETCH_ROWS.min(spare / 2)
}

/// The rendered pages to drop so the retained images fit [`RETAIN_BUDGET`],
/// furthest from the view first. Pages in view are never chosen, even over
/// the budget, since dropping what is on screen would only have it rendered
/// again straight away.
fn pages_over_budget(inner: &Inner) -> Vec<usize> {
    let mut used: usize = inner.retained.values().map(|retained| retained.bytes).sum();
    if used <= RETAIN_BUDGET {
        return Vec::new();
    }
    let view = rows_in_view(inner);
    let mut candidates: Vec<(usize, usize)> = inner
        .retained
        .keys()
        .filter_map(|&page| {
            let row = inner.page_loc[page].0;
            let distance = if row < *view.start() {
                view.start() - row
            } else {
                row.saturating_sub(*view.end())
            };
            (distance > 0).then_some((distance, page))
        })
        .collect();
    candidates.sort_unstable_by(|a, b| b.cmp(a));
    let mut evicted = Vec::new();
    for (_, page) in candidates {
        if used <= RETAIN_BUDGET {
            break;
        }
        used -= inner.retained[&page].bytes;
        evicted.push(page);
    }
    evicted
}

/// The on-screen height of one row (all rows are uniform) in logical pixels.
/// The page part is rounded up to whole pixels and multiplied in the order
/// the window does, so the two agree on every row's place exactly; the
/// window rounds for its ListView's sake (see `PageRowView`).
fn row_height_px(inner: &Inner) -> f32 {
    (inner.ref_h_pt * (BASE_DENSITY * inner.zoom)).ceil() + inner.row_gap
}

/// The largest valid continuous scroll offset in logical pixels.
fn max_scroll_px(inner: &Inner) -> f32 {
    let view_height = inner.view.map_or(0.0, |(_, h)| h);
    (inner.specs.len() as f32 * row_height_px(inner) - view_height).max(0.0)
}

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
            (w, h, inner.spread_spacing)
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

/// The width and height in points that continuous mode lays every row out at
/// and fits to: the largest width and height among the usual rows. A row is
/// usual when neither its width nor its height is more than
/// [`OUTLIER_FACTOR`] times the one at the [`USUAL_SHARE`] mark. A few much
/// larger rows, such as a fold-out map, are left out, so they do not leave
/// every other page small in a row far too large for it. A document whose
/// pages simply come in two sizes has more than a few of the larger, which
/// then count as usual.
fn reference_dims(specs: &[RowSpec], pages_pt: &[(f32, f32)]) -> (f32, f32) {
    fn limit(mut sizes: Vec<f32>) -> f32 {
        sizes.sort_by(f32::total_cmp);
        let Some(last) = sizes.len().checked_sub(1) else {
            return 0.0;
        };
        sizes[(last as f32 * USUAL_SHARE) as usize] * OUTLIER_FACTOR
    }
    let dims: Vec<(f32, f32)> = specs.iter().map(|spec| row_dims(pages_pt, spec)).collect();
    let limit_w = limit(dims.iter().map(|&(width, _)| width).collect());
    let limit_h = limit(dims.iter().map(|&(_, height)| height).collect());
    dims.into_iter()
        .filter(|&(width, height)| width <= limit_w && height <= limit_h)
        .fold((0.0, 0.0), |(max_w, max_h), (width, height)| (max_w.max(width), max_h.max(height)))
}

/// The areas of `page` in `areas`, as its slot in a row carries them.
fn areas_of(areas: &HashMap<usize, Vec<Highlight>>, page: usize) -> ModelRc<Highlight> {
    areas
        .get(&page)
        .map_or_else(ModelRc::default, |areas| ModelRc::new(VecModel::from(areas.clone())))
}

/// Logical pixels per point that `page` is drawn at: its row's zoom in
/// continuous mode, and the view's in paged mode, where the row shown sets
/// the zoom (see [`Viewer::fit_paged_row`]).
fn page_density(inner: &Inner, page: usize) -> f32 {
    let zoom = match inner.page_loc.get(page) {
        Some(&(row, _)) if inner.continuous => row_zoom(inner, row),
        _ => inner.zoom,
    };
    BASE_DENSITY * zoom
}

/// Where `page`'s top-left corner is, in logical pixels from the top-left of
/// the continuous list's content or of the paged view's content box, as the
/// window lays the rows out: each row centred across the view, its pages
/// side by side and each centred in the row's height. Selection needs this
/// to follow a drag from one page onto another, since the drag reports where
/// it is from the page it began on.
fn page_origin_px(inner: &Inner, page: usize) -> Option<(f32, f32)> {
    let &(row, is_right) = inner.page_loc.get(page)?;
    let spec = &inner.specs[row];
    let density = page_density(inner, page);
    let spacing = if spec.right.is_some() { inner.spread_spacing } else { 0.0 };
    let page_x = if is_right { inner.pages_pt[spec.left].0 * density + spacing } else { 0.0 };
    if !inner.continuous {
        return Some((page_x, 0.0));
    }
    let (row_w, _) = row_dims(&inner.pages_pt, spec);
    let view_w = inner.view.map_or(0.0, |(width, _)| width);
    let row_px = row_height_px(inner);
    let page_h = inner.pages_pt[page].1 * density;
    Some((
        (view_w - (row_w * density + spacing)) / 2.0 + page_x,
        row as f32 * row_px + (row_px - page_h) / 2.0,
    ))
}

/// The page under a point in the frame of [`page_origin_px`], or the nearest
/// one: the row the point is level with, or the row shown in paged mode, and
/// of a spread the page it is nearer to.
fn page_at(inner: &Inner, x: f32, y: f32) -> Option<usize> {
    let row = if inner.continuous { row_at(inner, y).0 } else { inner.current_row };
    let spec = inner.specs.get(row)?;
    let Some(right) = spec.right else {
        return Some(spec.left);
    };
    let (left_x, _) = page_origin_px(inner, spec.left)?;
    let left_w = inner.pages_pt[spec.left].0 * page_density(inner, spec.left);
    let boundary = left_x + left_w + inner.spread_spacing / 2.0;
    Some(if x < boundary { spec.left } else { right })
}

/// Puts `area` of `page` in the window's screenshot outline.
fn show_area(shot: &Screenshot<'_>, page: usize, area: Area) {
    shot.set_page(page as i32);
    shot.set_x(area.x);
    shot.set_y(area.y);
    shot.set_width(area.width);
    shot.set_height(area.height);
}

/// Which areas a page's slot is overlaid with.
#[derive(Clone, Copy)]
enum Overlay {
    Hits,
    Selection,
}

/// The reader's selection: where it began and where it reaches, in the
/// document's text, and the drag that is making it.
#[derive(Default)]
struct SelectionState {
    /// The anchor and the focus: where the press was and where the drag has
    /// reached, in either order.
    range: Option<(TextPos, TextPos)>,
    /// What the selection grows by, from how many times the reader clicked.
    unit: Unit,
    /// While a drag goes on, the page it began on and the last point it
    /// reported, in points from that page's corner.
    drag: Option<(usize, f32, f32)>,
    /// When and where the last press was, for telling a double click.
    last_press: Option<(Instant, TextPos)>,
    clicks: u32,
    /// A press on a link, waiting for the release that follows it, or for
    /// the pointer to move, which makes it the start of a drag selecting
    /// text instead: the page and point pressed, and the link.
    pending_link: Option<(usize, f32, f32, PageLink)>,
}

/// Which images the sidebar's list leaves out, and whether it gathers the
/// ones drawn more than once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageFilters {
    pub hide_small: bool,
    pub group_repeats: bool,
}

impl Default for ImageFilters {
    fn default() -> Self {
        Self { hide_small: true, group_repeats: true }
    }
}

/// What heads a row of the images list.
#[derive(Clone, Copy)]
enum Heading {
    None,
    /// The page the row's image is on.
    Page,
    /// The images drawn more than once, gathered at the end.
    Repeated,
}

impl Heading {
    fn index(self) -> i32 {
        match self {
            Self::None => 0,
            Self::Page => 1,
            Self::Repeated => 2,
        }
    }
}

/// The document's images and the list built from them.
#[derive(Default)]
struct ImageState {
    /// Each page's images, for the pages catalogued so far.
    catalogue: Vec<Vec<ImageSpot>>,
    /// The previews rendered, by page and ordinal, which outlive the list
    /// being built again.
    previews: HashMap<(usize, usize), Image>,
    filters: ImageFilters,
    /// Where the rows gathering the repeated images start; the rows before
    /// are listed by page.
    repeated_from: usize,
    /// How many small images the list leaves out.
    small: usize,
}

/// The page being read when `spec` is the row at the top: the page last asked
/// for if the row holds it, otherwise the row's first page.
/// The outline entries the 0-based pages `shown` belong to, from the page
/// each entry goes to in the order they are listed. Every entry that goes
/// to one of the pages is theirs, in list order. Where none does, the pages
/// are still in the section begun last before them, which is the last
/// listed of the entries going to the nearest earlier page with any. An
/// entry that goes nowhere belongs to no page, and pages before the first
/// heading belong to none.
pub(crate) fn headings_for(pages: &[i32], shown: &[i32]) -> Vec<usize> {
    let on_shown: Vec<usize> =
        (0..pages.len()).filter(|&index| shown.contains(&pages[index])).collect();
    if !on_shown.is_empty() {
        return on_shown;
    }
    let Some(&last) = shown.iter().max() else {
        return Vec::new();
    };
    let nearest = pages.iter().copied().filter(|heading| (0..=last).contains(heading)).max();
    nearest
        .and_then(|nearest| pages.iter().rposition(|&heading| heading == nearest))
        .into_iter()
        .collect()
}

fn reading_page_in(inner: &Inner, spec: &RowSpec) -> usize {
    if spec.right == Some(inner.reading_page) { inner.reading_page } else { spec.left }
}

/// What a search looks for: the query's letters, or pages about it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchMode {
    /// Every place the query's text occurs, as [`search::search`] finds them.
    #[default]
    Exact,
    /// The pages most like the query in what they say, as
    /// [`semantic::search`] ranks them.
    Meaning,
}

impl SearchMode {
    /// The mode's index in the window's `search-mode` property.
    pub fn index(self) -> i32 {
        match self {
            Self::Exact => 0,
            Self::Meaning => 1,
        }
    }

    /// The mode at `index` in the window's `search-mode` property, or
    /// `None` for an index that is none of them.
    pub fn from_index(index: i32) -> Option<Self> {
        match index {
            0 => Some(Self::Exact),
            1 => Some(Self::Meaning),
            _ => None,
        }
    }
}

/// A document's search: its text, as much as has been indexed, and what the
/// current query found in it.
#[derive(Default)]
struct SearchState {
    /// Each page's text, from the first page on, as far as it is indexed.
    index: Vec<PageText>,
    mode: SearchMode,
    /// Each page's vector, from the first page on, as far as the vectorizer
    /// has made them. Made only once the reader searches by meaning, and
    /// then kept, since switching modes costs nothing.
    vectors: Vectors,
    /// Whether a vectorizer is making more of them.
    vectorizing: bool,
    /// Why the model could not be loaded, which ends vectorizing.
    model_error: Option<String>,
    query: String,
    /// What the query found: every occurrence in exact mode, and in meaning
    /// mode one hit per page listed, spanning nothing, best page first.
    hits: Vec<Hit>,
    /// How many hits there are in all, including those past [`MAX_HITS`].
    total: usize,
    /// The index in `hits` of the hit the reader is on.
    current: Option<usize>,
    /// Whether the query changed since the view last moved to one of its
    /// hits, so the next search that finds any moves to the first.
    jump: bool,
}

/// Everything the viewer knows about its document and how it is shown. Kept
/// behind one `RefCell`, borrowed in short scopes, so no borrow is held while
/// the viewer calls back into itself or into the window.
struct Inner {
    /// Every page's width and height in PDF points.
    pages_pt: Vec<(f32, f32)>,
    /// Every page's links, once the worker has read them.
    links: Vec<Vec<PageLink>>,
    /// The page each outline entry goes to, in the order the sidebar lists
    /// them, -1 for one that goes nowhere.
    outline_pages: Vec<i32>,
    /// The outline entries marked as the ones the pages shown belong to.
    outline_marked: Vec<usize>,
    /// The gap between rows and between the pages of a spread, in logical
    /// pixels, as the window's `PageLayout` lays them out.
    row_gap: f32,
    spread_spacing: f32,
    /// The zoom, where 1.0 shows a point at [`BASE_DENSITY`] logical pixels.
    zoom: f32,
    /// Physical pixels per logical pixel on the window's display.
    scale_factor: f32,
    /// The page area's width and height in logical pixels, once known.
    view: Option<(f32, f32)>,
    /// Whether the zoom follows the viewport, and how.
    fit: FitMode,
    /// The pages holding a rendered image.
    retained: HashMap<usize, Retained>,
    /// The pages that failed to render at the current scale.
    failed: HashSet<usize>,
    /// The pages whose thumbnail failed to render.
    thumb_failed: HashSet<usize>,
    /// The search hits outlined on each page.
    highlights: HashMap<usize, Vec<Highlight>>,
    /// The selected text's areas on each page.
    selection: HashMap<usize, Vec<Highlight>>,
    spread: Spread,
    /// Continuous scrolling, rather than one row at a time.
    continuous: bool,
    /// The row at the top of the view, or the one shown in paged mode.
    current_row: usize,
    /// Current continuous scroll offset from the top, in logical pixels.
    scroll_px: f32,
    /// Scroll offset within the current page in paged mode (logical pixels).
    paged_scroll_x: f32,
    paged_scroll_y: f32,
    /// How far the wheel has pushed past the edge of the page in paged mode,
    /// positive past the bottom, towards [`FLIP_OVERSCROLL`].
    overscroll: f32,
    /// View epoch, bumped whenever the visible set changes, so the render worker
    /// can drop requests from earlier views.
    generation: u64,
    /// The rows the pages are grouped into for the current spread.
    specs: Vec<RowSpec>,
    /// For each page, its row and whether it is the right page of a spread.
    page_loc: Vec<(usize, bool)>,
    /// The width and height in points of the largest usual row (see
    /// [`reference_dims`]), which continuous mode fits and sizes every row
    /// to.
    ref_w_pt: f32,
    ref_h_pt: f32,
    /// Rendered sidebar thumbnails, indexed by page (empty until rendered).
    thumb_images: Vec<Image>,
    /// The page the reader last asked for (the page field, the outline, a
    /// thumbnail or a restored setting). A spread's row holds two pages, and
    /// when it is the one at the top this says which of them is being read, so
    /// going back to single pages lands on it rather than on its neighbour.
    reading_page: usize,
    /// The offset the continuous list holds, as last set or reported.
    shown_px: f32,
    /// An offset set by the viewer that the list has not yet confirmed.
    settling: Option<Settling>,
    /// Whether the continuous view still has to scroll to `current_row`. A
    /// restored position can only become a scroll offset once the viewport is
    /// known, because a fit mode's zoom decides how tall each row is.
    position_pending: bool,
}

/// One open document's view: its layout, zoom, modes and reading position,
/// the rows the window shows for it, and the render requests that fill them.
/// Every method runs on the UI thread.
pub struct Viewer {
    /// This viewer, for the timer that renders once a resize settles.
    me: rc::Weak<Viewer>,
    inner: RefCell<Inner>,
    /// The rows of pages the continuous list shows, and paged mode takes its
    /// row from.
    model: Rc<VecModel<PageRow>>,
    /// The sidebar's thumbnails, grouped into rows like the pages.
    thumb_model: Rc<VecModel<PageRow>>,
    /// For each outline entry, whether it is marked as one the pages
    /// shown belong to.
    outline_marks: Rc<VecModel<bool>>,
    /// The document's images as the cataloguer has read them, and what the
    /// sidebar's list is built from them with.
    images: RefCell<ImageState>,
    /// The sidebar's list of the document's images, built once every page
    /// has been catalogued.
    image_rows: Rc<VecModel<ImageRow>>,
    window: Weak<MainWindow>,
    /// Whether this viewer's tab is the one shown. Only then may it write to the
    /// window, which every other tab's viewer shares.
    active: Cell<bool>,
    /// Requests to the document's page worker.
    sender: Sender<WorkerMessage>,
    /// Pages to the document's thumbnail worker.
    thumb_sender: Sender<ThumbRequest>,
    /// Aborts a page render the view no longer wants.
    control: RenderControl,
    /// Renders the view once the viewport has stopped changing size.
    resize_timer: Timer,
    /// The document's text as far as it has been indexed, and the search
    /// over it.
    search: RefCell<SearchState>,
    /// Makes the page vectors a search by meaning ranks.
    vectorizer: Vectorizer,
    /// The hits of the search, as the results list shows them.
    results: Rc<VecModel<SearchResult>>,
    /// Runs for [`SEARCH_INTERVAL`] after each search, holding the next one
    /// back until it ends.
    search_timer: Timer,
    /// Whether a search is waiting for the timer.
    search_pending: Cell<bool>,
    /// Runs while the wheel is scrolling in paged mode, until it has been
    /// still for [`WHEEL_PAUSE`].
    wheel_scrolling: Timer,
    /// The selected text and the drag making it.
    selection: RefCell<SelectionState>,
    /// Runs while a drag is held past the top or bottom of the view, scrolling
    /// it every [`DRAG_SCROLL_INTERVAL`].
    drag_scroll: Timer,
    /// The drag taking a screenshot, while one goes on.
    capture: RefCell<Option<Capture>>,
}

/// The channels to one document's background workers.
pub struct Workers {
    pub(crate) pages: Sender<WorkerMessage>,
    pub(crate) thumbnails: Sender<ThumbRequest>,
    pub(crate) control: RenderControl,
    pub(crate) vectorizer: Vectorizer,
}

impl Viewer {
    pub fn new(
        window: &MainWindow,
        pages_pt: Vec<(f32, f32)>,
        scale_factor: f32,
        workers: Workers,
        settings: &ViewSettings,
        filters: ImageFilters,
    ) -> Rc<Self> {
        let Workers { pages: sender, thumbnails: thumb_sender, control, vectorizer } = workers;
        let page_count = pages_pt.len();
        let model = Rc::new(VecModel::<PageRow>::default());
        let thumb_model = Rc::new(VecModel::<PageRow>::default());

        let viewer = Rc::new_cyclic(|me| Self {
            me: me.clone(),
            inner: RefCell::new(Inner {
                pages_pt,
                links: Vec::new(),
                outline_pages: Vec::new(),
                outline_marked: Vec::new(),
                row_gap: window.global::<PageLayout>().get_row_gap(),
                spread_spacing: window.global::<PageLayout>().get_spread_spacing(),
                zoom: settings.zoom.clamp(MIN_ZOOM, MAX_ZOOM),
                scale_factor,
                view: None,
                fit: settings.fit,
                retained: HashMap::new(),
                failed: HashSet::new(),
                thumb_failed: HashSet::new(),
                highlights: HashMap::new(),
                selection: HashMap::new(),
                spread: settings.spread,
                continuous: settings.continuous,
                current_row: 0,
                scroll_px: 0.0,
                paged_scroll_x: 0.0,
                paged_scroll_y: 0.0,
                overscroll: 0.0,
                generation: 0,
                specs: Vec::new(),
                page_loc: Vec::new(),
                ref_w_pt: 0.0,
                ref_h_pt: 0.0,
                thumb_images: vec![Image::default(); page_count],
                position_pending: false,
                reading_page: 0,
                shown_px: 0.0,
                settling: None,
            }),
            model,
            thumb_model,
            images: RefCell::new(ImageState { filters, ..ImageState::default() }),
            image_rows: Rc::new(VecModel::default()),
            outline_marks: Rc::new(VecModel::default()),
            window: window.as_weak(),
            active: Cell::new(false),
            sender,
            thumb_sender,
            control,
            resize_timer: Timer::default(),
            search: RefCell::new(SearchState::default()),
            vectorizer,
            results: Rc::new(VecModel::default()),
            search_timer: Timer::default(),
            search_pending: Cell::new(false),
            wheel_scrolling: Timer::default(),
            selection: RefCell::new(SelectionState::default()),
            drag_scroll: Timer::default(),
            capture: RefCell::new(None),
        });

        viewer.build_layout();
        viewer.restore_page(settings.page);
        viewer
    }

    /// Physical pixels per point that the 0-based `page` is rendered at now.
    pub fn page_render_scale(&self, page: usize) -> f32 {
        let inner = self.inner.borrow();
        inner.page_loc.get(page).map_or(0.0, |&(row, _)| row_render_scale(&inner, row))
    }

    /// The view choices to remember for this document.
    pub fn settings(&self) -> ViewSettings {
        let inner = self.inner.borrow();
        ViewSettings {
            continuous: inner.continuous,
            spread: inner.spread,
            fit: inner.fit,
            zoom: inner.zoom,
            page: self.reading_page(),
        }
    }

    /// The 0-based page being read: the one a bookmark goes on, and the one
    /// the view comes back to when the document is reopened.
    pub fn reading_page(&self) -> usize {
        let inner = self.inner.borrow();
        inner.specs.get(inner.current_row).map_or(0, |spec| reading_page_in(&inner, spec))
    }

    /// Puts a restored page at the top of the view. Paged mode shows it right
    /// away; continuous mode scrolls to it once the viewport is known.
    fn restore_page(&self, page: usize) {
        let mut inner = self.inner.borrow_mut();
        let Some(&(row, _)) = inner.page_loc.get(page) else {
            return;
        };
        inner.current_row = row;
        inner.reading_page = page;
        inner.position_pending = inner.continuous && row > 0;
    }

    /// Makes this the viewer the window shows and publishes its whole state:
    /// models, modes, zoom, scroll position and page counter. A new viewer
    /// leaves the window alone until this is called, since its document may
    /// finish loading while another tab is shown.
    pub fn activate(&self) {
        self.active.set(true);
        if let Some(window) = self.window() {
            let inner = self.inner.borrow();
            window.set_rows(ModelRc::from(self.model.clone()));
            window.set_thumb_rows(ModelRc::from(self.thumb_model.clone()));
            window.set_image_rows(ModelRc::from(self.image_rows.clone()));
            window.set_outline_marks(ModelRc::from(self.outline_marks.clone()));
            window.set_page_count(inner.pages_pt.len() as i32);
            window.set_spread_mode(inner.spread.index());
            window.set_continuous(inner.continuous);
            window.set_row_height_pt(inner.ref_h_pt);
            window.set_search_results(ModelRc::from(self.results.clone()));
            window.set_search_text(self.search.borrow().query.as_str().into());
            window.set_search_mode(self.search.borrow().mode.index());
        }
        self.publish_search();
        self.publish_images();
        let scroll_px = self.inner.borrow().scroll_px;
        self.show_offset(scroll_px);
        self.apply_density();
        self.refresh_current_row();
        self.update_current_page();
    }

    /// Stops this viewer from writing to the window because another tab took
    /// it over. Renders still arrive and land in this viewer's own models.
    ///
    /// A tab in the background keeps only the pages it has on screen, which
    /// are what shows first when it comes back, and its worker drops its
    /// cache. Otherwise every open tab would hold a full budget of pixels
    /// that nobody is looking at.
    pub fn deactivate(&self) {
        self.active.set(false);
        self.capture_cancel();
        // Coming back replays the viewport, which renders whatever is due.
        self.resize_timer.stop();
        let dropped: Vec<usize> = {
            let mut inner = self.inner.borrow_mut();
            let view = rows_in_view(&inner);
            let dropped: Vec<usize> = inner
                .retained
                .keys()
                .copied()
                .filter(|&page| !view.contains(&inner.page_loc[page].0))
                .collect();
            for page in &dropped {
                inner.retained.remove(page);
            }
            dropped
        };
        for page in dropped {
            self.clear_page_image(page);
        }
        let _ = self.sender.send(WorkerMessage::ClearCache);
    }

    /// The window, while this viewer is the one it shows.
    fn window(&self) -> Option<MainWindow> {
        if self.active.get() { self.window.upgrade() } else { None }
    }

    /// Requests renders for every page in a row.
    pub fn request_render_row(&self, row: usize) {
        let inner = self.inner.borrow();
        let Some(&spec) = inner.specs.get(row) else {
            return;
        };
        let scale = row_render_scale(&inner, row);
        for page in spec.pages() {
            self.send(page, scale, false);
        }
    }

    /// Installs a freshly rendered page image into its row, evicting the
    /// images furthest from the view if that takes the viewer over its budget.
    /// `scale` is the scale the page was asked for at.
    ///
    /// Borrows of `inner` are kept to short scopes: the model updates and the
    /// self-calls below (which borrow `inner` themselves) must not run while a
    /// borrow is held, or a re-entrant call panics.
    pub fn on_page_rendered(&self, index: usize, scale: f32, image: Image) {
        let size = image.size();
        let (row_index, is_right, width_pt, height_pt, is_current_paged, evicted) = {
            let mut inner = self.inner.borrow_mut();
            let Some(&(row_index, is_right)) = inner.page_loc.get(index) else {
                return;
            };
            let (width_pt, height_pt) = inner.pages_pt[index];
            let is_current_paged = !inner.continuous && row_index == inner.current_row;

            let bytes = buffer_bytes(size.width, size.height);
            inner.retained.insert(index, Retained { bytes, scale: scale_key(scale) });
            inner.failed.remove(&index);
            let evicted = pages_over_budget(&inner);
            for page in &evicted {
                inner.retained.remove(page);
            }
            (row_index, is_right, width_pt, height_pt, is_current_paged, evicted)
        };

        let entry = {
            let inner = self.inner.borrow();
            PageEntry {
                page: index as i32,
                width_pt,
                height_pt,
                image,
                failed: false,
                highlights: areas_of(&inner.highlights, index),
                selection: areas_of(&inner.selection, index),
            }
        };
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
    ///
    /// The new layout applies at once, stretching the images already there,
    /// but the pages are only rendered for it once the size has settled (see
    /// [`RESIZE_SETTLE`]). The first viewport renders straight away, since
    /// that is what shows a freshly opened document. A viewport that has not
    /// changed, as when a tab is shown again, changes nothing.
    pub fn set_viewport(&self, width: f32, height: f32) {
        let scale_factor =
            self.window.upgrade().map(|window| window.window().scale_factor()).unwrap_or(1.0);

        let (first, unchanged, fit_active, density_changed) = {
            let mut inner = self.inner.borrow_mut();
            let first = inner.view.is_none();
            let density_changed = (inner.scale_factor - scale_factor).abs() > 1e-3;
            let unchanged = inner.view == Some((width, height)) && !density_changed;
            inner.view = Some((width, height));
            inner.scale_factor = scale_factor;
            (first, unchanged, inner.fit != FitMode::Free, density_changed)
        };

        if !unchanged {
            if fit_active {
                self.fit_to_view();
            }
            if fit_active || density_changed {
                if first {
                    self.rerender_view();
                } else {
                    self.rerender_when_settled();
                }
            }
            // Viewport size affects paged centering and scroll limits.
            self.push_paged_offsets();
        }

        let position_pending = std::mem::take(&mut self.inner.borrow_mut().position_pending);
        if position_pending {
            let row = self.inner.borrow().current_row;
            self.scroll_to_row(row);
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

    /// Selects continuous scroll or one-row-per-screen. Either direction keeps
    /// the reading position: paged mode tracks `current_row` (set on scroll), and
    /// returning to continuous scrolls the list back to that same row.
    pub fn set_continuous(&self, continuous: bool) {
        if self.inner.borrow().continuous == continuous {
            return;
        }
        self.inner.borrow_mut().continuous = continuous;
        if let Some(window) = self.window() {
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
            self.show_offset(target_px);
        } else {
            self.show_paged_row();
        }
        self.reapply_scale();
        self.update_current_page();
    }

    pub fn toggle_continuous(&self) {
        let continuous = self.inner.borrow().continuous;
        self.set_continuous(!continuous);
    }

    /// Sets the spread mode and rebuilds rows.
    pub fn set_spread(&self, spread: Spread) {
        let place = self.place();
        self.inner.borrow_mut().spread = spread;
        if let Some(window) = self.window() {
            window.set_spread_mode(spread.index());
        }
        self.build_layout();
        // Back to the same page before re-fitting, so the fit starts from a
        // position that matches the new rows.
        self.restore_place(place);
        self.reapply_scale();
        self.update_current_page();
    }

    /// Reports the continuous scroll offset (logical pixels from the top) so the
    /// current page can be tracked. Rows are uniform height, so the top row is an
    /// exact division.
    pub fn scrolled(&self, offset: f32) {
        {
            let mut inner = self.inner.borrow_mut();
            // Until the restored position is scrolled to, the list reports the
            // offset it starts at, which would overwrite that position.
            if inner.specs.is_empty() || inner.position_pending {
                return;
            }
            inner.shown_px = offset;
            // The list confirms an offset the viewer set before its layout pass
            // can still move it, so the watch lasts until the reader scrolls
            // (see `user_scrolled`) rather than ending at the first match.
            if let Some(settling) = inner.settling
                && (offset - settling.target).abs() > SETTLE_TOLERANCE
            {
                if settling.retries < MAX_SETTLE_RETRIES {
                    inner.settling =
                        Some(Settling { target: settling.target, retries: settling.retries + 1 });
                    drop(inner);
                    if let Some(window) = self.window() {
                        window.set_scroll_y(-settling.target);
                    }
                    return;
                }
                inner.settling = None;
            }
            inner.scroll_px = offset.max(0.0);
            inner.current_row = row_at(&inner, inner.scroll_px).0;
        }
        self.update_current_page();
        self.request_visible();
    }

    /// The reader moved the continuous list (wheel, drag or scrollbar), so
    /// offsets it reports from now on are theirs, not the list landing off an
    /// offset the viewer set.
    pub fn user_scrolled(&self) {
        self.inner.borrow_mut().settling = None;
    }

    /// Requests the visible rows (and a prefetch margin), top-first, skipping
    /// rows already rendered at the current scale. Also covers eviction and
    /// zoom: a delegate's `init` fires once and can't re-request a page cleared
    /// or rendered at an old scale since.
    fn request_visible(&self) {
        let (visible, prefetch) = {
            let inner = self.inner.borrow();
            if !inner.continuous || inner.specs.is_empty() || row_height_px(&inner) <= 0.0 {
                return;
            }
            let view = rows_in_view(&inner);
            let (top, visible_end) = (*view.start(), *view.end());
            let last = inner.specs.len() - 1;
            let scale = render_scale(&inner);
            let needs = |row: usize| !row_rendered(&inner, row);

            let visible: Vec<usize> = (top..=visible_end).filter(|&r| needs(r)).collect();

            // Prefetch a few rows on either side of the visible range.
            let margin = prefetch_rows(&inner, visible_end - top + 1, scale);
            let below = (visible_end + margin).min(last);
            let above = top.saturating_sub(margin);
            let prefetch: Vec<usize> =
                ((visible_end + 1)..=below).chain(above..top).filter(|&r| needs(r)).collect();

            (visible, prefetch)
        };
        self.dispatch(&visible, &prefetch);
    }

    /// Starts a new view and asks for the pages of the `visible` rows, then
    /// of the `prefetch` rows at low priority, each at its row's scale.
    fn dispatch(&self, visible: &[usize], prefetch: &[usize]) {
        let requests: Vec<(usize, f32, bool)> = {
            let inner = self.inner.borrow();
            let pages = |rows: &[usize], prefetch: bool| {
                rows.iter()
                    .flat_map(|&row| {
                        let scale = row_render_scale(&inner, row);
                        inner.specs[row].pages().map(move |page| (page, scale, prefetch))
                    })
                    .collect::<Vec<_>>()
            };
            let mut requests = pages(visible, false);
            requests.extend(pages(prefetch, true));
            requests
        };
        let wanted: Vec<(i32, f32)> =
            requests.iter().map(|&(page, scale, _)| (page as i32, scale)).collect();
        self.advance_epoch(&wanted);
        for (page, scale, prefetch) in requests {
            self.send(page, scale, prefetch);
        }
    }

    /// Jumps to a 1-based page typed into the toolbar field. A number out of
    /// range goes to the first or last page, and anything else is ignored.
    pub fn go_to_page(&self, text: &str) {
        let Ok(requested) = text.trim().parse::<i64>() else {
            return;
        };
        let count = self.inner.borrow().pages_pt.len() as i64;
        if count > 0 {
            self.nav_to_page((requested - 1).clamp(0, count - 1) as usize);
        }
    }

    /// Navigates to a 0-based page (from the outline or a thumbnail click).
    /// One past the end, from an outline that is out of date, goes to the
    /// last page.
    pub fn nav_to_page(&self, page: usize) {
        let row = {
            let mut inner = self.inner.borrow_mut();
            if inner.page_loc.is_empty() {
                return;
            }
            let page = page.min(inner.page_loc.len() - 1);
            inner.reading_page = page;
            inner.page_loc[page].0
        };
        self.scroll_to_row(row);
    }

    /// Installs a rendered thumbnail into the sidebar's thumbnail model.
    pub fn on_thumbnail_rendered(&self, index: usize, image: Image) {
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
        let entry = PageEntry {
            page: index as i32,
            width_pt,
            height_pt,
            image,
            failed: false,
            highlights: ModelRc::default(),
            selection: ModelRc::default(),
        };
        self.set_thumb_entry(row, is_right, entry);
    }

    /// Marks a page whose thumbnail failed to render, so its slot in the
    /// sidebar stops showing a spinner.
    pub fn on_thumbnail_failed(&self, index: usize) {
        let (row, is_right, entry) = {
            let mut inner = self.inner.borrow_mut();
            let Some(&(row, is_right)) = inner.page_loc.get(index) else {
                return;
            };
            inner.thumb_failed.insert(index);
            (row, is_right, Self::thumb_entry(&inner, index))
        };
        self.set_thumb_entry(row, is_right, entry);
    }

    /// Writes one page's entry into its thumbnail row's left or right slot.
    fn set_thumb_entry(&self, row: usize, is_right: bool, entry: PageEntry) {
        if let Some(mut page_row) = self.thumb_model.row_data(row) {
            if is_right {
                page_row.right = entry;
            } else {
                page_row.left = entry;
            }
            self.thumb_model.set_row_data(row, page_row);
        }
    }

    /// Marks a page that failed to render, so it shows as failed rather than
    /// loading and is not asked for again until the zoom changes.
    pub fn on_page_failed(&self, index: usize) {
        let (row_index, is_right, entry, is_current_paged) = {
            let mut inner = self.inner.borrow_mut();
            let Some(&(row_index, is_right)) = inner.page_loc.get(index) else {
                return;
            };
            inner.failed.insert(index);
            let is_current_paged = !inner.continuous && row_index == inner.current_row;
            (row_index, is_right, Self::empty_page(&inner, index), is_current_paged)
        };
        self.set_row_entry(row_index, is_right, entry);
        if is_current_paged {
            self.refresh_current_row();
        }
    }

    /// Requests thumbnails for a visible thumbnail row's pages. The thumbnail
    /// worker renders each page at most once, so re-requests are cheap.
    pub fn request_thumbnail_row(&self, row: usize) {
        let inner = self.inner.borrow();
        if let Some(&spec) = inner.specs.get(row) {
            for page in spec.pages() {
                let _ = self.thumb_sender.send(ThumbRequest::Page(page as i32));
            }
        }
    }

    /// Rebuilds the thumbnail model from the current spread specs, reusing any
    /// already-rendered thumbnails.
    fn build_thumb_model(&self) {
        let rows = Self::model_rows(&self.inner.borrow(), Self::thumb_entry);
        self.thumb_model.set_vec(rows);
    }

    /// One model row per row of pages, with each page's slot made by `entry`.
    fn model_rows(inner: &Inner, entry: fn(&Inner, usize) -> PageEntry) -> Vec<PageRow> {
        inner
            .specs
            .iter()
            .enumerate()
            .map(|(row, spec)| {
                let left = entry(inner, spec.left);
                let (right, has_right) = match spec.right {
                    Some(right) => (entry(inner, right), true),
                    None => (Self::placeholder(), false),
                };
                PageRow { left, right, has_right, scale: row_shrink(inner, row) }
            })
            .collect()
    }

    /// A thumbnail slot carrying the page's size and (possibly empty) thumbnail.
    fn thumb_entry(inner: &Inner, page: usize) -> PageEntry {
        let (width_pt, height_pt) = inner.pages_pt[page];
        PageEntry {
            page: page as i32,
            width_pt,
            height_pt,
            image: inner.thumb_images[page].clone(),
            failed: inner.thumb_failed.contains(&page),
            highlights: ModelRc::default(),
            selection: ModelRc::default(),
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
        self.jump(usize::MAX, true);
    }

    /// Paged-mode wheel handling: scroll within the current page, and move to the
    /// previous/next page once the wheel pushes past the top/bottom edge, at
    /// once when the push starts a new scroll and otherwise once it adds up to
    /// [`FLIP_OVERSCROLL`] (see [`WHEEL_PAUSE`]). Shift makes a vertical wheel
    /// scroll horizontally.
    pub fn paged_scroll(&self, delta_x: f32, delta_y: f32, shift: bool) {
        // A downward/rightward wheel carries a negative delta; scrolling in that
        // direction increases the offset.
        let (horizontal, vertical) = if shift { (-delta_y, 0.0) } else { (-delta_x, -delta_y) };
        let fresh = !self.wheel_scrolling.running();
        self.wheel_scrolling.start(TimerMode::SingleShot, WHEEL_PAUSE, || {});

        let jump = {
            let mut inner = self.inner.borrow_mut();
            let (content_w, content_h) = paged_content_size(&inner);
            let (view_w, view_h) = inner.view.unwrap_or((0.0, 0.0));
            let max_x = (content_w - view_w).max(0.0);
            let max_y = (content_h - view_h).max(0.0);

            inner.paged_scroll_x = (inner.paged_scroll_x + horizontal).clamp(0.0, max_x);

            let at_edge = (vertical > 0.0 && inner.paged_scroll_y >= max_y - 0.5)
                || (vertical < 0.0 && inner.paged_scroll_y <= 0.5);
            if !at_edge {
                inner.overscroll = 0.0;
                inner.paged_scroll_y = (inner.paged_scroll_y + vertical).clamp(0.0, max_y);
                0
            } else if fresh {
                vertical.signum() as i32
            } else {
                // Pushing the other way starts over.
                if inner.overscroll * vertical < 0.0 {
                    inner.overscroll = 0.0;
                }
                inner.overscroll += vertical;
                if inner.overscroll.abs() >= FLIP_OVERSCROLL {
                    inner.overscroll.signum() as i32
                } else {
                    0
                }
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
        {
            let mut inner = self.inner.borrow_mut();
            let last = inner.specs.len().saturating_sub(1) as i32;
            let target = (inner.current_row as i32 + dir).clamp(0, last) as usize;
            if target == inner.current_row {
                return; // at the first/last page already
            }
            inner.current_row = target;
            inner.paged_scroll_x = 0.0;
            inner.overscroll = 0.0;
        }
        // The landing offset depends on how large the new row is drawn.
        self.fit_paged_row();
        {
            let mut inner = self.inner.borrow_mut();
            let (_, content_h) = paged_content_size(&inner);
            let view_h = inner.view.map_or(0.0, |(_, h)| h);
            inner.paged_scroll_y = if dir < 0 { (content_h - view_h).max(0.0) } else { 0.0 };
        }
        self.refresh_current_row();
        self.request_current_row();
        self.push_paged_offsets();
        self.update_current_page();
    }

    /// Gives the view the zoom of the row now shown in paged mode, which with
    /// a fit mode on is fitted to that row alone (see [`row_zoom`]).
    fn fit_paged_row(&self) {
        let zoom = {
            let inner = self.inner.borrow();
            if inner.continuous || inner.specs.is_empty() {
                return;
            }
            row_zoom(&inner, inner.current_row)
        };
        if (self.inner.borrow().zoom - zoom).abs() > f32::EPSILON {
            self.inner.borrow_mut().zoom = zoom;
            self.apply_density();
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
        if let Some(window) = self.window() {
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
                let (row, into_row) = row_at(&inner, inner.scroll_px);
                let row = row as i32;
                if dir > 0 {
                    row + 1
                } else if into_row > PARTWAY {
                    // Scrolled partway into a page: snap to that page's start.
                    row
                } else {
                    row - 1
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
        self.show_offset(target);
        self.update_current_page();
    }

    /// Moves so the given row is at the top: scrolls the ListView in continuous
    /// mode, or swaps the shown row in paged mode.
    fn scroll_to_row(&self, row: usize) {
        self.jump(row, false);
    }

    /// Makes `row` (clamped to the last) the current row. Continuous mode
    /// scrolls the list to its top, or to the very bottom of the document
    /// when `to_bottom`, so the last page shows whole. Paged mode shows the
    /// row alone, from its top.
    fn jump(&self, row: usize, to_bottom: bool) {
        let (continuous, target_px) = {
            let mut inner = self.inner.borrow_mut();
            if inner.specs.is_empty() {
                return;
            }
            let row = row.min(inner.specs.len() - 1);
            inner.current_row = row;
            let target_px =
                if to_bottom { max_scroll_px(&inner) } else { row as f32 * row_height_px(&inner) };
            inner.scroll_px = target_px;
            (inner.continuous, target_px)
        };
        if continuous {
            self.show_offset(target_px);
            self.request_current_row();
        } else {
            self.show_paged_row();
        }
        self.update_current_page();
    }

    /// Shows the current row in paged mode from its top-left corner, as a
    /// freshly shown page starts, and asks for it and its neighbours.
    fn show_paged_row(&self) {
        {
            let mut inner = self.inner.borrow_mut();
            inner.paged_scroll_x = 0.0;
            inner.paged_scroll_y = 0.0;
        }
        self.fit_paged_row();
        self.refresh_current_row();
        self.request_current_row();
        self.push_paged_offsets();
    }

    fn set_zoom(&self, zoom: f32) {
        let place = self.place();
        {
            let mut inner = self.inner.borrow_mut();
            inner.fit = FitMode::Free;
            inner.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        }
        self.apply_density();
        self.restore_place(place);
        self.rerender_view();
    }

    /// Re-fits the zoom to the viewport and renders the view at it.
    fn apply_fit(&self) {
        self.fit_to_view();
        self.rerender_view();
    }

    /// Renders the view once [`RESIZE_SETTLE`] passes without another call,
    /// each call starting the wait again.
    fn rerender_when_settled(&self) {
        let me = self.me.clone();
        self.resize_timer.start(TimerMode::SingleShot, RESIZE_SETTLE, move || {
            if let Some(viewer) = me.upgrade() {
                viewer.rerender_view();
            }
        });
    }

    /// Sets the zoom a fit mode asks for at the current viewport, keeping the
    /// reader's place, without rendering anything for it.
    fn fit_to_view(&self) {
        let recomputed = {
            let inner = self.inner.borrow();
            let zoom = if inner.continuous {
                let pair = inner.spread != Spread::None;
                fit_zoom(&inner, inner.ref_w_pt, inner.ref_h_pt, pair)
            } else if inner.fit != FitMode::Free && !inner.specs.is_empty() {
                Some(row_zoom(&inner, inner.current_row))
            } else {
                None
            };
            let Some(zoom) = zoom else {
                return;
            };
            zoom
        };

        let place = self.place();
        self.inner.borrow_mut().zoom = recomputed;
        self.apply_density();
        self.restore_place(place);
    }

    /// Moves the continuous list to `target` (logical pixels from the top) and
    /// watches its next report in case it lands elsewhere (see [`Settling`]).
    /// A list already at `target` sends no report, so there is nothing to watch.
    fn show_offset(&self, target: f32) {
        {
            let mut inner = self.inner.borrow_mut();
            inner.settling = (inner.continuous
                && (target - inner.shown_px).abs() > SETTLE_TOLERANCE)
                .then_some(Settling { target, retries: 0 });
            inner.shown_px = target;
        }
        if let Some(window) = self.window() {
            window.set_scroll_y(-target);
        }
    }

    /// The reader's place, to be put back with [`Viewer::restore_place`] once
    /// the rows or their height have changed.
    fn place(&self) -> Option<Place> {
        let inner = self.inner.borrow();
        let last = inner.specs.len().checked_sub(1)?;
        // A position still waiting for the viewport has no offset yet; its row
        // is the whole truth.
        let (row, into_row) = if inner.continuous && !inner.position_pending {
            row_at(&inner, inner.scroll_px)
        } else {
            (inner.current_row.min(last), 0.0)
        };
        Some(Place { page: reading_page_in(&inner, &inner.specs[row]), into_row })
    }

    /// Puts the reader back at `place` after a relayout: in continuous mode by
    /// scrolling so its page's row is at the top again, as far into the row as
    /// before, and in paged mode by showing that row.
    fn restore_place(&self, place: Option<Place>) {
        let Some(place) = place else {
            return;
        };
        let scroll = {
            let mut inner = self.inner.borrow_mut();
            let Some(&(row, _)) = inner.page_loc.get(place.page) else {
                return;
            };
            inner.current_row = row;
            if inner.continuous && !inner.position_pending {
                let target = ((row as f32 + place.into_row) * row_height_px(&inner))
                    .clamp(0.0, max_scroll_px(&inner));
                inner.scroll_px = target;
                Some(target)
            } else {
                None
            }
        };
        match scroll {
            Some(target) => self.show_offset(target),
            None => {
                self.fit_paged_row();
                self.refresh_current_row();
                self.push_paged_offsets();
            }
        }
        self.update_current_page();
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

        let rows = Self::model_rows(&self.inner.borrow(), Self::empty_page);
        self.model.set_vec(rows);
        if let Some(window) = self.window() {
            window.set_row_height_pt(self.inner.borrow().ref_h_pt);
        }
        self.refresh_current_row();
        self.request_current_row_if_paged();
        self.update_current_page();
        self.push_paged_offsets();
        self.build_thumb_model();
    }

    /// An unrendered slot carrying only the page's fixed size, and whether it
    /// failed to render.
    fn empty_page(inner: &Inner, page: usize) -> PageEntry {
        let (width_pt, height_pt) = inner.pages_pt[page];
        PageEntry {
            page: page as i32,
            width_pt,
            height_pt,
            image: Image::default(),
            failed: inner.failed.contains(&page),
            highlights: areas_of(&inner.highlights, page),
            selection: areas_of(&inner.selection, page),
        }
    }

    /// A dummy entry for the unused right half of a single-page row.
    fn placeholder() -> PageEntry {
        PageEntry {
            page: -1,
            width_pt: 0.0,
            height_pt: 0.0,
            image: Image::default(),
            failed: false,
            highlights: ModelRc::default(),
            selection: ModelRc::default(),
        }
    }

    /// Takes in `pages`, the text of the pages from `first_page` on that the
    /// document's indexer has read, and searches again with them.
    pub(crate) fn on_text_indexed(&self, first_page: usize, pages: Vec<PageText>) {
        let searching = {
            let mut search = self.search.borrow_mut();
            if first_page != search.index.len() {
                return;
            }
            search.index.extend(pages);
            !search.query.trim().is_empty()
        };
        self.vectorize_more();
        if searching {
            self.request_search();
        } else {
            self.publish_search();
        }
    }

    /// Takes in the images of `pages`, which the cataloguer read from
    /// `first_page` on. The list is built once every page has been read:
    /// until then the tab says how far the cataloguer has got.
    pub(crate) fn on_catalogued(&self, first_page: usize, pages: Vec<Vec<ImageSpot>>) {
        {
            let mut images = self.images.borrow_mut();
            if first_page != images.catalogue.len() {
                return;
            }
            images.catalogue.extend(pages);
        }
        if self.images_catalogued() {
            self.build_image_rows();
        }
        self.publish_images();
    }

    /// Whether every page's images have been catalogued.
    pub fn images_catalogued(&self) -> bool {
        self.images.borrow().catalogue.len() >= self.inner.borrow().pages_pt.len()
    }

    /// Sets which images the list leaves out and which it gathers, and
    /// builds it again that way.
    pub fn set_image_filters(&self, filters: ImageFilters) {
        {
            let mut images = self.images.borrow_mut();
            if images.filters == filters {
                return;
            }
            images.filters = filters;
        }
        if self.images_catalogued() {
            self.build_image_rows();
        }
        self.publish_images();
    }

    /// Builds the sidebar's list from the catalogue: the images of each
    /// page under its heading, leaving out the small ones when asked, and
    /// with the images that are drawn more than once gathered at the end
    /// when asked, each once. Previews already rendered are kept.
    fn build_image_rows(&self) {
        let mut images = self.images.borrow_mut();
        let ImageState { catalogue, previews, filters, .. } = &*images;
        // Every image's occurrences, in the order the images first appear.
        let mut groups: Vec<(ImageKey, Vec<(usize, usize)>)> = Vec::new();
        let mut group_of: HashMap<ImageKey, usize> = HashMap::new();
        let mut small = 0;
        for (page, spots) in catalogue.iter().enumerate() {
            for spot in spots {
                if filters.hide_small && spot.is_small() {
                    small += 1;
                    continue;
                }
                let group = *group_of.entry(spot.key).or_insert_with(|| {
                    groups.push((spot.key, Vec::new()));
                    groups.len() - 1
                });
                groups[group].1.push((page, spot.ordinal));
            }
        }
        let repeated = |key: &ImageKey| filters.group_repeats && groups[group_of[key]].1.len() > 1;

        let mut rows = Vec::new();
        let preview = |page: usize, ordinal: usize| {
            previews.get(&(page, ordinal)).cloned().unwrap_or_default()
        };
        for (page, spots) in catalogue.iter().enumerate() {
            let mut first = true;
            for spot in spots {
                if (filters.hide_small && spot.is_small()) || repeated(&spot.key) {
                    continue;
                }
                rows.push(ImageRow {
                    page: page as i32,
                    ordinal: spot.ordinal as i32,
                    heading: if first { Heading::Page } else { Heading::None }.index(),
                    width: spot.width as i32,
                    height: spot.height as i32,
                    repeats: 0,
                    pages: 0,
                    preview: preview(page, spot.ordinal),
                });
                first = false;
            }
        }
        let repeated_from = rows.len();
        let mut first = true;
        for (key, places) in &groups {
            if !repeated(key) {
                continue;
            }
            let &(page, ordinal) = &places[0];
            let spot = &catalogue[page][ordinal];
            let mut pages: Vec<usize> = places.iter().map(|&(page, _)| page).collect();
            pages.dedup();
            rows.push(ImageRow {
                page: page as i32,
                ordinal: ordinal as i32,
                heading: if first { Heading::Repeated } else { Heading::None }.index(),
                width: spot.width as i32,
                height: spot.height as i32,
                repeats: places.len() as i32,
                pages: pages.len() as i32,
                preview: preview(page, ordinal),
            });
            first = false;
        }
        images.repeated_from = repeated_from;
        images.small = small;
        self.image_rows.set_vec(rows);
        drop(images);
        self.follow_images();
    }

    /// Puts the state of the images tab on the window: the list once it is
    /// built, and until then how far the cataloguer has got.
    fn publish_images(&self) {
        let Some(window) = self.window() else {
            return;
        };
        let images = self.images.borrow();
        let (read, total) = (images.catalogue.len(), self.inner.borrow().pages_pt.len());
        let ready = read >= total;
        window.set_images_ready(ready);
        window.set_small_images_hidden(images.small as i32);
        let status = if !ready {
            format!("Finding images, {read} of {total} pages.")
        } else if self.image_rows.row_count() == 0 && images.small > 0 {
            format!("No images, apart from {} small ones.", images.small)
        } else if self.image_rows.row_count() == 0 {
            "No images.".to_string()
        } else {
            String::new()
        };
        window.set_image_status(status.into());
    }

    /// Tells the window which image row the list follows: the first image
    /// on the page being read, or on the next page with any, among the
    /// rows listed by page.
    fn follow_images(&self) {
        let Some(window) = self.window() else {
            return;
        };
        let page = self.reading_page() as i32;
        // The rows are in page order, so the first at or after the page is
        // where they stop being before it.
        let count = self.images.borrow().repeated_from.min(self.image_rows.row_count());
        let (mut low, mut high) = (0, count);
        while low < high {
            let middle = (low + high) / 2;
            if self.image_rows.row_data(middle).is_some_and(|row| row.page < page) {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        window.set_image_current(if low < count { low as i32 } else { -1 });
    }

    /// The sidebar shows the image at `row` of its list, which wants its
    /// preview.
    pub fn request_preview(&self, row: usize) {
        if let Some(item) = self.image_rows.row_data(row)
            && item.preview.size().width == 0
        {
            let request = ThumbRequest::Preview { page: item.page, ordinal: item.ordinal as usize };
            let _ = self.thumb_sender.send(request);
        }
    }

    /// The preview of the image drawn `ordinal`-th on `page` has rendered.
    pub fn on_preview_rendered(&self, page: usize, ordinal: usize, preview: Image) {
        self.images.borrow_mut().previews.insert((page, ordinal), preview.clone());
        let row = (0..self.image_rows.row_count()).find(|&row| {
            self.image_rows
                .row_data(row)
                .is_some_and(|item| item.page == page as i32 && item.ordinal == ordinal as i32)
        });
        if let Some(row) = row
            && let Some(mut item) = self.image_rows.row_data(row)
        {
            item.preview = preview;
            self.image_rows.set_row_data(row, item);
        }
    }

    /// The reader clicked the image at `row` of the sidebar's list: it is
    /// copied as it is embedded, and its outline flashes on its page.
    pub fn copy_image(&self, row: usize) {
        let Some(item) = self.image_rows.row_data(row) else {
            return;
        };
        let (page, ordinal) = (item.page as usize, item.ordinal as usize);
        let request = ScreenshotRequest { page: page as i32, shot: Shot::Image { ordinal } };
        let _ = self.sender.send(WorkerMessage::Screenshot(request));
        let bounds = self
            .images
            .borrow()
            .catalogue
            .get(page)
            .and_then(|spots| spots.get(ordinal))
            .map(|spot| spot.bounds);
        if let Some(bounds) = bounds
            && let Some(window) = self.window()
        {
            let shot = window.global::<Screenshot>();
            show_area(&shot, page, bounds);
            shot.set_dragging(false);
            shot.set_flashing(true);
        }
    }

    /// Whether every page's text has been indexed.
    pub fn text_indexed(&self) -> bool {
        self.search.borrow().index.len() >= self.inner.borrow().pages_pt.len()
    }

    /// The reader picked what the search looks for. The query stays, and is
    /// searched for again the new way.
    pub fn set_search_mode(&self, mode: SearchMode) {
        {
            let mut search = self.search.borrow_mut();
            if search.mode == mode {
                return;
            }
            search.mode = mode;
            search.jump = true;
        }
        if let Some(window) = self.window() {
            window.set_search_mode(mode.index());
        }
        self.vectorize_more();
        self.request_search();
    }

    pub fn search_mode(&self) -> SearchMode {
        self.search.borrow().mode
    }

    /// Whether a vectorizer is making page vectors right now.
    pub fn vectorizing(&self) -> bool {
        self.search.borrow().vectorizing
    }

    /// Sets a vectorizer going on the pages whose vectors are not made yet,
    /// while the search is by meaning, nothing else is vectorizing, and the
    /// model has not failed to load. A page's chunk takes in part of the
    /// page after it, so a page is only ready once that one is indexed, or
    /// it is the last.
    fn vectorize_more(&self) {
        let job = {
            let mut search = self.search.borrow_mut();
            if search.mode != SearchMode::Meaning
                || search.vectorizing
                || search.model_error.is_some()
            {
                return;
            }
            let pages = self.inner.borrow().pages_pt.len();
            let indexed = search.index.len().min(pages);
            let ready = if indexed == pages { pages } else { indexed.saturating_sub(1) };
            let first_page = search.vectors.len();
            if first_page >= ready {
                return;
            }
            let ready = ready.min(first_page + VECTORIZE_BATCH);
            let chunks: Vec<String> = (first_page..ready)
                .map(|page| {
                    let text = |page: usize| search.index.get(page).map(|text| text.text.as_str());
                    semantic::chunk(
                        page.checked_sub(1).and_then(text),
                        text(page).unwrap_or(""),
                        text(page + 1),
                    )
                })
                .collect();
            search.vectorizing = true;
            (first_page, chunks)
        };
        self.vectorizer.spawn(job.0, job.1);
        self.publish_search();
    }

    /// Takes in what a vectorizer made of the pages from `first_page` on,
    /// sets one going on the rest, and searches again with them.
    pub(crate) fn on_vectorized(&self, first_page: usize, result: Result<Vectors, String>) {
        let searching = {
            let mut search = self.search.borrow_mut();
            search.vectorizing = false;
            match result {
                Ok(vectors) if first_page == search.vectors.len() => {
                    search.vectors.append(&vectors)
                }
                Ok(_) => {}
                Err(err) => search.model_error = Some(err),
            }
            search.mode == SearchMode::Meaning && !search.query.trim().is_empty()
        };
        self.vectorize_more();
        if searching {
            self.request_search();
        } else {
            self.publish_search();
        }
    }

    /// The reader edited the search query. The view moves to the first hit
    /// from the page being read once the search finds one.
    pub fn search_edited(&self, query: &str) {
        {
            let mut search = self.search.borrow_mut();
            search.query = query.to_string();
            search.jump = true;
        }
        self.request_search();
    }

    /// Moves to the next (`dir` +1) or previous (-1) hit, round from the last
    /// to the first. Without a current hit, starts from the page being read.
    pub fn search_step(&self, dir: i32) {
        let reading = self.settings().page;
        let next = {
            let mut search = self.search.borrow_mut();
            let count = search.hits.len();
            if count == 0 {
                return;
            }
            let next = match search.current {
                Some(current) => (current as i64 + dir as i64).rem_euclid(count as i64) as usize,
                None if dir < 0 => {
                    search.hits.iter().rposition(|hit| hit.page <= reading).unwrap_or(count - 1)
                }
                None => search.hits.iter().position(|hit| hit.page >= reading).unwrap_or(0),
            };
            search.current = Some(next);
            search.jump = false;
            next
        };
        self.show_current_hit();
        self.go_to_hit(next);
    }

    /// Moves to the hit at `index` in the results list.
    pub fn go_to_search_result(&self, index: usize) {
        {
            let mut search = self.search.borrow_mut();
            if index >= search.hits.len() {
                return;
            }
            search.current = Some(index);
            search.jump = false;
        }
        self.show_current_hit();
        self.go_to_hit(index);
    }

    /// Searches now, or once [`SEARCH_INTERVAL`] has passed since the last
    /// search, so typing searches as it goes without searching on every key.
    fn request_search(&self) {
        if self.search_timer.running() {
            self.search_pending.set(true);
            return;
        }
        self.run_search();
        let me = self.me.clone();
        self.search_timer.start(TimerMode::SingleShot, SEARCH_INTERVAL, move || {
            if let Some(viewer) = me.upgrade()
                && viewer.search_pending.replace(false)
            {
                viewer.request_search();
            }
        });
    }

    /// Searches the indexed text for the query and shows what it found. A hit
    /// the reader was on stays current if it is still found.
    fn run_search(&self) {
        let reading = self.settings().page;
        let jump_to = {
            let mut search = self.search.borrow_mut();
            let found = match search.mode {
                SearchMode::Exact => search::search(&search.index, &search.query, MAX_HITS),
                SearchMode::Meaning => self.related_pages(&search),
            };
            let previous = search.current.and_then(|current| search.hits.get(current).copied());
            search.current = if search.jump {
                // Pages by meaning come best first, and the best is the one
                // to go to wherever the reader is.
                match search.mode {
                    SearchMode::Exact => found.hits.iter().position(|hit| hit.page >= reading),
                    SearchMode::Meaning => None,
                }
                .or((!found.hits.is_empty()).then_some(0))
            } else {
                previous.and_then(|previous| found.hits.iter().position(|hit| *hit == previous))
            };
            let jump_to = if search.jump { search.current } else { None };
            if jump_to.is_some() || search.query.trim().is_empty() {
                search.jump = false;
            }
            search.hits = found.hits;
            search.total = found.total;
            jump_to
        };

        let rows: Vec<SearchResult> = {
            let search = self.search.borrow();
            let hits = &search.hits;
            let mut headings = 0;
            hits.iter()
                .enumerate()
                .map(|(index, hit)| {
                    let text = &search.index[hit.page];
                    let snippet = match search.mode {
                        SearchMode::Exact => text.snippet(hit.start, hit.end),
                        // How the passage starts, or the page when it has
                        // none, in plain text: none of it is the query's
                        // words, so none of it is bold.
                        SearchMode::Meaning => Snippet {
                            before: String::new(),
                            found: String::new(),
                            after: text.snippet(hit.start, hit.start).after,
                        },
                    };
                    // Hits come in document order, so a page's hits are
                    // together and only its first carries the count.
                    let first_on_page = index == 0 || hits[index - 1].page != hit.page;
                    let page_hits = if first_on_page {
                        headings += 1;
                        hits[index..].iter().take_while(|h| h.page == hit.page).count()
                    } else {
                        0
                    };
                    SearchResult {
                        page: hit.page as i32,
                        page_hits: page_hits as i32,
                        headings,
                        before: snippet.before.into(),
                        found: snippet.found.into(),
                        after: snippet.after.into(),
                    }
                })
                .collect()
        };
        self.results.set_vec(rows);
        self.show_current_hit();
        if let Some(index) = jump_to {
            self.go_to_hit(index);
        }
    }

    /// The pages most like the query, best first, each as a hit spanning the
    /// passage it matched by, or nothing when no passage stands out. None
    /// while the model is still loading, as the query is vectorized here on
    /// the UI thread and loading would hold it.
    fn related_pages(&self, search: &SearchState) -> search::Found {
        let query = search.query.trim();
        let Some(model) = self.vectorizer.model().loaded().filter(|_| !query.is_empty()) else {
            return search::Found::default();
        };
        let query = model.embed(&[query.to_string()]);
        let query = query.get(0);
        let hits: Vec<Hit> = semantic::search(&search.vectors, query, MAX_RELATED)
            .into_iter()
            .map(|(page, _)| {
                let text = search.index.get(page).map_or("", |text| text.text.as_str());
                let (start, end) = semantic::best_passage(&model, text, query).unwrap_or((0, 0));
                Hit { page, start, end }
            })
            .collect();
        let total = hits.len();
        search::Found { hits, total }
    }

    /// Outlines every hit on its page, the current one stronger, and tells
    /// the window how the search went.
    fn show_current_hit(&self) {
        let highlights = {
            let search = self.search.borrow();
            let mut highlights: HashMap<usize, Vec<Highlight>> = HashMap::new();
            for (index, hit) in search.hits.iter().enumerate() {
                let current = search.current == Some(index);
                for area in search.index[hit.page].areas(hit.start, hit.end) {
                    highlights.entry(hit.page).or_default().push(Highlight {
                        x: area.x,
                        y: area.y,
                        width: area.width,
                        height: area.height,
                        current,
                    });
                }
            }
            highlights
        };
        self.set_areas(Overlay::Hits, highlights);
        self.publish_search();
    }

    /// Replaces the areas of one overlay, updating only the pages whose
    /// areas changed.
    fn set_areas(&self, overlay: Overlay, areas: HashMap<usize, Vec<Highlight>>) {
        let changed: Vec<(usize, bool, ModelRc<Highlight>)> = {
            let mut inner = self.inner.borrow_mut();
            let inner = &mut *inner;
            let current = match overlay {
                Overlay::Hits => &mut inner.highlights,
                Overlay::Selection => &mut inner.selection,
            };
            let pages: HashSet<usize> = current.keys().chain(areas.keys()).copied().collect();
            let changed: Vec<usize> =
                pages.into_iter().filter(|page| current.get(page) != areas.get(page)).collect();
            *current = areas;
            changed
                .into_iter()
                .filter_map(|page| {
                    let &(row, is_right) = inner.page_loc.get(page)?;
                    Some((row, is_right, areas_of(current, page)))
                })
                .collect()
        };
        let current_row = self.inner.borrow().current_row;
        let mut current_changed = false;
        for (row, is_right, areas) in changed {
            let Some(mut page_row) = self.model.row_data(row) else {
                continue;
            };
            let entry = if is_right { &mut page_row.right } else { &mut page_row.left };
            match overlay {
                Overlay::Hits => entry.highlights = areas,
                Overlay::Selection => entry.selection = areas,
            }
            self.model.set_row_data(row, page_row);
            current_changed |= row == current_row;
        }
        if current_changed && !self.inner.borrow().continuous {
            self.refresh_current_row();
        }
    }

    /// Tells the window how the search went and which hit is current.
    fn publish_search(&self) {
        let Some(window) = self.window() else {
            return;
        };
        let pages = self.inner.borrow().pages_pt.len();
        let search = self.search.borrow();
        let indexed = search.index.len().min(pages);
        let indexing = indexed < pages;
        let so_far = if indexing { " so far" } else { "" };
        let status = if !indexing && search.index.iter().all(PageText::is_empty) {
            "This document has no searchable text.".to_string()
        } else if search.mode == SearchMode::Meaning {
            self.meaning_status(&search, pages)
        } else if search.query.trim().is_empty() {
            if indexing {
                format!("Indexing for search, {indexed} of {pages} pages.")
            } else {
                String::new()
            }
        } else {
            match search.total {
                0 => format!("No matches{so_far}."),
                1 => format!("1 match{so_far}."),
                total if total > search.hits.len() => {
                    format!("{total} matches{so_far}, showing the first {}.", search.hits.len())
                }
                total => format!("{total} matches{so_far}."),
            }
        };
        window.set_search_status(status.into());
        window.set_search_current(search.current.map_or(-1, |current| current as i32));
    }

    /// How a search by meaning is going, for a document of `pages` pages.
    fn meaning_status(&self, search: &SearchState, pages: usize) -> String {
        if let Some(err) = &search.model_error {
            return format!("Searching by meaning is unavailable: {err}.");
        }
        let vectorized = search.vectors.len().min(pages);
        let progress = format!("vectorizing {vectorized} of {pages} pages");
        if search.query.trim().is_empty() {
            if vectorized < pages {
                let progress = progress[..1].to_uppercase() + &progress[1..];
                format!("{progress}.")
            } else {
                String::new()
            }
        } else if self.vectorizer.model().loaded().is_none() {
            format!("Loading the model, which takes a moment, then {progress}.")
        } else {
            let found = match search.total {
                0 => "No related pages".to_string(),
                1 => "1 related page".to_string(),
                total => format!("{total} related pages"),
            };
            if vectorized < pages {
                format!("{found} so far, {progress}.")
            } else {
                format!("{found}.")
            }
        }
    }

    /// The page each outline entry goes to, in the order they are listed,
    /// for the sidebar to mark the one the page being read is under.
    pub fn set_outline_pages(&self, pages: Vec<i32>) {
        self.outline_marks.set_vec(vec![false; pages.len()]);
        {
            let mut inner = self.inner.borrow_mut();
            inner.outline_pages = pages;
            inner.outline_marked.clear();
        }
        self.update_current_page();
    }

    /// Marks the outline entries the pages shown belong to: those of the
    /// row the counter's `page` is in, both pages of a spread. Only the
    /// entries whose mark changes are touched, and the list is never
    /// scrolled, so it stays where the reader left it.
    fn mark_outline(&self, page: i32) {
        let marked = {
            let inner = self.inner.borrow();
            let shown: Vec<i32> = usize::try_from(page - 1)
                .ok()
                .and_then(|page| inner.page_loc.get(page))
                .and_then(|&(row, _)| inner.specs.get(row))
                .map(|spec| spec.pages().map(|page| page as i32).collect())
                .unwrap_or_default();
            headings_for(&inner.outline_pages, &shown)
        };
        let unmarked =
            std::mem::replace(&mut self.inner.borrow_mut().outline_marked, marked.clone());
        for index in unmarked.into_iter().filter(|index| !marked.contains(index)) {
            self.outline_marks.set_row_data(index, false);
        }
        for &index in &marked {
            if self.outline_marks.row_data(index) == Some(false) {
                self.outline_marks.set_row_data(index, true);
            }
        }
    }

    /// The document's links, page by page, once the worker has read them.
    pub fn set_links(&self, links: Vec<Vec<PageLink>>) {
        self.inner.borrow_mut().links = links;
    }

    /// The links on `page`.
    pub fn page_links(&self, page: usize) -> Vec<PageLink> {
        self.inner.borrow().links.get(page).cloned().unwrap_or_default()
    }

    /// The link under `x`, `y` points from the corner of `page`, if any.
    pub fn link_at(&self, page: usize, x: f32, y: f32) -> Option<PageLink> {
        let inner = self.inner.borrow();
        links::link_at(inner.links.get(page)?, x, y).cloned()
    }

    /// Whether a link is under `x`, `y` points from the corner of `page`,
    /// for the cursor to show.
    pub fn link_under(&self, page: usize, x: f32, y: f32) -> bool {
        self.link_at(page, x, y).is_some()
    }

    /// The reader pressed on `page` at `x`, `y` points from its corner: on
    /// a link, which letting go there follows, or else starting a selection
    /// there. A second or third press on the same place within
    /// [`MULTI_CLICK`] selects the word or the line.
    pub fn select_from(&self, page: usize, x: f32, y: f32) {
        if let Some(link) = self.link_at(page, x, y) {
            self.clear_selection();
            self.selection.borrow_mut().pending_link = Some((page, x, y, link));
            return;
        }
        self.start_selection(page, x, y);
    }

    /// Starts a selection at `x`, `y` points from the corner of `page`.
    fn start_selection(&self, page: usize, x: f32, y: f32) {
        let Some(position) = self.resolve(page, x, y) else {
            self.clear_selection();
            return;
        };
        let now = Instant::now();
        {
            let mut selection = self.selection.borrow_mut();
            let again = selection.last_press.is_some_and(|(at, there)| {
                now.duration_since(at) < MULTI_CLICK
                    && there.page == position.page
                    && there.byte.abs_diff(position.byte) <= 1
            });
            selection.clicks = if again { selection.clicks + 1 } else { 1 };
            selection.unit = Unit::from_clicks(selection.clicks);
            selection.last_press = Some((now, position));
            selection.range = Some((position, position));
            selection.drag = Some((page, x, y));
        }
        self.show_selection();
    }

    /// The drag that began on `page` is at `x`, `y` points from that page's
    /// corner, which may be off it, and the selection reaches there.
    pub fn select_to(&self, page: usize, x: f32, y: f32) {
        // A press on a link is a click until the pointer has moved as far
        // as a screenshot's may and still be one; then it was the start of
        // a drag, which selects text from where it pressed.
        let pending = self.selection.borrow_mut().pending_link.take();
        if let Some((origin, origin_x, origin_y, link)) = pending {
            let limit = CLICK_PX / page_density(&self.inner.borrow(), origin);
            if (x - origin_x).abs() < limit && (y - origin_y).abs() < limit {
                self.selection.borrow_mut().pending_link = Some((origin, origin_x, origin_y, link));
                return;
            }
            self.start_selection(origin, origin_x, origin_y);
        }
        if self.selection.borrow().drag.is_none() {
            return;
        }
        self.selection.borrow_mut().drag = Some((page, x, y));
        self.follow_drag();
        self.scroll_for_drag();
    }

    /// The drag ended, or a press on a link was let go of without moving,
    /// which is the click that follows the link, handed back to be followed.
    pub fn select_done(&self) -> Option<PageLink> {
        let pending = self.selection.borrow_mut().pending_link.take();
        if let Some((_, _, _, link)) = pending {
            return Some(link);
        }
        self.selection.borrow_mut().drag = None;
        self.drag_scroll.stop();
        None
    }

    /// The wheel turned by `delta_x`, `delta_y` during a drag, which holds the
    /// pointer, so the view scrolls here instead and the drag goes on from
    /// where the pointer now is over the moved pages. In paged mode only
    /// the page scrolls: turning to another page would leave the drag's
    /// origin off screen.
    pub fn select_scroll(&self, delta_x: f32, delta_y: f32, shift: bool) {
        let scrolled = self.scroll_under_drag(delta_x, delta_y, shift);
        self.drag_moved_by(scrolled);
    }

    /// Scrolls the view for the wheel turning by `delta_x`, `delta_y` while
    /// a drag holds the pointer, and says how far down the pages moved.
    fn scroll_under_drag(&self, delta_x: f32, delta_y: f32, shift: bool) -> f32 {
        let vertical = if shift { 0.0 } else { -delta_y };
        let horizontal = if shift { -delta_y } else { -delta_x };
        let scrolled = self.scroll_view_by(vertical);
        if !self.inner.borrow().continuous && horizontal != 0.0 {
            {
                let mut inner = self.inner.borrow_mut();
                inner.paged_scroll_x += horizontal;
            }
            self.push_paged_offsets();
        }
        scrolled
    }

    /// The reader pressed on `page` at `x`, `y` points from its corner to
    /// take a screenshot: of the page if they let go there, of the part of
    /// it they drag over otherwise.
    pub fn capture_from(&self, page: usize, x: f32, y: f32) {
        if self.inner.borrow().pages_pt.get(page).is_none() {
            return;
        }
        *self.capture.borrow_mut() = Some(Capture::begin(page, x, y));
        self.show_capture();
    }

    /// Whether a drag taking a screenshot is going on.
    pub fn capturing(&self) -> bool {
        self.capture.borrow().is_some()
    }

    /// The drag taking a screenshot is at `x`, `y` points from the corner of
    /// the page it began on, which may be off that page.
    pub fn capture_to(&self, x: f32, y: f32) {
        {
            let mut capture = self.capture.borrow_mut();
            let Some(capture) = capture.as_mut() else {
                return;
            };
            capture.reach = (x, y);
        }
        self.show_capture();
    }

    /// The wheel turned by `delta_x`, `delta_y` while a drag taking a
    /// screenshot holds the pointer, so the view scrolls here and the drag
    /// goes on from where the pointer now is over the moved page.
    pub fn capture_scroll(&self, delta_x: f32, delta_y: f32, shift: bool) {
        let scrolled = self.scroll_under_drag(delta_x, delta_y, shift);
        if scrolled == 0.0 {
            return;
        }
        {
            let mut capture = self.capture.borrow_mut();
            let Some(capture) = capture.as_mut() else {
                return;
            };
            capture.reach.1 += scrolled / page_density(&self.inner.borrow(), capture.page);
        }
        self.show_capture();
    }

    /// The reader let go: the screenshot is taken, of the whole page for a
    /// click, and its outline flashes where it was taken.
    pub fn capture_done(&self) {
        let Some(capture) = self.capture.borrow_mut().take() else {
            return;
        };
        let (request, area) = {
            let inner = self.inner.borrow();
            let (width, height) = inner.pages_pt[capture.page];
            capture.request(width, height, page_density(&inner, capture.page))
        };
        let _ = self.sender.send(WorkerMessage::Screenshot(request));
        if let Some(window) = self.window() {
            let shot = window.global::<Screenshot>();
            show_area(&shot, capture.page, area);
            shot.set_dragging(false);
            shot.set_flashing(true);
            shot.set_active(false);
        }
    }

    /// Gives up the screenshot being dragged out, if one is.
    pub fn capture_cancel(&self) {
        if self.capture.borrow_mut().take().is_none() {
            return;
        }
        if let Some(window) = self.window() {
            window.global::<Screenshot>().set_dragging(false);
        }
    }

    /// Puts the capture's outline on the window, as far as the drag has
    /// reached.
    fn show_capture(&self) {
        let Some(capture) = *self.capture.borrow() else {
            return;
        };
        let Some(window) = self.window() else {
            return;
        };
        let area = {
            let inner = self.inner.borrow();
            let (width, height) = inner.pages_pt[capture.page];
            capture.area(width, height, page_density(&inner, capture.page))
        };
        let shot = window.global::<Screenshot>();
        show_area(&shot, capture.page, area);
        shot.set_dragging(true);
        shot.set_flashing(false);
    }

    /// Lets the selection go.
    pub fn clear_selection(&self) {
        {
            let mut selection = self.selection.borrow_mut();
            selection.range = None;
            selection.drag = None;
        }
        self.drag_scroll.stop();
        self.show_selection();
    }

    /// Selects every page's text, as far as it has been indexed.
    pub fn select_all(&self) {
        {
            let search = self.search.borrow();
            let Some(last) = search.index.len().checked_sub(1) else {
                return;
            };
            let mut selection = self.selection.borrow_mut();
            selection.unit = Unit::Char;
            selection.drag = None;
            selection.range = Some((
                TextPos { page: 0, byte: 0 },
                TextPos { page: last, byte: search.index[last].len() },
            ));
        }
        self.drag_scroll.stop();
        self.show_selection();
    }

    /// The selected text as it goes on the clipboard, with each page's text
    /// on lines of its own, or `None` while nothing is selected.
    pub fn selected_text(&self) -> Option<String> {
        let search = self.search.borrow();
        let (start, end) = self.bounds(&search.index)?;
        let mut text = String::new();
        for page in start.page..=end.page {
            let page_text = &search.index[page];
            let from = if page == start.page { start.byte } else { 0 };
            let to = if page == end.page { end.byte } else { page_text.len() };
            if page > start.page {
                text.push('\n');
            }
            text.push_str(&page_text.copied_text(from, to));
        }
        (!text.is_empty()).then_some(text)
    }

    /// The selection in document order, grown to whole words or lines when
    /// the reader clicked for them, or `None` while nothing is selected.
    fn bounds(&self, index: &[PageText]) -> Option<(TextPos, TextPos)> {
        let selection = self.selection.borrow();
        let (anchor, focus) = selection.range?;
        let (mut start, mut end) = if anchor <= focus { (anchor, focus) } else { (focus, anchor) };
        if index.len() <= end.page {
            return None;
        }
        let grow = |text: &PageText, byte: usize| match selection.unit {
            Unit::Char => (byte, byte),
            Unit::Word => text.word_at(byte),
            Unit::Line => text.line_at(byte),
        };
        start.byte = grow(&index[start.page], start.byte).0;
        // The focus of a drag sits just past what it reached, so the unit
        // it ends in is the one the character before it is in, unless the
        // selection is empty. That character may take more than one byte.
        let last = if end > start {
            index[end.page].text.floor_char_boundary(end.byte.saturating_sub(1))
        } else {
            end.byte
        };
        end.byte = grow(&index[end.page], last).1.max(end.byte);
        (start < end).then_some((start, end))
    }

    /// Outlines the selected text on its pages.
    fn show_selection(&self) {
        let areas = {
            let search = self.search.borrow();
            let mut areas: HashMap<usize, Vec<Highlight>> = HashMap::new();
            if let Some((start, end)) = self.bounds(&search.index) {
                for page in start.page..=end.page {
                    let page_text = &search.index[page];
                    let from = if page == start.page { start.byte } else { 0 };
                    let to = if page == end.page { end.byte } else { page_text.len() };
                    let page_areas: Vec<Highlight> = page_text
                        .areas(from, to)
                        .into_iter()
                        .map(|area| Highlight {
                            x: area.x,
                            y: area.y,
                            width: area.width,
                            height: area.height,
                            current: false,
                        })
                        .collect();
                    if !page_areas.is_empty() {
                        areas.insert(page, page_areas);
                    }
                }
            }
            areas
        };
        self.set_areas(Overlay::Selection, areas);
    }

    /// The place in the text at `x`, `y` points from the corner of `page`,
    /// or the nearest place when the point is off that page: on the page
    /// under it, or the nearest page, and on the nearest line there. `None`
    /// when that page has no text, or none indexed yet.
    fn resolve(&self, page: usize, x: f32, y: f32) -> Option<TextPos> {
        let inner = self.inner.borrow();
        let search = self.search.borrow();
        let &(width, height) = inner.pages_pt.get(page)?;
        let (page, x, y) = if (0.0..=width).contains(&x) && (0.0..=height).contains(&y) {
            (page, x, y)
        } else {
            let (origin_x, origin_y) = page_origin_px(&inner, page)?;
            let density = page_density(&inner, page);
            let (doc_x, doc_y) = (origin_x + x * density, origin_y + y * density);
            let target = page_at(&inner, doc_x, doc_y)?;
            let (target_x, target_y) = page_origin_px(&inner, target)?;
            let density = page_density(&inner, target);
            let (width, height) = inner.pages_pt[target];
            (
                target,
                ((doc_x - target_x) / density).clamp(0.0, width),
                ((doc_y - target_y) / density).clamp(0.0, height),
            )
        };
        let byte = search.index.get(page)?.position_at(x, y)?;
        Some(TextPos { page, byte })
    }

    /// Moves the focus to where the drag is now.
    fn follow_drag(&self) {
        let Some((page, x, y)) = self.selection.borrow().drag else {
            return;
        };
        let Some(position) = self.resolve(page, x, y) else {
            return;
        };
        if let Some((_, focus)) = &mut self.selection.borrow_mut().range {
            *focus = position;
        }
        self.show_selection();
    }

    /// Scrolls the view by `delta` logical pixels, downwards when positive,
    /// as far as it goes, and says how far it went. Paged mode scrolls
    /// within the page shown.
    fn scroll_view_by(&self, delta: f32) -> f32 {
        if self.inner.borrow().continuous {
            let before = self.inner.borrow().scroll_px;
            self.scroll_by(delta);
            self.inner.borrow().scroll_px - before
        } else {
            let before = self.inner.borrow().paged_scroll_y;
            self.inner.borrow_mut().paged_scroll_y += delta;
            self.push_paged_offsets();
            self.inner.borrow().paged_scroll_y - before
        }
    }

    /// The pages moved up by `scrolled` logical pixels under a still pointer,
    /// so the drag is that much further down the page it began on.
    fn drag_moved_by(&self, scrolled: f32) {
        if scrolled == 0.0 {
            return;
        }
        {
            let mut selection = self.selection.borrow_mut();
            let Some((page, _, y)) = &mut selection.drag else {
                return;
            };
            *y += scrolled / page_density(&self.inner.borrow(), *page);
        }
        self.follow_drag();
    }

    /// How far past the top (negative) or bottom of the view the drag is, in
    /// logical pixels, or zero while it is within the view.
    fn drag_overshoot(&self) -> f32 {
        let Some((page, _, y)) = self.selection.borrow().drag else {
            return 0.0;
        };
        let inner = self.inner.borrow();
        let Some((_, origin_y)) = page_origin_px(&inner, page) else {
            return 0.0;
        };
        let doc_y = origin_y + y * page_density(&inner, page);
        let view_h = inner.view.map_or(0.0, |(_, height)| height);
        let view_y = if inner.continuous {
            doc_y - inner.scroll_px
        } else {
            let (_, content_h) = paged_content_size(&inner);
            let offset_y = if content_h <= view_h {
                (view_h - content_h) / 2.0
            } else {
                -inner.paged_scroll_y
            };
            doc_y + offset_y
        };
        if view_y < 0.0 {
            view_y
        } else if view_y > view_h {
            view_y - view_h
        } else {
            0.0
        }
    }

    /// Keeps the view scrolling while the drag is held past its top or
    /// bottom edge, so a selection can reach past what is on screen.
    fn scroll_for_drag(&self) {
        if self.drag_overshoot() == 0.0 {
            self.drag_scroll.stop();
            return;
        }
        if self.drag_scroll.running() {
            return;
        }
        let me = self.me.clone();
        self.drag_scroll.start(TimerMode::Repeated, DRAG_SCROLL_INTERVAL, move || {
            let Some(viewer) = me.upgrade() else {
                return;
            };
            let overshoot = viewer.drag_overshoot();
            if overshoot == 0.0 {
                viewer.drag_scroll.stop();
                return;
            }
            let scrolled =
                viewer.scroll_view_by(overshoot.clamp(-DRAG_SCROLL_MAX, DRAG_SCROLL_MAX));
            if scrolled == 0.0 {
                viewer.drag_scroll.stop();
            }
            viewer.drag_moved_by(scrolled);
        });
    }

    /// Scrolls so the hit at `index` is in view, a third of the way down the
    /// view where there is room, with its page as the one being read.
    fn go_to_hit(&self, index: usize) {
        let (page, top_pt) = {
            let search = self.search.borrow();
            let Some(hit) = search.hits.get(index) else {
                return;
            };
            let areas = search.index[hit.page].areas(hit.start, hit.end);
            (hit.page, areas.first().map_or(0.0, |area| area.y))
        };
        self.nav_to_point(page, Some(top_pt));
    }

    /// Scrolls so `page` is in view, with its top at the top of the view,
    /// or with `top_pt` points down it a third of the way down the view
    /// where there is room, as a link or a search hit asks, and with the
    /// page as the one being read.
    pub fn nav_to_point(&self, page: usize, top_pt: Option<f32>) {
        let Some(top_pt) = top_pt else {
            self.nav_to_page(page);
            return;
        };
        let (row, continuous) = {
            let mut inner = self.inner.borrow_mut();
            let Some(&(row, _)) = inner.page_loc.get(page) else {
                return;
            };
            inner.reading_page = page;
            (row, inner.continuous)
        };
        if continuous {
            let target = {
                let mut inner = self.inner.borrow_mut();
                let row_px = row_height_px(&inner);
                let density = BASE_DENSITY * row_zoom(&inner, row);
                let row_top = row as f32 * row_px;
                // Each page sits in the middle of its row's height.
                let page_top = row_top + (row_px - inner.pages_pt[page].1 * density) / 2.0;
                let view_h = inner.view.map_or(0.0, |(_, height)| height);
                let target = (page_top + top_pt * density - view_h / 3.0)
                    .max(row_top)
                    .clamp(0.0, max_scroll_px(&inner));
                inner.current_row = row;
                inner.scroll_px = target;
                target
            };
            self.show_offset(target);
            self.request_visible();
            self.update_current_page();
        } else {
            self.scroll_to_row(row);
            {
                let mut inner = self.inner.borrow_mut();
                let density = BASE_DENSITY * inner.zoom;
                let view_h = inner.view.map_or(0.0, |(_, height)| height);
                inner.paged_scroll_y = (top_pt * density - view_h / 3.0).max(0.0);
            }
            self.push_paged_offsets();
        }
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
        if let (Some(window), Some(row)) = (self.window(), self.model.row_data(index)) {
            window.set_current_row_content(row);
        }
    }

    /// Requests the current row (high priority) plus a few neighbors on each
    /// side as prefetch, each unless already rendered at the current scale.
    /// Used by paged mode and one-shot navigation.
    fn request_current_row(&self) {
        let (visible, neighbors) = {
            let inner = self.inner.borrow();
            if inner.specs.is_empty() {
                return;
            }
            let current = inner.current_row;
            let last = inner.specs.len() - 1;
            let scale = render_scale(&inner);
            let margin = prefetch_rows(&inner, 1, scale);
            let start = current.saturating_sub(margin);
            let end = (current + margin).min(last);
            let (visible, neighbors): (Vec<usize>, Vec<usize>) = (start..=end)
                .filter(|&row| !row_rendered(&inner, row))
                .partition(|&row| row == current);
            (visible, neighbors)
        };
        self.dispatch(&visible, &neighbors);
    }

    fn request_current_row_if_paged(&self) {
        if !self.inner.borrow().continuous {
            self.request_current_row();
        }
    }

    fn send(&self, page: usize, scale: f32, prefetch: bool) {
        let generation = self.inner.borrow().generation;
        let request = RenderRequest { page: page as i32, scale, generation, prefetch };
        let _ = self.sender.send(WorkerMessage::Render(request));
    }

    /// Marks a new view: bumped before issuing a fresh set of render requests so
    /// the worker drops pending renders from the previous view. A render in
    /// progress is aborted only if the new view does not want its page at its
    /// scale, given as `wanted`.
    fn advance_epoch(&self, wanted: &[(i32, f32)]) {
        let epoch = {
            let mut inner = self.inner.borrow_mut();
            inner.generation += 1;
            inner.generation
        };
        self.control.advance(epoch, wanted);
    }

    fn apply_density(&self) {
        let density = BASE_DENSITY * self.inner.borrow().zoom;
        if let Some(window) = self.window() {
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
        // A page that failed at the old scale may render at the new one, such
        // as one that was too large for memory.
        self.inner.borrow_mut().failed.clear();
        if self.inner.borrow().continuous {
            self.request_visible();
        } else {
            self.request_current_row();
        }
    }

    /// Publishes the page number shown by the toolbar. Normally the page at the
    /// top of the view, and of a spread there the one the reader asked for. At
    /// the very bottom of the document it reports the last page, so "End" reads
    /// as the last page even in spread mode (where the top row's left page
    /// would otherwise be one short).
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
                inner
                    .specs
                    .get(inner.current_row)
                    .map_or(0, |spec| reading_page_in(&inner, spec) as i32 + 1)
            }
        };
        if let Some(window) = self.window() {
            window.set_current_page(page);
        }
        // The marks follow the page the counter shows, so the two agree.
        self.mark_outline(page);
        self.follow_images();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pages_belong_to_their_headings_or_to_the_section_begun_before_them() {
        use super::headings_for;
        let pages = [0, 4, 6, -1, 6, 10];
        assert_eq!(headings_for(&pages, &[0]), [0]);
        assert_eq!(headings_for(&pages, &[3]), [0]);
        assert_eq!(headings_for(&pages, &[5]), [1]);
        // A page that begins two sections belongs to both.
        assert_eq!(headings_for(&pages, &[6]), [2, 4]);
        // After it, the pages are in the second.
        assert_eq!(headings_for(&pages, &[9]), [4]);
        assert_eq!(headings_for(&pages, &[30]), [5]);
        // A spread belongs to the headings on both of its pages.
        assert_eq!(headings_for(&pages, &[4, 5]), [1]);
        assert_eq!(headings_for(&pages, &[5, 6]), [2, 4]);
        assert_eq!(headings_for(&pages, &[3, 4]), [1]);
        assert_eq!(headings_for(&pages, &[7, 8]), [4]);
        // An outline out of page order still finds the nearest page.
        assert_eq!(headings_for(&[8, 2, 5], &[6]), [2]);
        assert!(headings_for(&[3, 5], &[1]).is_empty());
        assert!(headings_for(&[], &[1]).is_empty());
        assert!(headings_for(&[-1], &[1]).is_empty());
        assert!(headings_for(&pages, &[]).is_empty());
    }

    use std::sync::mpsc::Sender;

    use slint::ComponentHandle;

    use super::{Spread, Viewer, Workers, build_row_specs, page_locations, reference_dims};
    use crate::MainWindow;
    use crate::render::WorkerMessage;
    use crate::semantic::Vectorizer;

    fn as_pairs(specs: &[super::RowSpec]) -> Vec<(usize, Option<usize>)> {
        specs.iter().map(|s| (s.left, s.right)).collect()
    }

    /// A window on Slint's testing backend, installed once per test thread.
    /// The platform's real backend would refuse to run off the main thread on
    /// macOS and could not start without a display in CI.
    fn window() -> MainWindow {
        thread_local! {
            static INSTALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        }
        if !INSTALLED.replace(true) {
            i_slint_backend_testing::init_no_event_loop();
        }
        MainWindow::new().expect("failed to create the window")
    }

    /// Workers that answer nothing, and a vectorizer whose model is never
    /// loaded, as these tests never search by meaning. `sender` is the page
    /// worker's, so a test can see what the viewer asks it for.
    fn workers(window: &MainWindow, sender: Sender<WorkerMessage>) -> Workers {
        let model = std::sync::Arc::new(crate::semantic::EmbeddingModel::locate());
        Workers {
            pages: sender,
            thumbnails: std::sync::mpsc::channel().0,
            control: crate::render::RenderControl::inert(),
            vectorizer: Vectorizer::new(0, model, std::sync::mpsc::channel().0, window.as_weak()),
        }
    }

    /// An image as large as a page at a high zoom. The pixels are shared, so
    /// handing it out for several pages costs its memory only once.
    fn large_image() -> slint::Image {
        slint::Image::from_rgb8(slint::SharedPixelBuffer::new(5000, 5000))
    }

    /// Regression: a page arriving in paged mode used to re-enter a held borrow
    /// of `inner` via `refresh_current_row` and panic. Drive that exact path.
    #[test]
    fn paged_render_does_not_reenter_borrow() {
        let window = window();
        let (sender, _receiver) = std::sync::mpsc::channel();
        let pages = vec![(600.0, 800.0); 5];
        let viewer = Viewer::new(
            &window,
            pages,
            1.0,
            workers(&window, sender),
            &super::ViewSettings::default(),
            super::ImageFilters::default(),
        );
        viewer.activate();

        // Switch to paged mode, then deliver renders — including enough to force
        // the eviction path, which also calls back into the viewer.
        viewer.toggle_continuous();
        let image = large_image();
        for page in 0..5 {
            viewer.on_page_rendered(page, 1.0, image.clone());
        }
        viewer.nav_page(1);
        viewer.on_page_rendered(2, 1.0, image);
    }

    /// go_to_page parses, clamps, and reports the resulting page.
    #[test]
    fn go_to_page_parses_and_clamps() {
        let window = window();
        let (sender, _receiver) = std::sync::mpsc::channel();
        let viewer = Viewer::new(
            &window,
            vec![(600.0, 800.0); 10],
            1.0,
            workers(&window, sender),
            &super::ViewSettings::default(),
            super::ImageFilters::default(),
        );
        viewer.activate();
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
    fn a_few_outsized_pages_do_not_set_the_reference_size() {
        // A scanned book: pages of slightly different sizes, a larger cover,
        // and a fold-out map with the page after it.
        let mut pages: Vec<(f32, f32)> = (0..40)
            .map(|index| (492.0 + (index % 9) as f32 * 10.0, 686.0 + (index % 7) as f32 * 8.0))
            .collect();
        pages[0] = (612.0, 837.0);
        pages[30] = (1551.0, 1202.0);
        pages[31] = (773.0, 1055.0);
        let specs = build_row_specs(pages.len(), Spread::None);
        assert_eq!(reference_dims(&specs, &pages), (612.0, 837.0));

        // In spreads the map's row is wider still, and is left out the same way.
        let specs = build_row_specs(pages.len(), Spread::Even);
        let (width, height) = reference_dims(&specs, &pages);
        assert!(width < 1300.0, "the map's spread set the width: {width}");
        assert_eq!(height, 837.0);
    }

    #[test]
    fn pages_of_two_common_sizes_both_count() {
        // Half portrait, half landscape: neither is an outlier.
        let pages: Vec<(f32, f32)> = (0..20)
            .map(|index| if index % 2 == 0 { (612.0, 792.0) } else { (792.0, 612.0) })
            .collect();
        let specs = build_row_specs(pages.len(), Spread::None);
        assert_eq!(reference_dims(&specs, &pages), (792.0, 792.0));
    }

    #[test]
    fn uniform_pages_are_their_own_reference() {
        let pages = vec![(600.0, 800.0); 5];
        let specs = build_row_specs(pages.len(), Spread::Odd);
        assert_eq!(reference_dims(&specs, &pages), (1200.0, 800.0));
    }

    #[test]
    fn handles_empty_and_single_page_documents() {
        assert!(build_row_specs(0, Spread::Even).is_empty());
        assert_eq!(as_pairs(&build_row_specs(1, Spread::Odd)), vec![(0, None)]);
        assert_eq!(as_pairs(&build_row_specs(1, Spread::Even)), vec![(0, None)]);
    }
}
