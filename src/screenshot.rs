//! Screenshots: which part of a page a press or a drag picks out.
//!
//! The drag is kept in points from the corner of the page it began on, the
//! frame the selection uses too, so the area it picks out is the same at any
//! zoom and the render worker can draw it afresh at screenshot resolution
//! (see [`crate::render::screenshot_scale`]) rather than copy the window's
//! pixels.

use crate::render::{ScreenshotRequest, Shot};
use crate::search::Area;

/// How close to a page's edge, in logical pixels, a drag snaps onto it, so
/// a strip across the whole page is easy to get.
pub const SNAP_PX: f32 = 6.0;

/// How far a press may move, in logical pixels, and still be a click, which
/// takes the whole page.
pub const CLICK_PX: f32 = 4.0;

/// A drag taking a screenshot: the page it began on, where it began and
/// where it has reached, in points from that page's corner. The reach may be
/// off the page, as the drag is measured from the page it began on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Capture {
    pub page: usize,
    pub origin: (f32, f32),
    pub reach: (f32, f32),
}

impl Capture {
    /// A capture that has just begun at `x`, `y` on `page`.
    pub fn begin(page: usize, x: f32, y: f32) -> Self {
        Self { page, origin: (x, y), reach: (x, y) }
    }

    /// The area the capture outlines on a page of `width`×`height` points,
    /// drawn at `density` logical pixels per point: the rectangle between
    /// its two corners, cut at the page's edges and snapped onto an edge it
    /// comes within [`SNAP_PX`] of.
    pub fn area(&self, width: f32, height: f32, density: f32) -> Area {
        let snap = SNAP_PX / density;
        let edge = |value: f32, limit: f32| {
            let value = value.clamp(0.0, limit);
            if value < snap {
                0.0
            } else if value > limit - snap {
                limit
            } else {
                value
            }
        };
        let (x0, x1) = (edge(self.origin.0, width), edge(self.reach.0, width));
        let (y0, y1) = (edge(self.origin.1, height), edge(self.reach.1, height));
        Area { x: x0.min(x1), y: y0.min(y1), width: (x1 - x0).abs(), height: (y1 - y0).abs() }
    }

    /// Whether the drag is short enough to be a click.
    pub fn is_click(&self, density: f32) -> bool {
        let limit = CLICK_PX / density;
        (self.reach.0 - self.origin.0).abs() < limit && (self.reach.1 - self.origin.1).abs() < limit
    }

    /// What the capture asks the render worker for on a page of
    /// `width`×`height` points: the whole page on a click, the area the drag
    /// outlines otherwise, and the area the flash outlines, which is the
    /// whole page for a click.
    pub fn request(&self, width: f32, height: f32, density: f32) -> (ScreenshotRequest, Area) {
        let page = self.page as i32;
        if self.is_click(density) {
            let whole = Area { x: 0.0, y: 0.0, width, height };
            (ScreenshotRequest { page, shot: Shot::Page }, whole)
        } else {
            let area = self.area(width, height, density);
            (ScreenshotRequest { page, shot: Shot::Area(area) }, area)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Area, Capture, Shot};

    const PAGE: (f32, f32) = (600.0, 800.0);

    fn area(capture: &Capture) -> Area {
        capture.area(PAGE.0, PAGE.1, 1.0)
    }

    #[test]
    fn a_drag_outlines_the_rectangle_between_its_corners_whichever_way_it_goes() {
        let mut capture = Capture::begin(0, 100.0, 200.0);
        capture.reach = (300.0, 250.0);
        assert_eq!(area(&capture), Area { x: 100.0, y: 200.0, width: 200.0, height: 50.0 });
        capture.reach = (50.0, 150.0);
        assert_eq!(area(&capture), Area { x: 50.0, y: 150.0, width: 50.0, height: 50.0 });
    }

    #[test]
    fn a_drag_past_the_page_is_cut_at_its_edge() {
        let mut capture = Capture::begin(0, 100.0, 200.0);
        capture.reach = (900.0, -50.0);
        assert_eq!(area(&capture), Area { x: 100.0, y: 0.0, width: 500.0, height: 200.0 });
    }

    #[test]
    fn a_corner_near_an_edge_snaps_onto_it() {
        let mut capture = Capture::begin(0, 4.0, 200.0);
        capture.reach = (597.0, 300.0);
        assert_eq!(area(&capture), Area { x: 0.0, y: 200.0, width: 600.0, height: 100.0 });
        // At twice the density the snap is half as far in points.
        assert_eq!(
            capture.area(PAGE.0, PAGE.1, 2.0),
            Area { x: 4.0, y: 200.0, width: 593.0, height: 100.0 }
        );
    }

    #[test]
    fn a_short_drag_is_a_click_and_takes_the_whole_page() {
        let mut capture = Capture::begin(2, 100.0, 200.0);
        capture.reach = (102.0, 203.0);
        let (request, flash) = capture.request(PAGE.0, PAGE.1, 1.0);
        assert_eq!(request.page, 2);
        assert_eq!(request.shot, Shot::Page);
        assert_eq!(flash, Area { x: 0.0, y: 0.0, width: PAGE.0, height: PAGE.1 });
        // The same movement at a lower density is further in pixels.
        assert!(!capture.is_click(2.0));
    }

    #[test]
    fn a_longer_drag_asks_for_its_area() {
        let mut capture = Capture::begin(2, 100.0, 200.0);
        capture.reach = (150.0, 260.0);
        let (request, flash) = capture.request(PAGE.0, PAGE.1, 1.0);
        let expected = Area { x: 100.0, y: 200.0, width: 50.0, height: 60.0 };
        assert_eq!(request.shot, Shot::Area(expected));
        assert_eq!(flash, expected);
    }
}
