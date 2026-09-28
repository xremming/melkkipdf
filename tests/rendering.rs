//! Render-request prioritization. Run with `cargo test --features testing`.
#![cfg(feature = "testing")]

use melkkipdf::testing::Harness;

#[test]
fn scrolling_requests_visible_rows_top_first() {
    let h = Harness::uniform(100, 600.0, 800.0);
    h.viewport(1000.0, 900.0);

    // Position near page 50, then simulate the scroll callback the ListView fires.
    h.viewer.go_to_page("50");
    let offset = -h.scroll_y();
    let _ = h.take_render_requests_full(); // clear
    h.viewer.scrolled(offset);

    let requests = h.take_render_requests_full();
    assert!(!requests.is_empty(), "scrolling should request visible pages");

    // The visible (non-prefetch) pages...
    let visible: Vec<i32> =
        requests.iter().filter(|(_, _, prefetch)| !prefetch).map(|(page, ..)| *page).collect();
    // ...start at the topmost page (index 49 == page 50)...
    assert_eq!(visible[0], 49, "lowest-numbered visible page requested first");
    // ...and are in ascending (top-to-bottom) order.
    let mut ascending = visible.clone();
    ascending.sort();
    assert_eq!(visible, ascending, "visible requests should be ordered top-first");
}

#[test]
fn idle_prefetches_neighbors_at_lower_priority() {
    let h = Harness::uniform(100, 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    h.viewer.go_to_page("50"); // row/page index 49
    let offset = -h.scroll_y();
    let _ = h.take_render_requests_full();
    h.viewer.scrolled(offset);

    let requests = h.take_render_requests_full();
    let prefetched: Vec<i32> =
        requests.iter().filter(|(_, _, prefetch)| *prefetch).map(|(page, ..)| *page).collect();

    // Neighbors on both sides are prefetched (previous ~4 and next few pages).
    assert!(prefetched.contains(&45), "previous pages prefetched, got {prefetched:?}");
    assert!(prefetched.iter().any(|&p| p > 51), "next pages prefetched, got {prefetched:?}");
    // Every prefetch is within a few rows of the visible range.
    assert!(prefetched.iter().all(|&p| (45..=56).contains(&p)), "prefetch stays local");
}

#[test]
fn paged_mode_prefetches_neighbors() {
    let h = Harness::uniform(100, 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    h.viewer.set_continuous(false);
    let _ = h.take_render_requests_full();

    h.viewer.go_to_page("50"); // page/row index 49
    let requests = h.take_render_requests_full();

    let visible: Vec<i32> =
        requests.iter().filter(|(_, _, prefetch)| !prefetch).map(|(page, ..)| *page).collect();
    let prefetched: Vec<i32> =
        requests.iter().filter(|(_, _, prefetch)| *prefetch).map(|(page, ..)| *page).collect();

    assert!(visible.contains(&49), "current page requested at high priority");
    assert!(
        prefetched.contains(&45) && prefetched.contains(&53),
        "paged mode should prefetch neighbors on both sides, got {prefetched:?}"
    );
}

#[test]
fn each_scroll_is_a_newer_epoch_so_stale_requests_are_dropped() {
    let h = Harness::uniform(100, 600.0, 800.0);
    h.viewport(1000.0, 900.0);

    h.viewer.go_to_page("10");
    let near_top = -h.scroll_y();
    h.viewer.go_to_page("70");
    let far_down = -h.scroll_y();
    let _ = h.take_render_requests_full();

    h.scroll_by_user(near_top);
    let first = h.take_render_requests_full();
    h.scroll_by_user(far_down);
    let second = h.take_render_requests_full();

    let first_epoch = first.iter().map(|(_, epoch, _)| *epoch).max().unwrap();
    let second_epoch = second.iter().map(|(_, epoch, _)| *epoch).max().unwrap();
    // A later scroll carries a newer epoch; the worker renders only the newest,
    // so the earlier (now off-screen) requests are dropped.
    assert!(second_epoch > first_epoch, "later scroll must use a newer epoch");
}

#[test]
fn scrolling_does_not_rerequest_already_rendered_pages() {
    let h = Harness::uniform(100, 600.0, 800.0);
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
        h.deliver(page, slint::Image::default());
    }

    // Scrolling to the same spot again should not re-request rendered pages.
    h.viewer.scrolled(offset);
    let second = h.take_render_requests();
    assert!(
        second.is_empty(),
        "already-rendered visible pages should not be re-requested, got {second:?}"
    );
}

/// A rendered page image of `width`×`height` pixels. The pixels are shared, so
/// handing one image out for several pages costs its memory only once.
fn image(width: u32, height: u32) -> slint::Image {
    slint::Image::from_rgb8(slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(width, height))
}

#[test]
fn large_pages_are_evicted_by_size_keeping_those_in_view() {
    let h = Harness::uniform(100, 600.0, 800.0);
    h.viewport(1000.0, 900.0);

    // Each image is 75 MB, so the budget holds only a few. The rows in view
    // (the first page, the one below it and the one sliding in) stay; every
    // page past them is dropped again, however recently it arrived.
    let page = image(5000, 5000);
    for index in 0..10 {
        h.deliver(index, page.clone());
    }
    for index in 0..3 {
        assert!(h.page_rendered(index), "page {} in view was evicted", index + 1);
    }
    for index in 3..10 {
        assert!(!h.page_rendered(index), "page {} was kept over the budget", index + 1);
    }
}

#[test]
fn small_pages_are_all_kept() {
    let h = Harness::uniform(100, 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    let page = image(800, 1000);
    for index in 0..60 {
        h.deliver(index, page.clone());
    }
    assert!((0..60).all(|index| h.page_rendered(index)));
}

#[test]
fn high_zoom_prefetches_only_what_the_budget_holds() {
    let h = Harness::uniform(100, 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    for _ in 0..10 {
        h.viewer.zoom_in();
    }
    h.viewer.go_to_page("50");
    let offset = -h.scroll_y();
    let _ = h.take_render_requests_full();
    h.viewer.scrolled(offset);

    let requests = h.take_render_requests_full();
    assert!(requests.iter().any(|(_, _, prefetch)| !prefetch), "nothing visible was requested");
    let prefetched: Vec<i32> =
        requests.iter().filter(|(_, _, prefetch)| *prefetch).map(|(page, ..)| *page).collect();
    assert!(prefetched.is_empty(), "prefetched {prefetched:?} past the budget");
}

#[test]
fn a_background_tab_keeps_only_the_pages_in_view() {
    let h = Harness::uniform(20, 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    let page = image(800, 1000);
    for index in 0..8 {
        h.deliver(index, page.clone());
    }

    h.viewer.deactivate();
    for index in 0..3 {
        assert!(h.page_rendered(index), "page {} in view was dropped", index + 1);
    }
    for index in 3..8 {
        assert!(!h.page_rendered(index), "page {} was kept in the background", index + 1);
    }
}

#[test]
fn a_resize_renders_only_once_it_settles() {
    let h = Harness::uniform(100, 600.0, 800.0);
    // The first viewport renders at once, since that shows the document.
    h.viewport(1000.0, 900.0);
    assert!(!h.take_render_requests().is_empty(), "the first viewport rendered nothing");

    // Dragging the window reports every size on the way.
    for step in 1..=10 {
        h.viewport(1000.0 - step as f32 * 20.0, 900.0);
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(16));
    }
    assert!(h.take_render_requests().is_empty(), "rendered for a size still changing");

    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(200));
    let requests = h.take_render_requests_full();
    assert!(!requests.is_empty(), "nothing rendered once the size settled");
    let epochs: std::collections::HashSet<u64> = requests.iter().map(|(_, e, _)| *e).collect();
    assert_eq!(epochs.len(), 1, "rendered for more than the final size");
}

#[test]
fn pages_rendered_before_a_zoom_are_asked_for_again_in_view() {
    let h = Harness::uniform(100, 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    h.viewer.go_to_page("7");
    let offset = -h.scroll_y();
    h.viewer.nav_home();
    let page = image(800, 1000);
    for index in 0..10 {
        h.deliver(index, page.clone());
    }

    // Zooming asks for the rows in view at the new scale, but the rows below
    // still hold renders at the old one.
    h.viewer.zoom_in();
    h.viewer.go_to_page("7");
    let offset_after = -h.scroll_y();
    h.viewer.nav_home();
    assert_ne!(offset, offset_after, "the zoom did not change the layout");
    let _ = h.take_render_requests();

    h.scroll_by_user(offset_after);
    let requests = h.take_render_requests();
    assert!(
        requests.contains(&6),
        "a page rendered at the old zoom was not asked for: {requests:?}"
    );
}

#[test]
fn a_render_for_an_old_zoom_shows_but_is_asked_for_again() {
    let h = Harness::uniform(100, 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    let stale = h.viewer.render_scale() / 2.0;
    h.viewer.on_page_rendered(0, stale, image(400, 500));
    assert!(h.page_rendered(0), "the old render was not shown in the meantime");

    let _ = h.take_render_requests();
    h.scroll_by_user(1.0);
    assert!(h.take_render_requests().contains(&0), "the old render counted as current");
}
