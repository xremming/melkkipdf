//! Remembers each document's view settings and bookmarks, and the tabs that
//! were open, between runs.
//!
//! Everything lives in one small JSON file under the platform's state
//! directory. It is rewritten whole, through a temporary file renamed over the
//! old one, so a crash mid-write leaves the previous version intact.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::ViewSettings;

/// Bumped whenever the file's layout changes incompatibly, so an old build
/// never misreads a newer file as its own, nor overwrites it. Version 2 added
/// bookmarks, which a version 1 build would silently drop on its next save.
const VERSION: u32 = 2;
/// The oldest layout that still reads as the current one: version 1 only
/// lacks bookmarks, which default to none.
const OLDEST_READABLE: u32 = 1;
/// How many documents are remembered. The least recently opened are dropped
/// first, so the file cannot grow without bound.
const MAX_DOCUMENTS: usize = 500;
const FILE_NAME: &str = "documents.json";

/// The file as stored on disk.
#[derive(Serialize, Deserialize)]
struct StateFile {
    version: u32,
    #[serde(default)]
    documents: Vec<DocumentEntry>,
    #[serde(default)]
    session: Session,
}

#[derive(Clone, Serialize, Deserialize)]
struct DocumentEntry {
    path: PathBuf,
    /// Seconds since the Unix epoch, used to drop the least recently opened
    /// documents once there are too many.
    last_opened: u64,
    settings: ViewSettings,
    /// Sorted by page, with at most one bookmark per page.
    #[serde(default)]
    bookmarks: Vec<Bookmark>,
}

/// A page the reader has flagged.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bookmark {
    pub page: usize,
    /// The hue of its flag in degrees: one of [`PALETTE`], given in turn as
    /// flags are added so they are told apart at a glance, or whatever the
    /// reader picked for it.
    pub hue: u16,
}

/// The colours a flag can have, as names and hues in degrees. A new flag
/// takes the first of them used by the fewest of the document's flags, so
/// flags go round the palette and a colour freed up is taken again.
pub const PALETTE: [(&str, u16); 7] = [
    ("Red", 0),
    ("Orange", 28),
    ("Yellow", 52),
    ("Green", 125),
    ("Teal", 178),
    ("Blue", 212),
    ("Purple", 272),
];

/// The tabs that were open, in order, and which of them was shown.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub tabs: Vec<PathBuf>,
    pub active: Option<usize>,
}

/// The remembered settings, loaded from and saved to one file. Changes are
/// kept in memory and only written by [`Store::save`], and only when there is
/// something new to write.
pub struct Store {
    /// Where the file lives, or `None` for a store that is never written.
    file: Option<PathBuf>,
    documents: HashMap<PathBuf, DocumentEntry>,
    session: Session,
    dirty: bool,
}

impl Store {
    /// The store in the platform's usual place: `$XDG_STATE_HOME/melkkipdf`
    /// on Linux (inside the flatpak's own directory when sandboxed) and the
    /// application support directory on macOS, which has no state directory.
    pub fn open_default() -> Self {
        let directory = dirs::state_dir()
            .map(|dir| dir.join("melkkipdf"))
            .or_else(|| dirs::data_dir().map(|dir| dir.join("io.github.xremming.MelkkiPDF")));
        match directory {
            Some(directory) => Self::load(directory.join(FILE_NAME)),
            None => {
                eprintln!("Found no directory to keep settings in, so they will not be saved.");
                Self::in_memory()
            }
        }
    }

    /// A store that starts empty and never touches the disk.
    pub fn in_memory() -> Self {
        Self { file: None, documents: HashMap::new(), session: Session::default(), dirty: false }
    }

