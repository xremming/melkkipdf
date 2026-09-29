//! Text search: an index of every page's text, built in the background as a
//! document opens, and the search over it.
//!
//! The index holds each page's text with every character's position, so a
//! search is a scan over text already in memory, fast enough to run on every
//! keystroke, and each hit can be outlined on its page without asking MuPDF
//! again.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use mupdf::text_page::TextBlockType;
use mupdf::{Document, TextPageFlags};
use slint::Weak;

use crate::MainWindow;

/// Marks a byte of the index's text that no character on the page produced:
/// the space put between two lines.
const BETWEEN_LINES: u32 = u32::MAX;

/// Characters of context shown before and after a hit in the results list.
const CONTEXT_BEFORE: usize = 24;
const CONTEXT_AFTER: usize = 60;

/// How often the indexer hands over the pages it has read, so the first
/// searches see pages as they come rather than all of them at the end.
const INDEX_BATCH: Duration = Duration::from_millis(100);

/// One page's text in reading order, with where each character sits.
#[derive(Clone, Debug, Default)]
pub struct PageText {
    /// The text with every run of whitespace, line breaks included, as one
    /// space, so a phrase matches however the page wraps it.
    text: String,
    /// `text` with letters folded to lower case for matching. A letter whose
    /// lower case takes a different number of bytes is left as it is, so the
    /// two strings keep the same byte layout and a hit in one is the same
    /// range in the other.
    folded: String,
    /// For each byte of `text`, the glyph it came from, or [`BETWEEN_LINES`].
    glyph_of_byte: Vec<u32>,
    /// Each glyph's left and right edge in points from the page's left, and
    /// the line it is on.
    glyphs: Vec<Glyph>,
    /// Each line's top and bottom in points from the page's top.
    lines: Vec<(f32, f32)>,
}

#[derive(Clone, Copy, Debug)]
struct Glyph {
    x0: f32,
    x1: f32,
    line: u32,
}

/// A rectangle on a page in points from its top-left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// The text around a hit, for the results list.
#[derive(Clone, Debug, PartialEq)]
pub struct Snippet {
    pub before: String,
    pub found: String,
    pub after: String,
}

/// Builds a [`PageText`] one line and character at a time, in reading order.
#[derive(Default)]
pub struct PageTextBuilder {
    page: PageText,
}

impl PageTextBuilder {
    /// Starts a line spanning `top` to `bottom` points from the page's top.
    pub fn start_line(&mut self, top: f32, bottom: f32) {
        if !self.page.text.is_empty() && !self.page.text.ends_with(' ') {
            self.push_text(' ', ' ', BETWEEN_LINES);
        }
        self.page.lines.push((top, bottom));
    }

    /// Adds a character spanning `x0` to `x1` points from the page's left.
    pub fn push(&mut self, character: char, x0: f32, x1: f32) {
        let line = self.page.lines.len().saturating_sub(1) as u32;
        let glyph = self.page.glyphs.len() as u32;
        self.page.glyphs.push(Glyph { x0, x1, line });
        if character.is_whitespace() {
            if !self.page.text.is_empty() && !self.page.text.ends_with(' ') {
                self.push_text(' ', ' ', glyph);
            }
        } else {
            self.push_text(character, fold(character), glyph);
        }
    }

    fn push_text(&mut self, character: char, folded: char, glyph: u32) {
        self.page.text.push(character);
        self.page.folded.push(folded);
        for _ in 0..character.len_utf8() {
            self.page.glyph_of_byte.push(glyph);
        }
    }

    pub fn finish(mut self) -> PageText {
        // A trailing space from the last line's end matches nothing useful.
        if self.page.text.ends_with(' ') {
            self.page.text.pop();
            self.page.folded.pop();
            self.page.glyph_of_byte.pop();
        }
        self.page
    }
}

/// `character` in lower case, when that takes the same number of bytes (see
/// [`PageText::folded`]).
fn fold(character: char) -> char {
    let mut lower = character.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(folded), None) if folded.len_utf8() == character.len_utf8() => folded,
        _ => character,
    }
}

/// `query` prepared for matching the way the index's text is: trimmed, each
/// run of whitespace as one space, and folded. `None` when nothing is left.
fn normalize(query: &str) -> Option<String> {
    let words: Vec<String> =
        query.split_whitespace().map(|word| word.chars().map(fold).collect()).collect();
    (!words.is_empty()).then(|| words.join(" "))
}

