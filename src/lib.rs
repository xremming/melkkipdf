//! melkkipdf — a fast, minimal PDF viewer.
//!
//! The binary is a thin wrapper around [`run`]. The viewer state lives in
//! [`Viewer`]; the `testing` feature exposes a headless [`testing::Harness`] that
//! drives it without an event loop for integration tests.

#[cfg(unix)]
mod clipboard;
mod color;
mod images;
mod instance;
mod links;
#[cfg(target_os = "macos")]
mod macos;
mod opener;
mod render;
mod screenshot;
mod search;
mod selection;
mod semantic;
mod settings;
mod viewer;

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel, Weak};

use clipboard::Clipboard;
use images::{CatalogueHandle, Catalogued};
use links::{LinkTarget, PageLink};
use opener::Opener;
use render::{Loaded, RenderControl, Screenshot as Shot, WorkerMessage};
use search::{IndexHandle, Indexed};
use semantic::{Vectorized, Vectorizer};
use settings::{Bookmark, PALETTE, SHAPES, Session, Shape, Store};

pub use render::ThumbRequest;
pub use viewer::{FitMode, ImageFilters, SearchMode, Spread, ViewSettings, Viewer, Workers};

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

/// Whether a winit event is the mouse's back button, and if so whether it
/// was pressed rather than let go of. The button flips back as Tab does,
/// wherever the pointer is: over the outline just clicked as much as over
/// the pages.
fn back_button(event: &winit::event::WindowEvent) -> Option<bool> {
    match event {
        winit::event::WindowEvent::MouseInput {
            button: winit::event::MouseButton::Back,
            state,
            ..
        } => Some(state.is_pressed()),
        _ => None,
    }
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
    /// The reader's flags on the page edge, which the store remembers.
    bookmarks: Rc<VecModel<BookmarkFlag>>,
    /// The page a bookmark jump left, which Tab flips back to.
    return_page: Option<usize>,
    /// The worker reading the document, until it has.
    loading: Option<Loading>,
    /// The indexer reading the document's text for search, which stops when
    /// the tab closes.
    indexer: Option<IndexHandle>,
    /// The cataloguer reading the document's images for the sidebar, which
    /// stops when the tab closes.
    cataloguer: Option<CatalogueHandle>,
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
    /// Where each document's indexer sends the text it has read.
    indexes: Receiver<Indexed>,
    index_sender: Sender<Indexed>,
    /// Where each document's vectorizer sends the page vectors it has made.
    vectors: Receiver<Vectorized>,
    vector_sender: Sender<Vectorized>,
    /// Where each document's cataloguer sends the images it has read.
    catalogues: Receiver<Catalogued>,
    catalogue_sender: Sender<Catalogued>,
    /// The model every document's vectorizer shares, loaded once at most.
    model: Arc<semantic::EmbeddingModel>,
    /// Where each document's worker delivers the screenshots taken of it.
    shots: Receiver<Shot>,
    shot_sender: Sender<Shot>,
    /// Where copied text and screenshots go.
    clipboard: Clipboard,
    /// Opens the addresses links point to outside the document.
    opener: Opener,
}

