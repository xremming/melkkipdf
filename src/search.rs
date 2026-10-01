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

use icu_casemap::{CaseMapper, CaseMapperBorrowed};
use icu_normalizer::DecomposingNormalizerBorrowed;
use icu_properties::props::{DefaultIgnorableCodePoint, GeneralCategory, GeneralCategoryGroup};
use icu_properties::{
    CodePointMapData, CodePointMapDataBorrowed, CodePointSetData, CodePointSetDataBorrowed,
};
use mupdf::text_page::TextBlockType;
use mupdf::{Document, TextPageFlags};
use slint::Weak;

use crate::MainWindow;

/// Marks a byte of the index's text that no character on the page produced:
/// the space put between two lines.
pub(crate) const BETWEEN_LINES: u32 = u32::MAX;

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
    pub(crate) text: String,
    /// `text` as search matches it: each character through [`fold_into`],
    /// with runs of whitespace again as one space, since punctuation that
    /// folds away can leave two in a row, and none where a line ends in a
    /// soft hyphen.
    folded: String,
    /// For each byte of `folded`, where the character of `text` it came from
    /// starts. Folding changes lengths, as 'ß' to "ss" and 'é' to "e", so a
    /// hit in `folded` needs this to find its text.
    origin: Vec<u32>,
    /// For each byte of `text`, the glyph it came from, or [`BETWEEN_LINES`].
    pub(crate) glyph_of_byte: Vec<u32>,
    /// For each glyph, the byte of `text` its character starts at, or where
    /// the next character will start for a glyph that added none, as a space
    /// after a space. Selection finds glyphs by where they are and needs
    /// their text, the other way round from [`Self::areas`].
    pub(crate) byte_of_glyph: Vec<u32>,
    /// Each glyph's left and right edge in points from the page's left, and
    /// the line it is on. Glyphs come in reading order, so a line's glyphs
    /// are together and lines never go back.
    pub(crate) glyphs: Vec<Glyph>,
    /// Each line's top and bottom in points from the page's top.
    pub(crate) lines: Vec<(f32, f32)>,
    /// The images drawn on the page, in drawing order.
    pub images: Vec<ImageSpot>,
}

/// An image embedded in the document and drawn on a page: where it is drawn
/// and how many pixels it has of its own, which is what copying it gives.
/// `ordinal` is its place among the page's images, by which the render
/// worker finds it again.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageSpot {
    pub ordinal: usize,
    pub bounds: Area,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Glyph {
    pub(crate) x0: f32,
    pub(crate) x1: f32,
    pub(crate) line: u32,
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
    /// Whether a line ended in a soft hyphen and nothing searchable has come
    /// since, so the word it split goes on without a space in `folded`.
    joining: bool,
}

impl PageTextBuilder {
    /// Starts a line spanning `top` to `bottom` points from the page's top.
    pub fn start_line(&mut self, top: f32, bottom: f32) {
        // A word hyphenated with a soft hyphen at the end of the line goes on
        // on the next, so only `text` gets a space there, for the snippet,
        // and the word is still found whole.
        if self.page.text.ends_with(SOFT_HYPHEN) {
            self.joining = true;
        }
        self.push_space(BETWEEN_LINES);
        self.page.lines.push((top, bottom));
    }

    /// Adds a character spanning `x0` to `x1` points from the page's left.
    pub fn push(&mut self, character: char, x0: f32, x1: f32) {
        let line = self.page.lines.len().saturating_sub(1) as u32;
        let glyph = self.page.glyphs.len() as u32;
        self.page.glyphs.push(Glyph { x0, x1, line });
        self.page.byte_of_glyph.push(self.page.text.len() as u32);
        if character.is_whitespace() {
            self.push_space(glyph);
        } else {
            let start = self.page.text.len() as u32;
            self.push_text(character, glyph);
            let folded_from = self.page.folded.len();
            fold_into(character, &mut self.page.folded);
            let added = self.page.folded.len() - folded_from;
            if added > 0 {
                self.joining = false;
            }
            self.page.origin.extend(std::iter::repeat_n(start, added));
        }
    }

