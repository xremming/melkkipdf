//! UI-thread view state: layout, zoom, and the page model.
//!
//! Everything here runs on the UI thread, so interior mutability via `RefCell`
//! is enough — no locking. Each page's size is fixed in PDF points and stored in
//! the model once; zoom is applied through the window's shared `density`
//! property, so a zoom change writes one value instead of every row. The viewer
//! drives the render worker and keeps only a bounded window of images in memory.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use slint::{ComponentHandle, Image, Model, ModelRc, VecModel, Weak};

use crate::render::RenderRequest;
use crate::{MainWindow, PageSlot};

/// How pages are scaled to the viewport when a fit mode is active.
#[derive(Clone, Copy, PartialEq)]
enum FitMode {
    /// Explicit zoom set by the user; ignore the viewport size.
    Free,
    /// Fit the reference page's width to the viewport.
    Width,
    /// Fit the reference page entirely within the viewport.
    Page,
}

/// Multiplicative step for zoom in/out.
const ZOOM_STEP: f32 = 1.25;
const MIN_ZOOM: f32 = 0.1;
const MAX_ZOOM: f32 = 8.0;
/// Logical pixels per point when zoom is "100%". 96/72 renders a point at one CSS
/// pixel's worth of density, a comfortable default reading size.
const BASE_DENSITY: f32 = 96.0 / 72.0;
/// Horizontal room left for the scrollbar and a small gutter when fitting width.
const FIT_GUTTER: f32 = 24.0;
/// Maximum number of rendered page images retained at once. Beyond this the
/// furthest-back pages are dropped; the worker's cache makes re-rendering cheap.
const MAX_RETAINED: usize = 24;

struct Inner {
    /// Per-page size in PDF points (width, height).
    pages_pt: Vec<(f32, f32)>,
    /// Current zoom as a multiplier on `BASE_DENSITY`.
    zoom: f32,
    /// Window device-pixel ratio; render at this extra factor for crisp HiDPI.
    scale_factor: f32,
    /// Latest known page-area size in logical pixels.
    view: Option<(f32, f32)>,
    fit: FitMode,
    generation: i32,
    /// Indices of slots currently holding an image, in the order rendered.
    retained: VecDeque<usize>,
}

pub struct Viewer {
    inner: RefCell<Inner>,
    model: Rc<VecModel<PageSlot>>,
    window: Weak<MainWindow>,
    sender: Sender<RenderRequest>,
}

impl Viewer {
    /// Builds the viewer, populates the page model with correctly-sized (but not
    /// yet rendered) slots in a single update, and installs it on the window.
    pub fn new(
        window: &MainWindow,
        pages_pt: Vec<(f32, f32)>,
        scale_factor: f32,
        sender: Sender<RenderRequest>,
    ) -> Rc<Self> {
        let slots: Vec<PageSlot> = pages_pt
            .iter()
            .enumerate()
            .map(|(index, &(width_pt, height_pt))| PageSlot {
                page: index as i32,
                width_pt,
                height_pt,
                image: Image::default(),
                rendered: false,
            })
            .collect();
        let model = Rc::new(VecModel::from(slots));
        window.set_pages(ModelRc::from(model.clone()));

        let viewer = Rc::new(Self {
            inner: RefCell::new(Inner {
                pages_pt,
                zoom: 1.0,
                scale_factor,
                view: None,
                // Fit width by default, applied once the viewport size is known.
                fit: FitMode::Width,
                generation: 0,
                retained: VecDeque::new(),
            }),
            model,
            window: window.as_weak(),
            sender,
        });

        viewer.apply_density();
        viewer.update_status();
        viewer
    }

    /// Sends a render request for `page` at the current physical scale.
    pub fn request_render(&self, page: i32) {
        let inner = self.inner.borrow();
        if page < 0 || page as usize >= inner.pages_pt.len() {
            return;
        }
        let scale = inner.zoom * BASE_DENSITY * inner.scale_factor;
        let _ = self.sender.send(RenderRequest { page, scale });
    }

