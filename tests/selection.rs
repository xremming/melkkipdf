#![cfg(feature = "testing")]

//! Selecting text with the pointer and copying it: a drag within a page,
//! across pages and across a spread, clicks for a word or a line, and the
//! keys that copy, select all and let go. Uses Slint's testing backend so the
//! tests run without a display.

mod common;

use common::{Scratch, write_text_pdf};
use melkkipdf::testing::{Harness, Tabs, selection_areas};
use slint::ComponentHandle;
use slint::platform::{Key, WindowEvent};

/// Where the harness's text sits on a page: lines start 72pt from the top,
/// 14pt tall and 20pt apart, and characters are 7pt wide from 72pt in. This
/// gives a point on the left half of a character, which a selection begins
/// or ends before.
fn at(line: usize, column: usize) -> (f32, f32) {
    (72.0 + column as f32 * 7.0 + 2.0, 72.0 + line as f32 * 20.0 + 7.0)
}

/// A point on the right half of a character, which a selection takes in.
fn after(line: usize, column: usize) -> (f32, f32) {
    let (x, y) = at(line, column);
    (x + 3.0, y)
}

/// The area of `count` characters from `column` on `line`.
fn area(line: usize, column: usize, count: usize) -> (f32, f32, f32, f32) {
    (72.0 + column as f32 * 7.0, 72.0 + line as f32 * 20.0, count as f32 * 7.0, 14.0)
}