impl PageText {
    /// Whether the page has any text at all. A scanned page has none.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The areas covering the text from byte `start` to `end`, one per line.
    pub fn areas(&self, start: usize, end: usize) -> Vec<Area> {
        let mut areas: Vec<(u32, f32, f32)> = Vec::new();
        for &glyph in &self.glyph_of_byte[start..end] {
            let Some(glyph) = self.glyphs.get(glyph as usize) else {
                continue;
            };
            match areas.last_mut() {
                Some((line, x0, x1)) if *line == glyph.line => {
                    *x0 = x0.min(glyph.x0);
                    *x1 = x1.max(glyph.x1);
                }
                _ => areas.push((glyph.line, glyph.x0, glyph.x1)),
            }
        }
        areas
            .into_iter()
            .map(|(line, x0, x1)| {
                let (top, bottom) = self.lines[line as usize];
                Area { x: x0, y: top, width: x1 - x0, height: bottom - top }
            })
            .collect()
    }

    /// The hit from byte `start` to `end` with some of the text around it.
    pub fn snippet(&self, start: usize, end: usize) -> Snippet {
        let before = &self.text[..start];
        let skip = before.chars().count().saturating_sub(CONTEXT_BEFORE);
        let mut before: String = before.chars().skip(skip).collect();
        if skip > 0 {
            before.insert(0, '…');
        }
        let after = &self.text[end..];
        let mut after_text: String = after.chars().take(CONTEXT_AFTER).collect();
        if after_text.len() < after.len() {
            after_text.push('…');
        }
        Snippet { before, found: self.text[start..end].to_string(), after: after_text }
    }
}

/// One occurrence of the query: its page and its byte range in that page's
/// text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub page: usize,
    pub start: usize,
    pub end: usize,
}

/// What a search found: the first hits in document order, at most the limit
/// it was given, and how many there are in all.
#[derive(Debug, Default)]
pub struct Found {
    pub hits: Vec<Hit>,
    pub total: usize,
}

/// Finds `query` in `pages`, ignoring case and how whitespace and lines
/// break. Keeps at most `limit` hits but counts them all.
pub fn search(pages: &[PageText], query: &str, limit: usize) -> Found {
    let Some(needle) = normalize(query) else {
        return Found::default();
    };
    let mut found = Found::default();
    for (page, text) in pages.iter().enumerate() {
        for (start, matched) in text.folded.match_indices(needle.as_str()) {
            found.total += 1;
            if found.hits.len() < limit {
                found.hits.push(Hit { page, start, end: start + matched.len() });
            }
        }
    }
    found
}

/// Reads one page's text and where each character sits.
fn read_page(document: &Document, index: i32) -> Result<PageText, mupdf::Error> {
    let page = document.load_page(index)?;
    let bounds = page.bounds()?;
    let text_page = page.to_text_page(TextPageFlags::empty())?;
    let mut builder = PageTextBuilder::default();
    for block in text_page.blocks() {
        if block.r#type() != TextBlockType::Text {
            continue;
        }
        for line in block.lines() {
            let line_bounds = line.bounds();
            builder.start_line(line_bounds.y0 - bounds.y0, line_bounds.y1 - bounds.y0);
            for character in line.chars() {
                let Some(text) = character.char() else {
                    continue;
                };
                let quad = character.quad();
                let xs = [quad.ul.x, quad.ur.x, quad.ll.x, quad.lr.x];
                let x0 = xs.iter().copied().fold(f32::INFINITY, f32::min) - bounds.x0;
                let x1 = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max) - bounds.x0;
                builder.push(text, x0, x1);
            }
        }
    }
    Ok(builder.finish())
}

/// Pages a document's indexer has read: `pages` are the pages from
/// `first_page` on.
pub struct Indexed {
    pub doc: i32,
    pub first_page: usize,
    pub pages: Vec<PageText>,
}

/// Keeps a document's indexer running. Dropping it, as closing the tab does,
/// stops the indexer at the next page.
pub struct IndexHandle {
    stop: Arc<AtomicBool>,
}