    /// Ends a word with a space in `text` and in `folded`, each unless it is
    /// empty or already ends with one, and `folded` also not while joining a
    /// hyphenated word.
    fn push_space(&mut self, glyph: u32) {
        if !self.page.text.is_empty() && !self.page.text.ends_with(' ') {
            self.push_text(' ', glyph);
        }
        let folded = &mut self.page.folded;
        if !self.joining && !folded.is_empty() && !folded.ends_with(' ') {
            folded.push(' ');
            // `text` is not empty either, so it now ends with the space this
            // one stands for, whether or not that space was just added.
            self.page.origin.push(self.page.text.len() as u32 - 1);
        }
    }

    fn push_text(&mut self, character: char, glyph: u32) {
        self.page.text.push(character);
        for _ in 0..character.len_utf8() {
            self.page.glyph_of_byte.push(glyph);
        }
    }

    pub fn finish(mut self) -> PageText {
        // A trailing space from the last line's end matches nothing useful.
        if self.page.text.ends_with(' ') {
            self.page.text.pop();
            self.page.glyph_of_byte.pop();
        }
        if self.page.folded.ends_with(' ') {
            self.page.folded.pop();
            self.page.origin.pop();
        }
        let page = self.page;
        // Searching slices `text` at these offsets and tells characters apart
        // by them, so a mistake here would panic or lose hits far from it.
        debug_assert_eq!(page.glyph_of_byte.len(), page.text.len());
        debug_assert_eq!(page.byte_of_glyph.len(), page.glyphs.len());
        debug_assert!(page.byte_of_glyph.windows(2).all(|pair| pair[0] <= pair[1]));
        debug_assert!(page.byte_of_glyph.iter().all(|&start| start as usize <= page.text.len()));
        debug_assert_eq!(page.origin.len(), page.folded.len());
        debug_assert!(page.origin.windows(2).all(|pair| pair[0] <= pair[1]));
        debug_assert!(page.origin.iter().all(|&start| {
            (start as usize) < page.text.len() && page.text.is_char_boundary(start as usize)
        }));
        page
    }
}

pub(crate) const SOFT_HYPHEN: char = '\u{AD}';

const NFKD: DecomposingNormalizerBorrowed<'static> = DecomposingNormalizerBorrowed::new_nfkd();
const CASE: CaseMapperBorrowed<'static> = CaseMapper::new();
const CATEGORY: CodePointMapDataBorrowed<'static, GeneralCategory> =
    CodePointMapData::<GeneralCategory>::new();
const IGNORABLE: CodePointSetDataBorrowed<'static> =
    CodePointSetData::new::<DefaultIgnorableCodePoint>();

/// Punctuation that search keeps, because it is part of what readers search
/// for: "C#", "AT&T", "name@example.com", "50%", "§ 12". Without it such a
/// query would lose the punctuation and match far more than was meant, "C#"
/// every 'c' on the page.
const PUNCTUATION_ALLOWLIST: [char; 5] = ['#', '&', '@', '%', '§'];

/// Appends `character` to `out` as search matches it: close to Unicode's
/// compatibility caseless match (NFKD, full case folding, then NFKD again),
/// with diacritics, invisible characters, and punctuation outside
/// [`PUNCTUATION_ALLOWLIST`] removed. Readers type plain letters, while PDFs
/// have ligatures, full-width forms, typographic quotes, and accents that the
/// reader may not know or bother to type. The caller collapses the whitespace
/// it is given, and whitespace that decomposition produces, as from '¨', is
/// dropped.
fn fold_into(character: char, out: &mut String) {
    // Almost all PDF text is ASCII, and there a letter or digit needs no more
    // than lowering.
    if character.is_ascii_alphanumeric() {
        out.push(character.to_ascii_lowercase());
        return;
    }
    let mut buffer = [0; 4];
    let decomposed = NFKD.normalize(character.encode_utf8(&mut buffer));
    let folded = CASE.fold_string(&decomposed);
    for part in NFKD.normalize(&folded).chars() {
        let ignored = part.is_whitespace()
            || is_diacritic(part)
            || IGNORABLE.contains(part)
            || (GeneralCategoryGroup::Punctuation.contains(CATEGORY.get(part))
                && !PUNCTUATION_ALLOWLIST.contains(&part));
        if !ignored {
            out.push(part);
        }
    }
}

