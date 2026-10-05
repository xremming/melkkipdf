#![cfg(feature = "testing")]

//! The help page: every shortcut on one page over the window. Uses Slint's
//! testing backend so the tests run without a display.

use i_slint_backend_testing::ElementHandle;
use melkkipdf::testing::Tabs;
use slint::ComponentHandle;
use slint::platform::{Key, PointerEventButton, WindowEvent};

/// Presses and releases `key` in a shown window.
fn press(t: &Tabs, key: impl Into<slint::SharedString> + Clone) {
    let window = t.window.window();
    window.dispatch_event(WindowEvent::KeyPressed { text: key.clone().into() });
    window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
}

fn shown() -> Tabs {
    let t = Tabs::new();
    t.open("a.pdf", 5);
    t.window.window().set_size(slint::LogicalSize::new(1200.0, 900.0));
    t.window.show().unwrap();
    t
}

#[test]
fn a_question_mark_shows_the_help_and_esc_puts_it_away() {
    let t = shown();
    assert!(!t.window.get_help_open());
    press(&t, "?");
    assert!(t.window.get_help_open());
    press(&t, Key::Escape);
    assert!(!t.window.get_help_open());

    press(&t, Key::F1);
    assert!(t.window.get_help_open());
    press(&t, Key::F1);
    assert!(!t.window.get_help_open());
    press(&t, "?");
    press(&t, "?");
    assert!(!t.window.get_help_open());
}

#[test]
fn the_document_keeps_still_under_the_help() {
    let t = shown();
    press(&t, "?");
    press(&t, "c");
    press(&t, "l");
    assert!(t.window.get_continuous(), "C switched modes under the help");
    assert_eq!(t.window.get_current_page(), 1, "L turned the page under the help");

    // Put away, the keys reach the document again.
    press(&t, Key::Escape);
    press(&t, "c");
    assert!(!t.window.get_continuous());
}

#[test]
fn the_help_ends_with_the_licence_notice() {
    let t = shown();
    let texts = || {
        ElementHandle::find_by_element_type_name(&t.window, "Text")
            .filter_map(|text| text.accessible_label())
            .collect::<Vec<_>>()
    };
    let says =
        |texts: &[slint::SharedString], words: &str| texts.iter().any(|text| text.contains(words));
    assert!(!says(&texts(), "GNU Affero General Public License"));

    // The GNU AGPL asks for the copyright, the lack of warranty and where
    // the licence is to be shown together.
    press(&t, "?");
    let texts = texts();
    assert!(says(&texts, "copyright © 2026 Maximilian Remming"), "{texts:?}");
    assert!(says(&texts, "GNU Affero General Public License, version 3 or later"));
    assert!(says(&texts, "no warranty"));
    assert!(says(&texts, "github.com/xremming/melkkipdf"));
}

#[test]
fn the_toolbar_button_shows_the_help_and_leaves_the_keys_with_the_document() {
    let t = shown();
    press(&t, "/");
    let button = ElementHandle::find_by_element_id(&t.window, "MainWindow::help-toggle")
        .next()
        .expect("no help button");
    button.mock_single_click(PointerEventButton::Left);
    assert!(t.window.get_help_open());
    press(&t, Key::Escape);
    press(&t, "c");
    assert!(!t.window.get_continuous(), "the keys stayed with the search field");
}
