#![cfg(feature = "testing")]

//! Links on the pages: a click follows one to its page, dog-earing the page
//! left, or hands its address to the system, while a drag from one selects
//! text as a drag anywhere does. Uses Slint's testing backend so the tests
//! run without a display.

mod common;

use common::{Scratch, broken_link, link_to_page, link_to_uri, write_linked_pdf};
use melkkipdf::testing::{Area, Harness, LinkTarget, PageLink, Tabs};

/// Where the harness's text sits on a page: lines start 72pt from the top,
/// 14pt tall and 20pt apart, and characters are 7pt wide from 72pt in.
fn at(line: usize, column: usize) -> (f32, f32) {
    (72.0 + column as f32 * 7.0 + 2.0, 72.0 + line as f32 * 20.0 + 7.0)
}

/// A point on the right half of a character, which a selection takes in.
fn after(line: usize, column: usize) -> (f32, f32) {
    let (x, y) = at(line, column);
    (x + 3.0, y)
}

/// A link over `count` characters from `column` on `line`.
fn link_over(line: usize, column: usize, count: usize, target: LinkTarget) -> PageLink {
    let area = Area {
        x: 72.0 + column as f32 * 7.0,
        y: 72.0 + line as f32 * 20.0,
        width: count as f32 * 7.0,
        height: 14.0,
    };
    PageLink { area, target }
}