impl Drop for IndexHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Starts reading the text of every page of the document at `path` on a
/// thread of its own, sending the pages to `sender` in batches and calling
/// the window's `text-indexed` callback after each, tagged with `doc`. A page
/// whose text cannot be read counts as having none.
pub fn spawn_indexer(
    path: String,
    doc: i32,
    window: Weak<MainWindow>,
    sender: Sender<Indexed>,
) -> IndexHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    thread::spawn(move || {
        let document = match Document::open(&path) {
            Ok(document) => document,
            Err(err) => {
                eprintln!("Failed to open {path} for searching: {err}.");
                return;
            }
        };
        let count = document.page_count().unwrap_or(0).max(0);
        let mut batch = Vec::new();
        let mut first_page = 0;
        let mut last_sent = Instant::now();
        for index in 0..count {
            if stopped.load(Ordering::Relaxed) {
                return;
            }
            batch.push(read_page(&document, index).unwrap_or_else(|err| {
                eprintln!("Failed to read the text of page {} of {path}: {err}.", index + 1);
                PageText::default()
            }));
            if last_sent.elapsed() >= INDEX_BATCH || index + 1 == count {
                let pages = std::mem::take(&mut batch);
                let sent = pages.len();
                if sender.send(Indexed { doc, first_page, pages }).is_err() {
                    return;
                }
                let _ = window.upgrade_in_event_loop(|window| window.invoke_text_indexed());
                first_page += sent;
                last_sent = Instant::now();
            }
        }
    });
    IndexHandle { stop }
}

#[cfg(test)]
mod tests {
    use super::{Area, Hit, PageText, PageTextBuilder, Snippet, search};

    /// A page with `lines` of text, each 10pt tall and 20pt apart, and each
    /// character 5pt wide.
    fn page(lines: &[&str]) -> PageText {
        let mut builder = PageTextBuilder::default();
        for (index, line) in lines.iter().enumerate() {
            let top = index as f32 * 20.0;
            builder.start_line(top, top + 10.0);
            for (column, character) in line.chars().enumerate() {
                let x = column as f32 * 5.0;
                builder.push(character, x, x + 5.0);
            }
        }
        builder.finish()
    }

    #[test]
    fn finds_every_occurrence_in_document_order() {
        let pages = [page(&["the cat sat"]), page(&["no match here"]), page(&["cat and cat"])];
        let found = search(&pages, "cat", 100);
        assert_eq!(found.total, 3);
        let places: Vec<(usize, usize)> =
            found.hits.iter().map(|hit| (hit.page, hit.start)).collect();
        assert_eq!(places, [(0, 4), (2, 0), (2, 8)]);
    }

    #[test]
    fn ignores_case_and_how_whitespace_breaks() {
        let pages = [page(&["The Orient", "EXPRESS  departs"])];
        let found = search(&pages, "  orient   express ", 100);
        assert_eq!(found.total, 1);
        let hit = found.hits[0];
        assert_eq!(&pages[0].text[hit.start..hit.end], "Orient EXPRESS");
    }

    #[test]
    fn a_blank_query_finds_nothing() {
        let pages = [page(&["anything"])];
        assert_eq!(search(&pages, "   ", 100).total, 0);
    }

    #[test]
    fn keeps_only_the_first_hits_but_counts_them_all() {
        let pages = [page(&["a a a a a"])];
        let found = search(&pages, "a", 2);
        assert_eq!(found.hits.len(), 2);
        assert_eq!(found.total, 5);
    }

    #[test]
    fn a_hit_across_lines_has_an_area_per_line() {
        let pages = [page(&["see the Orient", "Express leave"])];
        let hit: Hit = search(&pages, "orient express", 1).hits[0];
        let areas = pages[0].areas(hit.start, hit.end);
        assert_eq!(
            areas,
            [
                Area { x: 40.0, y: 0.0, width: 30.0, height: 10.0 },
                Area { x: 0.0, y: 20.0, width: 35.0, height: 10.0 },
            ]
        );
    }

    #[test]
    fn letters_whose_case_changes_length_still_line_up() {
        // Lower-case 'ẞ' is 'ß', which takes fewer bytes, so it is left
        // alone and the text after it still maps to the right characters.
        let pages = [page(&["GROẞE Bahn"])];
        let hit = search(&pages, "bahn", 1).hits[0];
        assert_eq!(&pages[0].text[hit.start..hit.end], "Bahn");
        assert_eq!(pages[0].areas(hit.start, hit.end)[0].x, 30.0);
    }

    #[test]
    fn a_snippet_shows_the_text_around_the_hit() {
        let text = "Beginning of the page with plenty of words before the word sought, then more.";
        let pages = [page(&[text])];
        let hit = search(&pages, "sought", 1).hits[0];
        let Snippet { before, found, after } = pages[0].snippet(hit.start, hit.end);
        assert_eq!(found, "sought");
        assert!(before.starts_with('…') && before.ends_with("the word "), "{before:?}");
        assert_eq!(after, ", then more.");
    }

    #[test]
    fn a_page_without_text_is_empty() {
        assert!(page(&[]).is_empty());
        assert!(!page(&["x"]).is_empty());
    }
}
