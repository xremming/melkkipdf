#![cfg(feature = "testing")]

//! Bookmarks: flags on the page edge that mark pages, flip to them and back,
//! and are remembered between runs. Uses Slint's testing backend so the tests
//! run without a display.

mod common;

use std::path::Path;

use common::{Scratch, write_pdf};
use melkkipdf::testing::Tabs;
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, Model, SharedString};

fn tabs() -> Tabs {
    Tabs::new()
}

/// A window remembering its settings in `file`, as one run of the app.
fn run_with(file: &Path) -> Tabs {
    Tabs::with_settings_file(file.to_path_buf())
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

/// The 0-based pages flagged, in the order drawn.
fn flagged(t: &Tabs) -> Vec<i32> {
    t.flags().into_iter().map(|(page, _)| page).collect()
}

#[test]
fn flagging_a_page_puts_a_flag_on_it_and_flagging_again_takes_it_off() {
    let t = tabs();
    t.open("a.pdf", 20);
    t.viewer(0).go_to_page("5");
    press(&t, &[], "d");
    assert_eq!(flagged(&t), [4]);

    press(&t, &[], "d");
    assert!(flagged(&t).is_empty());
}

#[test]
fn flags_are_drawn_in_page_order() {
    let t = tabs();
    t.open("a.pdf", 20);
    for page in ["12", "3", "8"] {
        t.viewer(0).go_to_page(page);
        t.window.invoke_toggle_bookmark();
    }
    assert_eq!(flagged(&t), [2, 7, 11]);

    t.window.invoke_remove_bookmark(7);
    assert_eq!(flagged(&t), [2, 11]);
}

#[test]
fn a_flag_is_named_after_the_heading_its_page_is_under() {
    let t = tabs();
    t.open_outlined(
        "a.pdf",
        vec![(600.0, 800.0); 20],
        &[("Introduction", 0, 0), ("Methods", 4, 0), ("Sampling", 6, 1), ("Results", 10, 0)],
    );
    for page in ["1", "8", "10", "15"] {
        t.viewer(0).go_to_page(page);
        t.window.invoke_toggle_bookmark();
    }
    let labels: Vec<String> = t.flags().into_iter().map(|(_, label)| label).collect();
    assert_eq!(
        labels,
        ["Page 1 · Introduction", "Page 8 · Sampling", "Page 10 · Sampling", "Page 15 · Results",]
    );
}

#[test]
fn a_document_without_an_outline_names_a_flag_by_its_page() {
    let t = tabs();
    t.open("a.pdf", 20);
    t.viewer(0).go_to_page("7");
    t.window.invoke_toggle_bookmark();
    assert_eq!(t.flags(), [(6, "Page 7".to_owned())]);
}

#[test]
fn clicking_a_flag_goes_there_and_tab_flips_back_and_forth() {
    let t = tabs();
    t.open("a.pdf", 20);
    t.viewer(0).go_to_page("15");
    t.window.invoke_toggle_bookmark();
    t.viewer(0).go_to_page("3");
    assert_eq!(t.return_page(), None);

    t.window.invoke_go_to_bookmark(14);
    assert_eq!(t.window.get_current_page(), 15);
    assert_eq!(t.return_page(), Some(2));

    press(&t, &[], Key::Tab);
    assert_eq!(t.window.get_current_page(), 3);
    assert_eq!(t.return_page(), Some(14));

    press(&t, &[], Key::Tab);
    assert_eq!(t.window.get_current_page(), 15);
    assert_eq!(t.return_page(), Some(2));
}

#[test]
fn reading_on_does_not_move_the_place_to_flip_back_to() {
    let t = tabs();
    t.open("a.pdf", 20);
    t.viewer(0).go_to_page("15");
    t.window.invoke_toggle_bookmark();
    t.viewer(0).go_to_page("3");
    t.window.invoke_go_to_bookmark(14);
    t.viewer(0).go_to_page("18");

    press(&t, &[], Key::Tab);
    assert_eq!(t.window.get_current_page(), 3);
}

#[test]
fn the_dog_ear_can_be_put_on_the_page_being_read() {
    let t = tabs();
    t.open("a.pdf", 20);
    t.viewer(0).go_to_page("5");
    press(&t, &[], "m");
    assert_eq!(t.return_page(), Some(4));
    assert!(flagged(&t).is_empty(), "a dog-ear is not a flag");

    t.viewer(0).go_to_page("12");
    press(&t, &[], Key::Tab);
    assert_eq!(t.window.get_current_page(), 5);
    assert_eq!(t.return_page(), Some(11));
}

#[test]
fn a_jump_through_the_page_field_leaves_the_dog_ear_behind() {
    let t = tabs();
    t.open("a.pdf", 20);
    t.viewer(0).go_to_page("3");
    t.window.invoke_go_to_page("12".into());
    assert_eq!(t.window.get_current_page(), 12);
    assert_eq!(t.return_page(), Some(2));

    press(&t, &[], Key::Tab);
    assert_eq!(t.window.get_current_page(), 3);

    // Asking for the page already being read is no jump.
    t.window.invoke_mark_return_page();
    t.window.invoke_go_to_page("3".into());
    assert_eq!(t.return_page(), Some(2));
}

#[test]
fn tab_does_nothing_before_any_flip() {
    let t = tabs();
    t.open("a.pdf", 20);
    t.viewer(0).go_to_page("3");
    press(&t, &[], Key::Tab);
    assert_eq!(t.window.get_current_page(), 3);
}

#[test]
fn b_steps_through_the_flags_and_wraps_around() {
    let t = tabs();
    t.open("a.pdf", 20);
    for page in ["4", "10", "16"] {
        t.viewer(0).go_to_page(page);
        t.window.invoke_toggle_bookmark();
    }
    t.viewer(0).go_to_page("1");

    press(&t, &[], "b");
    assert_eq!(t.window.get_current_page(), 4);
    press(&t, &[], "b");
    assert_eq!(t.window.get_current_page(), 10);
    press(&t, &[], "b");
    assert_eq!(t.window.get_current_page(), 16);
    press(&t, &[], "b");
    assert_eq!(t.window.get_current_page(), 4);

    press(&t, &[Key::Shift], "B");
    assert_eq!(t.window.get_current_page(), 16);
    press(&t, &[Key::Shift], "B");
    assert_eq!(t.window.get_current_page(), 10);

    // Stepping is a flip too, so Tab goes back to where it came from.
    press(&t, &[], Key::Tab);
    assert_eq!(t.window.get_current_page(), 16);
}

#[test]
fn stepping_with_no_flags_stays_put() {
    let t = tabs();
    t.open("a.pdf", 20);
    t.viewer(0).go_to_page("5");
    press(&t, &[], "b");
    assert_eq!(t.window.get_current_page(), 5);
    assert_eq!(t.return_page(), None);
}

#[test]
fn each_tab_has_its_own_flags_and_place_to_flip_back_to() {
    let t = tabs();
    t.open("a.pdf", 20);
    t.viewer(0).go_to_page("5");
    t.window.invoke_toggle_bookmark();
    t.viewer(0).go_to_page("1");
    t.window.invoke_go_to_bookmark(4);

    t.open("b.pdf", 10);
    assert!(flagged(&t).is_empty());
    assert_eq!(t.return_page(), None);
    t.viewer(1).go_to_page("9");
    t.window.invoke_toggle_bookmark();
    assert_eq!(flagged(&t), [8]);

    t.window.invoke_select_tab(0);
    assert_eq!(flagged(&t), [4]);
    assert_eq!(t.return_page(), Some(0));
}

#[test]
fn closing_the_last_tab_clears_the_page_edge() {
    let t = tabs();
    t.open("a.pdf", 20);
    t.window.invoke_toggle_bookmark();
    t.window.invoke_close_tab(0);
    assert!(flagged(&t).is_empty());
    assert_eq!(t.return_page(), None);
}

#[test]
fn flags_take_the_palettes_colours_in_turn_and_the_menu_can_change_one() {
    let t = tabs();
    t.open("a.pdf", 20);
    let palette = t.palette();
    assert_eq!(palette.len(), 7);
    assert_eq!(palette[0].0, "Red");
    for page in ["3", "7", "12"] {
        t.viewer(0).go_to_page(page);
        press(&t, &[], "d");
    }
    let hues: Vec<i32> = palette.iter().map(|&(_, hue)| hue).collect();
    assert_eq!(t.flag_hues(), hues[..3]);

    // The menu gives a flag the colour picked, and leaves the others.
    t.window.invoke_color_bookmark(6, hues[5]);
    assert_eq!(t.flag_hues(), [hues[0], hues[5], hues[2]]);
    // The flag's colour follows its hue.
    let colors: Vec<slint::Color> =
        t.window.get_bookmarks().iter().map(|flag| flag.color).collect();
    assert_ne!(colors[0], colors[1]);
    assert_eq!(colors[1], slint::Color::from_hsva(hues[5] as f32, 0.6, 0.9, 1.0));
}

#[test]
fn flags_survive_a_restart() {
    let directory = Scratch::new("restart");
    let settings = directory.join("documents.json");
    let a = directory.join("a.pdf");
    write_pdf(&a, 12);

    {
        let run = run_with(&settings);
        run.open_file(&a);
        run.viewer(0).go_to_page("9");
        run.window.invoke_toggle_bookmark();
        run.viewer(0).go_to_page("2");
        run.window.invoke_toggle_bookmark();
        run.window.invoke_color_bookmark(1, 272);
        run.save();
    }

    let run = run_with(&settings);
    run.restore();
    assert_eq!(flagged(&run), [1, 8]);
    assert_eq!(run.flag_hues(), [272, 0]);
    // Where a flip came from is not worth remembering across runs.
    assert_eq!(run.return_page(), None);
}
