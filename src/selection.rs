//! Text selection: turning points on a page into places in its text, and
//! the text between two such places into what goes on the clipboard.
//!
//! A selection is kept as a range of the search index's text, not as points,
//! so it survives a zoom, a new spread or a switch between the reading modes
//! untouched, and its outline on the page comes from [`PageText::areas`] like
//! a search hit's.

use crate::search::{BETWEEN_LINES, PageText, SOFT_HYPHEN};

/// A place in the document's text: a page and a byte offset into that page's
/// text. Ordered by page, then by offset, so two of them sort into document
/// order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPos {
    pub page: usize,
    pub byte: usize,
}

/// What a selection is made of, from how many times the reader clicked: one
/// click selects by the character, two by the word, three by the line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Unit {
    #[default]
    Char,
    Word,
    Line,
}

impl Unit {
    pub fn from_clicks(clicks: u32) -> Self {
        match clicks {
            0 | 1 => Self::Char,
            2 => Self::Word,
            _ => Self::Line,
        }
    }
}

impl PageText {
    /// The length of the page's text in bytes.
    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// The glyphs of `line`, as a range of indices into `glyphs`.
    fn line_glyphs(&self, line: u32) -> std::ops::Range<usize> {
        let start = self.glyphs.partition_point(|glyph| glyph.line < line);
        let end = start + self.glyphs[start..].partition_point(|glyph| glyph.line == line);
        start..end
    }

    /// The byte of `text` just after glyph `glyph`'s character: the byte the
    /// next glyph on the line starts at, or the end of the line's text. The
    /// space put between two lines belongs to neither.
    fn glyph_end(&self, glyph: usize) -> usize {
        let start = self.byte_of_glyph[glyph] as usize;
        let mut end = self.byte_of_glyph.get(glyph + 1).map_or(self.text.len(), |&b| b as usize);
        while end > start && self.glyph_of_byte[end - 1] == BETWEEN_LINES {
            end -= 1;
        }
        end
    }

    /// The place in the text nearest to the point `x`, `y` points from the
    /// page's top-left corner, or `None` on a page without text. The nearest
    /// line is found first, by how far the point is above or below it and
    /// then by how far to its side, so a point in the gutter between two
    /// columns goes to the column it is level with. On that line the point
    /// falls before or after the nearest glyph, whichever half of it the
    /// point is on, and a point past the line's end is the end of the line.
    pub fn position_at(&self, x: f32, y: f32) -> Option<usize> {
        let mut nearest: Option<(f32, f32, std::ops::Range<usize>)> = None;
        for (line, &(top, bottom)) in self.lines.iter().enumerate() {
            let glyphs = self.line_glyphs(line as u32);
            if glyphs.is_empty() {
                continue;
            }
            let x0 = self.glyphs[glyphs.clone()].iter().map(|g| g.x0).fold(f32::INFINITY, f32::min);
            let x1 =
                self.glyphs[glyphs.clone()].iter().map(|g| g.x1).fold(f32::NEG_INFINITY, f32::max);
            let dy = (top - y).max(y - bottom).max(0.0);
            let dx = (x0 - x).max(x - x1).max(0.0);
            let closer = nearest.as_ref().is_none_or(|&(best_dy, best_dx, _)| {
                dy < best_dy || (dy == best_dy && dx < best_dx)
            });
            if closer {
                nearest = Some((dy, dx, glyphs));
            }
        }
        let (_, _, glyphs) = nearest?;
        let distance = |glyph: usize| {
            let g = &self.glyphs[glyph];
            (g.x0 - x).max(x - g.x1).max(0.0)
        };
        // The first of the nearest glyphs, so a point between two glyphs
        // that touch goes after the first, which the midpoint test below
        // agrees with.
        let glyph = glyphs
            .clone()
            .min_by(|&a, &b| distance(a).total_cmp(&distance(b)).then(a.cmp(&b)))
            .expect("a line with glyphs has a nearest one");
        let g = &self.glyphs[glyph];
        let byte = if x < (g.x0 + g.x1) / 2.0 {
            self.byte_of_glyph[glyph] as usize
        } else {
            self.glyph_end(glyph)
        };
        Some(byte.min(self.text.len()))
    }

    /// The word around `byte`: the run of text without spaces it is in, or
    /// the space itself when it is on one.
    pub fn word_at(&self, byte: usize) -> (usize, usize) {
        let byte = byte.min(self.text.len());
        if self.text[byte..].starts_with(' ') {
            return (byte, byte + 1);
        }
        let start = self.text[..byte].rfind(' ').map_or(0, |space| space + 1);
        let end = self.text[byte..].find(' ').map_or(self.text.len(), |space| byte + space);
        (start, end)
    }

