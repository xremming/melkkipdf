#![cfg(feature = "testing")]

//! Browser-style tabs: opening, switching and closing documents, and the tab
//! strip's layout. Uses Slint's testing backend so the tests run without a
//! display.

use i_slint_backend_testing::ElementHandle;
use melkkipdf::testing::Tabs;
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, Image, Model, Rgb8Pixel, SharedPixelBuffer, SharedString};

fn tabs() -> Tabs {
    Tabs::new()
}

/// A rendered page, told apart from an unrendered one by its nonzero size.
fn rendered() -> Image {
    Image::from_rgb8(SharedPixelBuffer::<Rgb8Pixel>::new(1, 1))
}

/// Whether the window's first row shows a rendered image.
fn first_row_rendered(tabs: &Tabs) -> bool {
    let row = tabs.window.get_rows().row_data(0).expect("no rows");
    row.left.image.size().width > 0
}

#[test]
fn starts_empty_with_the_open_prompt() {
    let t = tabs();
    assert!(t.titles().is_empty());
    assert_eq!(t.active_tab(), -1);
    assert_eq!(t.window.get_page_count(), 0);
    assert_eq!(t.window.get_status(), "Open a PDF to get started.");
}

#[test]
fn opening_adds_a_tab_and_shows_it() {
    let t = tabs();
    t.open("a.pdf", 3);
    t.open("b.pdf", 5);
    assert_eq!(t.titles(), ["a.pdf", "b.pdf"]);
    assert_eq!(t.active_tab(), 1);
    assert_eq!(t.window.get_page_count(), 5);
    assert_eq!(t.window.get_doc_title(), "b.pdf");
}

#[test]
fn switching_back_restores_the_tabs_view() {
    let t = tabs();
    t.open("a.pdf", 10);
    let a = t.viewer(0);
    a.set_continuous(false);
    a.go_to_page("4");
    a.zoom_in();
    let density = t.window.get_density();

    // A new document starts in the default view, whatever the last tab showed.
    t.open("b.pdf", 5);
    assert!(t.window.get_continuous());
    assert_eq!(t.window.get_current_page(), 1);
    assert_ne!(t.window.get_density(), density);

    t.window.invoke_select_tab(0);
    assert_eq!(t.active_tab(), 0);
    assert!(!t.window.get_continuous());
    assert_eq!(t.window.get_current_page(), 4);
    assert_eq!(t.window.get_density(), density);
    assert_eq!(t.window.get_page_count(), 10);
    assert_eq!(t.window.get_doc_title(), "a.pdf");
}

#[test]
fn switching_back_restores_the_scroll_position() {
    let t = tabs();
    t.open("a.pdf", 20);
    t.viewer(0).set_viewport(800.0, 600.0);
    t.viewer(0).go_to_page("7");
    let scroll = t.window.get_scroll_y();
    assert!(scroll < 0.0, "go_to_page did not scroll");

    t.open("b.pdf", 20);
    assert_eq!(t.window.get_scroll_y(), 0.0);

    t.window.invoke_select_tab(0);
    assert_eq!(t.window.get_scroll_y(), scroll);
    assert_eq!(t.window.get_current_page(), 7);
}

#[test]
fn renders_for_a_background_tab_land_in_that_tab() {
    let t = tabs();
    let a = t.open("a.pdf", 3);
    t.open("b.pdf", 3);

    // Every worker reports through the one window, so a page finished for a
    // background tab must not show up in the tab on screen.
    t.window.invoke_page_rendered(a, 0, 1.0, rendered());
    assert!(!first_row_rendered(&t));

    t.window.invoke_select_tab(0);
    assert!(first_row_rendered(&t));
}

#[test]
fn closing_the_active_tab_shows_its_right_neighbour() {
    let t = tabs();
    t.open("a.pdf", 1);
    t.open("b.pdf", 1);
    t.open("c.pdf", 1);
    t.window.invoke_select_tab(1);

    t.window.invoke_close_tab(1);
    assert_eq!(t.titles(), ["a.pdf", "c.pdf"]);
    assert_eq!(t.active_tab(), 1);
    assert_eq!(t.window.get_doc_title(), "c.pdf");
}

#[test]
fn closing_the_last_tab_shows_its_left_neighbour() {
    let t = tabs();
    t.open("a.pdf", 1);
    t.open("b.pdf", 1);

    t.window.invoke_close_tab(1);
    assert_eq!(t.titles(), ["a.pdf"]);
    assert_eq!(t.active_tab(), 0);
    assert_eq!(t.window.get_doc_title(), "a.pdf");
}

