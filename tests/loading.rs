#![cfg(feature = "testing")]

//! Documents are read on their worker thread, so a long one does not freeze
//! the window. Its tab appears at once and says it is loading until then.
//! Uses Slint's testing backend so the tests run without a display.

mod common;

use common::{Scratch, write_pdf};
use melkkipdf::testing::Tabs;

#[test]
fn a_loading_document_has_its_tab_at_once() {
    let directory = Scratch::new("at-once");
    let a = directory.join("a.pdf");
    write_pdf(&a, 3);

    let t = Tabs::new();
    t.start_opening(&a);
    assert_eq!(t.titles(), ["a.pdf"]);
    assert_eq!(t.active_tab(), 0);
    assert_eq!(t.window.get_doc_title(), "a.pdf");
    assert_eq!(t.window.get_page_count(), 0);
    assert_eq!(t.status(), "Loading a.pdf…");

    t.finish_loading();
    assert_eq!(t.window.get_page_count(), 3);
    assert_eq!(t.window.get_current_page(), 1);
}

#[test]
fn a_document_loading_in_the_background_leaves_the_shown_tab_alone() {
    let directory = Scratch::new("background");
    let a = directory.join("a.pdf");
    write_pdf(&a, 3);

    let t = Tabs::new();
    t.open("shown.pdf", 5);
    t.start_opening(&a);
    t.window.invoke_select_tab(0);

    t.finish_loading();
    assert_eq!(t.active_tab(), 0);
    assert_eq!(t.window.get_doc_title(), "shown.pdf");
    assert_eq!(t.window.get_page_count(), 5);

    t.window.invoke_select_tab(1);
    assert_eq!(t.window.get_doc_title(), "a.pdf");
    assert_eq!(t.window.get_page_count(), 3);
}

#[test]
fn opening_a_loading_document_again_shows_its_tab() {
    let directory = Scratch::new("again");
    let a = directory.join("a.pdf");
    write_pdf(&a, 2);

    let t = Tabs::new();
    t.start_opening(&a);
    t.open("other.pdf", 1);
    t.start_opening(&a);
    assert_eq!(t.titles(), ["a.pdf", "other.pdf"]);
    assert_eq!(t.active_tab(), 0);
    t.finish_loading();
    assert_eq!(t.window.get_page_count(), 2);
}
