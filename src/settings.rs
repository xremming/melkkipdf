//! Remembers each document's view settings, and the tabs that were open,
//! between runs.
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
/// never misreads a newer file as its own.
const VERSION: u32 = 1;
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
}

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
    /// unreadable one is moved aside to `<file>.bak`, rather than overwritten by
    /// the next save, and the store starts empty.
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

        let parsed = serde_json::from_slice::<StateFile>(&contents)
            .map_err(|err| err.to_string())
            .and_then(|state| match state.version {
                VERSION => Ok(state),
                other => Err(format!("unknown version {other}")),
            });
        match parsed {
            Ok(state) => {
                store.documents =
                    state.documents.into_iter().map(|entry| (entry.path.clone(), entry)).collect();
                store.session = state.session;
            }
            Err(err) => {
                let backup = file.with_extension("json.bak");
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
        });
        entry.last_opened = now;
        self.dirty = true;
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
        let session = Session {
            tabs: self.session.tabs.iter().filter(|tab| tab.to_str().is_some()).cloned().collect(),
            active: self.session.active,
        };
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
    /// sparing any that are open in a tab.
    fn forget_oldest(&mut self) {
        if self.documents.len() <= MAX_DOCUMENTS {
            return;
        }
        let open: HashSet<&PathBuf> = self.session.tabs.iter().collect();
        let mut candidates: Vec<(u64, PathBuf)> = self
            .documents
            .values()
            .filter(|entry| !open.contains(&entry.path))
            .map(|entry| (entry.last_opened, entry.path.clone()))
            .collect();
        candidates.sort();
        let excess = self.documents.len() - MAX_DOCUMENTS;
        for (_, path) in candidates.into_iter().take(excess) {
            self.documents.remove(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{MAX_DOCUMENTS, Session, Store};
    use crate::{FitMode, Spread, ViewSettings};

    /// A fresh, empty directory for one test's settings file.
    fn scratch(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("melkkipdf-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
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
        let file = scratch("round-trip").join("documents.json");
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
    fn a_missing_file_is_an_empty_store() {
        let store = Store::load(scratch("missing").join("documents.json"));
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
    fn a_file_from_another_version_is_moved_aside() {
        let directory = scratch("version");
        let file = directory.join("documents.json");
        std::fs::write(&file, r#"{"version": 99, "documents": []}"#).unwrap();

        Store::load(file.clone());
        assert!(!file.exists());
        assert!(directory.join("documents.json.bak").exists());
    }

    #[test]
    fn saving_without_changes_does_not_write() {
        let file = scratch("unchanged").join("documents.json");
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
        // The oldest document is open in a tab, so it survives in place of the
        // next oldest.
        store.set_session(Session { tabs: vec!["/docs/0.pdf".into()], active: Some(0) });
        store.save().unwrap();

        assert_eq!(store.documents.len(), MAX_DOCUMENTS);
        assert!(store.document(Path::new("/docs/0.pdf")).is_some());
        assert!(store.document(Path::new("/docs/1.pdf")).is_none());
        assert!(store.document(Path::new("/docs/2.pdf")).is_none());
        assert!(store.document(Path::new("/docs/3.pdf")).is_some());
    }
}
