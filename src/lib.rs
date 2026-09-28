//! melkkipdf — a fast, minimal PDF viewer for Linux.
//!
//! The binary is a thin wrapper around [`run`]. The viewer state lives in
//! [`Viewer`]; the `testing` feature exposes a headless [`testing::Harness`] that
//! drives it without an event loop for integration tests.

#[cfg(target_os = "macos")]
mod macos;
mod render;
mod viewer;

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use mupdf::Document;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel, Weak};

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

/// One open document: its viewer, plus what the window shows for it outside
/// the viewer.
struct Tab {
    /// Tags this document's renders, which arrive through the shared window.
    id: i32,
    /// The canonical path, so opening a document twice finds its tab.
    path: PathBuf,
    title: SharedString,
    viewer: Rc<Viewer>,
    outline: ModelRc<OutlineItem>,
}

/// Holds the live window and a tab per open document. There is one window and
/// one set of callbacks, so the callbacks dispatch through here to whichever
/// tab is active rather than capturing a fixed viewer.
pub(crate) struct App {
    window: Weak<MainWindow>,
    tabs: RefCell<Vec<Tab>>,
    /// Index into `tabs` of the tab shown, or `None` while nothing is open.
    active: Cell<Option<usize>>,
    /// The tab titles, in tab order, which the tab strip draws.
    titles: Rc<VecModel<SharedString>>,
    next_id: Cell<i32>,
    /// Last viewport size reported by the window. Only the active viewer hears
    /// about resizes, so this is replayed into a viewer when its tab is shown.
    viewport: Cell<(f32, f32)>,
}

impl App {
    pub(crate) fn new(window: &MainWindow) -> Rc<Self> {
        let titles = Rc::new(VecModel::default());
        window.set_tabs(ModelRc::from(titles.clone()));
        let app = Rc::new(Self {
            window: window.as_weak(),
            tabs: RefCell::new(Vec::new()),
            active: Cell::new(None),
            titles,
            next_id: Cell::new(0),
            viewport: Cell::new((0.0, 0.0)),
        });
        app.show_empty();
        wire_callbacks(window, &app);
        app
    }

    /// The active tab's viewer. Cloned out so no borrow of `tabs` is held while
    /// the viewer runs.
    fn active_viewer(&self) -> Option<Rc<Viewer>> {
        let index = self.active.get()?;
        self.tabs.borrow().get(index).map(|tab| tab.viewer.clone())
    }

    /// Runs `action` on the active tab's viewer, if a document is open.
    fn with_viewer(&self, action: impl FnOnce(&Viewer)) {
        if let Some(viewer) = self.active_viewer() {
            action(&viewer);
        }
    }

    /// Runs `action` on the viewer of the document `id`, whether or not its tab
    /// is active. Renders for a tab closed since they were requested find none.
    fn with_document(&self, id: i32, action: impl FnOnce(&Viewer)) {
        let viewer =
            self.tabs.borrow().iter().find(|tab| tab.id == id).map(|tab| tab.viewer.clone());
        if let Some(viewer) = viewer {
            action(&viewer);
        }
    }

    /// Opens `path` in a new tab and shows it. A document that already has a
    /// tab is shown there instead, since a second copy would only split the
    /// reading position between two tabs. On a read error the message is shown
    /// and the open tabs are left as they are.
    pub(crate) fn open(&self, path: String) {
        let Some(window) = self.window.upgrade() else {
            return;
        };

        let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| PathBuf::from(&path));
        let existing = self.tabs.borrow().iter().position(|tab| tab.path == canonical);
        if let Some(index) = existing {
            self.select(index);
            return;
        }

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

        let outline: Vec<OutlineItem> = read_outline(&path)
            .into_iter()
            .map(|(title, page, depth)| OutlineItem { title: title.into(), page, depth })
            .collect();

        let id = self.allocate_id();
        let scale_factor = window.window().scale_factor();
        let (sender, control) = render::spawn(path.clone(), id, window.as_weak());
        let thumb_sender = render::spawn_thumbnails(path.clone(), id, window.as_weak());
        let viewer = Viewer::new(&window, pages_pt, scale_factor, sender, thumb_sender, control);

