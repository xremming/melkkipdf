//! Go-to-page, mode switching, and zoom/fit. Run with `cargo test --features testing`.
#![cfg(feature = "testing")]

use melkkipdf::testing::Harness;

const BASE_DENSITY: f32 = 96.0 / 72.0;

fn setup(count: usize) -> Option<Harness> {
    let harness = Harness::uniform(count, 600.0, 800.0)?;
    harness.viewport(1000.0, 900.0);
    Some(harness)
}

#[test]
fn go_to_page_jumps_to_the_requested_page() {
    let Some(h) = setup(50) else {
        return;
    };
    h.viewer.go_to_page("25");
    assert_eq!(h.current_page(), 25);
}

#[test]
fn go_to_page_clamps_out_of_range() {
    let Some(h) = setup(50) else {
        return;
    };
    h.viewer.go_to_page("999");
    assert_eq!(h.current_page(), 50);
    h.viewer.go_to_page("0");
    assert_eq!(h.current_page(), 1);
    h.viewer.go_to_page("-4");
    assert_eq!(h.current_page(), 1);
}

#[test]
fn go_to_page_ignores_non_numeric_input() {
    let Some(h) = setup(50) else {
        return;
    };
    h.viewer.go_to_page("10");
    assert_eq!(h.current_page(), 10);
    h.viewer.go_to_page("abc");
    assert_eq!(h.current_page(), 10);
    h.viewer.go_to_page("");
    assert_eq!(h.current_page(), 10);
}

#[test]
fn toggle_continuous_flips_the_mode() {
    let Some(h) = setup(20) else {
        return;
    };
    assert!(h.continuous());
    h.viewer.toggle_continuous();
    assert!(!h.continuous());
    h.viewer.toggle_continuous();
    assert!(h.continuous());
}

#[test]
fn switching_to_paged_keeps_the_position() {
    let Some(h) = setup(50) else {
        return;
    };
    h.viewer.fit_width();
    h.viewer.go_to_page("30");
    assert_eq!(h.current_page(), 30);
    h.viewer.set_continuous(false);
    assert_eq!(h.current_page(), 30, "paged mode should keep the reading position");
}

#[test]
fn zoom_in_then_out_returns_to_the_start() {
    let Some(h) = setup(20) else {
        return;
    };
    h.viewer.zoom_reset();
    let base = h.density();
    h.viewer.zoom_in();
    assert!(h.density() > base, "zoom in increases density");
    h.viewer.zoom_out();
    assert!((h.density() - base).abs() < 0.001, "zoom out undoes zoom in");
}

#[test]
fn zoom_reset_is_base_density() {
    let Some(h) = setup(20) else {
        return;
    };
    h.viewer.zoom_in();
    h.viewer.zoom_in();
    h.viewer.zoom_reset();
    assert!((h.density() - BASE_DENSITY).abs() < 0.001);
}

#[test]
fn fit_width_makes_the_page_span_the_viewport() {
    let Some(h) = setup(20) else {
        return;
    };
    h.viewer.fit_width();
    // Page is 600pt wide; view is 1000 logical px with a 24px gutter.
    let page_px = 600.0 * h.density();
    assert!((page_px - 976.0).abs() < 5.0, "page width was {page_px}, expected ~976");
}

#[test]
fn fit_page_makes_the_page_fit_the_height() {
    let Some(h) = setup(20) else {
        return;
    };
    h.viewer.fit_page();
    // Portrait page in a wider-than-tall viewport: height is the constraint.
    let page_h = 800.0 * h.density();
    assert!(page_h <= 900.0, "page height {page_h} should fit the 900px viewport");
    assert!((page_h - 876.0).abs() < 5.0, "page height was {page_h}, expected ~876");
}
