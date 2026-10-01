#![cfg(feature = "testing")]

//! The sidebar's list of the document's images: how it fills in as pages
//! are indexed, the previews it asks for, and copying an image as it is
//! embedded. Uses Slint's testing backend so the tests run without a
//! display.

mod common;

use common::{Embedded, Scratch, write_image_pdf};
use melkkipdf::testing::{Area, Harness, Screenshot, Tabs, ThumbRequest};
use slint::{ComponentHandle, Model};

fn area(x: f32, y: f32, width: f32, height: f32) -> Area {
    Area { x, y, width, height }
}

#[test]
fn the_list_fills_in_page_by_page_as_the_index_arrives() {
    let h = Harness::uniform(4, 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    h.index_images_from(
        0,
        &[
            &[],
            &[(area(10.0, 10.0, 100.0, 50.0), 400, 200), (area(10.0, 100.0, 50.0, 50.0), 64, 64)],
        ],
    );
    assert_eq!(h.image_rows(), [(1, 0, true, 400, 200, false), (1, 1, false, 64, 64, false)]);

    h.index_images_from(2, &[&[(area(0.0, 0.0, 600.0, 800.0), 2400, 3200)], &[]]);
    assert_eq!(h.image_rows().len(), 3);
    assert_eq!(h.image_rows()[2], (2, 0, true, 2400, 3200, false));
}

#[test]
fn the_list_follows_the_page_being_read() {
    let h = Harness::uniform(6, 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    h.index_images_from(
        0,
        &[
            &[],
            &[(area(10.0, 10.0, 100.0, 50.0), 400, 200), (area(10.0, 100.0, 50.0, 50.0), 64, 64)],
            &[],
            &[(area(0.0, 0.0, 600.0, 800.0), 2400, 3200)],
        ],
    );
    // Each row knows how many headings there are up to its own.
    let rows = h.window.get_image_rows();
    let headings: Vec<i32> =
        (0..rows.row_count()).map(|row| rows.row_data(row).unwrap().headings).collect();
    assert_eq!(headings, [1, 1, 2]);

    // On the first page, which has no images, the list is on the next
    // page's first image; on a page with images, on its first; past the
    // last image, on none.
    assert_eq!(h.window.get_image_current(), 0);
    h.viewer.nav_to_page(1);
    assert_eq!(h.window.get_image_current(), 0);
    h.viewer.nav_to_page(2);
    assert_eq!(h.window.get_image_current(), 2);
    h.viewer.nav_to_page(3);
    assert_eq!(h.window.get_image_current(), 2);
    h.viewer.nav_to_page(5);
    assert_eq!(h.window.get_image_current(), -1);

    // Images arriving for the page being read are followed too.
    h.index_images_from(4, &[&[], &[(area(10.0, 10.0, 100.0, 50.0), 8, 8)]]);
    assert_eq!(h.window.get_image_current(), 3);
    assert_eq!(h.window.get_image_rows().row_data(3).unwrap().headings, 3);
}

#[test]
fn a_shown_row_asks_for_its_preview_once_and_takes_it_in() {
    let h = Harness::uniform(2, 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    h.index_images_from(0, &[&[(area(10.0, 10.0, 100.0, 50.0), 400, 200)], &[]]);
    h.take_thumb_requests();

    h.viewer.request_preview(0);
    assert_eq!(h.take_thumb_requests(), [ThumbRequest::Preview { page: 0, ordinal: 0 }]);

    let preview = slint::Image::from_rgba8(slint::SharedPixelBuffer::new(4, 2));
    h.viewer.on_preview_rendered(0, 0, preview);
    assert!(h.image_rows()[0].5, "the preview is there");
    // With the preview in hand the row asks for nothing more.
    h.viewer.request_preview(0);
    assert!(h.take_thumb_requests().is_empty());
}

#[test]
fn a_click_copies_the_image_as_embedded_and_flashes_it_on_its_page() {
    let h = Harness::uniform(2, 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    let bounds = area(10.0, 100.0, 50.0, 50.0);
    h.index_images_from(0, &[&[(area(10.0, 10.0, 100.0, 50.0), 400, 200), (bounds, 64, 64)]]);

    h.viewer.copy_image(1);
    let requests = h.take_screenshot_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].page, 0);
    assert_eq!(requests[0].shot, melkkipdf::testing::Shot::Image { ordinal: 1 });
    assert_eq!(h.screenshot_outline(), Some((0, bounds, true)));
    // The screenshot mode is not touched by it.
    assert!(!h.window.global::<Screenshot>().get_active());
}

#[test]
fn a_real_documents_images_are_listed_and_copied_at_their_own_size() {
    let directory = Scratch::new("images-real");
    let path = directory.join("pictures.pdf");
    write_image_pdf(
        &path,
        &[
            &[],
            &[
                Embedded::at(72.0, 72.0, 200.0, 100.0, (800, 400)),
                Embedded::at(72.0, 300.0, 50.0, 50.0, (3, 3)),
            ],
            &[Embedded::at(0.0, 0.0, 612.0, 792.0, (1224, 1584))],
        ],
    );

    let t = Tabs::new();
    t.open_file(&path);
    t.finish_indexing();
    assert_eq!(
        t.image_rows(),
        [
            (1, 0, true, 800, 400, false),
            (1, 1, false, 3, 3, false),
            (2, 0, true, 1224, 1584, false)
        ]
    );

    let viewer = t.viewer(0);
    viewer.copy_image(0);
    t.finish_screenshot();
    assert_eq!(t.copied_image(), Some((800, 400)));
    viewer.copy_image(1);
    t.finish_screenshot();
    assert_eq!(t.copied_image(), Some((3, 3)));
    // The outline flashes where the image is drawn.
    let shot = t.window.global::<Screenshot>();
    assert!(shot.get_flashing());
    assert_eq!((shot.get_page(), shot.get_x(), shot.get_y()), (1, 72.0, 300.0));
    assert_eq!((shot.get_width(), shot.get_height()), (50.0, 50.0));
}

#[test]
fn an_image_drawn_only_to_mask_another_is_not_listed() {
    let directory = Scratch::new("images-masked");
    let path = directory.join("shadowed.pdf");
    let mut shadowed = Embedded::at(72.0, 72.0, 200.0, 100.0, (40, 20));
    shadowed.masked = true;
    write_image_pdf(&path, &[&[shadowed, Embedded::at(72.0, 300.0, 50.0, 50.0, (3, 3))]]);

    // The gray image the mask is made of is drawn too, as MuPDF's own text
    // page shows, but is no image of the document's.
    let document = mupdf::Document::open(path.to_str().unwrap()).unwrap();
    let text_page =
        document.load_page(0).unwrap().to_text_page(mupdf::TextPageFlags::PRESERVE_IMAGES).unwrap();
    let drawn = text_page
        .blocks()
        .filter(|block| block.r#type() == mupdf::text_page::TextBlockType::Image)
        .count();
    assert_eq!(drawn, 3);

    let t = Tabs::new();
    t.open_file(&path);
    t.finish_indexing();
    assert_eq!(t.image_rows(), [(0, 0, true, 40, 20, false), (0, 1, false, 3, 3, false)]);
    t.viewer(0).copy_image(1);
    t.finish_screenshot();
    assert_eq!(t.copied_image(), Some((3, 3)));
}
