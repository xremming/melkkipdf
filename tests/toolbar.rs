#![cfg(feature = "testing")]

//! Toolbar layout: the button groups must hug their content instead of sharing
//! out the toolbar's width. Uses Slint's testing backend so the real layout
//! runs and the resulting geometry can be read back.

use i_slint_backend_testing::ElementHandle;
use melkkipdf::MainWindow;
use slint::ComponentHandle;

/// The one element with this id. Panics if it is missing or ambiguous.
fn element(window: &MainWindow, id: &str) -> ElementHandle {
    let mut found = ElementHandle::find_by_element_id(window, id);
    let handle = found.next().unwrap_or_else(|| panic!("no element with id {id}"));
    assert!(found.next().is_none(), "{id} matched more than one element");
    handle
}

/// Width of the single element with this id, in logical pixels.
fn width_of(window: &MainWindow, id: &str) -> f32 {
    element(window, id).size().width
}

/// Far wider than the toolbar's content, so a stretching group stands out.
const WINDOW_WIDTH: f32 = 1600.0;
/// Must match `SegmentedControl::segment-padding` in `app.slint`.
const PADDING: f32 = 16.0;
/// Must match the icon size in `Segment` in `app.slint`.
const ICON: f32 = 18.0;

fn toolbar_window() -> MainWindow {
    i_slint_backend_testing::init_no_event_loop();
    let window = MainWindow::new().expect("failed to create the window");
    // The size only takes effect once the window is shown.
    window.window().set_size(slint::LogicalSize::new(WINDOW_WIDTH, 800.0));
    window.show().expect("failed to show the window");
    window
}

#[test]
fn icon_groups_are_their_icons_plus_padding() {
    let window = toolbar_window();

    // Each segment is exactly one icon wide plus the guaranteed gap on each
    // side; the group is just its segments end to end.
    let segment = ICON + 2.0 * PADDING;
    assert_eq!(width_of(&window, "MainWindow::zoom-group"), 2.0 * segment);
    assert_eq!(width_of(&window, "MainWindow::spread-group"), 3.0 * segment);
    assert_eq!(width_of(&window, "MainWindow::page-field"), 48.0);
    // The standalone buttons carry a single icon, so they match one segment.
    assert_eq!(width_of(&window, "MainWindow::open-frame"), segment);
}

#[test]
fn segment_icons_sit_on_the_vertical_center() {
    let window = toolbar_window();

    // A fixed-size child of a HorizontalLayout lands at the top of the cross
    // axis unless it is centered explicitly, so check every icon in the toolbar
    // against the middle of the group it belongs to.
    let group = element(&window, "MainWindow::zoom-group");
    let group_middle = group.absolute_position().y + group.size().height / 2.0;

    let icons: Vec<ElementHandle> =
        ElementHandle::find_by_element_id(&window, "Segment::icon-image").collect();
    // Every group is 28px tall and centered in the same toolbar row, so the one
    // middle applies to all of them.
    assert_eq!(icons.len(), 7, "expected 2 zoom, 2 mode and 3 spread icons");

    for icon in &icons {
        let middle = icon.absolute_position().y + icon.size().height / 2.0;
        assert_eq!(
            middle, group_middle,
            "icon centered at {middle} but the segment's middle is {group_middle}"
        );
    }
}

#[test]
fn text_group_is_its_labels_plus_padding() {
    let window = toolbar_window();

    // The only text segments in the toolbar are this group's two labels, so
    // their measured widths are exactly the group's content. Asserting against
    // them (rather than a hardcoded width) is what pins the padding down as a
    // floor that holds whatever the labels turn out to measure.
    let labels: Vec<f32> = ElementHandle::find_by_element_id(&window, "Segment::label-text")
        .map(|handle| handle.size().width)
        .collect();
    assert_eq!(labels.len(), 2, "expected two text segments, got {labels:?}");
    let content: f32 = labels.iter().sum();
    assert!(content > 0.0, "labels measured zero: {labels:?}");
    assert_eq!(
        width_of(&window, "MainWindow::fit-group"),
        content + 4.0 * PADDING
    );
}

#[test]
fn toolbar_slack_goes_to_the_spacer() {
    let window = toolbar_window();

    // All the leftover width belongs to the spacer between the last mode group
    // and the page counter. The groups need well under half the toolbar, so on a
    // window this wide the gap has to be the larger share.
    let spread = element(&window, "MainWindow::spread-group");
    let field = element(&window, "MainWindow::page-field");
    let gap = field.absolute_position().x - (spread.absolute_position().x + spread.size().width);
    assert!(
        gap > WINDOW_WIDTH / 2.0,
        "only {gap}px of slack reached the spacer, so the groups are absorbing it"
    );
}
