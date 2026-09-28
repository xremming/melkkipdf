#![cfg(feature = "testing")]

//! Files dropped onto the window from another application open in new tabs.
//! Uses Slint's testing backend so the tests run without a display.

use std::path::{Path, PathBuf};
use std::time::Duration;

use i_slint_backend_testing::ElementHandle;
use melkkipdf::testing::Tabs;
use mupdf::Size;
use mupdf::pdf::PdfDocument;
use slint::ComponentHandle;

fn tabs() -> Tabs {
    Tabs::new()
}

/// A fresh, empty directory for one test's files.
fn scratch(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("melkkipdf-drop-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

/// Writes a PDF of `pages` blank A4 pages.
fn write_pdf(path: &Path, pages: usize) {
    let mut document = PdfDocument::new();
    for _ in 0..pages {
        document.new_page(Size::A4).unwrap();
    }
    document.save(path.to_str().unwrap()).unwrap();
}

/// Lets the event loop run what the drop scheduled.
fn run_pending() {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
}

#[test]
fn a_dropped_pdf_opens_in_a_new_tab() {
    let directory = scratch("new-tab");
    let (a, b) = (directory.join("a.pdf"), directory.join("b.pdf"));
    write_pdf(&a, 2);
    write_pdf(&b, 5);

    let t = tabs();
    t.open_file(&a);
    t.drop_file(&b);
    run_pending();

    assert_eq!(t.titles(), ["a.pdf", "b.pdf"]);
    assert_eq!(t.active_tab(), 1);
    assert_eq!(t.window.get_page_count(), 5);
}

#[test]
fn several_dropped_files_each_get_a_tab_in_order() {
    let directory = scratch("several");
    let paths: Vec<PathBuf> =
        ["one.pdf", "two.pdf", "three.pdf"].iter().map(|name| directory.join(name)).collect();
    for path in &paths {
        write_pdf(path, 1);
    }

    let t = tabs();
    // The windowing system reports a multi-file drop as one event per file.
    for path in &paths {
        t.drop_file(path);
    }
    run_pending();

    assert_eq!(t.titles(), ["one.pdf", "two.pdf", "three.pdf"]);
    assert_eq!(t.active_tab(), 2);
}

#[test]
fn dropping_an_open_document_shows_its_tab() {
    let directory = scratch("already-open");
    let (a, b) = (directory.join("a.pdf"), directory.join("b.pdf"));
    write_pdf(&a, 1);
    write_pdf(&b, 1);

    let t = tabs();
    t.open_file(&a);
    t.open_file(&b);
    t.drop_file(&a);
    run_pending();

    assert_eq!(t.titles(), ["a.pdf", "b.pdf"]);
    assert_eq!(t.active_tab(), 0);
}

#[test]
fn dropping_something_unreadable_leaves_the_tabs_alone() {
    let directory = scratch("unreadable");
    let (a, junk) = (directory.join("a.pdf"), directory.join("notes.pdf"));
    write_pdf(&a, 1);
    std::fs::write(&junk, "not a pdf").unwrap();

    let t = tabs();
    t.open_file(&a);
    t.drop_file(&junk);
    run_pending();

    assert_eq!(t.titles(), ["a.pdf"]);
    assert_eq!(t.active_tab(), 0);
    assert!(t.window.get_notice().contains("notes.pdf"), "the failure was not reported");
}

fn overlay_shown(t: &Tabs) -> bool {
    ElementHandle::find_by_element_id(&t.window, "MainWindow::drop-overlay").next().is_some()
}

#[test]
fn the_window_highlights_while_files_hover() {
    let directory = scratch("hover");
    let a = directory.join("a.pdf");
    write_pdf(&a, 1);

    let t = tabs();
    t.window.window().set_size(slint::LogicalSize::new(900.0, 700.0));
    t.window.show().unwrap();
    assert!(!overlay_shown(&t));

    t.drag_files_over();
    let overlay = ElementHandle::find_by_element_id(&t.window, "MainWindow::drop-overlay")
        .next()
        .expect("no overlay while files hover");
    // It covers the whole window, tab strip included, not just the page area.
    assert_eq!(overlay.absolute_position(), slint::LogicalPosition::new(0.0, 0.0));
    assert_eq!(overlay.size(), slint::LogicalSize::new(900.0, 700.0));

    t.drag_away();
    assert!(!overlay_shown(&t));

    t.drag_files_over();
    t.drop_file(&a);
    assert!(!overlay_shown(&t), "the highlight should go as soon as the files drop");
}
