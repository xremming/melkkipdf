//! Keyboard / wheel navigation behavior. Run with `cargo test --features testing`.
#![cfg(feature = "testing")]

use melkkipdf::Spread;
use melkkipdf::testing::{Harness, Tabs};
use slint::ComponentHandle;
use slint::platform::{Key, WindowEvent};

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

/// Presses and releases `key` in a shown window.
fn press(t: &Tabs, key: impl Into<slint::SharedString> + Clone) {
    let window = t.window.window();
    window.dispatch_event(WindowEvent::KeyPressed { text: key.clone().into() });
    window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
}

#[test]
fn hjkl_move_as_the_arrows_do() {
    let t = Tabs::new();
    t.open("a.pdf", 20);
    t.window.window().set_size(slint::LogicalSize::new(1000.0, 900.0));
    t.window.show().unwrap();
    // Paged, with the page fitting, so every key moves a whole page.
    t.viewer(0).set_continuous(false);
    t.viewer(0).fit_page();
    assert_eq!(t.window.get_current_page(), 1);

    let pairs = [
        ("j", Key::DownArrow, 1),
        ("k", Key::UpArrow, -1),
        ("l", Key::RightArrow, 1),
        ("h", Key::LeftArrow, -1),
    ];
    let mut page = 1;
    for (letter, arrow, step) in pairs {
        press(&t, letter);
        page += step;
        assert_eq!(t.window.get_current_page(), page, "{letter} did not move");
        press(&t, arrow);
        page += step;
        assert_eq!(t.window.get_current_page(), page, "{arrow:?} did not move");
    }
}

/// Presses and releases `key` with Shift held.
fn press_shifted(t: &Tabs, key: &str) {
    let window = t.window.window();
    window.dispatch_event(WindowEvent::KeyPressed { text: Key::Shift.into() });
    press(t, key);
    window.dispatch_event(WindowEvent::KeyReleased { text: Key::Shift.into() });
}

#[test]
fn shift_j_k_and_space_turn_pages_as_in_zathura() {
    let t = Tabs::new();
    t.open("a.pdf", 20);
    t.window.window().set_size(slint::LogicalSize::new(1000.0, 900.0));
    t.window.show().unwrap();
    t.viewer(0).set_continuous(false);
    t.viewer(0).fit_page();

    press_shifted(&t, "J");
    press_shifted(&t, "J");
    assert_eq!(t.window.get_current_page(), 3);
    press_shifted(&t, "K");
    assert_eq!(t.window.get_current_page(), 2);
    press(&t, " ");
    assert_eq!(t.window.get_current_page(), 3);
    press_shifted(&t, " ");
    assert_eq!(t.window.get_current_page(), 2);
}

#[test]
fn a_colon_types_a_page_number_into_the_page_field() {
    let t = Tabs::new();
    t.open("a.pdf", 20);
    t.window.window().set_size(slint::LogicalSize::new(1000.0, 900.0));
    t.window.show().unwrap();

    for key in [":", "1", "2"] {
        press(&t, key);
    }
    press(&t, Key::Return);
    assert_eq!(t.window.get_current_page(), 12, "the number did not reach the page field");

    // The keys are back with the document, where C switches modes.
    press(&t, "c");
    assert!(!t.window.get_continuous(), "the keys stayed with the page field");

    // Esc leaves the field without going anywhere.
    for key in [":", "5"] {
        press(&t, key);
    }
    press(&t, Key::Escape);
    press(&t, "c");
    assert!(t.window.get_continuous());
    assert_eq!(t.window.get_current_page(), 12);
}

#[test]
fn zoom_and_fit_have_single_keys() {
    let t = Tabs::new();
    t.open("a.pdf", 20);
    t.window.window().set_size(slint::LogicalSize::new(1000.0, 900.0));
    t.window.show().unwrap();
    t.viewer(0).set_continuous(false);

    press(&t, "s");
    let width = t.window.get_density();
    press(&t, "a");
    let page = t.window.get_density();
    assert!(page < width, "fitting the page should zoom out from fitting the width");
    press(&t, "+");
    assert!(t.window.get_density() > page);
    press(&t, "-");
    press(&t, "-");
    assert!(t.window.get_density() < page);
    press(&t, "=");
    // At 100% a point is a CSS pixel: 96 per inch over 72 points.
    assert!((t.window.get_density() - 96.0 / 72.0).abs() < 1e-4, "= is 100%");
}
