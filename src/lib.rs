//! melkkipdf — a fast, minimal PDF viewer for Linux.
//!
//! The binary is a thin wrapper around [`run`]. The viewer state lives in
//! [`Viewer`]; the `testing` feature exposes a headless [`testing::Harness`] that
//! drives it without an event loop for integration tests.

#[cfg(target_os = "macos")]
mod macos;
mod render;
mod settings;
mod viewer;

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel, Weak};

use render::{Loaded, RenderControl, WorkerMessage};
use settings::{Session, Store};

pub use viewer::{FitMode, Spread, ViewSettings, Viewer};

slint::include_modules!();

/// How often changes are written out between the explicit save points, which
/// bounds what a crash or a killed process can lose.
const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(5);
/// How long a problem stays on screen, long enough to read a sentence or two.
const NOTICE_DURATION: Duration = Duration::from_secs(8);

/// A file dragged from another application over the window. Kept apart from
/// winit's event type so the tests can drive it.
pub(crate) enum FileDrag {
    Hovered,
    Cancelled,
    Dropped(PathBuf),
}

impl FileDrag {
    fn from_winit(event: &winit::event::WindowEvent) -> Option<Self> {
        match event {
            winit::event::WindowEvent::HoveredFile(_) => Some(Self::Hovered),
            winit::event::WindowEvent::HoveredFileCancelled => Some(Self::Cancelled),
            winit::event::WindowEvent::DroppedFile(path) => Some(Self::Dropped(path.clone())),
            _ => None,
        }
    }
}

/// The channels to one document's render workers.
struct Workers {
    pages: Sender<WorkerMessage>,
    thumbnails: Sender<i32>,
    control: RenderControl,
}

/// A document's page worker while it reads the document, before there is a
/// viewer to take over its channel.
struct Loading {
    /// The path as it was asked for, which the messages about it name.
    path: String,
    pages: Sender<WorkerMessage>,
    control: RenderControl,
}

/// One open document: its viewer, plus what the window shows for it outside
/// the viewer.
struct Tab {
    /// Tags this document's renders, which arrive through the shared window.
    id: i32,
    /// The canonical path, so opening a document twice finds its tab.
    path: PathBuf,
    title: SharedString,
    /// The document's viewer, or `None` while the document is still being
    /// read, which takes a while for a long one.
    viewer: Option<Rc<Viewer>>,
    outline: ModelRc<OutlineItem>,
    /// The worker reading the document, until it has.
    loading: Option<Loading>,
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
    /// Each document's remembered view, and the tabs to reopen next time.
    store: RefCell<Store>,
    /// Files dropped onto the window and not yet opened, in the order they
    /// arrived.
    dropped: RefCell<Vec<PathBuf>>,
    /// Takes the current notice down once it has been up long enough.
    notice_timer: Timer,
    /// Where each document's worker reports that it has read the document.
    loads: Receiver<Loaded>,
    load_sender: Sender<Loaded>,
}

impl App {
    pub(crate) fn new(window: &MainWindow, store: Store) -> Rc<Self> {
        let titles = Rc::new(VecModel::default());
        window.set_tabs(ModelRc::from(titles.clone()));
        let (load_sender, loads) = mpsc::channel();
        let app = Rc::new(Self {
            window: window.as_weak(),
            tabs: RefCell::new(Vec::new()),
            active: Cell::new(None),
            titles,
            next_id: Cell::new(0),
            viewport: Cell::new((0.0, 0.0)),
            store: RefCell::new(store),
            dropped: RefCell::new(Vec::new()),
            notice_timer: Timer::default(),
            loads,
            load_sender,
        });
        app.show_empty();
        wire_callbacks(window, &app);
        app
    }

