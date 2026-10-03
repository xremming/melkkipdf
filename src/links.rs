//! Links in the document: the parts of a page a click follows, to another
//! page of it or to an address outside it.
//!
//! MuPDF gives every link as a rectangle and a URI string, and resolves the
//! ones within the document to a page and a view. They are reduced here, on
//! the worker, to what the viewer needs: an area in points from the page's
//! corner, the frame the text and the search hits are kept in, and where
//! the link goes.

use mupdf::{DestinationKind, Link, Rect};

use crate::search::Area;

/// Where a link goes.
#[derive(Clone, Debug, PartialEq)]
pub enum LinkTarget {
    /// A page of the document, and how far down it the link asks to be
    /// shown from, in points from its top, where it says.
    Page { page: usize, top: Option<f32> },
    /// An address outside the document.
    Uri(String),
}

/// A link on a page: the area a click on follows it, in points from the
/// page's top-left corner, and where it goes.
#[derive(Clone, Debug, PartialEq)]
pub struct PageLink {
    pub area: Area,
    pub target: LinkTarget,
}

/// Reduces a link MuPDF read from a page of `bounds` to what the viewer
/// needs, or to nothing for one that goes nowhere: a link within the
/// document MuPDF could not resolve, one to another file, which has no
/// scheme, or one with no area to click on.
pub fn reduce(link: &Link, bounds: &Rect) -> Option<PageLink> {
    let area = Area {
        x: link.bounds.x0 - bounds.x0,
        y: link.bounds.y0 - bounds.y0,
        width: link.bounds.width(),
        height: link.bounds.height(),
    };
    if area.width <= 0.0 || area.height <= 0.0 {
        return None;
    }
    let target = match &link.dest {
        Some(dest) => LinkTarget::Page {
            page: dest.loc.page_number as usize,
            top: top_of(dest.kind).map(|top| top - bounds.y0),
        },
        None if has_scheme(&link.uri) => LinkTarget::Uri(link.uri.clone()),
        None => return None,
    };
    Some(PageLink { area, target })
}

/// How far down the page a destination asks to be shown from, where its
/// kind says so. The rest show the page from its top. The zoom some kinds
/// carry is left alone: a click should not change how large the reader has
/// the pages.
fn top_of(kind: DestinationKind) -> Option<f32> {
    match kind {
        DestinationKind::XYZ { top, .. }
        | DestinationKind::FitH { top }
        | DestinationKind::FitBH { top } => top,
        DestinationKind::FitR { top, .. } => Some(top),
        _ => None,
    }
}