    /// Loads the store from `file`. A missing file is simply an empty store. An
    /// unreadable one is moved aside to a backup (see [`backup_path`]), rather
    /// than overwritten by the next save, and the store starts empty.
    ///
    /// A file written by a newer version is left where it is and the store
    /// never writes it, so running an older build does not lose what the
    /// newer one remembered. That run remembers nothing.
    pub fn load(file: PathBuf) -> Self {
        let mut store = Self { file: Some(file.clone()), ..Self::in_memory() };
        let contents = match fs::read(&file) {
            Ok(contents) => contents,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return store,
            Err(err) => {
                eprintln!("Failed to read settings from {}: {err}.", file.display());
                return store;
            }
        };

        // The version alone, so a newer file is recognised even when the rest
        // of its layout would not parse.
        #[derive(Deserialize)]
        struct Versioned {
            version: u32,
        }
        if let Ok(Versioned { version }) = serde_json::from_slice(&contents)
            && version > VERSION
        {
            eprintln!(
                "Settings in {} come from a newer version, so they are left alone and nothing \
                 is remembered this run.",
                file.display()
            );
            store.file = None;
            return store;
        }

        let parsed = serde_json::from_slice::<StateFile>(&contents)
            .map_err(|err| err.to_string())
            .and_then(|state| match state.version {
                OLDEST_READABLE..=VERSION => Ok(state),
                other => Err(format!("unknown version {other}")),
            });
        match parsed {
            Ok(state) => {
                store.documents =
                    state.documents.into_iter().map(|entry| (entry.path.clone(), entry)).collect();
                store.session = state.session;
            }
            Err(err) => {
                let backup = backup_path(&file);
                eprintln!(
                    "Settings in {} are unreadable ({err}), so they were moved to {}.",
                    file.display(),
                    backup.display()
                );
                if let Err(err) = fs::rename(&file, &backup) {
                    eprintln!("Failed to move the unreadable settings aside: {err}.");
                }
            }
        }
        store
    }

    /// The settings remembered for the document at `path`, if any.
    pub fn document(&self, path: &Path) -> Option<ViewSettings> {
        self.documents.get(path).map(|entry| entry.settings.clone())
    }