        let title = Path::new(&path)
            .file_name()
            .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned());
        self.add_tab(Tab {
            id,
            path: canonical,
            title: title.into(),
            viewer,
            outline: ModelRc::new(VecModel::from(outline)),
        });
    }

    /// A fresh id to tag a new document's renders with.
    fn allocate_id(&self) -> i32 {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        id
    }

    /// Appends `tab` after the last one and shows it, as a browser does with a
    /// new tab.
    fn add_tab(&self, tab: Tab) {
        let title = tab.title.clone();
        let index = {
            let mut tabs = self.tabs.borrow_mut();
            tabs.push(tab);
            tabs.len() - 1
        };
        self.titles.push(title);
        self.select(index);
    }

    /// Shows the tab at `index`, handing the window over to its viewer.
    pub(crate) fn select(&self, index: usize) {
        if self.active.get() == Some(index) {
            return;
        }
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let Some((viewer, outline, title)) = self
            .tabs
            .borrow()
            .get(index)
            .map(|tab| (tab.viewer.clone(), tab.outline.clone(), tab.title.clone()))
        else {
            return;
        };

        // A viewer created for a new tab has already taken over the window, but
        // the previous one still believes it owns it until told otherwise.
        if let Some(previous) = self.active_viewer()
            && !Rc::ptr_eq(&previous, &viewer)
        {
            previous.deactivate();
        }
        self.active.set(Some(index));

        window.set_active_tab(index as i32);
        window.set_outline(outline);
        window.set_doc_title(title);
        viewer.activate();
        // The window may have been resized while this tab was in the background.
        let (width, height) = self.viewport.get();
        if width > 0.0 && height > 0.0 {
            viewer.set_viewport(width, height);
        }
    }

    /// Closes the tab at `index`. Closing the active tab shows its right-hand
    /// neighbour, or the left-hand one when it was the last, as browsers do.
    /// Dropping the tab's viewer closes its render channels, so its worker
    /// threads shut themselves down.
    pub(crate) fn close(&self, index: usize) {
        let removed = {
            let mut tabs = self.tabs.borrow_mut();
            if index >= tabs.len() {
                return;
            }
            tabs.remove(index)
        };
        self.titles.remove(index);
        let remaining = self.tabs.borrow().len();

        match self.active.get() {
            Some(active) if active == index => {
                removed.viewer.deactivate();
                self.active.set(None);
                if remaining == 0 {
                    self.show_empty();
                } else {
                    self.select(index.min(remaining - 1));
                }
            }
            Some(active) if active > index => {
                self.active.set(Some(active - 1));
                if let Some(window) = self.window.upgrade() {
                    window.set_active_tab(active as i32 - 1);
                }
            }
            _ => {}
        }
    }

    /// Resets the window to the state it starts in, with no document open.
    fn show_empty(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        window.set_active_tab(-1);
        window.set_rows(ModelRc::default());
        window.set_thumb_rows(ModelRc::default());
        window.set_outline(ModelRc::default());
        window.set_page_count(0);
        window.set_current_page(0);
        window.set_doc_title(SharedString::new());
        window.set_status("Open a PDF to get started.".into());
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

/// Opens the window with a tab for each of `paths` (or an empty window with
/// the open button when there are none) and runs the event loop until the
/// window closes.
pub fn run(paths: Vec<String>) -> Result<(), Box<dyn Error>> {
    let window = MainWindow::new()?;

    // Without an app ID matching the desktop entry, compositors cannot tie the
    // window back to it and show a generic icon and title instead. Creating the
    // window above initialises the platform this needs, and the ID is only read
    // when the window is actually shown, so setting it here is in time.
    slint::set_xdg_app_id("io.github.xremming.MelkkiPDF")?;

    let app = App::new(&window);

    #[cfg(target_os = "macos")]
    macos::on_open_document({
        let app = app.clone();
        move |path| app.open(path.to_string_lossy().into_owned())
    });

    for path in paths {
        app.open(path);
    }

    window.run()?;
    Ok(())
}

/// Connects the window's callbacks to the app, dispatching each to the viewer of
/// the active tab, or of the document a render belongs to.
fn wire_callbacks(window: &MainWindow, app: &Rc<App>) {
    window.on_open_document({
        let app = app.clone();
        move || app.pick_and_open()
    });
    window.on_select_tab({
        let app = app.clone();
        move |index| app.select(index.max(0) as usize)
    });
    window.on_close_tab({
        let app = app.clone();
        move |index| app.close(index.max(0) as usize)
    });
    window.on_request_render_row({
        let app = app.clone();
        move |row| app.with_viewer(|v| v.request_render_row(row))
    });
    window.on_page_rendered({
        let app = app.clone();
        move |doc, page, image| app.with_document(doc, |v| v.on_page_rendered(page, image.clone()))
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
        move |doc, page, image| {
            app.with_document(doc, |v| v.on_thumbnail_rendered(page, image.clone()))
        }
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
