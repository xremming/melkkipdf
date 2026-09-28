#![cfg(feature = "testing")]

//! Problems reach the reader while a document is open: a notice over the
//! document for anything that failed, and a failed page marked as such rather
//! than loading forever. Uses Slint's testing backend so the tests run without
//! a display.

use std::time::Duration;

use i_slint_backend_testing::ElementHandle;
use melkkipdf::Spread;
use melkkipdf::testing::{Harness, Tabs};
use slint::{ComponentHandle, Model};

fn banner_shown(t: &Tabs) -> bool {
    ElementHandle::find_by_element_id(&t.window, "MainWindow::notice-banner").next().is_some()
}

#[test]
fn a_notice_shows_over_an_open_document_and_goes_away() {
    let t = Tabs::new();
    t.open("a.pdf", 3);
    t.window.window().set_size(slint::LogicalSize::new(900.0, 700.0));
    t.window.show().unwrap();
    assert!(!banner_shown(&t));

    t.window.invoke_notify("Failed to render page 2 of a.pdf: broken.".into());
    assert_eq!(t.window.get_notice(), "Failed to render page 2 of a.pdf: broken.");
    assert!(banner_shown(&t), "the notice is hidden behind the open document");

    i_slint_backend_testing::mock_elapsed_time(Duration::from_secs(9));
    assert_eq!(t.window.get_notice(), "");
    assert!(!banner_shown(&t));
}

#[test]
fn a_document_that_fails_to_open_is_reported_over_the_open_ones() {
    let t = Tabs::new();
    t.open("a.pdf", 3);
    t.open_file(std::path::Path::new("/nonexistent/missing.pdf"));
    assert_eq!(t.titles(), ["a.pdf"]);
    assert!(
        t.window.get_notice().contains("missing.pdf"),
        "the notice was {:?}",
        t.window.get_notice()
    );
}

/// Whether the viewer shows the 0-based `page` as failed.
fn shown_failed(h: &Harness, page: i32) -> bool {
    let model = h.window.get_rows();
    (0..model.row_count())
        .filter_map(|index| model.row_data(index))
        .flat_map(|row| [row.left, row.right])
        .any(|entry| entry.page == page && entry.failed)
}

#[test]
fn a_failed_page_is_marked_and_not_asked_for_again() {
    let h = Harness::uniform(20, 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    h.viewer.on_page_failed(0);
    assert!(shown_failed(&h, 0));

    let _ = h.take_render_requests();
    h.scroll_by_user(0.0);
    h.scroll_by_user(10.0);
    assert!(
        !h.take_render_requests().contains(&0),
        "a page that failed was asked for again at the same zoom"
    );

    // Another zoom may render it, such as a page that was too large.
    h.viewer.zoom_out();
    assert!(h.take_render_requests().contains(&0), "the new zoom did not try the page again");
}

#[test]
fn a_failed_page_keeps_its_mark_across_a_spread_change() {
    let h = Harness::uniform(20, 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    h.viewer.on_page_failed(3);
    h.viewer.set_spread(Spread::Odd);
    assert!(shown_failed(&h, 3));
}
