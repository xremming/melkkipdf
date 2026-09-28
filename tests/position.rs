#![cfg(feature = "testing")]

//! Changing how pages are laid out must not lose the reader's place: switching
//! the spread, switching between continuous and paged, zooming, fitting and
//! resizing all keep the page being read at the top of the view. Uses Slint's
//! testing backend so the tests run without a display.

use melkkipdf::testing::Harness;

const PAGES: usize = 60;

fn setup() -> Harness {
    // The backend can be installed once per thread, and some tests here build
    // many windows on one.
    thread_local! {
        static INSTALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if !INSTALLED.replace(true) {
        i_slint_backend_testing::init_no_event_loop();
    }
    let h = Harness::uniform(PAGES, 600.0, 800.0).expect("failed to create the window");
    h.viewport(1000.0, 900.0);
    h
}

/// Does what the live list does whenever its offset changes: reports it back,
/// which is also what moves the page counter.
fn settle(h: &Harness) {
    if h.continuous() {
        h.viewer.scrolled(-h.scroll_y());
    }
}

/// Asserts the 0-based `page` is in the row at the top of the view once the
/// list has reported its offset.
#[track_caller]
fn assert_reading(h: &Harness, page: i32, context: &str) {
    settle(h);
    let top = h.pages_at_top();
    assert!(top.contains(&page), "{context}: reading page {} but the top row is {top:?}", page + 1);
    let counter = h.current_page() - 1;
    assert!(
        top.contains(&counter),
        "{context}: the counter says {} but the top row is {top:?}",
        counter + 1
    );
}

/// Opens at the 0-based `page`, checking the jump itself landed there.
fn go_to(h: &Harness, page: i32) {
    h.viewer.go_to_page(&(page + 1).to_string());
    assert_reading(h, page, "after go_to_page");
}

/// A named step in a sequence of view changes.
type Step<T> = (&'static str, fn(&T));

const SPREADS: [(i32, &str); 3] = [(0, "single"), (1, "odd"), (2, "even")];
/// A left page in odd spreads, a right page in odd spreads, and one near the end.
const START_PAGES: [i32; 3] = [10, 11, 57];

#[test]
fn changing_the_spread_keeps_the_page() {
    for continuous in [true, false] {
        for (from, from_name) in SPREADS {
            for (to, to_name) in SPREADS {
                for page in START_PAGES {
                    let h = setup();
                    h.viewer.set_continuous(continuous);
                    h.viewer.set_spread(from);
                    go_to(&h, page);
                    h.viewer.set_spread(to);
                    let mode = if continuous { "continuous" } else { "paged" };
                    assert_reading(&h, page, &format!("{mode}, {from_name} to {to_name}"));
                }
            }
        }
    }
}

#[test]
fn switching_between_continuous_and_paged_keeps_the_page() {
    for (spread, name) in SPREADS {
        for page in START_PAGES {
            let h = setup();
            h.viewer.set_spread(spread);
            go_to(&h, page);
            h.viewer.set_continuous(false);
            assert_reading(&h, page, &format!("{name}, to paged"));
            h.viewer.set_continuous(true);
            assert_reading(&h, page, &format!("{name}, back to continuous"));
        }
    }
}

#[test]
fn cycling_through_every_mode_keeps_the_page() {
    let h = setup();
    go_to(&h, 23);
    for step in 0..12 {
        match step % 4 {
            0 => h.viewer.set_spread(1),
            1 => h.viewer.toggle_continuous(),
            2 => h.viewer.set_spread(2),
            _ => h.viewer.set_spread(0),
        }
        assert_reading(&h, 23, &format!("step {step}"));
    }
}

#[test]
fn zooming_and_fitting_keep_the_page() {
    for continuous in [true, false] {
        let h = setup();
        h.viewer.set_continuous(continuous);
        go_to(&h, 30);
        let steps: [Step<Harness>; 5] = [
            ("zoom in", |h| h.viewer.zoom_in()),
            ("zoom in again", |h| h.viewer.zoom_in()),
            ("zoom out", |h| h.viewer.zoom_out()),
            ("page width", |h| h.viewer.fit_width()),
            ("page fit", |h| h.viewer.fit_page()),
        ];
        for (name, step) in steps {
            step(&h);
            assert_reading(&h, 30, name);
        }
    }
}

#[test]
fn resizing_the_window_keeps_the_page() {
    let h = setup();
    h.viewer.fit_width();
    go_to(&h, 30);
    for (width, height) in [(600.0, 900.0), (1400.0, 700.0), (1000.0, 900.0)] {
        h.viewport(width, height);
        assert_reading(&h, 30, &format!("resized to {width}x{height}"));
    }
}

#[test]
fn a_page_scrolled_partway_into_stays_on_top() {
    let h = setup();
    go_to(&h, 11);
    // Scroll a third of a row further, as the wheel would, then regroup.
    let row = -h.scroll_y() / 11.0;
    h.scroll_by_user(-h.scroll_y() + row / 3.0);
    assert_reading(&h, 11, "scrolled partway");
    h.viewer.set_spread(1);
    assert_reading(&h, 11, "odd spread");
    h.viewer.set_spread(0);
    assert_reading(&h, 11, "single again");
}

// The tests below run the real list view: the window is shown and laid out
// between steps, so the list reports its offset back through the app the way
// it does on screen, including where its own layout moves it.

use std::time::Duration;

use melkkipdf::testing::{Tabs, pages_at_top};
use slint::ComponentHandle;

/// A shown window with the app wired up, sized like a small desktop window.
fn live() -> Tabs {
    setup();
    let t = Tabs::new().expect("failed to create the window");
    t.window.window().set_size(slint::LogicalSize::new(1000.0, 900.0));
    t.window.show().unwrap();
    t
}

/// Lets the window lay itself out and run what that triggers, a few rounds
/// over, as successive frames would.
fn pump(t: &Tabs) {
    for _ in 0..6 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
        // Looking the rows up lays the list out, as drawing a frame would.
        let _ = i_slint_backend_testing::ElementHandle::find_by_element_type_name(
            &t.window,
            "PageRowView",
        )
        .count();
    }
}

#[track_caller]
fn assert_live_reading(t: &Tabs, page: i32, context: &str) {
    pump(t);
    let top = pages_at_top(&t.window);
    assert!(top.contains(&page), "{context}: reading page {} but the top row is {top:?}", page + 1);
    assert_eq!(t.window.get_current_page(), page + 1, "{context}: the counter moved");
}

#[test]
fn live_spread_changes_keep_the_page() {
    for (from, from_name) in SPREADS {
        for (to, to_name) in SPREADS {
            for page in [29, 30] {
                let t = live();
                t.open("a.pdf", PAGES);
                pump(&t);
                t.viewer(0).set_spread(from);
                t.viewer(0).go_to_page(&(page + 1).to_string());
                assert_live_reading(&t, page, &format!("{from_name}, after go_to_page"));
                t.viewer(0).set_spread(to);
                assert_live_reading(&t, page, &format!("{from_name} to {to_name}"));
            }
        }
    }
}

#[test]
fn live_mode_zoom_and_fit_changes_keep_the_page() {
    let t = live();
    t.open("a.pdf", PAGES);
    pump(&t);
    t.viewer(0).go_to_page("31");
    let steps: [Step<Tabs>; 8] = [
        ("odd spread", |t| t.viewer(0).set_spread(1)),
        ("paged", |t| t.viewer(0).set_continuous(false)),
        ("continuous", |t| t.viewer(0).set_continuous(true)),
        ("zoom in", |t| t.viewer(0).zoom_in()),
        ("page width", |t| t.viewer(0).fit_width()),
        ("even spread", |t| t.viewer(0).set_spread(2)),
        ("zoom out", |t| t.viewer(0).zoom_out()),
        ("single", |t| t.viewer(0).set_spread(0)),
    ];
    for (name, step) in steps {
        step(&t);
        assert_live_reading(&t, 30, name);
    }
}

#[test]
fn live_switching_tabs_keeps_each_tabs_page() {
    let t = live();
    // Different page shapes give the two documents different row heights,
    // which is what trips the list up when one replaces the other.
    t.open("portrait.pdf", PAGES);
    pump(&t);
    t.viewer(0).go_to_page("41");
    pump(&t);
    t.open_sized("landscape.pdf", 90, 800.0, 450.0);
    pump(&t);
    t.viewer(1).go_to_page("12");
    pump(&t);

    for round in 0..3 {
        t.window.invoke_select_tab(0);
        assert_live_reading(&t, 40, &format!("round {round}, portrait"));
        t.window.invoke_select_tab(1);
        assert_live_reading(&t, 11, &format!("round {round}, landscape"));
    }
}
