//! melkkipdf — a fast, minimal PDF viewer for Linux.
//!
//! The binary is a thin wrapper around [`run`]. The viewer state lives in
//! [`Viewer`]; the `testing` feature exposes a headless [`testing::Harness`] that
//! drives it without an event loop for integration tests.

mod render;
mod viewer;

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::path::Path;
use std::rc::Rc;

use mupdf::Document;
use slint::{ComponentHandle, Weak};

pub use viewer::Viewer;

slint::include_modules!();

/// Reads every page's size in points, used to lay out the scrollable document
/// before any page is rendered. This is fast even for large documents.
pub fn read_page_sizes(path: &str) -> Result<Vec<(f32, f32)>, mupdf::Error> {
    let document = Document::open(path)?;
    let count = document.page_count()?;
    let mut sizes = Vec::with_capacity(count.max(0) as usize);
    for index in 0..count {
        let bounds = document.load_page(index)?.bounds()?;
        sizes.push((bounds.width(), bounds.height()));
    }
    Ok(sizes)
}

/// Reads the document outline (bookmarks) as a flat list of
/// `(title, 0-based page, depth)`, depth-first. Returns empty on any error or
/// when the document has no outline.
pub fn read_outline(path: &str) -> Vec<(String, i32, i32)> {
    fn flatten(outlines: &[mupdf::Outline], depth: i32, out: &mut Vec<(String, i32, i32)>) {
        for outline in outlines {
            let page = outline.dest.as_ref().map_or(-1, |dest| dest.loc.page_number as i32);
            out.push((outline.title.clone(), page, depth));
            flatten(&outline.down, depth + 1, out);
        }
    }
    let Ok(document) = Document::open(path) else {
        return Vec::new();
    };
    let Ok(outlines) = document.outlines() else {
        return Vec::new();
    };
    let mut items = Vec::new();
    flatten(&outlines, 0, &mut items);
    items
}

/// Holds the live window and the viewer for the document currently open. The
/// viewer is swapped out (not the window) when a new PDF is opened, so all the
/// UI callbacks dispatch through here rather than capturing a fixed viewer.
struct App {
    window: Weak<MainWindow>,
    viewer: RefCell<Option<Rc<Viewer>>>,
    /// Last viewport size reported by the window. A viewer created after the
    /// window is already shown never sees a `viewport-resized` event (the size
    /// did not change), so we replay the last one into it.
    viewport: Cell<(f32, f32)>,
}

impl App {
    /// Runs `action` on the current viewer, if a document is open.
    fn with_viewer(&self, action: impl FnOnce(&Viewer)) {
        if let Some(viewer) = self.viewer.borrow().as_ref() {
            action(viewer);
        }
    }

    /// Loads `path` into the window, replacing any document already open. On a
    /// read error the message is shown and the current document (if any) is
    /// kept. Dropping the previous viewer closes its render channels, so the old
    /// worker threads shut themselves down.
    fn open(&self, path: String) {
        let Some(window) = self.window.upgrade() else {
            return;
        };

        let pages_pt = match read_page_sizes(&path) {
            Ok(sizes) if !sizes.is_empty() => sizes,
            Ok(_) => {
                window.set_status("Document has no pages.".into());
                return;
            }
            Err(err) => {
                window.set_status(format!("Failed to open {path}: {err}").into());
                return;
            }
        };

        // Populate the outline (bookmarks) sidebar.
        let outline: Vec<OutlineItem> = read_outline(&path)
            .into_iter()
            .map(|(title, page, depth)| OutlineItem { title: title.into(), page, depth })
            .collect();
        window.set_outline(slint::ModelRc::new(slint::VecModel::from(outline)));

        let scale_factor = window.window().scale_factor();
        let (sender, control) = render::spawn(path.clone(), window.as_weak());
        let thumb_sender = render::spawn_thumbnails(path.clone(), window.as_weak());
        let viewer = Viewer::new(&window, pages_pt, scale_factor, sender, thumb_sender, control);

        // Replay the current viewport so the fresh viewer lays out immediately
        // instead of waiting for a resize.
        let (view_w, view_h) = self.viewport.get();
        if view_w > 0.0 && view_h > 0.0 {
            viewer.set_viewport(view_w, view_h);
        }

        let name = Path::new(&path).file_name().map_or(path.as_str(), |n| n.to_str().unwrap_or(&path));
        window.set_doc_title(name.into());

        // Dropping the previous viewer here shuts down its render workers.
        *self.viewer.borrow_mut() = Some(viewer);
    }