fn indexed(pages: &[&[&str]]) -> Harness {
    let h = Harness::uniform(pages.len(), 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    h.index_text(pages);
    h
}

/// Drags from one point to another on `page`, both in points from its
/// corner, and lets go.
fn drag(h: &Harness, page: usize, from: (f32, f32), to: (f32, f32)) {
    h.viewer.select_from(page, from.0, from.1);
    h.viewer.select_to(page, to.0, to.1);
    h.viewer.select_done();
}

#[test]
fn a_drag_selects_the_text_between_and_copies_it_by_the_line() {
    let h = indexed(&[&["the cat sat", "on the mat"]]);
    // From the 'c' of "cat" to the end of "the" on the next line.
    drag(&h, 0, at(0, 4), after(1, 5));
    assert_eq!(h.selection(0), [area(0, 4, 7), area(1, 0, 6)]);

    assert_eq!(h.viewer.selected_text().as_deref(), Some("cat sat\non the"));
}

#[test]
fn dragging_backwards_selects_the_same_text() {
    let h = indexed(&[&["the cat sat", "on the mat"]]);
    drag(&h, 0, after(1, 5), at(0, 4));
    assert_eq!(h.selection(0), [area(0, 4, 7), area(1, 0, 6)]);
}

#[test]
fn a_click_alone_selects_nothing() {
    let h = indexed(&[&["the cat sat"]]);
    drag(&h, 0, at(0, 4), at(0, 4));
    assert!(h.selection(0).is_empty());
    assert_eq!(h.viewer.selected_text(), None);
}

#[test]
fn a_second_click_selects_the_word_and_a_third_the_line() {
    let h = indexed(&[&["the cat sat", "on the mat"]]);
    let (x, y) = at(0, 5);
    h.viewer.select_from(0, x, y);
    h.viewer.select_done();
    h.viewer.select_from(0, x, y);
    h.viewer.select_done();
    assert_eq!(h.selection(0), [area(0, 4, 3)]);
    assert_eq!(h.viewer.selected_text().as_deref(), Some("cat"));

    h.viewer.select_from(0, x, y);
    h.viewer.select_done();
    assert_eq!(h.selection(0), [area(0, 0, 11)]);
    assert_eq!(h.viewer.selected_text().as_deref(), Some("the cat sat"));
}

#[test]
fn dragging_by_the_word_takes_in_whole_words() {
    let h = indexed(&[&["the cat sat", "on the mat"]]);
    let (x, y) = at(0, 5);
    h.viewer.select_from(0, x, y);
    h.viewer.select_done();
    h.viewer.select_from(0, x, y);
    // Onto the 'm' of "mat", which is then taken in whole.
    let (to_x, to_y) = after(1, 7);
    h.viewer.select_to(0, to_x, to_y);
    h.viewer.select_done();
    assert_eq!(h.selection(0), [area(0, 4, 7), area(1, 0, 10)]);
    assert_eq!(h.viewer.selected_text().as_deref(), Some("cat sat\non the mat"));
}

#[test]
fn dragging_by_the_word_onto_a_letter_of_two_bytes_takes_in_its_word() {
    let h = indexed(&[&["hyvää päivää"]]);
    let (x, y) = at(0, 1);
    h.viewer.select_from(0, x, y);
    h.viewer.select_done();
    h.viewer.select_from(0, x, y);
    // Onto the first 'ä' of "päivää", the drag's end falling just past it.
    let (to_x, to_y) = after(0, 7);
    h.viewer.select_to(0, to_x, to_y);
    h.viewer.select_done();
    assert_eq!(h.selection(0), [area(0, 0, 12)]);
    assert_eq!(h.viewer.selected_text().as_deref(), Some("hyvää päivää"));
}

#[test]
fn a_drag_past_the_page_reaches_the_next_one() {
    let h = indexed(&[&["first page"], &["second page"]]);
    // Rows are a page's height plus the gap, which in points depends on the
    // density: the gap is in pixels.
    let row_pt = 800.0 + 16.0 / h.density();
    let (from_x, from_y) = at(0, 0);
    let (to_x, to_y) = at(0, 7);
    drag(&h, 0, (from_x, from_y), (to_x, to_y + row_pt));
    assert_eq!(h.selection(0), [area(0, 0, 10)]);
    assert_eq!(h.selection(1), [area(0, 0, 7)]);
    assert_eq!(h.viewer.selected_text().as_deref(), Some("first page\nsecond "));

    // Far below the last page is the end of its text.
    drag(&h, 0, (from_x, from_y), (5000.0, 5000.0));
    assert_eq!(h.selection(1), [area(0, 0, 11)]);
    assert_eq!(h.viewer.selected_text().as_deref(), Some("first page\nsecond page"));
}

#[test]
fn a_drag_across_a_spread_reaches_its_other_page() {
    let h = indexed(&[&["left page"], &["right page"]]);
    h.viewer.set_spread(melkkipdf::Spread::Odd);
    h.viewer.set_continuous(false);
    // The right page starts a page's width and the spread's gap, which is
    // in pixels, to the right.
    let right_pt = 600.0 + 4.0 / h.density();
    let (from_x, from_y) = at(0, 0);
    let (to_x, to_y) = at(0, 5);
    drag(&h, 0, (from_x, from_y), (to_x + right_pt, to_y));
    assert_eq!(h.selection(0), [area(0, 0, 9)]);
    assert_eq!(h.selection(1), [area(0, 0, 5)]);
    assert_eq!(h.viewer.selected_text().as_deref(), Some("left page\nright"));
}

#[test]
fn the_selection_survives_a_zoom_and_a_change_of_mode() {
    let h = indexed(&[&["the cat sat"]]);
    drag(&h, 0, at(0, 4), after(0, 6));
    h.viewer.zoom_in();
    h.viewer.set_continuous(false);
    assert_eq!(h.selection(0), [area(0, 4, 3)]);
}

#[test]
fn a_page_without_text_selects_nothing() {
    let h = indexed(&[&[]]);
    drag(&h, 0, at(0, 0), at(3, 0));
    assert!(h.selection(0).is_empty());
    assert_eq!(h.viewer.selected_text(), None);
}

fn press(t: &Tabs, key: impl Into<slint::SharedString> + Clone, control: bool) {
    let window = t.window.window();
    if control {
        window.dispatch_event(WindowEvent::KeyPressed { text: Key::Control.into() });
    }
    window.dispatch_event(WindowEvent::KeyPressed { text: key.clone().into() });
    window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    if control {
        window.dispatch_event(WindowEvent::KeyReleased { text: Key::Control.into() });
    }
}

#[test]
fn the_keys_copy_select_all_and_let_go() {
    let t = Tabs::new();
    t.open("a.pdf", 2);
    t.index_text(0, &[&["the cat sat"], &["on the mat"]]);
    let viewer = t.viewer(0);
    let (x, y) = at(0, 4);
    viewer.select_from(0, x, y);
    let (x, y) = after(0, 6);
    viewer.select_to(0, x, y);
    viewer.select_done();

    press(&t, "c", true);
    assert_eq!(t.copied(), "cat");

    press(&t, "a", true);
    assert_eq!(selection_areas(&t.window, 0), [area(0, 0, 11)]);
    assert_eq!(selection_areas(&t.window, 1), [area(0, 0, 10)]);
    press(&t, "c", true);
    assert_eq!(t.copied(), "the cat sat\non the mat");

    press(&t, Key::Escape, false);
    assert!(selection_areas(&t.window, 0).is_empty());
    assert!(selection_areas(&t.window, 1).is_empty());
    // Nothing to copy leaves the clipboard as it was.
    press(&t, "c", true);
    assert_eq!(t.copied(), "the cat sat\non the mat");
}

#[test]
fn a_real_documents_text_is_selected_where_it_is_drawn() {
    let directory = Scratch::new("selection-real");
    let path = directory.join("book.pdf");
    write_text_pdf(&path, &[&["All aboard the Orient Express", "bound for Istanbul"]]);

    let t = Tabs::new();
    t.open_file(&path);
    t.finish_indexing();
    let viewer = t.viewer(0);
    // Helvetica at 12pt: "All aboard the " is about 80pt wide from 72pt in,
    // and the baseline of the first line is 84pt from the top. A drag from
    // the 'O' of "Orient" to the end of the second line.
    viewer.select_from(0, 151.0, 80.0);
    viewer.select_to(0, 400.0, 100.0);
    viewer.select_done();
    assert_eq!(viewer.selected_text().as_deref(), Some("Orient Express\nbound for Istanbul"));

    let areas = selection_areas(&t.window, 0);
    assert_eq!(areas.len(), 2, "one area per line: {areas:?}");
    assert!((145.0..160.0).contains(&areas[0].0), "the first area starts at {}", areas[0].0);
    assert!((70.0..90.0).contains(&areas[0].1), "the first area is at {}", areas[0].1);
    assert!((70.0..74.0).contains(&areas[1].0), "the second area starts at {}", areas[1].0);
}
