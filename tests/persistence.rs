#![cfg(feature = "testing")]

//! What is remembered between runs: each document's view, and the tabs that
//! were open. Uses Slint's testing backend so the tests run without a display.

mod common;

use std::path::Path;

use common::{Scratch, write_pdf};
use melkkipdf::Spread;
use melkkipdf::testing::Tabs;

fn tabs() -> Tabs {
    Tabs::new()
}

/// A window remembering its settings in `file`, as one run of the app.
fn run_with(file: &Path) -> Tabs {
    Tabs::with_settings_file(file.to_path_buf())
}

#[test]
fn reopening_a_document_restores_its_view() {
    let t = tabs();
    t.open("a.pdf", 20);
    let a = t.viewer(0);
    a.set_continuous(false);
    a.set_spread(Spread::Odd);
    a.zoom_in();
    a.go_to_page("9");
    let density = t.window.get_density();
    t.window.invoke_close_tab(0);

    t.open("a.pdf", 20);
    assert!(!t.window.get_continuous());
    assert_eq!(t.window.get_spread_mode(), 1);
    assert_eq!(t.window.get_density(), density);
    assert_eq!(t.window.get_current_page(), 9);
}

#[test]
fn a_document_never_opened_before_gets_the_defaults() {
    let t = tabs();
    t.open("a.pdf", 20);
    t.viewer(0).set_spread(Spread::Even);
    t.viewer(0).set_continuous(false);
    t.window.invoke_close_tab(0);

    t.open("b.pdf", 20);
    assert!(t.window.get_continuous());
    assert_eq!(t.window.get_spread_mode(), 0);
    assert_eq!(t.window.get_current_page(), 1);
}

#[test]
fn a_continuous_position_waits_for_the_viewport() {
    let t = tabs();
    t.open("a.pdf", 20);
    t.viewer(0).set_viewport(800.0, 600.0);
    t.viewer(0).go_to_page("7");
    let scroll = t.window.get_scroll_y();
    t.window.invoke_close_tab(0);

    t.open("a.pdf", 20);
    // The page counter shows the restored page at once, but the offset that
    // reaches it depends on the zoom the viewport decides.
    assert_eq!(t.window.get_current_page(), 7);
    assert_eq!(t.window.get_scroll_y(), 0.0);

    // The list reports its initial offset before the viewport is known, which
    // must not overwrite the restored position.
    t.viewer(0).scrolled(0.0);
    t.viewer(0).set_viewport(800.0, 600.0);
    assert_eq!(t.window.get_scroll_y(), scroll);
    assert_eq!(t.window.get_current_page(), 7);
}

#[test]
fn settings_and_tabs_survive_a_restart() {
    let directory = Scratch::new("restart");
    let settings = directory.join("state").join("documents.json");
    let (a, b) = (directory.join("a.pdf"), directory.join("b.pdf"));
    write_pdf(&a, 6);
    write_pdf(&b, 3);

    {
        let run = run_with(&settings);
        run.open_file(&a);
        run.open_file(&b);
        run.window.invoke_select_tab(0);
        run.viewer(0).set_spread(Spread::Even);
        run.viewer(0).set_continuous(false);
        run.viewer(0).go_to_page("4");
        run.save();
    }

    let run = run_with(&settings);
    run.restore();
    assert_eq!(run.titles(), ["a.pdf", "b.pdf"]);
    assert_eq!(run.active_tab(), 0);
    assert_eq!(run.window.get_spread_mode(), 2);
    assert!(!run.window.get_continuous());
    assert_eq!(run.window.get_current_page(), 4);

    // The other tab kept the defaults it was left with.
    run.window.invoke_select_tab(1);
    assert_eq!(run.window.get_spread_mode(), 0);
    assert!(run.window.get_continuous());
}

#[test]
fn a_document_gone_since_last_time_is_not_reopened() {
    let directory = Scratch::new("gone");
    let settings = directory.join("documents.json");
    let (a, b) = (directory.join("a.pdf"), directory.join("b.pdf"));
    write_pdf(&a, 2);
    write_pdf(&b, 2);

    {
        let run = run_with(&settings);
        run.open_file(&a);
        run.open_file(&b);
        run.save();
    }
    std::fs::remove_file(&a).unwrap();

    let run = run_with(&settings);
    run.restore();
    assert_eq!(run.titles(), ["b.pdf"]);
    assert_eq!(run.active_tab(), 0);
}

#[test]
fn closing_every_tab_leaves_nothing_to_reopen() {
    let directory = Scratch::new("closed");
    let settings = directory.join("documents.json");
    let a = directory.join("a.pdf");
    write_pdf(&a, 2);

    {
        let run = run_with(&settings);
        run.open_file(&a);
        run.window.invoke_close_tab(0);
    }

    let run = run_with(&settings);
    run.restore();
    assert!(run.titles().is_empty());
    assert_eq!(run.window.get_page_count(), 0);
}
