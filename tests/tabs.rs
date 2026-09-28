#![cfg(feature = "testing")]

//! Browser-style tabs: opening, switching and closing documents, and the tab
//! strip's layout. Uses Slint's testing backend so the tests run without a
//! display.

use i_slint_backend_testing::ElementHandle;
use melkkipdf::testing::Tabs;
use slint::{ComponentHandle, Image, Model, Rgb8Pixel, SharedPixelBuffer};

fn tabs() -> Tabs {
    Tabs::new()
}

/// A rendered page, told apart from an unrendered one by its nonzero size.
fn rendered() -> Image {
    Image::from_rgb8(SharedPixelBuffer::<Rgb8Pixel>::new(1, 1))
}

/// Whether the window's first row shows a rendered image.
fn first_row_rendered(tabs: &Tabs) -> bool {
    let row = tabs.window.get_rows().row_data(0).expect("no rows");
    row.left.image.size().width > 0
}

#[test]
fn starts_empty_with_the_open_prompt() {
    let t = tabs();
    assert!(t.titles().is_empty());
    assert_eq!(t.active_tab(), -1);
    assert_eq!(t.window.get_page_count(), 0);
    assert_eq!(t.window.get_status(), "Open a PDF to get started.");
}

#[test]
fn opening_adds_a_tab_and_shows_it() {
    let t = tabs();
    t.open("a.pdf", 3);
    t.open("b.pdf", 5);
    assert_eq!(t.titles(), ["a.pdf", "b.pdf"]);
    assert_eq!(t.active_tab(), 1);
    assert_eq!(t.window.get_page_count(), 5);
    assert_eq!(t.window.get_doc_title(), "b.pdf");
}

#[test]
fn switching_back_restores_the_tabs_view() {
    let t = tabs();
    t.open("a.pdf", 10);
    let a = t.viewer(0);
    a.set_continuous(false);
    a.go_to_page("4");
    a.zoom_in();
    let density = t.window.get_density();

    // A new document starts in the default view, whatever the last tab showed.
    t.open("b.pdf", 5);
    assert!(t.window.get_continuous());
    assert_eq!(t.window.get_current_page(), 1);
    assert_ne!(t.window.get_density(), density);

    t.window.invoke_select_tab(0);
    assert_eq!(t.active_tab(), 0);
    assert!(!t.window.get_continuous());
    assert_eq!(t.window.get_current_page(), 4);
    assert_eq!(t.window.get_density(), density);
    assert_eq!(t.window.get_page_count(), 10);
    assert_eq!(t.window.get_doc_title(), "a.pdf");
}

#[test]
fn switching_back_restores_the_scroll_position() {
    let t = tabs();
    t.open("a.pdf", 20);
    t.viewer(0).set_viewport(800.0, 600.0);
    t.viewer(0).go_to_page("7");
    let scroll = t.window.get_scroll_y();
    assert!(scroll < 0.0, "go_to_page did not scroll");

    t.open("b.pdf", 20);
    assert_eq!(t.window.get_scroll_y(), 0.0);

    t.window.invoke_select_tab(0);
    assert_eq!(t.window.get_scroll_y(), scroll);
    assert_eq!(t.window.get_current_page(), 7);
}

#[test]
fn renders_for_a_background_tab_land_in_that_tab() {
    let t = tabs();
    let a = t.open("a.pdf", 3);
    t.open("b.pdf", 3);

    // Every worker reports through the one window, so a page finished for a
    // background tab must not show up in the tab on screen.
    t.window.invoke_page_rendered(a, 0, 1.0, rendered());
    assert!(!first_row_rendered(&t));

    t.window.invoke_select_tab(0);
    assert!(first_row_rendered(&t));
}

#[test]
fn closing_the_active_tab_shows_its_right_neighbour() {
    let t = tabs();
    t.open("a.pdf", 1);
    t.open("b.pdf", 1);
    t.open("c.pdf", 1);
    t.window.invoke_select_tab(1);

    t.window.invoke_close_tab(1);
    assert_eq!(t.titles(), ["a.pdf", "c.pdf"]);
    assert_eq!(t.active_tab(), 1);
    assert_eq!(t.window.get_doc_title(), "c.pdf");
}