impl App {
    pub(crate) fn new(
        window: &MainWindow,
        store: Store,
        clipboard: Clipboard,
        opener: Opener,
        model: semantic::EmbeddingModel,
    ) -> Rc<Self> {
        let titles = Rc::new(VecModel::default());
        window.set_tabs(ModelRc::from(titles.clone()));
        window.set_palette(ModelRc::new(VecModel::from(palette())));
        window.set_shapes(ModelRc::new(VecModel::from(shapes())));
        let (load_sender, loads) = mpsc::channel();
        let (index_sender, indexes) = mpsc::channel();
        let (vector_sender, vectors) = mpsc::channel();
        let (shot_sender, shots) = mpsc::channel();
        let (catalogue_sender, catalogues) = mpsc::channel();
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
            indexes,
            index_sender,
            vectors,
            vector_sender,
            catalogues,
            catalogue_sender,
            model: Arc::new(model),
            shots,
            shot_sender,
            clipboard,
            opener,
        });
        let filters = app.store.borrow().image_filters();
        window.set_hide_small_images(filters.hide_small);
        window.set_group_repeated_images(filters.group_repeats);
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
        let (pages, control) = render::spawn(
            path.clone(),
            id,
            window.as_weak(),
            self.load_sender.clone(),
            self.shot_sender.clone(),
        );
        let bookmarks = self.stored_bookmarks(&canonical);
        Some(self.add_tab(Tab {
            id,
            path: canonical,
            title: title.into(),
            viewer: None,
            outline: ModelRc::default(),
            bookmarks,
            return_page: None,
            loading: Some(Loading { path, pages, control }),
            indexer: None,
            cataloguer: None,
        }))
    }

    /// The flags of the document at `path` as the store remembers them, with
    /// no outline yet to name them after.
    fn stored_bookmarks(&self, path: &Path) -> Rc<VecModel<BookmarkFlag>> {
        let bookmarks = self.store.borrow().bookmarks(path);
        Rc::new(VecModel::from(flags(&bookmarks, &ModelRc::default())))
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
        // Indexing starts as soon as the document is open, so its text is
        // ready by the time the reader searches.
        let indexer = search::spawn_indexer(
            loading.path.clone(),
            loaded.doc,
            window.as_weak(),
            self.index_sender.clone(),
        );
        let cataloguer = images::spawn_cataloguer(
            loading.path.clone(),
            loaded.doc,
            window.as_weak(),
            self.catalogue_sender.clone(),
        );
        let thumbnails = render::spawn_thumbnails(loading.path, loaded.doc, window.as_weak());
        let workers = Workers {
            pages: loading.pages,
            thumbnails,
            control: loading.control,
            vectorizer: self.vectorizer(loaded.doc),
        };
        let path = self.tabs.borrow()[index].path.clone();
        let viewer = self.make_viewer(&window, &path, info.pages_pt, workers);
        viewer.set_links(info.links);
        {
            let mut tabs = self.tabs.borrow_mut();
            let tab = &mut tabs[index];
            tab.viewer = Some(viewer);
            tab.outline = ModelRc::new(VecModel::from(outline));
            tab.indexer = Some(indexer);
            tab.cataloguer = Some(cataloguer);
            // The flags were shown before the outline arrived to name them.
            let bookmarks = self.store.borrow().bookmarks(&tab.path);
            tab.bookmarks.set_vec(flags(&bookmarks, &tab.outline));
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
        let bookmarks = self.store.borrow().bookmarks(&path);
        Some(self.add_tab(Tab {
            id,
            path,
            title,
            viewer: Some(viewer),
            bookmarks: Rc::new(VecModel::from(flags(&bookmarks, &outline))),
            outline,
            return_page: None,
            loading: None,
            indexer: None,
            cataloguer: None,
        }))
    }

    /// Takes in the text every document's indexer has read since last time.
    pub(crate) fn receive_indexes(&self) {
        while let Ok(indexed) = self.indexes.try_recv() {
            self.take_indexed(indexed);
        }
    }

    /// Hands pages an indexer has read to its document's viewer. A tab closed
    /// since has nothing to take them.
    pub(crate) fn take_indexed(&self, indexed: Indexed) {
        let Indexed { doc, first_page, pages } = indexed;
        self.with_document(doc, |viewer| viewer.on_text_indexed(first_page, pages));
    }

    /// Takes in the images every document's cataloguer has read since last
    /// time.
    pub(crate) fn receive_catalogues(&self) {
        while let Ok(catalogued) = self.catalogues.try_recv() {
            self.take_catalogued(catalogued);
        }
    }

    /// Hands pages a cataloguer has read to its document's viewer.
    pub(crate) fn take_catalogued(&self, catalogued: Catalogued) {
        let Catalogued { doc, first_page, pages } = catalogued;
        self.with_document(doc, |viewer| viewer.on_catalogued(first_page, pages));
    }

    /// Sets which images every document's list leaves out and gathers,
    /// which is remembered across runs.
    pub(crate) fn set_image_filters(&self, filters: ImageFilters) {
        self.store.borrow_mut().set_image_filters(filters);
        let viewers: Vec<Rc<Viewer>> =
            self.tabs.borrow().iter().filter_map(|tab| tab.viewer.clone()).collect();
        for viewer in viewers {
            viewer.set_image_filters(filters);
        }
    }

    /// Takes in the page vectors every document's vectorizer has made since
    /// last time.
    pub(crate) fn receive_vectors(&self) {
        while let Ok(vectorized) = self.vectors.try_recv() {
            self.take_vectorized(vectorized);
        }
    }

    /// Hands page vectors a vectorizer has made to its document's viewer.
    pub(crate) fn take_vectorized(&self, vectorized: Vectorized) {
        let Vectorized { doc, first_page, result } = vectorized;
        self.with_document(doc, |viewer| viewer.on_vectorized(first_page, result));
    }

    /// Takes in every screenshot a document's worker has delivered since
    /// last time.
    pub(crate) fn receive_screenshots(&self) {
        while let Ok(shot) = self.shots.try_recv() {
            self.take_screenshot(shot);
        }
    }

    /// Puts a screenshot on the clipboard. This is where a notice that it
    /// was taken would go, were one wanted: the outline flashing on the page
    /// says so for now.
    pub(crate) fn take_screenshot(&self, shot: Shot) {
        let number = shot.request.page + 1;
        match shot.result {
            Ok(picture) => self.clipboard.set_image(picture.width, picture.height, picture.rgba),
            Err(err) => self.notify(&format!("Page {number} could not be screenshot: {err}.")),
        }
    }

    /// A vectorizer for the document whose workers are tagged `id`.
    fn vectorizer(&self, id: i32) -> Vectorizer {
        Vectorizer::new(id, self.model.clone(), self.vector_sender.clone(), self.window.clone())
    }

    /// A viewer for the document at `path`, in the view it was last left in.
    fn make_viewer(
        &self,
        window: &MainWindow,
        path: &Path,
        pages_pt: Vec<(f32, f32)>,
        workers: Workers,
    ) -> Rc<Viewer> {
        let (settings, filters) = {
            let mut store = self.store.borrow_mut();
            let settings = store.document(path).unwrap_or_default();
            store.record_open(path);
            (settings, store.image_filters())
        };
        Viewer::new(window, pages_pt, window.window().scale_factor(), workers, &settings, filters)
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
        let Some((viewer, outline, bookmarks, return_page, title)) =
            self.tabs.borrow().get(index).map(|tab| {
                (
                    tab.viewer.clone(),
                    tab.outline.clone(),
                    tab.bookmarks.clone(),
                    tab.return_page,
                    tab.title.clone(),
                )
            })
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
        window.set_bookmarks(ModelRc::from(bookmarks));
        window.set_return_page(return_page.map_or(-1, |page| page as i32));
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

    /// Moves the tab at `from` to `to`, shifting the tabs between them over by
    /// one, as dragging a tab along the strip does. The shown tab stays shown
    /// wherever it ends up.
    pub(crate) fn move_tab(&self, from: usize, to: usize) {
        {
            let mut tabs = self.tabs.borrow_mut();
            if from == to || from >= tabs.len() || to >= tabs.len() {
                return;
            }
            let tab = tabs.remove(from);
            tabs.insert(to, tab);
        }
        let title = self.titles.remove(from);
        self.titles.insert(to, title);

        let Some(active) = self.active.get() else {
            return;
        };
        let active = if active == from {
            to
        } else if from < active && active <= to {
            active - 1
        } else if to <= active && active < from {
            active + 1
        } else {
            active
        };
        self.active.set(Some(active));
        if let Some(window) = self.window.upgrade() {
            window.set_active_tab(active as i32);
        }
    }

    /// Flags the page being read in the active tab, or takes its flag away.
    /// The store remembers the change; the autosave writes it out.
    pub(crate) fn toggle_bookmark(&self) {
        let Some((index, viewer)) = self.active.get().zip(self.active_viewer()) else {
            return;
        };
        let page = viewer.reading_page();
        let path = self.tabs.borrow()[index].path.clone();
        let bookmarks = self.store.borrow_mut().toggle_bookmark(&path, page);
        self.set_flags(index, &bookmarks);
    }

    /// Gives the flag on `page` of the active tab's document the colour of
    /// `hue`.
    pub(crate) fn color_bookmark(&self, page: usize, hue: u16) {
        let Some(index) = self.active.get() else {
            return;
        };
        let path = self.tabs.borrow()[index].path.clone();
        let bookmarks = self.store.borrow_mut().set_bookmark_hue(&path, page, hue);
        self.set_flags(index, &bookmarks);
    }

    /// Gives the flag on `page` of the active tab's document the shape
    /// `shape`.
    pub(crate) fn shape_bookmark(&self, page: usize, shape: Shape) {
        let Some(index) = self.active.get() else {
            return;
        };
        let path = self.tabs.borrow()[index].path.clone();
        let bookmarks = self.store.borrow_mut().set_bookmark_shape(&path, page, shape);
        self.set_flags(index, &bookmarks);
    }

    /// Takes the flag off `page` of the active tab's document.
    pub(crate) fn remove_bookmark(&self, page: usize) {
        let Some(index) = self.active.get() else {
            return;
        };
        let path = self.tabs.borrow()[index].path.clone();
        let bookmarks = self.store.borrow_mut().remove_bookmark(&path, page);
        self.set_flags(index, &bookmarks);
    }

    /// Redraws the flags of the tab at `index` from `bookmarks`.
    fn set_flags(&self, index: usize, bookmarks: &[Bookmark]) {
        let tabs = self.tabs.borrow();
        let tab = &tabs[index];
        tab.bookmarks.set_vec(flags(bookmarks, &tab.outline));
    }

    /// Goes to the flagged `page`, leaving the page being read as the place
    /// to flip back to. Going to the page already being read leaves nothing
    /// to flip back to, so it does not disturb the place.
    pub(crate) fn go_to_bookmark(&self, page: usize) {
        let Some((index, viewer)) = self.active.get().zip(self.active_viewer()) else {
            return;
        };
        let from = viewer.reading_page();
        if from == page {
            return;
        }
        viewer.nav_to_page(page);
        self.set_return_page(index, from);
    }

    /// Goes to a page picked in the sidebar, an outline entry or a
    /// thumbnail, leaving the dog-ear on the page being read: a click there
    /// is as much a jump as going to a flag, and the reader wants Tab to
    /// bring them back from it just the same. Picking the page already being
    /// read leaves the dog-ear alone.
    pub(crate) fn go_to_page_index(&self, page: usize) {
        let Some((index, viewer)) = self.active.get().zip(self.active_viewer()) else {
            return;
        };
        let from = viewer.reading_page();
        if from == page {
            return;
        }
        viewer.nav_to_page(page);
        self.set_return_page(index, from);
    }

    /// Follows a link clicked on a page. One to a page of the document goes
    /// there, leaving the dog-ear on the page being read as every jump does.
    /// One to an address outside is handed to the system to open, if it is
    /// a web or mail address; anything else is only told of, so a link
    /// cannot run anything.
    pub(crate) fn follow_link(&self, link: PageLink) {
        match link.target {
            LinkTarget::Page { page, top } => {
                let Some((index, viewer)) = self.active.get().zip(self.active_viewer()) else {
                    return;
                };
                let from = viewer.reading_page();
                viewer.nav_to_point(page, top);
                if viewer.reading_page() != from {
                    self.set_return_page(index, from);
                }
            }
            LinkTarget::Uri(uri) => {
                if !links::opens_externally(&uri) {
                    self.notify(&format!("The link points to {uri}, which is left alone."));
                } else if let Err(err) = self.opener.open(&uri) {
                    self.notify(&format!("Could not open {uri}: {err}."));
                } else {
                    self.notify(&format!("Opening {}.", links::describe(&uri)));
                }
            }
        }
    }

    /// Goes to the next (`dir > 0`) or previous flag from the page being
    /// read, wrapping around at the ends. A page's own flag does not count as
    /// next or previous, so pressing on always moves.
    pub(crate) fn bookmark_step(&self, dir: i32) {
        let Some(viewer) = self.active_viewer() else {
            return;
        };
        let page = viewer.reading_page();
        let pages: Vec<usize> = self.flagged_pages();
        let target = if dir < 0 {
            pages.iter().rev().find(|&&flagged| flagged < page).or(pages.last())
        } else {
            pages.iter().find(|&&flagged| flagged > page).or(pages.first())
        };
        if let Some(&target) = target {
            self.go_to_bookmark(target);
        }
    }

    /// Flips back to the page the last bookmark jump left, and remembers the
    /// page left now, so flipping again returns to the flag: the two pages
    /// swap, like a ribbon and a flag in a book.
    pub(crate) fn flip_back(&self) {
        let Some((index, viewer)) = self.active.get().zip(self.active_viewer()) else {
            return;
        };
        let Some(target) = self.tabs.borrow()[index].return_page else {
            return;
        };
        let from = viewer.reading_page();
        viewer.nav_to_page(target);
        self.set_return_page(index, from);
    }

    /// Goes to the page asked for in the page field, leaving the dog-ear on
    /// the page being read, since a jump is what the reader most wants to
    /// come back from. Asking for the page already being read, or for none,
    /// leaves the dog-ear alone.
    pub(crate) fn go_to_page(&self, text: &str) {
        let Some((index, viewer)) = self.active.get().zip(self.active_viewer()) else {
            return;
        };
        let from = viewer.reading_page();
        viewer.go_to_page(text);
        if viewer.reading_page() != from {
            self.set_return_page(index, from);
        }
    }

    /// Puts the dog-ear on the page being read, so Tab comes back
    /// here from wherever the reader wanders off to, without a flag.
    pub(crate) fn mark_return_page(&self) {
        let Some((index, viewer)) = self.active.get().zip(self.active_viewer()) else {
            return;
        };
        self.set_return_page(index, viewer.reading_page());
    }

    /// The flagged pages of the active tab, in page order.
    fn flagged_pages(&self) -> Vec<usize> {
        let Some(index) = self.active.get() else {
            return Vec::new();
        };
        let tabs = self.tabs.borrow();
        tabs[index].bookmarks.iter().filter_map(|flag| self::index(flag.page)).collect()
    }

    fn set_return_page(&self, index: usize, page: usize) {
        self.tabs.borrow_mut()[index].return_page = Some(page);
        if let Some(window) = self.window.upgrade() {
            window.set_return_page(page as i32);
        }
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
        window.set_bookmarks(ModelRc::default());
        window.set_return_page(-1);
        window.set_doc_title(SharedString::new());
        Self::clear_document(&window, "Open a PDF to get started.");
    }

    /// Shows no pages, only `status` in their place, and nothing to search.
    fn clear_document(window: &MainWindow, status: &str) {
        window.set_rows(ModelRc::default());
        window.set_thumb_rows(ModelRc::default());
        window.set_image_rows(ModelRc::default());
        window.set_page_count(0);
        window.set_current_page(0);
        window.set_status(status.into());
        window.set_search_results(ModelRc::default());
        window.set_search_text(SharedString::new());
        window.set_search_status(SharedString::new());
        window.set_search_current(-1);
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

    /// Opens each document another instance was asked to open in a new tab,
    /// and brings the window forward, since the reader has just asked for it.
    /// Asking with no documents only brings the window forward.
    pub(crate) fn open_requested(&self, paths: Vec<String>) {
        for path in paths {
            self.open(path);
        }
        if let Some(window) = self.window.upgrade() {
            window.window().set_minimized(false);
            window.window().with_winit_window(|window| window.focus_window());
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
/// runs the event loop until the window closes. When the viewer is already
/// running, `paths` open as tabs in its window instead.
pub fn run(paths: Vec<String>) -> Result<(), Box<dyn Error>> {
    #[cfg(unix)]
    let listener = match instance::claim(&paths) {
        instance::Claim::First(listener) => Some(listener),
        instance::Claim::Forwarded => return Ok(()),
        instance::Claim::Alone => None,
    };

    let window = MainWindow::new()?;

    // Without an app ID matching the desktop entry, compositors cannot tie the
    // window back to it and show a generic icon and title instead. Creating the
    // window above initialises the platform this needs, and the ID is only read
    // when the window is actually shown, so setting it here is in time.
    slint::set_xdg_app_id("io.github.xremming.MelkkiPDF")?;

    let app = App::new(
        &window,
        Store::open_default(),
        Clipboard::system(),
        Opener::system(),
        semantic::EmbeddingModel::locate(),
    );

    #[cfg(target_os = "macos")]
    macos::on_open_document({
        let app = app.clone();
        move |path| {
            app.open(path.to_string_lossy().into_owned());
        }
    });

    // Slint's own DropArea does not receive drops from other applications on
    // winit yet, so files dropped onto the window are taken from winit. The
    // mouse's back button is taken here too: Slint hands a button to the
    // touch area under the pointer, which would leave it to whichever of the
    // window's many is there, while here one place sees it wherever it is.
    // Neither the press nor the release goes on to Slint, so nothing under
    // the pointer takes it as the start of a drag.
    window.window().on_winit_window_event({
        let app = Rc::downgrade(&app);
        move |_, event| {
            let Some(app) = app.upgrade() else {
                return EventResult::Propagate;
            };
            if let Some(pressed) = back_button(event) {
                if pressed {
                    app.flip_back();
                }
                return EventResult::PreventDefault;
            }
            match FileDrag::from_winit(event) {
                Some(drag) => {
                    app.file_drag(drag);
                    EventResult::PreventDefault
                }
                None => EventResult::Propagate,
            }
        }
    });

    app.restore_session();
    for path in paths {
        app.open(path);
    }

    #[cfg(unix)]
    if let Some(listener) = listener {
        let (requests, received) = mpsc::channel();
        window.on_documents_requested({
            let app = app.clone();
            move || {
                while let Ok(paths) = received.try_recv() {
                    app.open_requested(paths);
                }
            }
        });
        let window = window.as_weak();
        listener.serve(requests, move || {
            let _ = window.upgrade_in_event_loop(|window| window.invoke_documents_requested());
        });
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

/// How light a flag is at least, in OKLCH. Each flag is the strongest
/// colour of its hue, a neon, and blue and purple are strongest dark, where
/// they would sink into the chrome, so those are lifted to this.
const FLAG_MIN_LIGHTNESS: f64 = 0.6;

/// The flags to draw for `bookmarks`, each named after the last entry of
/// `outline` that starts on or before its page, since a flag has no name of
/// its own and the heading is what the reader knows the page by.
fn flags(bookmarks: &[Bookmark], outline: &ModelRc<OutlineItem>) -> Vec<BookmarkFlag> {
    bookmarks
        .iter()
        .map(|bookmark| {
            let page = bookmark.page as i32;
            let heading = outline
                .iter()
                .filter(|item| item.page >= 0 && item.page <= page)
                .last()
                .map(|item| item.title.trim().to_owned())
                .filter(|title| !title.is_empty());
            let label = match heading {
                Some(heading) => format!("Page {} · {heading}", page + 1),
                None => format!("Page {}", page + 1),
            };
            BookmarkFlag {
                page,
                color: flag_color(bookmark.hue),
                hue: i32::from(bookmark.hue),
                shape: bookmark.shape.index() as i32,
                label: label.into(),
            }
        })
        .collect()
}

/// The colour of a flag with `hue`, in degrees of OKLCH.
fn flag_color(hue: u16) -> slint::Color {
    color::neon(f64::from(hue), FLAG_MIN_LIGHTNESS)
}

/// The size of an icon in a flag's menu, in pixels.
const ICON_SIZE: u32 = 16;
/// How many samples across a pixel an icon's edge is judged by, so it is
/// not jagged.
const ICON_SAMPLES: u32 = 4;
/// The grey of a shape's icon, which shows on a light or a dark menu.
const SHAPE_GREY: u8 = 0x8c;

/// An icon of `color` covering the points for which `inside` holds, in
/// pixels from the icon's top-left corner.
fn icon(color: slint::Color, inside: impl Fn(f32, f32) -> bool) -> slint::Image {
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(ICON_SIZE, ICON_SIZE);
    let step = 1.0 / ICON_SAMPLES as f32;
    for (index, pixel) in buffer.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = ((index as u32 % ICON_SIZE) as f32, (index as u32 / ICON_SIZE) as f32);
        let hits = (0..ICON_SAMPLES * ICON_SAMPLES)
            .filter(|&sample| {
                let dx = ((sample % ICON_SAMPLES) as f32 + 0.5) * step;
                let dy = ((sample / ICON_SAMPLES) as f32 + 0.5) * step;
                inside(x + dx, y + dy)
            })
            .count();
        let alpha = hits as f32 / (ICON_SAMPLES * ICON_SAMPLES) as f32;
        *pixel = slint::Rgba8Pixel {
            r: color.red(),
            g: color.green(),
            b: color.blue(),
            a: (alpha * 255.0).round() as u8,
        };
    }
    slint::Image::from_rgba8(buffer)
}

/// The colours a flag's menu offers, each with a round swatch of it.
fn palette() -> Vec<FlagColor> {
    let centre = ICON_SIZE as f32 / 2.0;
    PALETTE
        .iter()
        .map(|&(name, hue)| FlagColor {
            name: name.into(),
            hue: i32::from(hue),
            swatch: icon(flag_color(hue), |x, y| {
                (x - centre).powi(2) + (y - centre).powi(2) <= centre * centre
            }),
        })
        .collect()
}

/// The shapes a flag's menu offers, each with a picture of a flag cut that
/// way, as the window draws them (see `flag-path` there).
fn shapes() -> Vec<FlagShape> {
    let (width, height) = (10.0, 14.0);
    let (left, top) = ((ICON_SIZE as f32 - width) / 2.0, (ICON_SIZE as f32 - height) / 2.0);
    let cut = 4.0;
    let corner = 2.5;
    SHAPES
        .iter()
        .map(|&(name, shape)| FlagShape {
            name: name.into(),
            shape: shape.index() as i32,
            icon: icon(
                slint::Color::from_rgb_u8(SHAPE_GREY, SHAPE_GREY, SHAPE_GREY),
                move |x, y| {
                    let (x, y) = (x - left, y - top);
                    if !(0.0..=width).contains(&x) || !(0.0..=height).contains(&y) {
                        return false;
                    }
                    // How far across the flag the point is, 0 at the edges and 1
                    // in the middle, which the cuts of the end follow.
                    let across = 1.0 - (x - width / 2.0).abs() / (width / 2.0);
                    let rounded = |radius: f32| {
                        let (cx, cy) = (
                            (x - width / 2.0).abs() - (width / 2.0 - radius),
                            y - (height - radius),
                        );
                        cx <= 0.0 || cy <= 0.0 || cx * cx + cy * cy <= radius * radius
                    };
                    match shape {
                        Shape::Tab => rounded(corner),
                        Shape::Pennant => y <= height - cut * across,
                        Shape::Arrow => y <= height - cut + cut * across,
                        Shape::Round => rounded(width / 2.0),
                    }
                },
            ),
        })
        .collect()
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
    // Like switching, the new order is left for the autosave to write.
    window.on_move_tab({
        let app = app.clone();
        move |from, to| {
            if let (Some(from), Some(to)) = (index(from), index(to)) {
                app.move_tab(from, to);
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
        let weak = window.as_weak();
        move |width, height| {
            // The window's display may have changed with its size.
            if let Some(window) = weak.upgrade() {
                window.set_device_pixel(1.0 / window.window().scale_factor());
            }
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
        move |text| app.go_to_page(text.as_str())
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
    // A press on a page, and the drag from it, selects text, or takes a
    // screenshot while that mode is on.
    window.on_select_from({
        let app = app.clone();
        let window = window.as_weak();
        move |page, x, y| {
            let capturing = window.upgrade().is_some_and(|w| w.global::<Screenshot>().get_active());
            if let Some(page) = index(page) {
                app.with_viewer(|v| {
                    if capturing { v.capture_from(page, x, y) } else { v.select_from(page, x, y) }
                });
            }
        }
    });
    window.on_select_to({
        let app = app.clone();
        move |page, x, y| {
            if let Some(page) = index(page) {
                app.with_viewer(|v| {
                    if v.capturing() { v.capture_to(x, y) } else { v.select_to(page, x, y) }
                });
            }
        }
    });
    window.on_select_done({
        let app = app.clone();
        move || {
            let Some(viewer) = app.active_viewer() else {
                return;
            };
            if viewer.capturing() {
                viewer.capture_done();
            } else if let Some(link) = viewer.select_done() {
                app.follow_link(link);
            }
        }
    });
    // The pointer over a page, or gone from it, for the cursor to show a
    // link under it.
    window.on_hover_page({
        let app = app.clone();
        let window = window.as_weak();
        move |page, x, y| {
            let over = index(page)
                .zip(app.active_viewer())
                .is_some_and(|(page, viewer)| viewer.link_under(page, x, y));
            if let Some(window) = window.upgrade()
                && window.get_link_under_pointer() != over
            {
                window.set_link_under_pointer(over);
            }
        }
    });
    window.on_leave_page({
        let window = window.as_weak();
        move || {
            if let Some(window) = window.upgrade() {
                window.set_link_under_pointer(false);
            }
        }
    });
    window.on_select_scroll({
        let app = app.clone();
        move |delta_x, delta_y, shift| {
            app.with_viewer(|v| {
                if v.capturing() {
                    v.capture_scroll(delta_x, delta_y, shift)
                } else {
                    v.select_scroll(delta_x, delta_y, shift)
                }
            })
        }
    });
    window.on_copy_selection({
        let app = app.clone();
        move || {
            if let Some(text) = app.active_viewer().and_then(|v| v.selected_text()) {
                app.clipboard.set_text(&text);
            }
        }
    });
    window.on_clear_selection({
        let app = app.clone();
        move || app.with_viewer(|v| v.clear_selection())
    });
    window.on_select_all({
        let app = app.clone();
        move || app.with_viewer(|v| v.select_all())
    });
    window.on_go_to_page_index({
        let app = app.clone();
        move |page| {
            if let Some(page) = index(page) {
                app.go_to_page_index(page);
            }
        }
    });
    window.on_request_preview({
        let app = app.clone();
        move |row| {
            if let Some(row) = index(row) {
                app.with_viewer(|v| v.request_preview(row));
            }
        }
    });
    window.on_preview_rendered({
        let app = app.clone();
        move |doc, page, ordinal, image| {
            if let (Some(page), Some(ordinal)) = (index(page), index(ordinal)) {
                app.with_document(doc, |v| v.on_preview_rendered(page, ordinal, image));
            }
        }
    });
    window.on_copy_image({
        let app = app.clone();
        move |row| {
            if let Some(row) = index(row) {
                app.with_viewer(|v| v.copy_image(row));
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
    window.on_text_indexed({
        let app = app.clone();
        move || app.receive_indexes()
    });
    window.on_pages_vectorized({
        let app = app.clone();
        move || app.receive_vectors()
    });
    window.on_screenshot_taken({
        let app = app.clone();
        move || app.receive_screenshots()
    });
    window.on_images_catalogued({
        let app = app.clone();
        move || app.receive_catalogues()
    });
    window.on_image_filters_changed({
        let app = app.clone();
        move |hide_small, group_repeats| {
            app.set_image_filters(ImageFilters { hide_small, group_repeats })
        }
    });
    window.on_capture_cancel({
        let app = app.clone();
        move || app.with_viewer(|v| v.capture_cancel())
    });
    window.on_search_mode_changed({
        let app = app.clone();
        move |mode| {
            if let Some(mode) = SearchMode::from_index(mode) {
                app.with_viewer(|v| v.set_search_mode(mode));
            }
        }
    });
    window.on_search_edited({
        let app = app.clone();
        move |query| app.with_viewer(|v| v.search_edited(query.as_str()))
    });
    window.on_search_step({
        let app = app.clone();
        move |dir| app.with_viewer(|v| v.search_step(dir))
    });
    window.on_go_to_search_result({
        let app = app.clone();
        move |result| {
            if let Some(result) = index(result) {
                app.with_viewer(|v| v.go_to_search_result(result));
            }
        }
    });
    window.on_toggle_bookmark({
        let app = app.clone();
        move || app.toggle_bookmark()
    });
    window.on_color_bookmark({
        let app = app.clone();
        move |page, hue| {
            if let (Some(page), Ok(hue)) = (index(page), u16::try_from(hue)) {
                app.color_bookmark(page, hue);
            }
        }
    });
    window.on_shape_bookmark({
        let app = app.clone();
        move |page, shape| {
            if let (Some(page), Some(shape)) =
                (index(page), index(shape).and_then(Shape::from_index))
            {
                app.shape_bookmark(page, shape);
            }
        }
    });
    window.on_remove_bookmark({
        let app = app.clone();
        move |page| {
            if let Some(page) = index(page) {
                app.remove_bookmark(page);
            }
        }
    });
    window.on_go_to_bookmark({
        let app = app.clone();
        move |page| {
            if let Some(page) = index(page) {
                app.go_to_bookmark(page);
            }
        }
    });
    window.on_bookmark_step({
        let app = app.clone();
        move |dir| app.bookmark_step(dir)
    });
    window.on_flip_back({
        let app = app.clone();
        move || app.flip_back()
    });
    window.on_mark_return_page({
        let app = app.clone();
        move || app.mark_return_page()
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

#[cfg(test)]
mod tests {
    use super::{back_button, winit};
    use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};

    fn click(button: MouseButton, state: ElementState) -> WindowEvent {
        WindowEvent::MouseInput { device_id: DeviceId::dummy(), state, button }
    }

    #[test]
    fn the_back_button_is_told_from_the_other_buttons() {
        assert_eq!(back_button(&click(MouseButton::Back, ElementState::Pressed)), Some(true));
        assert_eq!(back_button(&click(MouseButton::Back, ElementState::Released)), Some(false));
        assert_eq!(back_button(&click(MouseButton::Forward, ElementState::Pressed)), None);
        assert_eq!(back_button(&click(MouseButton::Left, ElementState::Pressed)), None);
        assert_eq!(back_button(&WindowEvent::HoveredFileCancelled), None);
    }
}
