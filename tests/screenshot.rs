#![cfg(feature = "testing")]

//! Taking screenshots: the mode the keys and the toolbar turn on, the page a
//! click takes and the part of one a drag takes, cut and snapped to the
//! page's edges, and the image that lands on the clipboard. Uses Slint's
//! testing backend so the tests run without a display.

mod common;

use common::{Scratch, write_text_pdf};
use melkkipdf::testing::{Area, Harness, Shot, Tabs};
use slint::ComponentHandle;
use slint::platform::{Key, WindowEvent};

const PAGE: (f32, f32) = (600.0, 800.0);

fn harness() -> Harness {
    let h = Harness::uniform(3, PAGE.0, PAGE.1);
    h.viewport(1000.0, 900.0);
    h.set_screenshot_mode(true);
    h
}

/// Drags from one point to another on `page`, both in points from its
/// corner, and lets go.
fn drag(h: &Harness, page: usize, from: (f32, f32), to: (f32, f32)) {
    h.viewer.capture_from(page, from.0, from.1);
    h.viewer.capture_to(to.0, to.1);
    h.viewer.capture_done();
}

fn area(x: f32, y: f32, width: f32, height: f32) -> Area {
    Area { x, y, width, height }
}

#[test]
fn a_click_takes_the_whole_page_and_leaves_the_mode() {
    let h = harness();
    h.viewer.capture_from(1, 100.0, 200.0);
    assert_eq!(h.screenshot_outline(), Some((1, area(100.0, 200.0, 0.0, 0.0), false)));
    h.viewer.capture_done();

    let requests = h.take_screenshot_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].page, 1);
    assert_eq!(requests[0].shot, Shot::Page);
    // The whole page flashes, and the next press selects text again.
    assert_eq!(h.screenshot_outline(), Some((1, area(0.0, 0.0, PAGE.0, PAGE.1), true)));
    assert!(!h.screenshot_mode());
    assert!(!h.viewer.capturing());
}

#[test]
fn a_drag_takes_the_part_of_the_page_it_outlines() {
    let h = harness();
    h.viewer.capture_from(0, 100.0, 100.0);
    h.viewer.capture_to(300.0, 150.0);
    assert_eq!(h.screenshot_outline(), Some((0, area(100.0, 100.0, 200.0, 50.0), false)));
    h.viewer.capture_to(50.0, 200.0);
    assert_eq!(h.screenshot_outline(), Some((0, area(50.0, 100.0, 50.0, 100.0), false)));
    h.viewer.capture_done();

    let requests = h.take_screenshot_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].shot, Shot::Area(area(50.0, 100.0, 50.0, 100.0)));
    assert_eq!(h.screenshot_outline(), Some((0, area(50.0, 100.0, 50.0, 100.0), true)));
}

#[test]
fn a_drag_past_the_page_is_cut_at_its_edge_and_snaps_near_it() {
    let h = harness();
    // Off the page to the right and above it, on to the next page.
    drag(&h, 0, (100.0, 100.0), (900.0, 1000.0));
    let requests = h.take_screenshot_requests();
    assert_eq!(requests[0].page, 0);
    assert_eq!(requests[0].shot, Shot::Area(area(100.0, 100.0, 500.0, 700.0)));

    // Within a few pixels of the left and right edges at 100% zoom, where
    // a point is 96/72 pixels.
    h.set_screenshot_mode(true);
    drag(&h, 1, (3.0, 100.0), (597.0, 200.0));
    let requests = h.take_screenshot_requests();
    assert_eq!(requests[0].shot, Shot::Area(area(0.0, 100.0, PAGE.0, 100.0)));
}

#[test]
fn a_press_selects_text_until_the_mode_is_on() {
    let t = Tabs::new();
    t.open("a.pdf", 2);
    t.index_text(0, &[&["the cat sat"], &[]]);
    let mode = t.window.global::<melkkipdf::testing::Screenshot>();
    let viewer = t.viewer(0);
    // From the 't' to the middle of the 'c': the harness's text starts 72pt
    // in and 72pt down, 7pt a character.
    t.window.invoke_select_from(0, 74.0, 79.0);
    assert!(!viewer.capturing());
    t.window.invoke_select_to(0, 105.0, 79.0);
    t.window.invoke_select_done();
    assert!(t.take_screenshot_requests().is_empty());
    assert_eq!(viewer.selected_text().as_deref(), Some("the c"));

    // In the mode the same press captures, through the same callbacks.
    mode.set_active(true);
    t.window.invoke_select_from(0, 74.0, 79.0);
    assert!(viewer.capturing());
    t.window.invoke_select_to(0, 174.0, 179.0);
    t.window.invoke_select_done();
    let requests = t.take_screenshot_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].shot, Shot::Area(area(74.0, 79.0, 100.0, 100.0)));
    assert!(!mode.get_active());
}