#[test]
fn closing_the_last_tab_shows_its_left_neighbour() {
    let t = tabs();
    t.open("a.pdf", 1);
    t.open("b.pdf", 1);

    t.window.invoke_close_tab(1);
    assert_eq!(t.titles(), ["a.pdf"]);
    assert_eq!(t.active_tab(), 0);
    assert_eq!(t.window.get_doc_title(), "a.pdf");
}

#[test]
fn closing_a_tab_before_the_active_one_keeps_it_shown() {
    let t = tabs();
    t.open("a.pdf", 1);
    t.open("b.pdf", 1);
    t.open("c.pdf", 4);

    t.window.invoke_close_tab(0);
    assert_eq!(t.titles(), ["b.pdf", "c.pdf"]);
    assert_eq!(t.active_tab(), 1);
    assert_eq!(t.window.get_doc_title(), "c.pdf");
    assert_eq!(t.window.get_page_count(), 4);
}

#[test]
fn closing_every_tab_returns_to_the_empty_window() {
    let t = tabs();
    let a = t.open("a.pdf", 3);

    t.window.invoke_close_tab(0);
    assert!(t.titles().is_empty());
    assert_eq!(t.active_tab(), -1);
    assert_eq!(t.window.get_page_count(), 0);
    assert_eq!(t.window.get_rows().row_count(), 0);
    assert_eq!(t.window.get_doc_title(), "");
    assert_eq!(t.window.get_status(), "Open a PDF to get started.");

    // A render the closed tab's worker finished late has nowhere to go.
    t.window.invoke_page_rendered(a, 0, 1.0, rendered());
    assert_eq!(t.window.get_rows().row_count(), 0);
}

/// Shows the window at `width` so the tab strip is laid out.
fn show(t: &Tabs, width: f32) {
    t.window.window().set_size(slint::LogicalSize::new(width, 800.0));
    t.window.show().expect("failed to show the window");
}

fn document_tabs(t: &Tabs) -> Vec<ElementHandle> {
    ElementHandle::find_by_element_type_name(&t.window, "DocumentTab").collect()
}

fn open_button(t: &Tabs) -> ElementHandle {
    ElementHandle::find_by_element_id(&t.window, "MainWindow::open-frame")
        .next()
        .expect("no open button")
}

#[test]
fn a_single_document_still_gets_a_tab() {
    let t = tabs();
    t.open("a.pdf", 1);
    show(&t, 1600.0);
    assert_eq!(document_tabs(&t).len(), 1);
}

#[test]
fn open_button_sits_left_of_the_tabs() {
    let t = tabs();
    t.open("a.pdf", 1);
    t.open("b.pdf", 1);
    show(&t, 1600.0);

    let tabs = document_tabs(&t);
    let first = tabs.first().expect("no tabs");
    let open = open_button(&t);

    // The button leads the strip, and the tabs follow it rather than being
    // spread across the window, since they stop at their cap.
    assert_eq!(open.absolute_position().x, 8.0, "the open button should lead the strip");
    let gap = first.absolute_position().x - (open.absolute_position().x + open.size().width);
    assert_eq!(gap, 6.0, "the tabs should follow the open button");
    assert_eq!(first.size().width, 220.0);

    let tab_middle = first.absolute_position().y + first.size().height / 2.0;
    let open_middle = open.absolute_position().y + open.size().height / 2.0;
    assert_eq!(open_middle, tab_middle, "the open button should share the tabs' line");
}

#[test]
fn open_button_stays_put_with_no_tabs() {
    let t = tabs();
    show(&t, 1600.0);
    assert_eq!(open_button(&t).absolute_position().x, 8.0);
}

#[test]
fn many_tabs_narrow_to_fit_the_window() {
    let t = tabs();
    for index in 0..20 {
        t.open(&format!("{index}.pdf"), 1);
    }
    show(&t, 800.0);

    // Twenty tabs at their full width would need 4400px; they have to share
    // the 800px instead, so the last one is still on screen.
    let tabs = document_tabs(&t);
    let last = tabs.last().expect("no tabs");
    assert!(
        last.absolute_position().x + last.size().width <= 800.0,
        "the last tab was pushed off screen to {}",
        last.absolute_position().x
    );
    for tab in &tabs {
        assert!(tab.size().width < 220.0);
    }
}