/// Whether `uri` names a scheme, as an address outside the document does
/// and the fragment MuPDF makes of a link within it does not.
pub fn has_scheme(uri: &str) -> bool {
    let Some(colon) = uri.find(':') else {
        return false;
    };
    let mut scheme = uri[..colon].chars();
    scheme.next().is_some_and(|c| c.is_ascii_alphabetic())
        && scheme.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Whether the viewer hands `uri` to the system to open: only web and mail
/// addresses, so a link cannot run anything or reach into the files.
pub fn opens_externally(uri: &str) -> bool {
    let lower = uri.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
}

/// What a notice calls `uri`: the host of a web address, the address mail
/// goes to, and anything else as it is.
pub fn describe(uri: &str) -> String {
    if let Some(address) = uri.strip_prefix("mailto:") {
        return address.split('?').next().unwrap_or(address).to_string();
    }
    if let Some((_, rest)) = uri.split_once("://") {
        let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        if !host.is_empty() {
            return host.to_string();
        }
    }
    uri.to_string()
}

/// The link among `links` under `x`, `y` points from the page's corner.
pub fn link_at(links: &[PageLink], x: f32, y: f32) -> Option<&PageLink> {
    links.iter().find(|link| {
        let area = &link.area;
        (area.x..=area.x + area.width).contains(&x) && (area.y..=area.y + area.height).contains(&y)
    })
}

#[cfg(test)]
mod tests {
    use mupdf::document::Location;
    use mupdf::link::LinkDestination;

    use super::*;

    fn link(uri: &str, dest: Option<LinkDestination>) -> Link {
        Link { bounds: Rect::new(10.0, 20.0, 110.0, 40.0), dest, uri: uri.into() }
    }

    fn destination(page: u32, kind: DestinationKind) -> LinkDestination {
        LinkDestination {
            loc: Location { chapter: 0, page_in_chapter: page, page_number: page },
            kind,
        }
    }

    #[test]
    fn a_link_within_the_document_keeps_its_page_and_how_far_down_it() {
        let bounds = Rect::new(0.0, 0.0, 612.0, 792.0);
        let kind = DestinationKind::XYZ { left: None, top: Some(300.0), zoom: None };
        let reduced = reduce(&link("#page=4", Some(destination(3, kind))), &bounds).unwrap();
        assert_eq!(reduced.area, Area { x: 10.0, y: 20.0, width: 100.0, height: 20.0 });
        assert_eq!(reduced.target, LinkTarget::Page { page: 3, top: Some(300.0) });

        // A page whose bounds do not start at the origin has its links and
        // their destinations measured from where it does start.
        let bounds = Rect::new(5.0, 10.0, 617.0, 802.0);
        let kind = DestinationKind::FitH { top: Some(300.0) };
        let reduced = reduce(&link("#page=4", Some(destination(3, kind))), &bounds).unwrap();
        assert_eq!(reduced.area.x, 5.0);
        assert_eq!(reduced.area.y, 10.0);
        assert_eq!(reduced.target, LinkTarget::Page { page: 3, top: Some(290.0) });

        // A destination that only names the page shows it from the top.
        let reduced = reduce(&link("#page=4", Some(destination(3, DestinationKind::Fit))), &bounds);
        assert_eq!(reduced.unwrap().target, LinkTarget::Page { page: 3, top: None });
    }

    #[test]
    fn a_link_outside_the_document_keeps_its_address_and_a_broken_one_goes() {
        let bounds = Rect::new(0.0, 0.0, 612.0, 792.0);
        let reduced = reduce(&link("https://example.com/a", None), &bounds).unwrap();
        assert_eq!(reduced.target, LinkTarget::Uri("https://example.com/a".into()));
        assert!(reduce(&link("#page=99", None), &bounds).is_none());
        assert!(reduce(&link("other.pdf#page=2", None), &bounds).is_none());
        let flat = Link {
            bounds: Rect::new(10.0, 20.0, 110.0, 20.0),
            dest: None,
            uri: "https://x".into(),
        };
        assert!(reduce(&flat, &bounds).is_none());
    }

    #[test]
    fn only_web_and_mail_addresses_are_opened() {
        assert!(has_scheme("https://example.com"));
        assert!(has_scheme("mailto:someone@example.com"));
        assert!(has_scheme("x-custom+v1.0:thing"));
        assert!(!has_scheme("#page=3"));
        assert!(!has_scheme("other.pdf"));
        assert!(!has_scheme("1abc:thing"));

        assert!(opens_externally("HTTPS://Example.com/"));
        assert!(opens_externally("http://example.com"));
        assert!(opens_externally("mailto:someone@example.com"));
        assert!(!opens_externally("file:///etc/passwd"));
        assert!(!opens_externally("javascript:alert(1)"));
        assert!(!opens_externally("ftp://example.com"));
    }

    #[test]
    fn a_notice_names_the_host_or_the_address() {
        assert_eq!(describe("https://example.com/paper?x=1"), "example.com");
        assert_eq!(describe("mailto:someone@example.com?subject=hi"), "someone@example.com");
        assert_eq!(describe("javascript:alert(1)"), "javascript:alert(1)");
        assert_eq!(describe("https:///"), "https:///");
    }

    #[test]
    fn the_link_under_a_point_is_found_by_its_area() {
        let links = vec![
            PageLink {
                area: Area { x: 10.0, y: 20.0, width: 100.0, height: 20.0 },
                target: LinkTarget::Page { page: 1, top: None },
            },
            PageLink {
                area: Area { x: 10.0, y: 50.0, width: 100.0, height: 20.0 },
                target: LinkTarget::Uri("https://example.com".into()),
            },
        ];
        assert_eq!(link_at(&links, 50.0, 30.0), Some(&links[0]));
        assert_eq!(link_at(&links, 110.0, 70.0), Some(&links[1]));
        assert_eq!(link_at(&links, 50.0, 45.0), None);
        assert_eq!(link_at(&links, 5.0, 30.0), None);
    }
}
