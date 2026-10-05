#![cfg(feature = "testing")]

//! Pinching, on a trackpad or a touchscreen, zooms about the point between the
//! fingers, leaving the fit as + and - do. The pinches go through the window
//! as the platform sends them, and the pages are measured where the real
//! layout puts them.

use std::time::Duration;

use i_slint_backend_testing::ElementHandle;
use i_slint_core::input::{BackendMouseEvent, TouchPhase};
use i_slint_core::lengths::logical_point_from_api;
use melkkipdf::FitMode;
use melkkipdf::testing::{Harness, Tabs};
use slint::ComponentHandle;

/// A shown window with one document of ten 600×800 point pages, and the
/// sidebar closed so the page area is simply the window's.
fn shown() -> Tabs {
    let t = Tabs::new();
    t.open("a.pdf", 10);
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

/// The page area's top-left corner in window coordinates.
fn page_area(t: &Tabs) -> (f32, f32) {
    let content = ElementHandle::find_by_element_id(&t.window, "MainWindow::content")
        .next()
        .expect("no page area");
    let position = content.absolute_position();
    (position.x, position.y)
}

/// Sends one step of a pinch at the point `x`, `y` of the window, as the
/// platform reports it.
fn send_pinch(t: &Tabs, delta: f32, x: f32, y: f32, phase: TouchPhase) {
    let position = logical_point_from_api(slint::LogicalPosition::new(x, y));
    t.window.window().dispatch_event(slint::platform::WindowEvent::internal(
        BackendMouseEvent::PinchGesture { position, delta, phase },
    ));
}

/// Pinches by `scales`, each the step from the one before, at the point `x`,
/// `y` of the page area, as a trackpad reports a pinch: begun, moved and
/// ended.
fn pinch(t: &Tabs, x: f32, y: f32, scales: &[f32]) {
    let (left, top) = page_area(t);
    let (x, y) = (left + x, top + y);
    send_pinch(t, 0.0, x, y, TouchPhase::Started);
    for scale in scales {
        send_pinch(t, scale - 1.0, x, y, TouchPhase::Moved);
    }
    send_pinch(t, 0.0, x, y, TouchPhase::Ended);
    pump(t);
}

/// Where the point `x`, `y` of the page area falls on the page under it, as
/// fractions of the page's width and height.
fn on_page(t: &Tabs, x: f32, y: f32) -> (f32, f32) {
    let (left, top) = page_area(t);
    let (x, y) = (left + x, top + y);
    ElementHandle::find_by_element_type_name(&t.window, "PageImageView")
        .map(|page| (page.absolute_position(), page.size()))
        .find(|(at, size)| {
            (at.x..at.x + size.width).contains(&x) && (at.y..at.y + size.height).contains(&y)
        })
        .map(|(at, size)| ((x - at.x) / size.width, (y - at.y) / size.height))
        .expect("no page under the point")
}

/// Zooms in with + until the pages are wider than the view: narrower, a page
/// stays centred across it, so no point on it can stay put across.
fn zoom_past_the_width(t: &Tabs) {
    for _ in 0..3 {
        t.viewer(0).zoom_in();
    }
    pump(t);
}

#[track_caller]
fn assert_close(actual: f32, expected: f32, what: &str) {
    assert!((actual - expected).abs() < 2e-3, "{what} was {actual}, expected {expected}");
}

#[test]
fn a_pinch_scales_the_zoom_and_leaves_the_fit() {
    let t = shown();
    let before = t.viewer(0).settings();
    assert_eq!(before.fit, FitMode::Page);

    pinch(&t, 500.0, 450.0, &[1.25, 1.2]);
    let after = t.viewer(0).settings();
    assert_eq!(after.fit, FitMode::Free);
    assert!((after.zoom - before.zoom * 1.5).abs() < 1e-3, "zoomed to {}", after.zoom);

    // A pinch inwards takes it back down, from the zoom it begins at.
    pinch(&t, 500.0, 450.0, &[0.5]);
    assert!((t.viewer(0).settings().zoom - before.zoom * 0.75).abs() < 1e-3);
}

#[test]
fn the_place_under_the_fingers_stays_put_while_scrolling() {
    let t = shown();
    zoom_past_the_width(&t);
    // Off the middle, to the right and below, where zooming about the top or
    // the middle of the view would move it.
    let (x, y) = (700.0, 600.0);
    let before = on_page(&t, x, y);

    pinch(&t, x, y, &[1.5, 1.5]);
    let after = on_page(&t, x, y);
    assert_close(after.0, before.0, "the point across the page");
    assert_close(after.1, before.1, "the point down the page");

    // And back out, from a view now scrolled across.
    pinch(&t, x, y, &[0.8]);
    let out = on_page(&t, x, y);
    assert_close(out.0, before.0, "the point across the page, zoomed out");
    assert_close(out.1, before.1, "the point down the page, zoomed out");
}

#[test]
fn the_place_under_the_fingers_stays_put_in_paged_mode() {
    let t = shown();
    t.viewer(0).set_continuous(false);
    zoom_past_the_width(&t);
    let (x, y) = (700.0, 600.0);
    let before = on_page(&t, x, y);

    pinch(&t, x, y, &[1.5, 1.5]);
    let after = on_page(&t, x, y);
    assert_close(after.0, before.0, "the point across the page");
    assert_close(after.1, before.1, "the point down the page");
}

#[test]
fn the_pages_render_for_a_pinch_when_it_pauses_or_ends() {
    let h = Harness::uniform(5, 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    h.take_render_requests();

    h.viewer.pinch_started();
    h.viewer.pinch_moved(1.5, 500.0, 450.0);
    h.viewer.pinch_moved(2.0, 500.0, 450.0);
    assert!(h.take_render_requests().is_empty(), "rendered in the middle of a pinch");

    // Fingers held still, the pages are rendered for where they are.
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));
    assert!(!h.take_render_requests().is_empty(), "nothing rendered for a paused pinch");

    h.viewer.pinch_moved(2.5, 500.0, 450.0);
    h.viewer.pinch_ended();
    assert!(!h.take_render_requests().is_empty(), "nothing rendered once the pinch ended");
}

#[test]
fn a_scale_that_is_not_a_number_changes_nothing() {
    let h = Harness::uniform(5, 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    let zoom = h.viewer.settings().zoom;

    h.viewer.pinch_started();
    h.viewer.pinch_moved(f32::NAN, 500.0, 450.0);
    h.viewer.pinch_ended();
    assert_eq!(h.viewer.settings().zoom, zoom);
}