    /// Notes that the document at `path` was just opened, so it is the last to
    /// be forgotten.
    pub fn record_open(&mut self, path: &Path) {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs());
        let entry = self.documents.entry(path.to_path_buf()).or_insert_with(|| DocumentEntry {
            path: path.to_path_buf(),
            last_opened: now,
            settings: ViewSettings::default(),
            bookmarks: Vec::new(),
        });
        entry.last_opened = now;
        self.dirty = true;
    }

    /// The bookmarks of the document at `path`, sorted by page.
    pub fn bookmarks(&self, path: &Path) -> Vec<Bookmark> {
        self.documents.get(path).map_or_else(Vec::new, |entry| entry.bookmarks.clone())
    }

    /// Flags `page` of the document at `path`, or takes its flag away if it
    /// has one, and returns the document's bookmarks as they are now.
    pub fn toggle_bookmark(&mut self, path: &Path, page: usize) -> Vec<Bookmark> {
        if self
            .documents
            .get(path)
            .is_some_and(|entry| entry.bookmarks.iter().any(|b| b.page == page))
        {
            return self.remove_bookmark(path, page);
        }
        if !self.documents.contains_key(path) {
            self.record_open(path);
        }
        let entry = self.documents.get_mut(path).expect("the document was just recorded");
        let at = entry.bookmarks.partition_point(|bookmark| bookmark.page < page);
        let hue = next_hue(&entry.bookmarks);
        entry.bookmarks.insert(at, Bookmark { page, hue });
        self.dirty = true;
        entry.bookmarks.clone()
    }

    /// Gives the flag on `page` of the document at `path` the colour of
    /// `hue`, if it has one, and returns the document's bookmarks as they are
    /// now.
    pub fn set_bookmark_hue(&mut self, path: &Path, page: usize, hue: u16) -> Vec<Bookmark> {
        let Some(entry) = self.documents.get_mut(path) else {
            return Vec::new();
        };
        if let Some(bookmark) = entry.bookmarks.iter_mut().find(|bookmark| bookmark.page == page)
            && bookmark.hue != hue
        {
            bookmark.hue = hue;
            self.dirty = true;
        }
        entry.bookmarks.clone()
    }

    /// Takes away the flag on `page` of the document at `path`, if it has one,
    /// and returns the document's bookmarks as they are now.
    pub fn remove_bookmark(&mut self, path: &Path, page: usize) -> Vec<Bookmark> {
        let Some(entry) = self.documents.get_mut(path) else {
            return Vec::new();
        };
        let before = entry.bookmarks.len();
        entry.bookmarks.retain(|bookmark| bookmark.page != page);
        if entry.bookmarks.len() != before {
            self.dirty = true;
        }
        entry.bookmarks.clone()
    }

    /// Remembers `settings` for the document at `path`.
    pub fn update(&mut self, path: &Path, settings: ViewSettings) {
        match self.documents.get_mut(path) {
            Some(entry) if entry.settings == settings => {}
            Some(entry) => {
                entry.settings = settings;
                self.dirty = true;
            }
            None => {
                self.record_open(path);
                self.update(path, settings);
            }
        }
    }

    pub fn session(&self) -> Session {
        self.session.clone()
    }

    pub fn set_session(&mut self, session: Session) {
        if self.session != session {
            self.session = session;
            self.dirty = true;
        }
    }

    /// Writes the store out if anything changed since it was last written.
    pub fn save(&mut self) -> io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        self.forget_oldest();
        let Some(file) = &self.file else {
            self.dirty = false;
            return Ok(());
        };

        // A path that is not valid UTF-8 cannot be written as a JSON string, so
        // such a document is not remembered rather than failing the whole save.
        let mut documents: Vec<DocumentEntry> = self
            .documents
            .values()
            .filter(|entry| entry.path.to_str().is_some())
            .cloned()
            .collect();
        documents.sort_by(|a, b| a.path.cmp(&b.path));
        // Dropping a tab moves the ones after it down, so the active index
        // has to follow its tab, or go if the tab itself was dropped.
        let mut session = Session::default();
        for (index, tab) in self.session.tabs.iter().enumerate() {
            if tab.to_str().is_none() {
                continue;
            }
            if self.session.active == Some(index) {
                session.active = Some(session.tabs.len());
            }
            session.tabs.push(tab.clone());
        }
        let state = StateFile { version: VERSION, documents, session };
        let contents = serde_json::to_vec_pretty(&state).map_err(io::Error::other)?;

        if let Some(directory) = file.parent() {
            fs::create_dir_all(directory)?;
        }
        let temporary = file.with_extension("json.tmp");
        fs::write(&temporary, contents)?;
        fs::rename(&temporary, file)?;
        self.dirty = false;
        Ok(())
    }

    /// Drops the least recently opened documents beyond [`MAX_DOCUMENTS`],
    /// sparing any that are open in a tab. A bookmarked document is spared
    /// too: its view is disposable, but its bookmarks are the reader's own
    /// work, and flagging takes enough effort that they cannot pile up.
    fn forget_oldest(&mut self) {
        if self.documents.len() <= MAX_DOCUMENTS {
            return;
        }
        let open: HashSet<&PathBuf> = self.session.tabs.iter().collect();
        let mut candidates: Vec<(u64, PathBuf)> = self
            .documents
            .values()
            .filter(|entry| !open.contains(&entry.path) && entry.bookmarks.is_empty())
            .map(|entry| (entry.last_opened, entry.path.clone()))
            .collect();
        candidates.sort();
        let excess = self.documents.len() - MAX_DOCUMENTS;
        for (_, path) in candidates.into_iter().take(excess) {
            self.documents.remove(&path);
        }
    }
}

/// The hue for a flag added to `bookmarks`: the first of [`PALETTE`] that
/// the fewest of them have.
fn next_hue(bookmarks: &[Bookmark]) -> u16 {
    PALETTE
        .iter()
        .map(|&(_, hue)| (bookmarks.iter().filter(|bookmark| bookmark.hue == hue).count(), hue))
        .min_by_key(|&(used, _)| used)
        .map_or(0, |(_, hue)| hue)
}

