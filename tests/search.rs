#![cfg(feature = "testing")]

//! Searching a document's text from the sidebar on the right: results as the
//! query is typed, stepping between hits, and the index filling in behind
//! it. Uses Slint's testing backend so the tests run without a display.

mod common;

use std::time::Duration;

use common::{Scratch, write_text_pdf};
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use melkkipdf::testing::{Harness, Tabs};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, Model};

/// A 20 page document whose text mentions the Orient Express on pages 4, 8
/// and 13, the last far down its page.
fn book() -> Vec<Vec<&'static str>> {
    let mut pages: Vec<Vec<&str>> = vec![vec!["Nothing to see on this page"]; 20];
    pages[0] = vec!["Introduction"];
    pages[3] = vec!["The Orient Express leaves Paris"];
    pages[7] = vec!["Orient", "Express, again across a line"];
    let mut page_13 = vec!["filler line"; 30];
    page_13.push("the orient express at last");
    pages[12] = page_13;
    pages
}

fn indexed(pages: &[Vec<&str>]) -> Harness {
    let h = Harness::uniform(pages.len(), 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    let pages: Vec<&[&str]> = pages.iter().map(Vec::as_slice).collect();
    h.index_text(&pages);
    h
}

fn wait(milliseconds: u64) {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(milliseconds));
}

fn status(h: &Harness) -> String {
    h.window.get_search_status().into()
}

#[test]
fn typing_lists_every_hit_and_goes_to_the_first() {
    let h = indexed(&book());
    h.viewer.search_edited("orient express");
    assert_eq!(h.result_pages(), [3, 7, 12]);
    assert_eq!(status(&h), "3 matches.");
    assert_eq!(h.window.get_search_current(), 0);
    assert_eq!(h.current_page(), 4, "the view did not move to the first hit");

    let result = h.window.get_search_results().row_data(1).unwrap();
    assert_eq!(result.found, "Orient Express");
    assert_eq!(result.after, ", again across a line");
}

#[test]
fn the_first_hit_is_the_one_from_the_page_being_read() {
    let h = indexed(&book());
    h.viewer.go_to_page("10");
    h.viewer.search_edited("orient");
    assert_eq!(h.window.get_search_current(), 2);
    assert_eq!(h.current_page(), 13);
}

#[test]
fn hits_are_outlined_on_their_pages() {
    let h = indexed(&book());
    h.viewer.search_edited("orient express");
    // The current hit, then one broken over two lines, then an ordinary one.
    assert_eq!(h.highlights(3), [(72.0, true)]);
    assert_eq!(h.highlights(7), [(72.0, false), (92.0, false)]);
    assert_eq!(h.highlights(12), [(672.0, false)]);
    assert!(h.highlights(0).is_empty());

    h.viewer.search_step(1);
    assert_eq!(h.highlights(3), [(72.0, false)]);
    assert_eq!(h.highlights(7), [(72.0, true), (92.0, true)]);

    // Clearing the query clears everything.
    h.viewer.search_edited("");
    wait(150);
    assert!(h.highlights(7).is_empty());
    assert!(h.result_pages().is_empty());
    assert_eq!(status(&h), "");
}

#[test]
fn hits_on_the_same_page_are_listed_under_one_heading() {
    let h = indexed(&book());
    h.viewer.search_edited("line");
    // One hit on page 8, then 30 on page 13, where only the first carries
    // the page's heading and count.
    let results = h.window.get_search_results();
    let page_hits: Vec<i32> = results.iter().map(|r| r.page_hits).collect();
    let mut expected = vec![1, 30];
    expected.resize(31, 0);
    assert_eq!(page_hits, expected);
    assert_eq!(h.result_pages()[1..], [12; 30]);

    // The rows still stand for single hits, so picking one goes to that hit.
    h.viewer.go_to_search_result(5);
    assert_eq!(h.window.get_search_current(), 5);
    assert_eq!(h.current_page(), 13);
}

#[test]
fn stepping_goes_round_the_hits_both_ways() {
    let h = indexed(&book());
    h.viewer.search_edited("orient");
    let mut pages = Vec::new();
    for _ in 0..3 {
        h.viewer.search_step(1);
        pages.push(h.current_page());
    }
    assert_eq!(pages, [8, 13, 4]);
    h.viewer.search_step(-1);
    assert_eq!(h.current_page(), 13);
    assert_eq!(h.window.get_search_current(), 2);
}

