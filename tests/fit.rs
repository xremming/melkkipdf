#![cfg(feature = "testing")]

//! Page Fit and Page Width leave no more room around the pages than the layout
//! needs: none in paged mode, and in continuous mode only the gap between rows
//! and the scrollbar's width. Measured on a shown window, so the real layout
//! decides where the pages land.

use std::time::Duration;

use i_slint_backend_testing::ElementHandle;
use melkkipdf::PageLayout;
use melkkipdf::Spread;
use melkkipdf::testing::Tabs;
use slint::ComponentHandle;

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
    t.viewer(0).set_spread(Spread::Odd);
    t.viewer(0).fit_page();
    let ((content_x, _, content_w, _), pages) = layout(&t);
    let (left_x, _, left_w, _) = pages[0];
    let (_, _, right_w, _) = pages[1];
    // Two portrait pages side by side are wider than tall, so the width binds.
    assert_close(left_x, content_x, "the space left of the spread");
    let spacing = t.window.global::<PageLayout>().get_spread_spacing();
    assert_close(left_w + spacing + right_w, content_w, "the spread's width");
}

#[test]
fn continuous_page_fit_leaves_only_the_gap_between_pages() {
    let t = window_with(600.0, 800.0);
    t.viewer(0).fit_page();
    let ((_, content_y, _, content_h), pages) = layout(&t);
    let (_, page_y, _, page_h) = pages[0];
    // Each row carries its gap as half above the page and half below.
    let row_gap = t.window.global::<PageLayout>().get_row_gap();
    assert_close(page_y - content_y, row_gap / 2.0, "the space above the page");
    assert_close(content_y + content_h - (page_y + page_h), row_gap / 2.0, "the space below");
}

/// A shown window with a document of 20 pages of 500×700 points, but for a
/// fold-out map of 1551×1202 points as the 11th.
fn window_with_a_map() -> Tabs {
    let mut pages = vec![(500.0, 700.0); 20];
    pages[10] = (1551.0, 1202.0);
    let t = Tabs::new();
    t.open_pages("a.pdf", pages);
    t.window.set_sidebar_open(false);
    t.window.window().set_size(slint::LogicalSize::new(1000.0, 900.0));
    t.window.show().unwrap();
    pump(&t);
    t
}

/// The page nearest the top of the page area.
fn top_page(content: Rect, pages: &[Rect]) -> Rect {
    let (_, content_y, _, _) = content;
    *pages
        .iter()
        .filter(|(_, y, _, _)| *y >= content_y - 1.0)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .expect("no page in view")
}

#[test]
fn continuous_page_fit_ignores_an_outsized_page() {
    let t = window_with_a_map();
    t.viewer(0).fit_page();
    let (content, pages) = layout(&t);
    let (_, content_y, _, content_h) = content;
    let (_, page_y, _, page_h) = top_page(content, &pages);
    let row_gap = t.window.global::<PageLayout>().get_row_gap();
    assert_close(page_y - content_y, row_gap / 2.0, "the space above the first page");
    assert_close(page_h, content_h - row_gap, "the first page's height");
}

#[test]
fn continuous_mode_shrinks_an_outsized_page_to_the_usual_row() {
    let t = window_with_a_map();
    t.viewer(0).fit_page();
    let (content, pages) = layout(&t);
    let (_, _, page_w, _) = top_page(content, &pages);

    t.viewer(0).go_to_page("11");
    let (content, pages) = layout(&t);
    let (_, content_y, _, content_h) = content;
    let (_, map_y, map_w, map_h) = top_page(content, &pages);
    // The map is far wider than a page for its height, so the usual page's
    // width is what bounds it.
    assert_close(map_w, page_w, "the map's width");
    assert!(map_y + map_h <= content_y + content_h, "the map overflows its row");
}

#[test]
fn paged_page_fit_fits_each_page_on_its_own() {
    let t = window_with_a_map();
    t.viewer(0).set_continuous(false);
    t.viewer(0).fit_page();
    let ((content_x, content_y, content_w, content_h), pages) = layout(&t);
    let (_, page_y, _, page_h) = pages[0];
    assert_close(page_y, content_y, "the space above the first page");
    assert_close(page_h, content_h, "the first page's height");

    // The map is wider for its height than the view, so it fills the width.
    t.viewer(0).go_to_page("11");
    let (_, pages) = layout(&t);
    let (map_x, _, map_w, _) = pages[0];
    assert_close(map_x, content_x, "the space left of the map");
    assert_close(map_w, content_w, "the map's width");

    // Turning back to a usual page fits that page again.
    t.viewer(0).nav_page(1);
    let (_, pages) = layout(&t);
    let (_, page_y, _, page_h) = pages[0];
    assert_close(page_y, content_y, "the space above the page after the map");
    assert_close(page_h, content_h, "the height of the page after the map");
}