    /// The line around `byte`, as the page lays it out.
    pub fn line_at(&self, byte: usize) -> (usize, usize) {
        let byte = byte.min(self.text.len());
        let is_break = |index: usize| self.glyph_of_byte[index] == BETWEEN_LINES;
        let start = (0..byte).rev().find(|&index| is_break(index)).map_or(0, |index| index + 1);
        let end = (byte..self.text.len()).find(|&index| is_break(index)).unwrap_or(self.text.len());
        (start, end)
    }

    /// The text from byte `start` to `end` as it goes on the clipboard: the
    /// page's lines on lines of their own, and a word hyphenated across two
    /// of them put back together.
    pub fn copied_text(&self, start: usize, end: usize) -> String {
        let mut out = String::with_capacity(end - start);
        for (offset, character) in self.text[start..end].char_indices() {
            if self.glyph_of_byte[start + offset] == BETWEEN_LINES {
                if out.ends_with(SOFT_HYPHEN) {
                    out.pop();
                } else {
                    out.push('\n');
                }
            } else {
                out.push(character);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::Unit;
    use crate::search::{PageText, PageTextBuilder};

    /// A page with `lines` of text, each 10pt tall and 20pt apart from the
    /// top, and each character 5pt wide from the left.
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
    fn a_point_on_a_glyph_falls_on_its_nearer_side() {
        let page = page(&["abc"]);
        assert_eq!(page.position_at(1.0, 5.0), Some(0));
        assert_eq!(page.position_at(4.0, 5.0), Some(1));
        assert_eq!(page.position_at(11.0, 5.0), Some(2));
        assert_eq!(page.position_at(14.0, 5.0), Some(3));
    }

    #[test]
    fn a_point_past_a_line_goes_to_its_ends() {
        let page = page(&["abc", "def"]);
        assert_eq!(page.position_at(-50.0, 5.0), Some(0));
        assert_eq!(page.position_at(500.0, 5.0), Some(3));
        // The end of the first line, not the space between the lines.
        assert_eq!(page.position_at(500.0, 25.0), Some(7));
        assert_eq!(page.position_at(-50.0, 25.0), Some(4));
    }

    #[test]
    fn a_point_between_lines_goes_to_the_nearer_one() {
        let page = page(&["abc", "def"]);
        assert_eq!(page.position_at(0.0, 12.0), Some(0));
        assert_eq!(page.position_at(0.0, 18.0), Some(4));
        assert_eq!(page.position_at(0.0, -100.0), Some(0));
        assert_eq!(page.position_at(0.0, 100.0), Some(4));
    }

    #[test]
    fn a_point_in_a_gutter_goes_to_the_column_it_is_level_with() {
        // Two columns: the left one is one line, the right one two, and the
        // right column comes after the left in reading order.
        let mut builder = PageTextBuilder::default();
        builder.start_line(0.0, 10.0);
        builder.push('a', 0.0, 5.0);
        builder.start_line(0.0, 10.0);
        builder.push('b', 100.0, 105.0);
        builder.start_line(20.0, 30.0);
        builder.push('c', 100.0, 105.0);
        let page = builder.finish();
        // Level with the second right-hand line, where the left column has
        // nothing, even though the left column's line is nearer in all.
        assert_eq!(page.position_at(40.0, 25.0), Some(4));
        // Level with both, the nearer column wins.
        assert_eq!(page.position_at(10.0, 5.0), Some(1));
        assert_eq!(page.position_at(90.0, 5.0), Some(2));
    }

    #[test]
    fn a_page_without_text_has_no_position() {
        assert_eq!(page(&[]).position_at(0.0, 0.0), None);
        assert_eq!(page(&[""]).position_at(0.0, 0.0), None);
    }

    #[test]
    fn words_and_lines_around_a_byte() {
        let page = page(&["the cat", "sat"]);
        assert_eq!(page.word_at(0), (0, 3));
        assert_eq!(page.word_at(5), (4, 7));
        assert_eq!(page.word_at(3), (3, 4));
        assert_eq!(page.word_at(9), (8, 11));
        assert_eq!(page.line_at(5), (0, 7));
        assert_eq!(page.line_at(8), (8, 11));
        assert_eq!(page.line_at(11), (8, 11));
    }

    #[test]
    fn copied_text_keeps_line_breaks_and_joins_hyphenated_words() {
        let page = page(&["the hyphen\u{AD}", "ation of", "words"]);
        assert_eq!(page.copied_text(0, page.len()), "the hyphenation of\nwords");
        assert_eq!(page.copied_text(4, 18), "hyphenation");
    }

    #[test]
    fn units_come_from_clicks() {
        assert_eq!(Unit::from_clicks(1), Unit::Char);
        assert_eq!(Unit::from_clicks(2), Unit::Word);
        assert_eq!(Unit::from_clicks(3), Unit::Line);
        assert_eq!(Unit::from_clicks(7), Unit::Line);
    }
}
