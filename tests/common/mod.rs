//! Helpers shared by the integration tests that work with real files.

use std::path::{Path, PathBuf};

use mupdf::Size;
use mupdf::pdf::PdfDocument;

/// A fresh, empty directory for one test's files, removed again when the test
/// is done with it.
pub struct Scratch(PathBuf);

impl Scratch {
    /// A directory named after the test binary, `name` and the process, so
    /// tests running at the same time never share one.
    pub fn new(name: &str) -> Self {
        let binary = std::env::current_exe()
            .ok()
            .and_then(|path| path.file_stem().map(|stem| stem.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let directory =
            std::env::temp_dir().join(format!("melkkipdf-{binary}-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        Self(directory)
    }

    pub fn join(&self, path: impl AsRef<Path>) -> PathBuf {
        self.0.join(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Writes a PDF of `pages` blank A4 pages.
pub fn write_pdf(path: &Path, pages: usize) {
    let mut document = PdfDocument::new();
    for _ in 0..pages {
        document.new_page(Size::A4).unwrap();
    }
    document.save(path.to_str().unwrap()).unwrap();
}