/// Whether `character` is a mark that search ignores: one of the combining
/// marks that Latin, Greek, and Cyrillic letters or symbols take, or a Hebrew
/// or Arabic point, cantillation mark, or Quranic sign, which ordinary text
/// mostly leaves out. The marks of scripts such as Devanagari or Thai are
/// kept, since there they are written as part of the word and change it.
fn is_diacritic(character: char) -> bool {
    // The gaps in the Hebrew range are punctuation, which a hit should not
    // take in at its end the way it does a mark.
    matches!(
        character,
        '\u{0300}'..='\u{036F}'
            | '\u{0591}'..='\u{05BD}'
            | '\u{05BF}'
            | '\u{05C1}'..='\u{05C2}'
            | '\u{05C4}'..='\u{05C5}'
            | '\u{05C7}'
            | '\u{0610}'..='\u{061A}'
            | '\u{064B}'..='\u{065F}'
            | '\u{0670}'
            | '\u{1AB0}'..='\u{1AFF}'
            | '\u{1DC0}'..='\u{1DFF}'
            | '\u{20D0}'..='\u{20FF}'
            | '\u{FE20}'..='\u{FE2F}'
    )
}

/// `query` prepared for matching the way the index's text is: folded, each
/// run of whitespace as one space, and trimmed. `None` when nothing is left.
fn normalize(query: &str) -> Option<String> {
    let mut needle = String::new();
    for character in query.chars() {
        if !character.is_whitespace() {
            fold_into(character, &mut needle);
        } else if !needle.is_empty() && !needle.ends_with(' ') {
            needle.push(' ');
        }
    }
    if needle.ends_with(' ') {
        needle.pop();
    }
    (!needle.is_empty()).then_some(needle)
}

impl PageText {
    /// Whether the page has any text at all. A scanned page has none.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Whether byte `index` of `folded` is where a character of `text` begins
    /// its folding, or the end of `folded`.
    fn starts_character(&self, index: usize) -> bool {
        index == 0 || index == self.folded.len() || self.origin[index] != self.origin[index - 1]
    }