#[test]
fn closing_a_tab_before_the_active_one_keeps_it_shown() {
    let t = tabs();
    t.open("a.pdf", 1);
    t.open("b.pdf", 1);
    t.open("c.pdf", 4);

    t.window.invoke_close_tab(0);
    assert_eq!(t.titles(), ["b.pdf", "c.pdf"]);
    assert_eq!(t.active_tab(), 1);
    assert_eq!(t.window.get_doc_title(), "c.pdf");
    assert_eq!(t.window.get_page_count(), 4);
}

#[test]
fn closing_every_tab_returns_to_the_empty_window() {
    let t = tabs();
    let a = t.open("a.pdf", 3);

    t.window.invoke_close_tab(0);
    assert!(t.titles().is_empty());
    assert_eq!(t.active_tab(), -1);
    assert_eq!(t.window.get_page_count(), 0);
    assert_eq!(t.window.get_rows().row_count(), 0);
    assert_eq!(t.window.get_doc_title(), "");
    assert_eq!(t.window.get_status(), "Open a PDF to get started.");

    // A render the closed tab's worker finished late has nowhere to go.
    t.window.invoke_page_rendered(a, 0, 1.0, rendered());
    assert_eq!(t.window.get_rows().row_count(), 0);
}

/// Presses and releases `key` with `modifiers` held.
fn press(t: &Tabs, modifiers: &[Key], key: impl Into<SharedString> + Clone) {
    let window = t.window.window();
    for modifier in modifiers {
        window.dispatch_event(WindowEvent::KeyPressed { text: (*modifier).into() });
    }
    window.dispatch_event(WindowEvent::KeyPressed { text: key.clone().into() });
    window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    for modifier in modifiers.iter().rev() {
        window.dispatch_event(WindowEvent::KeyReleased { text: (*modifier).into() });
    }
}

/// Four documents open, with the last one shown.
fn four_tabs() -> Tabs {
    let t = tabs();
    for name in ["a.pdf", "b.pdf", "c.pdf", "d.pdf"] {
        t.open(name, 3);
    }
    show(&t, 1200.0);
    t
}

/// The key tab shortcuts are pressed with: Cmd on macOS, which Slint reports
/// as Control, and Control elsewhere.
const COMMAND: Key = Key::Control;

/// The key that cycles through the tabs with Tab: Control everywhere, which
/// Slint reports as Meta on macOS.
const CYCLE: Key = if cfg!(target_os = "macos") { Key::Meta } else { Key::Control };

#[test]
fn a_number_with_command_shows_that_tab_and_nine_the_last() {
    let t = four_tabs();
    press(&t, &[COMMAND], "1");
    assert_eq!(t.active_tab(), 0);
    press(&t, &[COMMAND], "3");
    assert_eq!(t.active_tab(), 2);
    // A number past the last tab changes nothing.
    press(&t, &[COMMAND], "5");
    assert_eq!(t.active_tab(), 2);
    press(&t, &[COMMAND], "9");
    assert_eq!(t.active_tab(), 3);
    // Without the modifier, the number still picks a spread mode.
    press(&t, &[], "1");
    assert_eq!(t.active_tab(), 3);
}

#[test]
fn control_tab_cycles_through_the_tabs_both_ways() {
    let t = four_tabs();
    press(&t, &[CYCLE], Key::Tab);
    assert_eq!(t.active_tab(), 0, "Ctrl+Tab did not go round to the first tab");
    press(&t, &[CYCLE, Key::Shift], Key::Tab);
    assert_eq!(t.active_tab(), 3, "Ctrl+Shift+Tab did not go round to the last tab");
    press(&t, &[CYCLE, Key::Shift], Key::Tab);
    assert_eq!(t.active_tab(), 2);
}

#[cfg(target_os = "macos")]
#[test]
fn command_tab_is_left_to_the_system_on_macos() {
    let t = four_tabs();
    press(&t, &[COMMAND], Key::Tab);
    assert_eq!(t.active_tab(), 3);
}

/// Shows the window at `width` so the tab strip is laid out.
fn show(t: &Tabs, width: f32) {
    t.window.window().set_size(slint::LogicalSize::new(width, 800.0));
    t.window.show().expect("failed to show the window");
}

fn document_tabs(t: &Tabs) -> Vec<ElementHandle> {
    ElementHandle::find_by_element_type_name(&t.window, "DocumentTab").collect()
}

fn open_button(t: &Tabs) -> ElementHandle {
    ElementHandle::find_by_element_id(&t.window, "MainWindow::open-frame")
        .next()
        .expect("no open button")
}

#[test]
fn a_single_document_still_gets_a_tab() {
    let t = tabs();
    t.open("a.pdf", 1);
    show(&t, 1600.0);
    assert_eq!(document_tabs(&t).len(), 1);
}