/// Where to move an unreadable settings `file`: `<file>.bak`, or the first of
/// `<file>.1.bak`, `<file>.2.bak` and so on that is free, so a second
/// unreadable file never replaces the backup of the first.
fn backup_path(file: &Path) -> PathBuf {
    let first = file.with_extension("json.bak");
    if !first.exists() {
        return first;
    }
    (1..)
        .map(|number| file.with_extension(format!("json.{number}.bak")))
        .find(|path| !path.exists())
        .expect("ran out of backup names")
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{MAX_DOCUMENTS, PALETTE, Session, Store};
    use crate::{FitMode, Spread, ViewSettings};

    /// A fresh, empty directory for one test's settings file, removed again
    /// when the test is done with it.
    struct Scratch(PathBuf);

    impl Scratch {
        fn join(&self, path: &str) -> PathBuf {
            self.0.join(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> Scratch {
        let directory =
            std::env::temp_dir().join(format!("melkkipdf-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        Scratch(directory)
    }

    fn custom() -> ViewSettings {
        ViewSettings {
            continuous: false,
            spread: Spread::Even,
            fit: FitMode::Free,
            zoom: 1.5,
            page: 7,
        }
    }

    #[test]
    fn saved_settings_load_back() {
        let directory = scratch("round-trip");
        let file = directory.join("documents.json");
        let mut store = Store::load(file.clone());
        store.update(Path::new("/docs/a.pdf"), custom());
        store.set_session(Session { tabs: vec!["/docs/a.pdf".into()], active: Some(0) });
        store.save().unwrap();

        let loaded = Store::load(file);
        assert_eq!(loaded.document(Path::new("/docs/a.pdf")), Some(custom()));
        assert_eq!(loaded.document(Path::new("/docs/b.pdf")), None);
        assert_eq!(loaded.session(), Session { tabs: vec!["/docs/a.pdf".into()], active: Some(0) });
    }

    #[test]
    fn flags_go_round_the_palette_and_take_a_freed_colour_again() {
        let mut store = Store::in_memory();
        let path = Path::new("/docs/a.pdf");
        let hues = |store: &Store| -> Vec<u16> {
            store.bookmarks(path).iter().map(|bookmark| bookmark.hue).collect()
        };
        for page in 0..PALETTE.len() + 1 {
            store.toggle_bookmark(path, page);
        }
        let mut expected: Vec<u16> = PALETTE.iter().map(|&(_, hue)| hue).collect();
        expected.push(PALETTE[0].1);
        assert_eq!(hues(&store), expected);

        // Taking away a flag frees its colour for the next flag added.
        store.remove_bookmark(path, 3);
        store.toggle_bookmark(path, 20);
        assert_eq!(hues(&store).last(), Some(&PALETTE[3].1));

        // A colour the reader picks sticks, and only that flag changes.
        let bookmarks = store.set_bookmark_hue(path, 20, PALETTE[6].1);
        assert_eq!(bookmarks.last().map(|bookmark| bookmark.hue), Some(PALETTE[6].1));
        assert_eq!(hues(&store)[0], PALETTE[0].1);
        assert!(store.set_bookmark_hue(path, 999, 0).len() == bookmarks.len());
    }

    #[test]
    fn bookmarks_are_kept_by_page() {
        let mut store = Store::in_memory();
        let path = Path::new("/docs/a.pdf");
        store.toggle_bookmark(path, 7);
        store.toggle_bookmark(path, 2);
        store.toggle_bookmark(path, 12);
        let pages = |store: &Store| -> Vec<usize> {
            store.bookmarks(path).iter().map(|bookmark| bookmark.page).collect()
        };
        assert_eq!(pages(&store), vec![2, 7, 12]);

        // Flagging a flagged page takes the flag away, and only that one.
        store.toggle_bookmark(path, 7);
        assert_eq!(pages(&store), vec![2, 12]);
        store.remove_bookmark(path, 12);
        store.remove_bookmark(path, 12);
        assert_eq!(pages(&store), vec![2]);
        assert_eq!(store.bookmarks(Path::new("/docs/b.pdf")), Vec::new());
    }

    #[test]
    fn saved_bookmarks_load_back() {
        let directory = scratch("bookmarks");
        let file = directory.join("documents.json");
        let mut store = Store::load(file.clone());
        let bookmarks = store.toggle_bookmark(Path::new("/docs/a.pdf"), 3);
        store.save().unwrap();

        let loaded = Store::load(file);
        assert_eq!(loaded.bookmarks(Path::new("/docs/a.pdf")), bookmarks);
    }

    #[test]
    fn a_file_from_before_bookmarks_still_loads() {
        let directory = scratch("v1");
        let file = directory.join("documents.json");
        let older = r#"{
            "version": 1,
            "documents": [{
                "path": "/docs/a.pdf",
                "last_opened": 1,
                "settings": {"continuous": false, "spread": "even", "fit": "free", "zoom": 1.5, "page": 7}
            }],
            "session": {"tabs": ["/docs/a.pdf"], "active": 0}
        }"#;
        std::fs::write(&file, older).unwrap();

        let store = Store::load(file);
        assert_eq!(store.document(Path::new("/docs/a.pdf")), Some(custom()));
        assert_eq!(store.bookmarks(Path::new("/docs/a.pdf")), Vec::new());
        assert!(!directory.join("documents.json.bak").exists());
    }

    #[test]
    fn a_missing_file_is_an_empty_store() {
        let directory = scratch("missing");
        let store = Store::load(directory.join("documents.json"));
        assert_eq!(store.session(), Session::default());
    }

    #[test]
    fn an_unreadable_file_is_moved_aside() {
        let directory = scratch("corrupt");
        let file = directory.join("documents.json");
        std::fs::write(&file, "{ not json").unwrap();

        let store = Store::load(file.clone());
        assert_eq!(store.session(), Session::default());
        assert!(!file.exists(), "the unreadable file should not stay in place");
        assert_eq!(
            std::fs::read_to_string(directory.join("documents.json.bak")).unwrap(),
            "{ not json"
        );
    }

    #[test]
    fn a_file_from_a_newer_version_is_left_alone() {
        let directory = scratch("version");
        let file = directory.join("documents.json");
        let newer = r#"{"version": 99, "documents": {"a new": "layout"}}"#;
        std::fs::write(&file, newer).unwrap();

        let mut store = Store::load(file.clone());
        store.update(Path::new("/docs/a.pdf"), custom());
        store.save().unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), newer);
        assert!(!directory.join("documents.json.bak").exists());
    }

    #[test]
    fn a_second_unreadable_file_keeps_the_first_backup() {
        let directory = scratch("backups");
        let file = directory.join("documents.json");
        std::fs::write(&file, "first").unwrap();
        Store::load(file.clone());
        std::fs::write(&file, "second").unwrap();
        Store::load(file.clone());

        let read = |name: &str| std::fs::read_to_string(directory.join(name)).unwrap();
        assert_eq!(read("documents.json.bak"), "first");
        assert_eq!(read("documents.json.1.bak"), "second");
    }

    #[cfg(unix)]
    #[test]
    fn the_active_tab_follows_it_past_an_unsaveable_path() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let directory = scratch("active");
        let file = directory.join("documents.json");
        let unsaveable = PathBuf::from(OsStr::from_bytes(b"/docs/\xff.pdf"));
        let mut store = Store::load(file.clone());
        store
            .set_session(Session { tabs: vec![unsaveable, "/docs/a.pdf".into()], active: Some(1) });
        store.save().unwrap();

        let loaded = Store::load(file);
        assert_eq!(loaded.session(), Session { tabs: vec!["/docs/a.pdf".into()], active: Some(0) });
    }

    #[test]
    fn saving_without_changes_does_not_write() {
        let directory = scratch("unchanged");
        let file = directory.join("documents.json");
        let mut store = Store::load(file.clone());
        store.update(Path::new("/docs/a.pdf"), custom());
        store.save().unwrap();
        std::fs::remove_file(&file).unwrap();

        store.update(Path::new("/docs/a.pdf"), custom());
        store.save().unwrap();
        assert!(!file.exists(), "an unchanged store was written again");
    }

    #[test]
    fn the_least_recently_opened_are_forgotten_first() {
        let mut store = Store::in_memory();
        for index in 0..MAX_DOCUMENTS + 2 {
            let path = PathBuf::from(format!("/docs/{index}.pdf"));
            store.update(&path, custom());
            store.documents.get_mut(&path).unwrap().last_opened = index as u64;
        }
        // The oldest document is open in a tab and the next oldest has a
        // bookmark, so they survive in place of the two after them.
        store.set_session(Session { tabs: vec!["/docs/0.pdf".into()], active: Some(0) });
        store.toggle_bookmark(Path::new("/docs/1.pdf"), 0);
        store.save().unwrap();

        assert_eq!(store.documents.len(), MAX_DOCUMENTS);
        assert!(store.document(Path::new("/docs/0.pdf")).is_some());
        assert!(store.document(Path::new("/docs/1.pdf")).is_some());
        assert!(store.document(Path::new("/docs/2.pdf")).is_none());
        assert!(store.document(Path::new("/docs/3.pdf")).is_none());
        assert!(store.document(Path::new("/docs/4.pdf")).is_some());
    }
}