    /// The range of `text` that bytes `start` to `end` of `folded` came from,
    /// taking in any diacritics that follow as separate characters, since
    /// they belong to the letter before them.
    fn source(&self, start: usize, end: usize) -> (usize, usize) {
        let last = self.origin[end - 1] as usize;
        let mut end = last;
        for character in self.text[last..].chars() {
            if end > last && !is_diacritic(character) {
                break;
            }
            end += character.len_utf8();
        }
        (self.origin[start] as usize, end)
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

/// Finds `query` in `pages`, ignoring case, diacritics, most punctuation, and
/// how whitespace and lines break (see [`fold_into`]). Keeps at most `limit` hits
/// but counts them all.
pub fn search(pages: &[PageText], query: &str, limit: usize) -> Found {
    let Some(needle) = normalize(query) else {
        return Found::default();
    };
    let mut found = Found::default();
    for (page, text) in pages.iter().enumerate() {
        let mut from = 0;
        while let Some(offset) = text.folded[from..].find(needle.as_str()) {
            let start = from + offset;
            let end = start + needle.len();
            if !text.starts_character(start) || !text.starts_character(end) {
                // Part of one character's folding, as "s" is of 'ß' folded to
                // "ss", is not a hit. A hit may still overlap this match, so
                // the search goes on one character later rather than after it.
                from = start + text.folded[start..].chars().next().map_or(1, char::len_utf8);
                continue;
            }
            found.total += 1;
            if found.hits.len() < limit {
                let (start, end) = text.source(start, end);
                found.hits.push(Hit { page, start, end });
            }
            from = end;
        }
    }
    found
}

/// Reads one page's text and where each character sits, and the images
/// drawn on it.
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
    let mut text = builder.finish();
    text.images = crate::images::spots_on(&page)?;
    Ok(text)
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
    use super::{Area, Hit, PageText, PageTextBuilder, Snippet, normalize, search};

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

    /// The text of every hit of `query` in `lines`.
    fn found(lines: &[&str], query: &str) -> Vec<String> {
        let pages = [page(lines)];
        let hits = search(&pages, query, 100).hits;
        hits.iter().map(|hit| pages[0].text[hit.start..hit.end].to_string()).collect()
    }

    #[test]
    fn letters_whose_folding_changes_length_still_line_up() {
        // 'ẞ' folds to "ss", which takes fewer bytes, so the text after it has
        // to be mapped back to find its characters.
        let pages = [page(&["GROẞE Bahn"])];
        let hit = search(&pages, "bahn", 1).hits[0];
        assert_eq!(&pages[0].text[hit.start..hit.end], "Bahn");
        assert_eq!(pages[0].areas(hit.start, hit.end)[0].x, 30.0);
        assert_eq!(found(&["GROẞE Bahn"], "grosse"), ["GROẞE"]);
    }

    #[test]
    fn folds_case_fully() {
        assert_eq!(found(&["Straße"], "STRASSE"), ["Straße"]);
        assert_eq!(found(&["ΟΔΟΣ"], "οδος"), ["ΟΔΟΣ"]);
        // Greek writes sigma as 'ς' at the end of a word and 'σ' elsewhere.
        assert_eq!(found(&["οδός"], "ΟΔΟΣ"), ["οδός"]);
    }

    #[test]
    fn part_of_one_letters_folding_is_not_a_hit() {
        assert_eq!(found(&["Straße"], "s"), ["S"]);
        assert_eq!(found(&["Straße"], "se"), Vec::<String>::new());
        assert_eq!(found(&["Straße"], "sse"), ["ße"]);
    }

    #[test]
    fn a_match_inside_a_letters_folding_does_not_hide_the_next() {
        // "sß" folds to "sss". The first "ss" in it ends inside 'ß', but the
        // one after it is all of 'ß'.
        assert_eq!(found(&["sß"], "ss"), ["ß"]);
        // Matches turned down this way are not counted, and the limit leaves
        // room for the hits after them.
        let pages = [page(&["ßs s"])];
        let found = search(&pages, "s", 1);
        assert_eq!(found.total, 2);
        assert_eq!(found.hits, [Hit { page: 0, start: 2, end: 3 }]);
    }

    #[test]
    fn ignores_diacritics_however_they_are_written() {
        let composed = "Le Café";
        let decomposed = "Le Cafe\u{301}";
        assert_eq!(found(&[composed], "cafe"), ["Café"]);
        assert_eq!(found(&[composed], "CAFÉ"), ["Café"]);
        // The accent is a character of its own here, and the hit takes it in
        // with the letter it sits on.
        assert_eq!(found(&[decomposed], "cafe"), ["Cafe\u{301}"]);
        assert_eq!(found(&[decomposed], "café"), ["Cafe\u{301}"]);
        assert_eq!(found(&["Άρης"], "αρης"), ["Άρης"]);
    }

    #[test]
    fn ignores_hebrew_and_arabic_points() {
        assert_eq!(found(&["كَتَبَ"], "كتب"), ["كَتَبَ"]);
        assert_eq!(found(&["שָׁלוֹם"], "שלום"), ["שָׁלוֹם"]);
        // Sof pasuq ends a verse. It is punctuation, not a point, so a hit
        // does not take it in.
        assert_eq!(found(&["שלום׃"], "שלום"), ["שלום"]);
    }

    #[test]
    fn keeps_marks_that_are_vowels() {
        // The vowel sign 'ु' is a nonspacing mark like an accent, but here it
        // makes "कुम" a different word from "कम".
        assert_eq!(found(&["कम"], "कुम"), Vec::<String>::new());
        assert_eq!(found(&["कुम"], "कुम"), ["कुम"]);
    }

    #[test]
    fn matches_compatibility_forms() {
        assert_eq!(found(&["eﬃcient"], "efficient"), ["eﬃcient"]);
        assert_eq!(found(&["ＡＢＣ ２０２６"], "abc 2026"), ["ＡＢＣ ２０２６"]);
    }

    #[test]
    fn ignores_punctuation() {
        assert_eq!(found(&["don’t stop"], "dont"), ["don’t"]);
        assert_eq!(found(&["don’t stop"], "don't stop"), ["don’t stop"]);
        assert_eq!(found(&["“Quoted,” he said."], "quoted he"), ["Quoted,” he"]);
        // A dash between spaces leaves one space, not two.
        assert_eq!(found(&["red – green"], "red green"), ["red – green"]);
        assert_eq!(search(&[page(&["a.b"])], "...", 100).total, 0);
    }

    #[test]
    fn keeps_punctuation_on_the_allowlist() {
        assert_eq!(found(&["Cat in C# code"], "C#"), ["C#"]);
        assert_eq!(found(&["AT&T and ATT"], "at&t"), ["AT&T"]);
        assert_eq!(found(&["name@example.com"], "@example"), ["@example"]);
        assert_eq!(found(&["up 50% from 50"], "50%"), ["50%"]);
        assert_eq!(found(&["see § 12 and 12"], "§ 12"), ["§ 12"]);
        // A full-width form is kept as its plain one.
        assert_eq!(found(&["Ｃ＃"], "c#"), ["Ｃ＃"]);
    }

    #[test]
    fn ignores_soft_hyphens() {
        assert_eq!(found(&["hyphen\u{AD}ation"], "hyphenation"), ["hyphen\u{AD}ation"]);
        assert_eq!(found(&["hyphen\u{AD}", "ation"], "hyphenation"), ["hyphen\u{AD} ation"]);
        // The word goes on past any space or empty line the break leaves.
        let spaced = ["hyphen\u{AD}", " ation"];
        assert_eq!(found(&spaced, "hyphenation"), ["hyphen\u{AD} ation"]);
        assert_eq!(found(&spaced, "ation"), ["ation"]);
        let gapped = ["hyphen\u{AD}", "", "ation"];
        assert_eq!(found(&gapped, "hyphenation"), ["hyphen\u{AD} ation"]);
        assert_eq!(found(&gapped, "ation"), ["ation"]);
    }

    #[test]
    fn a_word_hyphenated_across_lines_has_an_area_per_line() {
        let pages = [page(&["a hyphen\u{AD}", "ation b"])];
        let hit = search(&pages, "hyphenation", 1).hits[0];
        assert_eq!(
            pages[0].areas(hit.start, hit.end),
            [
                Area { x: 10.0, y: 0.0, width: 35.0, height: 10.0 },
                Area { x: 0.0, y: 20.0, width: 25.0, height: 10.0 },
            ]
        );
    }

    #[test]
    fn every_word_on_a_page_can_be_found() {
        // Building each page also checks the index's invariants, in `finish`,
        // since tests run with debug assertions.
        let pages: [&[&str]; 8] = [
            &["hyphen\u{AD}", " ation"],
            &["abc ."],
            &["“ ”"],
            &["ab\u{AD}", "\u{AD}", "cd"],
            &["red – green"],
            &["GROẞE eﬃcient Straße"],
            &["Cafe\u{301}", "next"],
            &["sß ßs", "C# AT&T"],
        ];
        for lines in pages {
            let pages = [page(lines)];
            for word in pages[0].text.split(' ') {
                if normalize(word).is_some() {
                    let total = search(&pages, word, 1).total;
                    assert!(total > 0, "{word:?} not found in {lines:?}");
                }
            }
        }
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