fn harness() -> Harness {
    let h = Harness::uniform(3, 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    h.index_text(&[&["Read chapter two for more"], &[""], &["Chapter two"]]);
    let target = LinkTarget::Page { page: 2, top: Some(100.0) };
    h.viewer.set_links(vec![vec![link_over(0, 5, 7, target)], vec![], vec![]]);
    h
}

#[test]
fn letting_go_on_a_link_hands_it_over_to_be_followed() {
    let h = harness();
    assert!(h.viewer.link_under(0, at(0, 6).0, at(0, 6).1));
    assert!(!h.viewer.link_under(0, at(0, 1).0, at(0, 1).1));
    assert!(!h.viewer.link_under(1, at(0, 6).0, at(0, 6).1));

    let (x, y) = at(0, 6);
    h.viewer.select_from(0, x, y);
    let followed = h.viewer.select_done();
    assert_eq!(
        followed.map(|link| link.target),
        Some(LinkTarget::Page { page: 2, top: Some(100.0) })
    );
    assert!(h.selection(0).is_empty(), "a click on a link selects nothing");
}

#[test]
fn a_drag_from_a_link_selects_text_instead_of_following_it() {
    let h = harness();
    let (x, y) = at(0, 5);
    h.viewer.select_from(0, x, y);
    // A wobble within a click's reach is still a click.
    h.viewer.select_to(0, x + 0.2, y + 0.2);
    assert!(h.selection(0).is_empty());
    let (to_x, to_y) = at(0, 20);
    h.viewer.select_to(0, to_x, to_y);
    assert!(!h.selection(0).is_empty(), "the drag selects from where it pressed");
    assert!(h.viewer.select_done().is_none(), "a drag follows no link");
    let text = h.viewer.selected_text().unwrap_or_default();
    assert!(text.starts_with("chapter"), "selected {text:?}");
}

#[test]
fn a_press_beside_a_link_selects_as_before() {
    let h = harness();
    let (x, y) = at(0, 1);
    h.viewer.select_from(0, x, y);
    let (to_x, to_y) = after(0, 3);
    h.viewer.select_to(0, to_x, to_y);
    assert!(h.viewer.select_done().is_none());
    assert_eq!(h.viewer.selected_text().as_deref(), Some("ead"));
}

#[test]
fn a_click_on_a_link_lets_go_of_the_selection() {
    let h = harness();
    let (x, y) = at(0, 1);
    h.viewer.select_from(0, x, y);
    let (to_x, to_y) = at(0, 3);
    h.viewer.select_to(0, to_x, to_y);
    h.viewer.select_done();
    assert!(!h.selection(0).is_empty());
    let (x, y) = at(0, 6);
    h.viewer.select_from(0, x, y);
    assert!(h.selection(0).is_empty());
    h.viewer.select_done();
}

/// The links in a file: a page's own `/Rect` and `/Dest` are in PDF user
/// space, with the origin at the bottom-left of its 792pt tall page, and
/// come out in points from the top-left corner like everything else.
#[test]
fn links_in_a_document_go_to_their_page_or_open_their_address() {
    let directory = Scratch::new("links-real");
    let path = directory.join("linked.pdf");
    let annotations = vec![
        link_to_page([72.0, 680.0, 200.0, 700.0], 3, 500.0),
        link_to_uri([72.0, 640.0, 200.0, 660.0], "https://example.com/paper"),
        link_to_uri([72.0, 600.0, 200.0, 620.0], "javascript:alert(1)"),
    ];
    write_linked_pdf(&path, &[&["Contents"], &[""], &[""], &["Chapter"]], &[annotations]);

    let t = Tabs::new();
    t.open_file(&path);
    let links = t.viewer(0).page_links(0);
    assert_eq!(links.len(), 3, "{links:?}");
    assert_eq!(links[0].target, LinkTarget::Page { page: 3, top: Some(292.0) });
    let area = links[0].area;
    assert!((area.x - 72.0).abs() < 0.5 && (area.y - 92.0).abs() < 0.5, "{area:?}");
    assert!((area.width - 128.0).abs() < 0.5 && (area.height - 20.0).abs() < 0.5, "{area:?}");
    assert_eq!(links[1].target, LinkTarget::Uri("https://example.com/paper".into()));
    assert!(t.viewer(0).page_links(1).is_empty());

    // The link to a page goes there and dog-ears the page left.
    t.window.invoke_select_from(0, 100.0, 100.0);
    t.window.invoke_select_done();
    assert_eq!(t.window.get_current_page(), 4);
    assert_eq!(t.return_page(), Some(0));

    // The web address is handed on and said so.
    t.window.invoke_select_from(0, 100.0, 140.0);
    t.window.invoke_select_done();
    assert_eq!(t.opened(), vec!["https://example.com/paper".to_string()]);
    assert!(t.window.get_notice().contains("example.com"), "{}", t.window.get_notice());

    // A script is not, and the notice says what the link was.
    t.window.invoke_select_from(0, 100.0, 180.0);
    t.window.invoke_select_done();
    assert_eq!(t.opened().len(), 1);
    assert!(t.window.get_notice().contains("javascript:alert(1)"), "{}", t.window.get_notice());

    // The cursor knows a link from the rest of the page.
    t.window.invoke_hover_page(0, 100.0, 100.0);
    assert!(t.window.get_link_under_pointer());
    t.window.invoke_hover_page(0, 400.0, 400.0);
    assert!(!t.window.get_link_under_pointer());
    t.window.invoke_hover_page(0, 100.0, 140.0);
    assert!(t.window.get_link_under_pointer());
    t.window.invoke_leave_page();
    assert!(!t.window.get_link_under_pointer());
}

/// MuPDF takes a destination naming an object the file lacks as the first
/// page rather than refusing it, so the document opens with the link in
/// place. The guard against a link MuPDF cannot resolve at all is in the
/// worker, where its iterator would otherwise panic.
#[test]
fn a_broken_link_does_not_keep_the_document_from_opening() {
    let directory = Scratch::new("links-broken");
    let path = directory.join("broken.pdf");
    let annotations = vec![
        broken_link([72.0, 680.0, 200.0, 700.0]),
        link_to_uri([72.0, 640.0, 200.0, 660.0], "https://example.com/"),
    ];
    write_linked_pdf(&path, &[&["Contents"], &["More"]], &[annotations]);

    let t = Tabs::new();
    t.open_file(&path);
    assert!(!t.window.get_notice().contains("Failed"), "{}", t.window.get_notice());
    let links = t.viewer(0).page_links(0);
    assert!(t.viewer(0).page_links(1).is_empty());
    assert_eq!(links.len(), 2, "{links:?}");
    assert_eq!(links[1].target, LinkTarget::Uri("https://example.com/".into()));
}