#[test]
fn picking_a_result_goes_to_it() {
    let h = indexed(&book());
    h.viewer.search_edited("orient");
    h.viewer.go_to_search_result(2);
    assert_eq!(h.current_page(), 13);
    assert_eq!(h.window.get_search_current(), 2);
}

#[test]
fn a_hit_far_down_a_page_is_scrolled_into_view() {
    let h = indexed(&book());
    h.viewer.fit_width();
    h.viewer.search_edited("at last");
    assert_eq!(h.current_page(), 13);

    // In continuous mode the view starts below the page's top, so the hit
    // is in view.
    h.viewer.go_to_page("13");
    let page_top = -h.scroll_y();
    h.viewer.search_step(1);
    assert!(-h.scroll_y() > page_top, "the view stayed at the top of the page");

    // In paged mode the page is scrolled within.
    h.viewer.set_continuous(false);
    h.viewer.search_step(1);
    assert!(h.window.get_paged_offset_y() < 0.0, "the page was not scrolled to the hit");
}

#[test]
fn typing_searches_at_once_then_at_most_every_interval() {
    let h = indexed(&book());
    h.viewer.search_edited("i");
    let first = status(&h);
    assert!(first.ends_with("matches."), "the first keystroke did not search: {first:?}");

    // Keys in quick succession wait for the interval, and the search that
    // then runs is for the latest query.
    h.viewer.search_edited("in");
    h.viewer.search_edited("int");
    assert_eq!(status(&h), first);
    wait(150);
    assert_eq!(status(&h), "1 match.");
    assert_eq!(h.result_pages(), [0]);
}

#[test]
fn a_long_query_already_searches_while_it_is_typed() {
    let h = indexed(&book());
    h.viewer.search_edited("o");
    let first_letter = status(&h);
    // Typing steadily, a key every 40ms, never pauses long enough for a
    // search that waits for typing to stop, yet the results already move on
    // from the first letter.
    for query in ["or", "ori", "orie", "orien"] {
        wait(40);
        h.viewer.search_edited(query);
    }
    assert_ne!(status(&h), first_letter, "no search ran while typing");
    assert_eq!(status(&h), "3 matches.");
}

