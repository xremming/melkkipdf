//! Paged-mode within-page scrolling. Run with `cargo test --features testing`.
#![cfg(feature = "testing")]

use melkkipdf::testing::Harness;

/// Paged mode with the page taller than the viewport (so there is room to
/// scroll within it before paging).
fn paged_tall(count: usize) -> Option<Harness> {
    let harness = Harness::uniform(count, 600.0, 800.0)?;
    harness.viewport(1000.0, 400.0);
    harness.viewer.set_continuous(false);
    harness.viewer.fit_width(); // ~1300px tall page in a 400px viewport
    harness.viewer.nav_home();
    Some(harness)
}

#[test]
fn scrolling_stays_within_the_page() {
    let Some(h) = paged_tall(20) else {
        return;
    };
    assert_eq!(h.current_page(), 1);
    assert!((h.window.get_paged_offset_y() - 0.0).abs() < 0.5, "starts at the top");

    // A downward wheel (negative delta_y) scrolls down within the page.
    h.viewer.paged_scroll(0.0, -120.0, false);
    assert_eq!(h.current_page(), 1, "must not page while there is room to scroll");
    assert!(h.window.get_paged_offset_y() < 0.0, "content scrolled up");
}

#[test]
fn paging_forward_lands_at_the_top_of_the_next_page() {
    let Some(h) = paged_tall(20) else {
        return;
    };
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
    let Some(h) = paged_tall(20) else {
        return;
    };
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
    let Some(h) = Harness::uniform(20, 600.0, 800.0) else {
        return;
    };
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
    let Some(h) = Harness::uniform(20, 600.0, 800.0) else {
        return;
    };
    h.viewport(1000.0, 1400.0); // tall viewport: the whole page fits
    h.viewer.set_continuous(false);
    h.viewer.fit_page();
    h.viewer.nav_home();
    // Nothing to scroll within, so a downward wheel pages straight away.
    h.viewer.paged_scroll(0.0, -120.0, false);
    assert_eq!(h.current_page(), 2);
}
