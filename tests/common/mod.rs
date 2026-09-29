//! Helpers shared by the integration tests that work with real files.
//!
//! Each test file compiles its own copy of this module and uses only some of
//! it, so what one file leaves unused is not dead code.
#![allow(dead_code)]

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

/// Writes a PDF of US Letter pages with the given text, one page per entry,
/// each line of it set in Helvetica 12pt, the first line 72pt from the top
/// and each next one 20pt below. The text must not contain parentheses or
/// backslashes.
pub fn write_text_pdf(path: &Path, pages: &[&[&str]]) {
    let mut objects: Vec<String> = Vec::new();
    let font = 3;
    let first_page = 4;
    let kids: Vec<String> =
        (0..pages.len()).map(|index| format!("{} 0 R", first_page + index * 2)).collect();
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
    objects.push(format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len()));
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
    for (index, lines) in pages.iter().enumerate() {
        let contents = first_page + index * 2 + 1;
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 {font} 0 R >> >> /Contents {contents} 0 R >>"
        ));
        let mut stream = String::from("BT /F1 12 Tf 14 TL 72 708 Td");
        for (line, text) in lines.iter().enumerate() {
            if line > 0 {
                stream.push_str(" 0 -20 Td");
            }
            stream.push_str(&format!(" ({text}) Tj"));
        }
        stream.push_str(" ET");
        objects.push(format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()));
    }

    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{object}\nendobj\n", index + 1));
    }
    let xref = pdf.len();
    pdf.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1));
    for offset in offsets {
        pdf.push_str(&format!("{offset:010} 00000 n \n"));
    }
    pdf.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    ));
    std::fs::write(path, pdf).unwrap();
}