#[test]
fn escape_leaves_the_mode_and_gives_up_the_drag() {
    let t = Tabs::new();
    t.open("a.pdf", 2);
    let mode = t.window.global::<melkkipdf::testing::Screenshot>();
    let viewer = t.viewer(0);
    mode.set_active(true);
    viewer.capture_from(0, 100.0, 100.0);
    viewer.capture_to(300.0, 150.0);
    press(&t, Key::Escape, false, false);
    assert!(!mode.get_active());
    assert!(!viewer.capturing());
    assert!(!mode.get_dragging());
    assert!(t.take_screenshot_requests().is_empty());
}

#[test]
fn a_tab_in_the_background_gives_up_its_drag() {
    let h = harness();
    h.viewer.capture_from(0, 100.0, 100.0);
    h.viewer.deactivate();
    assert!(!h.viewer.capturing());
    assert!(h.take_screenshot_requests().is_empty());
}

fn press(t: &Tabs, key: impl Into<slint::SharedString> + Clone, control: bool, shift: bool) {
    let window = t.window.window();
    if control {
        window.dispatch_event(WindowEvent::KeyPressed { text: Key::Control.into() });
    }
    if shift {
        window.dispatch_event(WindowEvent::KeyPressed { text: Key::Shift.into() });
    }
    window.dispatch_event(WindowEvent::KeyPressed { text: key.clone().into() });
    window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    if shift {
        window.dispatch_event(WindowEvent::KeyReleased { text: Key::Shift.into() });
    }
    if control {
        window.dispatch_event(WindowEvent::KeyReleased { text: Key::Control.into() });
    }
}

#[test]
fn the_keys_turn_the_mode_on_and_off() {
    let t = Tabs::new();
    t.open("a.pdf", 2);
    let mode = || t.window.global::<melkkipdf::testing::Screenshot>().get_active();

    press(&t, "y", false, false);
    assert!(mode());
    press(&t, Key::Escape, false, false);
    assert!(!mode());

    press(&t, "C", true, true);
    assert!(mode());
    press(&t, "C", true, true);
    assert!(!mode());
    // Ctrl+C alone still copies, rather than turning the mode on.
    press(&t, "c", true, false);
    assert!(!mode());
}

#[test]
fn a_real_page_and_a_part_of_it_land_on_the_clipboard_at_screenshot_resolution() {
    let directory = Scratch::new("screenshot-real");
    let path = directory.join("book.pdf");
    write_text_pdf(&path, &[&["All aboard the Orient Express"]]);

    let t = Tabs::new();
    t.open_file(&path);
    let viewer = t.viewer(0);
    t.window.global::<melkkipdf::testing::Screenshot>().set_active(true);

    // US Letter is 612×792 points, which at 300 dots per inch is 2550×3300.
    viewer.capture_from(0, 100.0, 100.0);
    viewer.capture_done();
    t.finish_screenshot();
    let (width, height) = t.copied_image().expect("the page was not copied");
    assert!((2549..=2551).contains(&width), "the page is {width} pixels wide");
    assert!((3299..=3301).contains(&height), "the page is {height} pixels tall");

    // A 200×100 point area is rendered at the same 300 dots per inch as the
    // page, so it stays in proportion to it: about 833×417 pixels, rounded
    // outward to whole pixels.
    t.window.global::<melkkipdf::testing::Screenshot>().set_active(true);
    viewer.capture_from(0, 100.0, 100.0);
    viewer.capture_to(300.0, 200.0);
    viewer.capture_done();
    t.finish_screenshot();
    let (width, height) = t.copied_image().expect("the area was not copied");
    assert!((833..=835).contains(&width), "the area is {width} pixels wide");
    assert!((416..=418).contains(&height), "the area is {height} pixels tall");
}
