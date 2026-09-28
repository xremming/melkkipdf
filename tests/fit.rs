#![cfg(feature = "testing")]

//! Page Fit and Page Width leave no more room around the pages than the layout
//! needs: none in paged mode, and in continuous mode only the gap between rows
//! and the scrollbar's width. Measured on a shown window, so the real layout
//! decides where the pages land.

use std::time::Duration;

use i_slint_backend_testing::ElementHandle;
use melkkipdf::testing::Tabs;
use slint::ComponentHandle;

/// Must match `ROW_GAP` in the viewer and the `+ 16px` in `PageRowView`.
const ROW_GAP: f32 = 16.0;
/// Must match `SPREAD_SPACING` in the viewer.
const SPREAD_SPACING: f32 = 4.0;

/// A shown window with one document of `count` pages of `width`×`height`
/// points, and the sidebar closed so the page area is simply the window's.
fn window_with(width: f32, height: f32) -> Tabs {
    let t = Tabs::new();
    t.open_sized("a.pdf", 10, width, height);
    t.window.set_sidebar_open(false);
    t.window.window().set_size(slint::LogicalSize::new(1000.0, 900.0));
    t.window.show().unwrap();
    pump(&t);
    t
}

/// Lets the window lay itself out, as successive frames would.
fn pump(t: &Tabs) {
    for _ in 0..6 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
        let _ = ElementHandle::find_by_element_type_name(&t.window, "PageRowView").count();
    }
}

/// An element's (x, y, width, height) in window coordinates.
type Rect = (f32, f32, f32, f32);

fn rect(element: &ElementHandle) -> Rect {
    let (position, size) = (element.absolute_position(), element.size());
    (position.x, position.y, size.width, size.height)
}

/// The page area and the first one or two pages on display in it.
fn layout(t: &Tabs) -> (Rect, Vec<Rect>) {
    pump(t);
    let content = ElementHandle::find_by_element_id(&t.window, "MainWindow::content")
        .next()
        .expect("no page area");
    let pages = ElementHandle::find_by_element_type_name(&t.window, "PageImageView")
        .map(|page| rect(&page))
        .collect::<Vec<_>>();
    (rect(&content), pages)
}

#[track_caller]
fn assert_close(actual: f32, expected: f32, what: &str) {
    assert!((actual - expected).abs() < 0.5, "{what} was {actual}, expected {expected}");
}

#[test]
fn paged_page_fit_fills_the_height_of_a_portrait_page() {
    let t = window_with(600.0, 800.0);
    t.viewer(0).set_continuous(false);
    t.viewer(0).fit_page();
    let ((_, content_y, _, content_h), pages) = layout(&t);
    let (_, page_y, _, page_h) = pages[0];
    assert_close(page_y, content_y, "the space above the page");
    assert_close(page_h, content_h, "the page's height");
}

#[test]
fn paged_page_fit_fills_the_width_of_a_landscape_page() {
    let t = window_with(800.0, 450.0);
    t.viewer(0).set_continuous(false);
    t.viewer(0).fit_page();
    let ((content_x, _, content_w, _), pages) = layout(&t);
    let (page_x, _, page_w, _) = pages[0];
    assert_close(page_x, content_x, "the space left of the page");
    assert_close(page_w, content_w, "the page's width");
}

#[test]
fn paged_page_fit_fits_a_spread_and_its_gap_exactly() {
    let t = window_with(600.0, 800.0);
    t.viewer(0).set_continuous(false);
    t.viewer(0).set_spread(1);
    t.viewer(0).fit_page();
    let ((content_x, _, content_w, _), pages) = layout(&t);
    let (left_x, _, left_w, _) = pages[0];
    let (_, _, right_w, _) = pages[1];
    // Two portrait pages side by side are wider than tall, so the width binds.
    assert_close(left_x, content_x, "the space left of the spread");
    assert_close(left_w + SPREAD_SPACING + right_w, content_w, "the spread's width");
}

#[test]
fn continuous_page_fit_leaves_only_the_gap_between_pages() {
    let t = window_with(600.0, 800.0);
    t.viewer(0).fit_page();
    let ((_, content_y, _, content_h), pages) = layout(&t);
    let (_, page_y, _, page_h) = pages[0];
    // Each row carries its gap as half above the page and half below.
    assert_close(page_y - content_y, ROW_GAP / 2.0, "the space above the page");
    assert_close(content_y + content_h - (page_y + page_h), ROW_GAP / 2.0, "the space below");
}