    /// Prompts for a PDF with a native file dialog and opens the chosen one. The
    /// picker runs as a future on Slint's event loop so the UI stays responsive.
    fn pick_and_open(self: &Rc<Self>) {
        let app = self.clone();
        let _ = slint::spawn_local(async move {
            let file = rfd::AsyncFileDialog::new()
                .add_filter("PDF", &["pdf"])
                .set_title("Open PDF")
                .pick_file()
                .await;
            if let Some(file) = file {
                app.open(file.path().to_string_lossy().into_owned());
            }
        });
    }
}

/// Opens the window for `path` (or an empty window with the open button when
/// `None`) and runs the event loop until the window closes.
pub fn run(path: Option<String>) -> Result<(), Box<dyn Error>> {
    let window = MainWindow::new()?;
    let app = Rc::new(App {
        window: window.as_weak(),
        viewer: RefCell::new(None),
        viewport: Cell::new((0.0, 0.0)),
    });
    wire_callbacks(&window, &app);

    if let Some(path) = path {
        app.open(path);
    } else {
        window.set_status("Open a PDF to get started.".into());
    }

    window.run()?;
    Ok(())
}

/// Connects the window's callbacks to the app, dispatching each to the viewer of
/// the document currently open.
fn wire_callbacks(window: &MainWindow, app: &Rc<App>) {
    window.on_open_document({
        let app = app.clone();
        move || app.pick_and_open()
    });
    window.on_request_render_row({
        let app = app.clone();
        move |row| app.with_viewer(|v| v.request_render_row(row))
    });
    window.on_page_rendered({
        let app = app.clone();
        move |page, image| app.with_viewer(|v| v.on_page_rendered(page, image.clone()))
    });
    window.on_viewport_resized({
        let app = app.clone();
        move |width, height| {
            app.viewport.set((width, height));
            app.with_viewer(|v| v.set_viewport(width, height));
        }
    });
    window.on_zoom_in({
        let app = app.clone();
        move || app.with_viewer(|v| v.zoom_in())
    });
    window.on_zoom_out({
        let app = app.clone();
        move || app.with_viewer(|v| v.zoom_out())
    });
    window.on_zoom_reset({
        let app = app.clone();
        move || app.with_viewer(|v| v.zoom_reset())
    });
    window.on_fit_width({
        let app = app.clone();
        move || app.with_viewer(|v| v.fit_width())
    });
    window.on_fit_page({
        let app = app.clone();
        move || app.with_viewer(|v| v.fit_page())
    });
    window.on_toggle_continuous({
        let app = app.clone();
        move || app.with_viewer(|v| v.toggle_continuous())
    });
    window.on_set_continuous({
        let app = app.clone();
        move |continuous| app.with_viewer(|v| v.set_continuous(continuous))
    });
    window.on_scrolled({
        let app = app.clone();
        move |offset| app.with_viewer(|v| v.scrolled(offset))
    });
    window.on_go_to_page({
        let app = app.clone();
        move |text| app.with_viewer(|v| v.go_to_page(text.as_str()))
    });
    window.on_set_spread({
        let app = app.clone();
        move |mode| app.with_viewer(|v| v.set_spread(mode))
    });
    window.on_nav_line({
        let app = app.clone();
        move |dir| app.with_viewer(|v| v.nav_line(dir))
    });
    window.on_nav_page({
        let app = app.clone();
        move |dir| app.with_viewer(|v| v.nav_page(dir))
    });
    window.on_nav_home({
        let app = app.clone();
        move || app.with_viewer(|v| v.nav_home())
    });
    window.on_nav_end({
        let app = app.clone();
        move || app.with_viewer(|v| v.nav_end())
    });
    window.on_paged_scroll({
        let app = app.clone();
        move |delta_x, delta_y, shift| app.with_viewer(|v| v.paged_scroll(delta_x, delta_y, shift))
    });
    window.on_go_to_page_index({
        let app = app.clone();
        move |page| app.with_viewer(|v| v.nav_to_page(page))
    });
    window.on_request_thumbnail_row({
        let app = app.clone();
        move |row| app.with_viewer(|v| v.request_thumbnail_row(row))
    });
    window.on_thumbnail_rendered({
        let app = app.clone();
        move |page, image| app.with_viewer(|v| v.on_thumbnail_rendered(page, image.clone()))
    });
    window.on_toggle_sidebar({
        let window = window.as_weak();
        move || {
            if let Some(window) = window.upgrade() {
                window.set_sidebar_open(!window.get_sidebar_open());
            }
        }
    });
}

#[cfg(feature = "testing")]
pub mod testing;