    /// Installs a freshly rendered page image, evicting the furthest-back image
    /// if we are over the retention budget.
    pub fn on_page_rendered(&self, page: i32, image: Image) {
        let mut inner = self.inner.borrow_mut();
        let index = page as usize;
        let Some(&(width_pt, height_pt)) = inner.pages_pt.get(index) else {
            return;
        };

        self.model.set_row_data(
            index,
            PageSlot { page, width_pt, height_pt, image, rendered: true },
        );

        inner.retained.push_back(index);
        while inner.retained.len() > MAX_RETAINED {
            if let Some(evicted) = inner.retained.pop_front() {
                // Skip if the page was re-rendered more recently and is still live.
                if evicted != index && !inner.retained.contains(&evicted) {
                    self.clear_slot(&inner, evicted);
                }
            }
        }
    }

    /// Updates the remembered viewport size and, if a fit mode is active,
    /// recomputes zoom to match. Also refreshes the HiDPI scale factor, which is
    /// only reliable once the window has actually been shown.
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
            // apply_fit re-renders at the (possibly new) scale factor for us.
            self.apply_fit();
        } else if density_changed {
            // Free zoom, but the display density changed: re-render crisply
            // (page sizes in logical pixels are unaffected).
            self.bump_generation();
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

    /// Sets an explicit zoom, leaving any fit mode.
    fn set_zoom(&self, zoom: f32) {
        {
            let mut inner = self.inner.borrow_mut();
            inner.fit = FitMode::Free;
            inner.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        }
        self.apply_density();
        self.bump_generation();
        self.update_status();
    }

    /// Recomputes zoom from the active fit mode and the reference (first) page.
    fn apply_fit(&self) {
        let recomputed = {
            let inner = self.inner.borrow();
            let (Some((view_w, view_h)), Some(&(page_w, page_h))) =
                (inner.view, inner.pages_pt.first())
            else {
                return;
            };
            if page_w <= 0.0 || page_h <= 0.0 {
                return;
            }
            // Convert a target logical size into a zoom multiplier on the base
            // density: logical = points * density * zoom.
            let width_zoom = (view_w - FIT_GUTTER).max(1.0) / (page_w * BASE_DENSITY);
            match inner.fit {
                FitMode::Width => width_zoom,
                FitMode::Page => width_zoom.min(view_h / (page_h * BASE_DENSITY)),
                FitMode::Free => return,
            }
            .clamp(MIN_ZOOM, MAX_ZOOM)
        };

        self.inner.borrow_mut().zoom = recomputed;
        self.apply_density();
        self.bump_generation();
        self.update_status();
    }

    /// Pushes the current zoom to the window's shared `density` property, resizing
    /// every page at once without touching the model.
    fn apply_density(&self) {
        let density = BASE_DENSITY * self.inner.borrow().zoom;
        if let Some(window) = self.window.upgrade() {
            window.set_density(density);
        }
    }

    /// Drops a slot's image to reclaim memory, keeping its geometry so layout is
    /// unchanged and the page re-renders if scrolled back into view.
    fn clear_slot(&self, inner: &Inner, index: usize) {
        let (width_pt, height_pt) = inner.pages_pt[index];
        self.model.set_row_data(
            index,
            PageSlot {
                page: index as i32,
                width_pt,
                height_pt,
                image: Image::default(),
                rendered: false,
            },
        );
    }

    fn bump_generation(&self) {
        let generation = {
            let mut inner = self.inner.borrow_mut();
            inner.generation += 1;
            inner.generation
        };
        if let Some(window) = self.window.upgrade() {
            window.set_render_generation(generation);
        }
    }

    fn update_status(&self) {
        let inner = self.inner.borrow();
        let percent = (inner.zoom * 100.0).round() as i32;
        let status = format!("{} pages · {percent}%", inner.pages_pt.len());
        if let Some(window) = self.window.upgrade() {
            window.set_status(status.into());
        }
    }
}
