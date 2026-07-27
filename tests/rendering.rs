//! Render-request prioritization. Run with `cargo test --features testing`.
#![cfg(feature = "testing")]

use melkkipdf::testing::Harness;

#[test]
fn scrolling_requests_visible_rows_top_first() {
    let Some(h) = Harness::uniform(100, 600.0, 800.0) else {
        return;
    };
    h.viewport(1000.0, 900.0);

    // Position near page 50, then simulate the scroll callback the ListView fires.
    h.viewer.go_to_page("50");
    let offset = -h.scroll_y();
    let _ = h.take_render_requests_full(); // clear
    h.viewer.scrolled(offset);

    let requests = h.take_render_requests_full();
    assert!(!requests.is_empty(), "scrolling should request visible pages");

    // The visible (non-prefetch) pages...
    let visible: Vec<i32> = requests
        .iter()
        .filter(|(_, _, prefetch)| !prefetch)
        .map(|(page, ..)| *page)
        .collect();
    // ...start at the topmost page (index 49 == page 50)...
    assert_eq!(visible[0], 49, "lowest-numbered visible page requested first");
    // ...and are in ascending (top-to-bottom) order.
    let mut ascending = visible.clone();
    ascending.sort();
    assert_eq!(visible, ascending, "visible requests should be ordered top-first");
}

#[test]
fn idle_prefetches_neighbors_at_lower_priority() {
    let Some(h) = Harness::uniform(100, 600.0, 800.0) else {
        return;
    };
    h.viewport(1000.0, 900.0);
    h.viewer.go_to_page("50"); // row/page index 49
    let offset = -h.scroll_y();
    let _ = h.take_render_requests_full();
    h.viewer.scrolled(offset);

    let requests = h.take_render_requests_full();
    let prefetched: Vec<i32> = requests
        .iter()
        .filter(|(_, _, prefetch)| *prefetch)
        .map(|(page, ..)| *page)
        .collect();

    // Neighbors on both sides are prefetched (previous ~4 and next few pages).
    assert!(prefetched.contains(&45), "previous pages prefetched, got {prefetched:?}");
    assert!(prefetched.iter().any(|&p| p > 51), "next pages prefetched, got {prefetched:?}");
    // Every prefetch is within a few rows of the visible range.
    assert!(prefetched.iter().all(|&p| (45..=56).contains(&p)), "prefetch stays local");
}

#[test]
fn paged_mode_prefetches_neighbors() {
    let Some(h) = Harness::uniform(100, 600.0, 800.0) else {
        return;
    };
    h.viewport(1000.0, 900.0);
    h.viewer.set_continuous(false);
    let _ = h.take_render_requests_full();

    h.viewer.go_to_page("50"); // page/row index 49
    let requests = h.take_render_requests_full();

    let visible: Vec<i32> = requests
        .iter()
        .filter(|(_, _, prefetch)| !prefetch)
        .map(|(page, ..)| *page)
        .collect();
    let prefetched: Vec<i32> = requests
        .iter()
        .filter(|(_, _, prefetch)| *prefetch)
        .map(|(page, ..)| *page)
        .collect();

    assert!(visible.contains(&49), "current page requested at high priority");
    assert!(
        prefetched.contains(&45) && prefetched.contains(&53),
        "paged mode should prefetch neighbors on both sides, got {prefetched:?}"
    );
}

#[test]
fn each_scroll_is_a_newer_epoch_so_stale_requests_are_dropped() {
    let Some(h) = Harness::uniform(100, 600.0, 800.0) else {
        return;
    };
    h.viewport(1000.0, 900.0);

    h.viewer.go_to_page("10");
    let near_top = -h.scroll_y();
    h.viewer.go_to_page("70");
    let far_down = -h.scroll_y();
    let _ = h.take_render_requests_full();

    h.viewer.scrolled(near_top);
    let first = h.take_render_requests_full();
    h.viewer.scrolled(far_down);
    let second = h.take_render_requests_full();

    let first_epoch = first.iter().map(|(_, epoch, _)| *epoch).max().unwrap();
    let second_epoch = second.iter().map(|(_, epoch, _)| *epoch).max().unwrap();
    // A later scroll carries a newer epoch; the worker renders only the newest,
    // so the earlier (now off-screen) requests are dropped.
    assert!(second_epoch > first_epoch, "later scroll must use a newer epoch");
}

#[test]
fn scrolling_does_not_rerequest_already_rendered_pages() {
    let Some(h) = Harness::uniform(100, 600.0, 800.0) else {
        return;
    };
    h.viewport(1000.0, 900.0);
    h.viewer.go_to_page("10");
    let offset = -h.scroll_y();

    // First scroll requests the visible pages.
    let _ = h.take_render_requests();
    h.viewer.scrolled(offset);
    let first = h.take_render_requests();
    assert!(!first.is_empty());

    // Pretend those pages finished rendering.
    for &page in &first {
        h.viewer.on_page_rendered(page, slint::Image::default());
    }

    // Scrolling to the same spot again should not re-request rendered pages.
    h.viewer.scrolled(offset);
    let second = h.take_render_requests();
    assert!(
        second.is_empty(),
        "already-rendered visible pages should not be re-requested, got {second:?}"
    );
}