    /// The active tab's viewer, unless its document is still loading. Cloned
    /// out so no borrow of `tabs` is held while the viewer runs.
    fn active_viewer(&self) -> Option<Rc<Viewer>> {
        let index = self.active.get()?;
        self.tabs.borrow().get(index).and_then(|tab| tab.viewer.clone())
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
            self.tabs.borrow().iter().find(|tab| tab.id == id).and_then(|tab| tab.viewer.clone());
        if let Some(viewer) = viewer {
            action(&viewer);
        }
    }

    /// Opens `path` in a new tab and shows it, returning the tab's index. A
    /// document that already has a tab is shown there instead, since a second
    /// copy would only split the reading position between two tabs.
    ///
    /// The document is read on its worker thread, so a long one does not
    /// freeze the window, and the tab says it is loading until the worker
    /// reports back (see [`App::receive_loads`]). A document that cannot be
    /// read then has its tab closed and the error shown.
    pub(crate) fn open(&self, path: String) -> Option<usize> {
        let window = self.window.upgrade()?;

        let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| PathBuf::from(&path));
        if let Some(index) = self.find(&canonical) {
            self.select(index);
            return Some(index);
        }

        let title = Path::new(&path)
            .file_name()
            .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned());
        let id = self.allocate_id();
        let (pages, control) =
            render::spawn(path.clone(), id, window.as_weak(), self.load_sender.clone());
        Some(self.add_tab(Tab {
            id,
            path: canonical,
            title: title.into(),
            viewer: None,
            outline: ModelRc::default(),
            loading: Some(Loading { path, pages, control }),
        }))
    }

    /// Takes in every document whose worker has finished reading it.
    pub(crate) fn receive_loads(&self) {
        while let Ok(loaded) = self.loads.try_recv() {
            self.finish_load(loaded);
        }
    }

    /// Gives a loading tab the viewer for its document, now that it has been
    /// read, or closes the tab and shows why it could not be. A tab closed
    /// while its document was loading has nothing left to do.
    pub(crate) fn finish_load(&self, loaded: Loaded) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let Some(index) = self.tabs.borrow().iter().position(|tab| tab.id == loaded.doc) else {
            return;
        };
        let Some(loading) = self.tabs.borrow_mut()[index].loading.take() else {
            return;
        };
        let info = match loaded.result {
            Ok(info) if !info.pages_pt.is_empty() => info,
            Ok(_) => {
                self.notify(&format!("Failed to open {}, which has no pages.", loading.path));
                self.close(index);
                return;
            }
            Err(err) => {
                self.notify(&format!("Failed to open {}: {err}.", loading.path));
                self.close(index);
                return;
            }
        };

        let outline: Vec<OutlineItem> = info
            .outline
            .into_iter()
            .map(|(title, page, depth)| OutlineItem { title: title.into(), page, depth })
            .collect();
        let thumbnails = render::spawn_thumbnails(loading.path, loaded.doc, window.as_weak());
        let workers = Workers { pages: loading.pages, thumbnails, control: loading.control };
        let path = self.tabs.borrow()[index].path.clone();
        let viewer = self.make_viewer(&window, &path, info.pages_pt, workers);
        {
            let mut tabs = self.tabs.borrow_mut();
            tabs[index].viewer = Some(viewer);
            tabs[index].outline = ModelRc::new(VecModel::from(outline));
        }
        if self.active.get() == Some(index) {
            self.show(index);
        }
    }

    /// The index of the tab showing the document at the canonical `path`.
    fn find(&self, path: &Path) -> Option<usize> {
        self.tabs.borrow().iter().position(|tab| tab.path == path)
    }

    /// Adds a tab for a document whose pages have already been read and shows
    /// it. `spawn` starts the document's render workers, tagged with the id it
    /// is given.
    #[cfg(feature = "testing")]
    fn insert(
        &self,
        path: PathBuf,
        title: SharedString,
        pages_pt: Vec<(f32, f32)>,
        outline: ModelRc<OutlineItem>,
        spawn: impl FnOnce(i32) -> Workers,
    ) -> Option<usize> {
        let window = self.window.upgrade()?;
        let id = self.allocate_id();
        let viewer = self.make_viewer(&window, &path, pages_pt, spawn(id));
        Some(self.add_tab(Tab { id, path, title, viewer: Some(viewer), outline, loading: None }))
    }

    /// A viewer for the document at `path`, in the view it was last left in.
    fn make_viewer(
        &self,
        window: &MainWindow,
        path: &Path,
        pages_pt: Vec<(f32, f32)>,
        workers: Workers,
    ) -> Rc<Viewer> {
        let settings = {
            let mut store = self.store.borrow_mut();
            let settings = store.document(path).unwrap_or_default();
            store.record_open(path);
            settings
        };
        Viewer::new(
            window,
            pages_pt,
            window.window().scale_factor(),
            workers.pages,
            workers.thumbnails,
            workers.control,
            &settings,
        )
    }

    /// A fresh id to tag a new document's renders with.
    fn allocate_id(&self) -> i32 {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        id
    }

    /// Appends `tab` after the last one and shows it, as a browser does with a
    /// new tab.
    fn add_tab(&self, tab: Tab) -> usize {
        let title = tab.title.clone();
        let index = {
            let mut tabs = self.tabs.borrow_mut();
            tabs.push(tab);
            tabs.len() - 1
        };
        self.titles.push(title);
        self.select(index);
        index
    }

    /// Shows the tab at `index`, unless it is already shown.
    pub(crate) fn select(&self, index: usize) {
        if self.active.get() != Some(index) {
            self.show(index);
        }
    }

    /// Shows the tab at `index`, handing the window over to its viewer, or
    /// saying that its document is loading.
    fn show(&self, index: usize) {
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

        if let Some(previous) = self.active_viewer()
            && viewer.as_ref().is_none_or(|viewer| !Rc::ptr_eq(&previous, viewer))
        {
            previous.deactivate();
        }
        self.active.set(Some(index));

        window.set_active_tab(index as i32);
        window.set_outline(outline);
        window.set_doc_title(title.clone());
        let Some(viewer) = viewer else {
            Self::clear_document(&window, &format!("Loading {title}…"));
            return;
        };
        viewer.activate();
        // The window may have been resized while this tab was in the background.
        let (width, height) = self.viewport.get();
        if width > 0.0 && height > 0.0 {
            viewer.set_viewport(width, height);
        }
    }

    /// Closes the tab at `index`. Closing the active tab shows its right-hand
    /// neighbour, or the left-hand one when it was the last, as browsers do.
    /// Dropping the tab's viewer, or its loading worker's channel, closes its
    /// render channels, so its worker threads shut themselves down.
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
        // Taken now, while the viewer is still here to ask. A document that
        // never finished loading has nothing new to remember.
        if let Some(viewer) = &removed.viewer {
            self.store.borrow_mut().update(&removed.path, viewer.settings());
        }

        match self.active.get() {
            Some(active) if active == index => {
                if let Some(viewer) = &removed.viewer {
                    viewer.deactivate();
                }
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
        self.save();
    }

    /// Records every open tab's view and the tabs themselves, then writes them
    /// out if anything changed since the last save.
    pub(crate) fn save(&self) {
        let open: Vec<(PathBuf, Option<ViewSettings>)> = self
            .tabs
            .borrow()
            .iter()
            .map(|tab| (tab.path.clone(), tab.viewer.as_ref().map(|viewer| viewer.settings())))
            .collect();
        let mut store = self.store.borrow_mut();
        let session = Session {
            tabs: open.iter().map(|(path, _)| path.clone()).collect(),
            active: self.active.get(),
        };
        for (path, settings) in open {
            if let Some(settings) = settings {
                store.update(&path, settings);
            }
        }
        store.set_session(session);
        if let Err(err) = store.save() {
            eprintln!("Failed to save settings: {err}.");
        }
    }

    /// Reopens the tabs that were open when the app last closed and shows the
    /// one that was active. A document that has since gone is skipped.
    pub(crate) fn restore_session(&self) {
        let session = self.store.borrow().session();
        let mut active = None;
        for (index, path) in session.tabs.iter().enumerate() {
            if !path.exists() {
                eprintln!("Not reopening {}, which no longer exists.", path.display());
                continue;
            }
            let opened = self.open(path.to_string_lossy().into_owned());
            if session.active == Some(index) {
                active = opened;
            }
        }
        if let Some(active) = active {
            self.select(active);
        }
    }

    /// Tells the reader about a problem, such as a document or page that failed
    /// to open, and logs it. The message goes away by itself after a while, or
    /// when clicked, and a newer one replaces it.
    pub(crate) fn notify(&self, message: &str) {
        eprintln!("{message}");
        let Some(window) = self.window.upgrade() else {
            return;
        };
        window.set_notice(message.into());
        let window = self.window.clone();
        self.notice_timer.start(TimerMode::SingleShot, NOTICE_DURATION, move || {
            if let Some(window) = window.upgrade() {
                window.set_notice(SharedString::new());
            }
        });
    }

    /// Resets the window to the state it starts in, with no document open.
    fn show_empty(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        window.set_active_tab(-1);
        window.set_outline(ModelRc::default());
        window.set_doc_title(SharedString::new());
        Self::clear_document(&window, "Open a PDF to get started.");
    }

    /// Shows no pages, only `status` in their place.
    fn clear_document(window: &MainWindow, status: &str) {
        window.set_rows(ModelRc::default());
        window.set_thumb_rows(ModelRc::default());
        window.set_page_count(0);
        window.set_current_page(0);
        window.set_status(status.into());
    }

    /// Highlights the window while files are dragged over it, and opens each
    /// dropped file in a new tab. Several files dropped together arrive one
    /// event each, so each gets its own tab, in the order they come.
    pub(crate) fn file_drag(self: &Rc<Self>, drag: FileDrag) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        match drag {
            FileDrag::Hovered => window.set_drop_hover(true),
            FileDrag::Cancelled => window.set_drop_hover(false),
            FileDrag::Dropped(path) => {
                window.set_drop_hover(false);
                // Opening reads the document and reshapes the window, which is
                // best done from the event loop rather than from inside the
                // windowing system's delivery of the drop. The files are queued
                // for one timer, because timers due at the same moment do not
                // fire in the order they were started, and the tabs should.
                let first = {
                    let mut dropped = self.dropped.borrow_mut();
                    dropped.push(path);
                    dropped.len() == 1
                };
                if first {
                    let app = Rc::downgrade(self);
                    Timer::single_shot(Duration::ZERO, move || {
                        if let Some(app) = app.upgrade() {
                            let dropped = std::mem::take(&mut *app.dropped.borrow_mut());
                            for path in dropped {
                                app.open(path.to_string_lossy().into_owned());
                            }
                        }
                    });
                }
            }
        }
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

/// Opens the window with the tabs left open last time plus one for each of
/// `paths` (or an empty window with the open button when there are none) and
/// runs the event loop until the window closes.
pub fn run(paths: Vec<String>) -> Result<(), Box<dyn Error>> {
    let window = MainWindow::new()?;

    // Without an app ID matching the desktop entry, compositors cannot tie the
    // window back to it and show a generic icon and title instead. Creating the
    // window above initialises the platform this needs, and the ID is only read
    // when the window is actually shown, so setting it here is in time.
    slint::set_xdg_app_id("io.github.xremming.MelkkiPDF")?;

    let app = App::new(&window, Store::open_default());

    #[cfg(target_os = "macos")]
    macos::on_open_document({
        let app = app.clone();
        move |path| {
            app.open(path.to_string_lossy().into_owned());
        }
    });

    // Slint's own DropArea does not receive drops from other applications on
    // winit yet, so files dropped onto the window are taken from winit.
    window.window().on_winit_window_event({
        let app = Rc::downgrade(&app);
        move |_, event| match (FileDrag::from_winit(event), app.upgrade()) {
            (Some(drag), Some(app)) => {
                app.file_drag(drag);
                EventResult::PreventDefault
            }
            _ => EventResult::Propagate,
        }
    });

    app.restore_session();
    for path in paths {
        app.open(path);
    }

    let autosave = slint::Timer::default();
    autosave.start(TimerMode::Repeated, AUTOSAVE_INTERVAL, {
        let app = Rc::downgrade(&app);
        move || {
            if let Some(app) = app.upgrade() {
                app.save();
            }
        }
    });

    window.run()?;
    app.save();
    Ok(())
}

/// A row, page or tab index from the window, which Slint passes as a signed
/// integer. A negative one means none, such as the page of an outline entry
/// that leads nowhere.
fn index(value: i32) -> Option<usize> {
    usize::try_from(value).ok()
}

/// Connects the window's callbacks to the app, dispatching each to the viewer of
/// the active tab, or of the document a render belongs to. Slint's integers
/// become indices and spreads here, so nothing past this point sees them.
fn wire_callbacks(window: &MainWindow, app: &Rc<App>) {
    window.on_open_document({
        let app = app.clone();
        move || app.pick_and_open()
    });
    // The autosave records which tab is shown, so switching, which can go
    // through several tabs a second, does not write the settings itself.
    window.on_select_tab({
        let app = app.clone();
        move |tab| {
            if let Some(tab) = index(tab) {
                app.select(tab);
            }
        }
    });
    window.on_close_tab({
        let app = app.clone();
        move |tab| {
            if let Some(tab) = index(tab) {
                app.close(tab);
            }
        }
    });
    window.on_request_render_row({
        let app = app.clone();
        move |row| {
            if let Some(row) = index(row) {
                app.with_viewer(|v| v.request_render_row(row));
            }
        }
    });
    window.on_page_rendered({
        let app = app.clone();
        move |doc, page, scale, image| {
            if let Some(page) = index(page) {
                app.with_document(doc, |v| v.on_page_rendered(page, scale, image.clone()));
            }
        }
    });
    window.on_document_loaded({
        let app = app.clone();
        move || app.receive_loads()
    });
    window.on_page_failed({
        let app = app.clone();
        move |doc, page| {
            if let Some(page) = index(page) {
                app.with_document(doc, |v| v.on_page_failed(page));
            }
        }
    });
    window.on_notify({
        let app = app.clone();
        move |message| app.notify(message.as_str())
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
    window.on_user_scrolled({
        let app = app.clone();
        move || app.with_viewer(|v| v.user_scrolled())
    });
    window.on_go_to_page({
        let app = app.clone();
        move |text| app.with_viewer(|v| v.go_to_page(text.as_str()))
    });
    window.on_set_spread({
        let app = app.clone();
        move |mode| app.with_viewer(|v| v.set_spread(Spread::from_index(mode)))
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
        move |page| {
            if let Some(page) = index(page) {
                app.with_viewer(|v| v.nav_to_page(page));
            }
        }
    });
    window.on_request_thumbnail_row({
        let app = app.clone();
        move |row| {
            if let Some(row) = index(row) {
                app.with_viewer(|v| v.request_thumbnail_row(row));
            }
        }
    });
    window.on_thumbnail_rendered({
        let app = app.clone();
        move |doc, page, image| {
            if let Some(page) = index(page) {
                app.with_document(doc, |v| v.on_thumbnail_rendered(page, image.clone()));
            }
        }
    });
    window.on_thumbnail_failed({
        let app = app.clone();
        move |doc, page| {
            if let Some(page) = index(page) {
                app.with_document(doc, |v| v.on_thumbnail_failed(page));
            }
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