#[test]
fn open_button_sits_left_of_the_tabs() {
    let t = tabs();
    t.open("a.pdf", 1);
    t.open("b.pdf", 1);
    show(&t, 1600.0);

    let tabs = document_tabs(&t);
    let first = tabs.first().expect("no tabs");
    let open = open_button(&t);

    // The button leads the strip, and the tabs follow it rather than being
    // spread across the window, since they stop at their cap.
    assert_eq!(open.absolute_position().x, 8.0, "the open button should lead the strip");
    let gap = first.absolute_position().x - (open.absolute_position().x + open.size().width);
    assert_eq!(gap, 6.0, "the tabs should follow the open button");
    assert_eq!(first.size().width, 220.0);

    let tab_middle = first.absolute_position().y + first.size().height / 2.0;
    let open_middle = open.absolute_position().y + open.size().height / 2.0;
    assert_eq!(open_middle, tab_middle, "the open button should share the tabs' line");
}

#[test]
fn open_button_stays_put_with_no_tabs() {
    let t = tabs();
    show(&t, 1600.0);
    assert_eq!(open_button(&t).absolute_position().x, 8.0);
}

#[test]
fn many_tabs_narrow_to_fit_the_window() {
    let t = tabs();
    for index in 0..20 {
        t.open(&format!("{index}.pdf"), 1);
    }
    show(&t, 800.0);

    // Twenty tabs at their full width would need 4400px; they have to share
    // the 800px instead, so the last one is still on screen.
    let tabs = document_tabs(&t);
    let last = tabs.last().expect("no tabs");
    assert!(
        last.absolute_position().x + last.size().width <= 800.0,
        "the last tab was pushed off screen to {}",
        last.absolute_position().x
    );
    for tab in &tabs {
        assert!(tab.size().width < 220.0);
    }
}

#[test]
fn a_negative_tab_index_closes_and_selects_nothing() {
    let t = tabs();
    t.open("a.pdf", 1);
    t.open("b.pdf", 1);
    t.window.invoke_close_tab(-1);
    t.window.invoke_select_tab(-1);
    assert_eq!(t.titles(), ["a.pdf", "b.pdf"]);
    assert_eq!(t.active_tab(), 1);
}

#[test]
fn moving_a_tab_keeps_the_shown_one_shown() {
    let t = tabs();
    for title in ["a.pdf", "b.pdf", "c.pdf", "d.pdf"] {
        t.open(title, 1);
    }
    t.window.invoke_select_tab(2);

    // Moving the shown tab takes it along.
    t.window.invoke_move_tab(2, 0);
    assert_eq!(t.titles(), ["c.pdf", "a.pdf", "b.pdf", "d.pdf"]);
    assert_eq!(t.active_tab(), 0);

    // Moving a tab past the shown one shifts it over by one.
    t.window.invoke_move_tab(3, 0);
    assert_eq!(t.titles(), ["d.pdf", "c.pdf", "a.pdf", "b.pdf"]);
    assert_eq!(t.active_tab(), 1);
    t.window.invoke_move_tab(0, 3);
    assert_eq!(t.titles(), ["c.pdf", "a.pdf", "b.pdf", "d.pdf"]);
    assert_eq!(t.active_tab(), 0);
    assert_eq!(t.window.get_doc_title(), "c.pdf");

    // Closing still closes the tab now at that place.
    t.window.invoke_close_tab(1);
    assert_eq!(t.titles(), ["c.pdf", "b.pdf", "d.pdf"]);
}

#[test]
fn moving_a_tab_out_of_range_does_nothing() {
    let t = tabs();
    t.open("a.pdf", 1);
    t.open("b.pdf", 1);
    t.window.invoke_move_tab(0, 2);
    t.window.invoke_move_tab(-1, 0);
    assert_eq!(t.titles(), ["a.pdf", "b.pdf"]);
    assert_eq!(t.active_tab(), 1);
}

/// Presses the left button at `from`, moves the pointer to each of `through`
/// in turn and lets go at the last.
fn drag(t: &Tabs, from: (f32, f32), through: &[(f32, f32)]) {
    let window = t.window.window();
    let at = |(x, y): (f32, f32)| slint::LogicalPosition::new(x, y);
    window.dispatch_event(WindowEvent::PointerMoved { position: at(from) });
    window.dispatch_event(WindowEvent::PointerPressed {
        position: at(from),
        button: PointerEventButton::Left,
    });
    for &point in through {
        window.dispatch_event(WindowEvent::PointerMoved { position: at(point) });
    }
    let last = *through.last().unwrap_or(&from);
    window.dispatch_event(WindowEvent::PointerReleased {
        position: at(last),
        button: PointerEventButton::Left,
    });
}

