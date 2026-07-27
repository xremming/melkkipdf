//! Spread / row-layout behavior. Run with `cargo test --features testing`.
#![cfg(feature = "testing")]

use melkkipdf::testing::Harness;

#[test]
fn initial_state_is_single_continuous() {
    let Some(h) = Harness::uniform(42, 600.0, 800.0) else {
        return;
    };
    assert_eq!(h.page_count(), 42);
    assert_eq!(h.current_page(), 1);
    assert!(h.continuous());
    assert_eq!(h.spread_mode(), 0);
}

#[test]
fn single_mode_is_one_page_per_row() {
    let Some(h) = Harness::uniform(3, 600.0, 800.0) else {
        return;
    };
    assert_eq!(h.rows(), vec![(0, None), (1, None), (2, None)]);
}

#[test]
fn odd_spread_pairs_from_the_first_page() {
    let Some(h) = Harness::uniform(5, 600.0, 800.0) else {
        return;
    };
    h.viewer.set_spread(1);
    assert_eq!(h.spread_mode(), 1);
    assert_eq!(h.rows(), vec![(0, Some(1)), (2, Some(3)), (4, None)]);
}

#[test]
fn even_spread_keeps_the_first_page_alone() {
    let Some(h) = Harness::uniform(5, 600.0, 800.0) else {
        return;
    };
    h.viewer.set_spread(2);
    assert_eq!(h.spread_mode(), 2);
    assert_eq!(h.rows(), vec![(0, None), (1, Some(2)), (3, Some(4))]);
}

#[test]
fn spread_mode_changes_row_count() {
    let Some(h) = Harness::uniform(10, 600.0, 800.0) else {
        return;
    };
    assert_eq!(h.row_count(), 10); // single: one row per page
    h.viewer.set_spread(1);
    assert_eq!(h.row_count(), 5); // odd: five pairs
    h.viewer.set_spread(2);
    assert_eq!(h.row_count(), 6); // even: [0] then five pairs
    h.viewer.set_spread(0);
    assert_eq!(h.row_count(), 10);
}

#[test]
fn switching_spread_clamps_the_current_page() {
    let Some(h) = Harness::uniform(9, 600.0, 800.0) else {
        return;
    };
    h.viewport(1000.0, 900.0);
    h.viewer.nav_end();
    assert_eq!(h.current_page(), 9);
    // Fewer rows after pairing; the current page must stay in range.
    h.viewer.set_spread(1);
    assert!(h.current_page() >= 1 && h.current_page() <= 9);
}