#[test]
fn hits_fill_in_as_the_index_grows() {
    let pages = book();
    let h = Harness::uniform(pages.len(), 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    let slices: Vec<&[&str]> = pages.iter().map(Vec::as_slice).collect();
    h.index_text_from(0, &slices[..5]);
    assert_eq!(status(&h), "Indexing for search, 5 of 20 pages.");

    h.viewer.search_edited("orient");
    assert_eq!(status(&h), "1 match so far.");
    h.index_text_from(5, &slices[5..]);
    wait(150);
    assert_eq!(status(&h), "3 matches.");
    assert_eq!(h.result_pages(), [3, 7, 12]);
}

#[test]
fn a_scanned_document_says_it_has_no_text() {
    let h = indexed(&vec![Vec::new(); 5]);
    assert_eq!(status(&h), "This document has no searchable text.");
    h.viewer.search_edited("anything");
    assert_eq!(status(&h), "This document has no searchable text.");
}

/// Presses and releases `key`, with Control held when `control`.
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

fn shown() -> Tabs {
    let t = Tabs::new();
    t.open("a.pdf", 5);
    t.window.window().set_size(slint::LogicalSize::new(1200.0, 800.0));
    t.window.show().unwrap();
    t
}

#[test]
fn ctrl_f_opens_the_sidebar_with_the_cursor_in_its_field() {
    let t = shown();
    assert!(!t.window.get_search_open());
    press(&t, "f", true);
    assert!(t.window.get_search_open());

    for key in ["o", "k"] {
        press(&t, key, false);
    }
    assert_eq!(t.window.get_search_text(), "ok", "typing did not reach the search field");

    // Esc hands the keys back to the document, where C switches modes.
    press(&t, Key::Escape, false);
    press(&t, "c", false);
    assert!(!t.window.get_continuous());
    assert_eq!(t.window.get_search_text(), "ok");
}

#[test]
fn ctrl_f_in_the_field_closes_the_sidebar() {
    let t = shown();
    press(&t, "f", true);
    press(&t, "f", true);
    assert!(!t.window.get_search_open(), "Ctrl+F in the field left the sidebar open");

    // The keys are back with the document, where C switches modes.
    press(&t, "c", false);
    assert!(!t.window.get_continuous());
    assert_eq!(t.window.get_search_text(), "");
}

#[test]
fn ctrl_f_selects_the_query_so_typing_replaces_it() {
    let t = shown();
    press(&t, "f", true);
    for key in ["o", "k"] {
        press(&t, key, false);
    }
    press(&t, Key::Escape, false);
    press(&t, "f", true);
    press(&t, "x", false);
    assert_eq!(t.window.get_search_text(), "x", "typing did not replace the query");
}

#[test]
fn clicking_the_document_hands_the_keys_back_to_it() {
    for continuous in [true, false] {
        let t = shown();
        t.viewer(0).set_continuous(continuous);
        press(&t, "f", true);
        press(&t, "o", false);

        let content = ElementHandle::find_by_element_id(&t.window, "MainWindow::content")
            .next()
            .expect("no page area");
        content.mock_single_click(PointerEventButton::Left);
        // With the keys back at the document, C switches modes and typing no
        // longer reaches the field.
        press(&t, "c", false);
        assert_ne!(t.window.get_continuous(), continuous, "the keys stayed with the field");
        assert_eq!(t.window.get_search_text(), "o");
        assert!(t.window.get_search_open(), "the click closed the sidebar");
    }
}

#[test]
fn ctrl_f_after_esc_goes_back_to_the_field() {
    let t = shown();
    press(&t, "f", true);
    press(&t, Key::Escape, false);
    press(&t, "f", true);
    assert!(t.window.get_search_open(), "Ctrl+F in the document closed the sidebar");

    press(&t, "o", false);
    assert_eq!(t.window.get_search_text(), "o", "typing did not reach the search field");
}

#[test]
fn the_clear_button_empties_the_query_and_keeps_the_cursor_in_the_field() {
    let directory = Scratch::new("search-clear");
    let path = directory.join("book.pdf");
    write_text_pdf(&path, &[&["Once upon a time"]]);
    let t = Tabs::new();
    t.open_file(&path);
    t.finish_indexing();
    t.window.window().set_size(slint::LogicalSize::new(1200.0, 800.0));
    t.window.show().unwrap();

    let clear_button =
        || ElementHandle::find_by_element_id(&t.window, "MainWindow::search-clear").next();
    press(&t, "f", true);
    assert!(clear_button().is_none(), "an empty field has a clear button");

    press(&t, "o", false);
    assert_eq!(t.window.get_search_results().row_count(), 2);
    let button = clear_button().expect("no clear button once there is a query");
    button.invoke_accessible_default_action();
    assert_eq!(t.window.get_search_text(), "");
    assert!(clear_button().is_none(), "the clear button stayed after clearing");
    // Clearing is an edit like any other, so it waits out the interval since
    // the search before it.
    wait(100);
    assert_eq!(t.window.get_search_results().row_count(), 0);
    assert_eq!(t.window.get_search_status(), "");

    press(&t, "k", false);
    assert_eq!(t.window.get_search_text(), "k", "typing did not go on in the field");
}

/// Opens a real document with `pages` of text and shows it, with its text
/// indexed for search.
fn shown_with_text(name: &str, pages: &[&[&str]]) -> (Scratch, Tabs) {
    let directory = Scratch::new(name);
    let path = directory.join("book.pdf");
    write_text_pdf(&path, pages);
    let t = Tabs::new();
    t.open_file(&path);
    t.finish_indexing();
    t.window.window().set_size(slint::LogicalSize::new(1200.0, 800.0));
    t.window.show().unwrap();
    (directory, t)
}

#[test]
fn arrow_keys_in_the_field_step_between_hits() {
    let (_directory, t) =
        shown_with_text("search-arrows", &[&["Chapter one"], &["The end"], &["Another end"]]);
    press(&t, "f", true);
    for key in ["e", "n", "d"] {
        press(&t, key, false);
    }
    assert_eq!(t.window.get_search_current(), 0);

    press(&t, Key::DownArrow, false);
    assert_eq!(t.window.get_search_current(), 1);
    press(&t, Key::UpArrow, false);
    assert_eq!(t.window.get_search_current(), 0);
    assert_eq!(t.window.get_search_text(), "end", "the arrows changed the query");
}

#[test]
fn stepping_keeps_the_current_hit_in_view_in_the_list() {
    let pages: Vec<[&str; 1]> = vec!["A line to find"; 60].into_iter().map(|line| [line]).collect();
    let pages: Vec<&[&str]> = pages.iter().map(|page| page.as_slice()).collect();
    let (_directory, t) = shown_with_text("search-list-scroll", &pages);
    press(&t, "f", true);
    press(&t, "f", false);
    assert_eq!(t.window.get_search_results().row_count(), 60);

    let list = ElementHandle::find_by_element_id(&t.window, "MainWindow::search-list")
        .next()
        .expect("no results list");
    let list_top = list.absolute_position().y;
    let list_bottom = list_top + list.size().height;
    let current_row_in_view = || {
        ElementHandle::find_by_element_id(&t.window, "MainWindow::search-result")
            .filter(|row| row.accessible_item_selected() == Some(true))
            .any(|row| {
                let top = row.absolute_position().y;
                top >= list_top && top + row.size().height <= list_bottom
            })
    };
    assert!(current_row_in_view(), "the first hit is not in view");

    // Up from the first hit goes round to the last, far down the list, and
    // Down from there back to the top.
    press(&t, Key::UpArrow, false);
    assert_eq!(t.window.get_search_current(), 59);
    assert!(current_row_in_view(), "the list did not scroll down to the last hit");
    press(&t, Key::DownArrow, false);
    assert_eq!(t.window.get_search_current(), 0);
    assert!(current_row_in_view(), "the list did not scroll back up to the first hit");
}

#[test]
fn a_long_hit_fits_the_width_of_the_list() {
    let (_directory, t) = shown_with_text(
        "search-long",
        &[&[
            "Somewhere in an exceedingly long sentence the remarkable expression wanted here sits",
        ]],
    );
    press(&t, "f", true);
    for key in "remarkable expression wanted".chars() {
        press(&t, key.to_string(), false);
    }
    wait(150);
    assert_eq!(t.window.get_search_results().row_count(), 1);

    // The text before the hit gives way, so the hit shows within the list
    // rather than pushed past its right edge.
    let list = ElementHandle::find_by_element_id(&t.window, "MainWindow::search-list")
        .next()
        .expect("no results list");
    let list_left = list.absolute_position().x;
    let list_right = list_left + list.size().width;
    let found = ElementHandle::find_by_accessible_label(&t.window, "remarkable expression wanted")
        .find(|text| text.accessible_role() == Some(AccessibleRole::Text))
        .expect("the hit is not shown");
    let left = found.absolute_position().x;
    assert!(found.size().width > 0.0, "the hit has no room");
    assert!(
        left >= list_left && left + found.size().width <= list_right,
        "the hit runs past the list"
    );
}

#[test]
fn the_sidebar_sits_on_the_right_and_takes_its_width_from_the_pages() {
    let t = shown();
    let element = |id: &str| {
        ElementHandle::find_by_element_id(&t.window, id).next().unwrap_or_else(|| panic!("no {id}"))
    };
    let content_width = element("MainWindow::content").size().width;

    t.window.set_search_open(true);
    let sidebar = element("MainWindow::search-sidebar");
    assert_eq!(sidebar.size().width, 300.0);
    assert_eq!(sidebar.absolute_position().x + sidebar.size().width, 1200.0);
    assert_eq!(element("MainWindow::content").size().width, content_width - 300.0);

    // The toolbar has a button for it at its right end, like the one on the
    // left for the other sidebar.
    let toggle = element("MainWindow::search-toggle");
    assert!(toggle.absolute_position().x > 1100.0);
}

#[test]
fn a_real_document_is_indexed_in_the_background_and_searched() {
    let directory = Scratch::new("search-real");
    let path = directory.join("book.pdf");
    write_text_pdf(
        &path,
        &[&["Chapter one"], &["All aboard the Orient Express", "bound for Istanbul"], &["The end"]],
    );

    let t = Tabs::new();
    t.open_file(&path);
    t.finish_indexing();
    t.window.invoke_search_edited("orient express".into());
    assert_eq!(t.window.get_search_status(), "1 match.");
    let result = t.window.get_search_results().row_data(0).expect("no result");
    assert_eq!(result.page, 1);
    assert_eq!(result.found, "Orient Express");
    assert_eq!(t.window.get_current_page(), 2);

    // Its outline sits on the first line, 72pt from the page's top.
    let row = t.window.get_rows().row_data(1).unwrap();
    let highlight = row.left.highlights.row_data(0).expect("no outline");
    assert!((70.0..90.0).contains(&highlight.y), "the outline is at {}", highlight.y);
    assert!(highlight.x > 72.0 && highlight.width > 0.0);
}