/// The middle of the tab at `index` in the strip.
fn tab_middle(t: &Tabs, index: usize) -> (f32, f32) {
    let tab = &document_tabs(t)[index];
    let (position, size) = (tab.absolute_position(), tab.size());
    (position.x + size.width / 2.0, position.y + size.height / 2.0)
}

#[test]
fn dragging_a_tab_along_the_strip_reorders_it() {
    let t = tabs();
    for title in ["a.pdf", "b.pdf", "c.pdf", "d.pdf"] {
        t.open(title, 1);
    }
    t.window.invoke_select_tab(3);
    show(&t, 1600.0);

    // Dragged past the middle of the tab two places over, it lands there.
    let (x, y) = tab_middle(&t, 0);
    let (target, _) = tab_middle(&t, 2);
    drag(&t, (x, y), &[(x + 20.0, y), (target + 20.0, y + 10.0)]);
    assert_eq!(t.titles(), ["b.pdf", "c.pdf", "a.pdf", "d.pdf"]);
    assert_eq!(t.active_tab(), 2, "the dragged tab was not the one shown");
    assert_eq!(t.window.get_doc_title(), "a.pdf");

    // And back to the start, however far past the end of the strip it goes.
    let (x, y) = tab_middle(&t, 2);
    drag(&t, (x, y), &[(x - 20.0, y), (0.0, y)]);
    assert_eq!(t.titles(), ["a.pdf", "b.pdf", "c.pdf", "d.pdf"]);
    assert_eq!(t.active_tab(), 0);
}

#[test]
fn a_tab_let_go_short_of_its_neighbours_middle_stays_put() {
    let t = tabs();
    for title in ["a.pdf", "b.pdf", "c.pdf"] {
        t.open(title, 1);
    }
    show(&t, 1600.0);

    let (x, y) = tab_middle(&t, 1);
    drag(&t, (x, y), &[(x + 20.0, y), (x + 100.0, y)]);
    assert_eq!(t.titles(), ["a.pdf", "b.pdf", "c.pdf"]);
    assert_eq!(t.active_tab(), 1, "pressing a tab did not select it");

    // A click that wobbles a little is only a click, and the tabs are
    // still dragged normally after it.
    let (x, y) = tab_middle(&t, 0);
    drag(&t, (x, y), &[(x + 3.0, y)]);
    assert_eq!(t.titles(), ["a.pdf", "b.pdf", "c.pdf"]);
    assert_eq!(t.active_tab(), 0);
    let (target, _) = tab_middle(&t, 1);
    drag(&t, (x, y), &[(x + 20.0, y), (target + 20.0, y)]);
    assert_eq!(t.titles(), ["b.pdf", "a.pdf", "c.pdf"]);
}

#[test]
fn the_dragged_tab_follows_the_pointer_and_the_others_make_room() {
    let t = tabs();
    for title in ["a.pdf", "b.pdf", "c.pdf"] {
        t.open(title, 1);
    }
    show(&t, 1600.0);
    // Where each tab is drawn, by title in tab order, since a raised tab
    // comes up in a different order.
    let faces = || -> Vec<f32> {
        let mut titles: Vec<(String, f32)> =
            ElementHandle::find_by_element_id(&t.window, "DocumentTab::tab-title")
                .map(|title| {
                    let label = title.accessible_label().unwrap_or_default().to_string();
                    (label, title.absolute_position().x)
                })
                .collect();
        titles.sort_by(|a, b| a.0.cmp(&b.0));
        titles.into_iter().map(|(_, x)| x).collect()
    };
    let before = faces();
    let stride = before[1] - before[0];

    let window = t.window.window();
    let (x, y) = tab_middle(&t, 0);
    let at = |x: f32| slint::LogicalPosition::new(x, y);
    window.dispatch_event(WindowEvent::PointerMoved { position: at(x) });
    window.dispatch_event(WindowEvent::PointerPressed {
        position: at(x),
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerMoved { position: at(x + 20.0) });
    window.dispatch_event(WindowEvent::PointerMoved { position: at(x + stride) });
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_secs(1));

    let during = faces();
    assert_eq!(during[0], before[0] + stride, "the dragged tab did not follow the pointer");
    assert_eq!(during[1], before[0], "the passed tab did not make room");
    assert_eq!(during[2], before[2], "a tab not passed moved");
    // Nothing is reordered until the tab is let go.
    assert_eq!(t.titles(), ["a.pdf", "b.pdf", "c.pdf"]);

    window.dispatch_event(WindowEvent::PointerReleased {
        position: at(x + stride),
        button: PointerEventButton::Left,
    });
    assert_eq!(t.titles(), ["b.pdf", "a.pdf", "c.pdf"]);
    let after = faces();
    assert_eq!(after, [before[1], before[0], before[2]], "the tabs were not in their new places");
}
