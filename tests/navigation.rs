//! Keyboard / wheel navigation behavior. Run with `cargo test --features testing`.
#![cfg(feature = "testing")]

use melkkipdf::Spread;
use melkkipdf::testing::Harness;

/// A harness with a known viewport, so fit and scroll math are defined.
fn setup(count: usize) -> Harness {
    let harness = Harness::uniform(count, 600.0, 800.0);
    harness.viewport(1000.0, 900.0);
    harness
}

#[test]
fn home_goes_to_the_first_page() {
    let h = setup(20);
    h.viewer.nav_page(1);
    h.viewer.nav_page(1);
    h.viewer.nav_home();
    assert_eq!(h.current_page(), 1);
    assert_eq!(h.scroll_y(), 0.0);
}

#[test]
fn end_reaches_the_last_page_when_fit_to_width() {
    let h = setup(449);
    h.viewer.fit_width();
    h.viewer.nav_end();
    assert_eq!(h.current_page(), 449);
}

#[test]
fn end_reaches_the_last_page_when_fit_to_page() {
    let h = setup(449);
    h.viewer.fit_page();
    h.viewer.nav_end();
    assert_eq!(h.current_page(), 449);
}

#[test]
fn end_reaches_the_last_page_in_spread_modes() {
    let h = setup(449);
    h.viewer.fit_width();

    h.viewer.set_spread(Spread::Even); // even
    h.viewer.nav_end();
    assert_eq!(h.current_page(), 449, "even spread End should reach the last page");

    h.viewer.set_spread(Spread::Odd); // odd
    h.viewer.nav_end();
    assert_eq!(h.current_page(), 449, "odd spread End should reach the last page");
}

#[test]
fn page_navigation_moves_one_page_at_a_time() {
    let h = setup(20);
    h.viewer.nav_home();
    assert_eq!(h.current_page(), 1);
    h.viewer.nav_page(1);
    assert_eq!(h.current_page(), 2);
    h.viewer.nav_page(1);
    assert_eq!(h.current_page(), 3);
    h.viewer.nav_page(-1);
    assert_eq!(h.current_page(), 2);
}

#[test]
fn page_navigation_clamps_at_both_ends() {
    let h = setup(5);
    h.viewer.nav_home();
    h.viewer.nav_page(-1);
    assert_eq!(h.current_page(), 1, "cannot page before the first page");
    h.viewer.nav_end();
    h.viewer.nav_page(1);
    assert_eq!(h.current_page(), 5, "cannot page past the last page");
}

#[test]
fn continuous_arrows_scroll_and_do_not_change_page() {
    let h = setup(20);
    h.viewer.fit_page(); // a whole page fits
    h.viewer.nav_home();
    assert_eq!(h.scroll_y(), 0.0);
    assert_eq!(h.current_page(), 1);

    h.viewer.nav_line(1); // arrow down
    assert!(h.scroll_y() < 0.0, "continuous mode should scroll on arrow down");
    assert_eq!(h.current_page(), 1, "continuous scrolling must not jump pages");
}

#[test]
fn continuous_arrow_scroll_steps_are_uniform() {
    let h = setup(20);
    h.viewer.nav_home();
    h.viewer.nav_line(1);
    let one = h.scroll_y();
    h.viewer.nav_line(1);
    let two = h.scroll_y();
    assert!(one < 0.0);
    // Two steps should be exactly twice one step.
    assert!((two - 2.0 * one).abs() < 0.5, "one={one}, two={two}");
}

#[test]
fn paged_arrows_change_pages_when_the_page_fits() {
    let h = Harness::uniform(20, 600.0, 800.0);
    h.viewport(1000.0, 1400.0); // tall viewport: the whole page fits
    h.viewer.set_continuous(false);
    h.viewer.fit_page();
    h.viewer.nav_home();
    assert_eq!(h.current_page(), 1);
    h.viewer.nav_line(1);
    assert_eq!(h.current_page(), 2, "paged arrow down moves to the next page");
    h.viewer.nav_line(-1);
    assert_eq!(h.current_page(), 1);
}

#[test]
fn scrolling_reports_the_page_at_the_top() {
    let h = setup(20);
    h.viewer.fit_width();
    // Jump to page 5, capture the offset, then reproduce it as a user scroll.
    h.viewer.go_to_page("5");
    let offset = -h.scroll_y();
    h.viewer.nav_home();
    assert_eq!(h.current_page(), 1);
    h.scroll_by_user(offset);
    assert_eq!(h.current_page(), 5);
}

#[test]
fn the_counter_keeps_the_page_at_the_top_until_it_leaves() {
    let h = setup(20);
    h.viewer.go_to_page("5");
    let offset = -h.scroll_y();
    let row_height = offset / 4.0;
    // Most of page 5 has scrolled past, but it is still the one at the top,
    // which is also the page a relayout would keep there.
    h.scroll_by_user(offset + row_height * 0.6);
    assert_eq!(h.current_page(), 5);
    h.viewer.set_spread(Spread::None);
    assert_eq!(h.current_page(), 5);
}

#[test]
fn paging_down_from_a_hair_short_of_a_page_moves_on() {
    let h = setup(20);
    h.viewer.go_to_page("4");
    let offset = -h.scroll_y();
    // A fit or the list's own layout can land a fraction of a pixel short of
    // a page's top, which is still that page.
    h.scroll_by_user(offset - 0.05);
    assert_eq!(h.current_page(), 4);
    h.viewer.nav_page(1);
    assert_eq!(h.current_page(), 5, "paging down stayed on the page at the top");
}
