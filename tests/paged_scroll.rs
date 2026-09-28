//! Paged-mode within-page scrolling. Run with `cargo test --features testing`.
#![cfg(feature = "testing")]

use std::time::Duration;

use melkkipdf::testing::Harness;

/// Paged mode with the page taller than the viewport (so there is room to
/// scroll within it before paging).
fn paged_tall(count: usize) -> Harness {
    let harness = Harness::uniform(count, 600.0, 800.0);
    harness.viewport(1000.0, 400.0);
    harness.viewer.set_continuous(false);
    harness.viewer.fit_width(); // ~1300px tall page in a 400px viewport
    harness.viewer.nav_home();
    harness
}

#[test]
fn scrolling_stays_within_the_page() {
    let h = paged_tall(20);
    assert_eq!(h.current_page(), 1);
    assert!((h.window.get_paged_offset_y() - 0.0).abs() < 0.5, "starts at the top");

    // A downward wheel (negative delta_y) scrolls down within the page.
    h.viewer.paged_scroll(0.0, -120.0, false);
    assert_eq!(h.current_page(), 1, "must not page while there is room to scroll");
    assert!(h.window.get_paged_offset_y() < 0.0, "content scrolled up");
}

#[test]
fn paging_forward_lands_at_the_top_of_the_next_page() {
    let h = paged_tall(20);
    // One huge scroll clamps at the page bottom without paging...
    h.viewer.paged_scroll(0.0, -100000.0, false);
    assert_eq!(h.current_page(), 1, "a single scroll clamps at the bottom, no page yet");
    assert!(h.window.get_paged_offset_y() < -0.5, "scrolled to the bottom of page 1");
    // ...and only the next downward scroll moves to the next page, at its top.
    h.viewer.paged_scroll(0.0, -120.0, false);
    assert_eq!(h.current_page(), 2, "scrolling at the bottom edge pages forward");
    assert!((h.window.get_paged_offset_y() - 0.0).abs() < 0.5, "lands at the top of page 2");
}

#[test]
fn scrolling_up_at_the_top_lands_at_the_bottom_of_the_previous_page() {
    let h = paged_tall(20);
    h.viewer.nav_page(1); // page 2, at its top
    assert_eq!(h.current_page(), 2);
    // At the top of page 2, scrolling up goes back to page 1...
    h.viewer.paged_scroll(0.0, 120.0, false);
    assert_eq!(h.current_page(), 1);
    // ...landing at the bottom of page 1, not its top.
    assert!(h.window.get_paged_offset_y() < -0.5, "should land at the bottom of the previous page");
}

#[test]
fn shift_wheel_scrolls_horizontally_without_paging() {
    let h = Harness::uniform(20, 600.0, 800.0);
    h.viewport(1000.0, 800.0);
    h.viewer.set_continuous(false);
    h.viewer.fit_width();
    h.viewer.zoom_in();
    h.viewer.zoom_in(); // page now wider than the viewport
    h.viewer.nav_home();

    let before = h.window.get_paged_offset_x();
    // Shift turns a vertical wheel into horizontal movement.
    h.viewer.paged_scroll(0.0, -120.0, true);
    assert!(h.window.get_paged_offset_x() < before, "shift-scroll moved horizontally");
    assert_eq!(h.current_page(), 1, "horizontal scrolling never pages");
}

#[test]
fn a_page_that_fits_pages_immediately() {
    let h = Harness::uniform(20, 600.0, 800.0);
    h.viewport(1000.0, 1400.0); // tall viewport: the whole page fits
    h.viewer.set_continuous(false);
    h.viewer.fit_page();
    h.viewer.nav_home();
    // Nothing to scroll within, so a downward wheel pages straight away.
    h.viewer.paged_scroll(0.0, -120.0, false);
    assert_eq!(h.current_page(), 2);
}

/// Paged mode with the whole page in view, so any push past it turns pages.
fn paged_fitting(count: usize) -> Harness {
    let harness = Harness::uniform(count, 600.0, 800.0);
    harness.viewport(1000.0, 1400.0);
    harness.viewer.set_continuous(false);
    harness.viewer.fit_page();
    harness.viewer.nav_home();
    harness
}

fn wait(milliseconds: u64) {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(milliseconds));
}

#[test]
fn small_trackpad_steps_add_up_to_a_page_turn() {
    let h = paged_fitting(20);
    for _ in 0..5 {
        h.viewer.paged_scroll(0.0, -10.0, false);
    }
    assert_eq!(h.current_page(), 1, "turned the page before the steps added up");
    h.viewer.paged_scroll(0.0, -10.0, false);
    assert_eq!(h.current_page(), 2);
}

#[test]
fn reversing_direction_starts_the_push_over() {
    let h = paged_fitting(20);
    h.viewer.nav_page(1);
    h.viewer.paged_scroll(0.0, -50.0, false);
    h.viewer.paged_scroll(0.0, 20.0, false);
    h.viewer.paged_scroll(0.0, -50.0, false);
    assert_eq!(h.current_page(), 2, "pushes in opposite directions added up");
}

#[test]
fn scrolling_right_after_a_page_turn_is_not_held_back() {
    let h = paged_fitting(20);
    h.viewer.paged_scroll(0.0, -60.0, false);
    assert_eq!(h.current_page(), 2);
    // Regression: a rest after each turn swallowed the next scroll, and a
    // steady stream of events kept extending it indefinitely.
    h.viewer.paged_scroll(0.0, -60.0, false);
    assert_eq!(h.current_page(), 3, "the scroll straight after a page turn was ignored");

    // A steady trackpad stream keeps turning a page per 60px it travels.
    for _ in 0..8 {
        h.viewer.paged_scroll(0.0, -30.0, false);
        wait(16);
    }
    assert_eq!(h.current_page(), 7);
}

#[test]
fn a_wheel_notch_turns_a_page_at_once() {
    let h = paged_fitting(20);
    h.viewer.paged_scroll(0.0, -60.0, false);
    assert_eq!(h.current_page(), 2);
}

#[test]
fn arrow_keys_turn_a_page_each() {
    let h = paged_fitting(20);
    for _ in 0..3 {
        h.viewer.nav_line(1);
    }
    assert_eq!(h.current_page(), 4);
}
