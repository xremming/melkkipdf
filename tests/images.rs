#![cfg(feature = "testing")]

//! The sidebar's list of the document's images: how it waits for every page
//! to be catalogued, leaves out the small images and gathers the repeated
//! ones, follows the page being read, the previews it asks for, and copying
//! an image as it is embedded. Uses Slint's testing backend so the tests
//! run without a display.

mod common;

use common::{Embedded, Scratch, write_image_pdf};
use melkkipdf::testing::{Area, Harness, ImageRowView, Screenshot, Tabs, ThumbRequest};
use slint::{ComponentHandle, Model};

fn area(x: f32, y: f32, width: f32, height: f32) -> Area {
    Area { x, y, width, height }
}

/// A picture of a size nothing filters out, with its own identity.
fn picture(identity: u8) -> (Area, u32, u32, u8) {
    (area(10.0, 10.0, 100.0, 50.0), 400, 200, identity)
}

/// A row listed by page, with its heading or none, and no preview yet.
fn row(page: i32, ordinal: i32, heading: i32, width: i32, height: i32) -> ImageRowView {
    (page, ordinal, heading, width, height, 0, false)
}

#[test]
fn the_list_waits_for_every_page_and_says_how_far_it_has_got() {
    let h = Harness::uniform(4, 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    h.catalogue_from(0, &[&[], &[picture(1), picture(2)]]);
    assert!(!h.window.get_images_ready());
    assert_eq!(h.window.get_image_status(), "Finding images, 2 of 4 pages.");
    assert!(h.image_rows().is_empty());

    h.catalogue_from(2, &[&[picture(3)], &[]]);
    assert!(h.window.get_images_ready());
    assert_eq!(h.window.get_image_status(), "");
    assert_eq!(
        h.image_rows(),
        [row(1, 0, 1, 400, 200), row(1, 1, 0, 400, 200), row(2, 0, 1, 400, 200)]
    );
}

#[test]
fn a_document_without_images_says_so() {
    let h = Harness::uniform(2, 600.0, 800.0);
    h.catalogue_from(0, &[&[], &[]]);
    assert_eq!(h.window.get_image_status(), "No images.");

    let h = Harness::uniform(1, 600.0, 800.0);
    h.catalogue_from(0, &[&[(area(10.0, 10.0, 8.0, 8.0), 16, 16, 1)]]);
    assert_eq!(h.window.get_image_status(), "No images, apart from 1 small ones.");
    assert_eq!(h.window.get_small_images_hidden(), 1);
}

#[test]
fn small_images_are_left_out_until_asked_for() {
    let h = Harness::uniform(1, 600.0, 800.0);
    // Drawn small, few pixels, and neither.
    let bullet = (area(10.0, 10.0, 8.0, 8.0), 400, 400, 1);
    let speck = (area(10.0, 10.0, 100.0, 100.0), 16, 16, 2);
    let thin = (area(10.0, 10.0, 500.0, 19.0), 2000, 80, 3);
    h.catalogue_from(0, &[&[bullet, picture(4), speck, thin]]);
    assert_eq!(h.image_rows(), [row(0, 1, 1, 400, 200)]);
    assert_eq!(h.window.get_small_images_hidden(), 3);

    h.set_image_filters(false, true);
    assert_eq!(h.image_rows().len(), 4);
    assert_eq!(h.window.get_small_images_hidden(), 0);
}

#[test]
fn repeated_images_are_gathered_at_the_end_once_each() {
    let h = Harness::uniform(4, 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    let logo = (area(500.0, 10.0, 60.0, 60.0), 232, 232, 9);
    h.catalogue_from(
        0,
        &[&[logo, picture(1)], &[logo, picture(2)], &[logo, picture(3), picture(3)], &[]],
    );
    // The pictures of their own by page, then the repeated images, each
    // with how often and on how many pages it is drawn.
    assert_eq!(
        h.image_rows(),
        [
            (0, 1, 1, 400, 200, 0, false),
            (1, 1, 1, 400, 200, 0, false),
            (0, 0, 2, 232, 232, 3, false),
            (2, 1, 0, 400, 200, 2, false),
        ]
    );
    let rows = h.window.get_image_rows();
    assert_eq!(rows.row_data(2).unwrap().pages, 3);
    assert_eq!(rows.row_data(3).unwrap().pages, 1);

    // Not gathered, every image is under its page.
    h.set_image_filters(true, false);
    assert_eq!(h.image_rows().len(), 7);
    assert_eq!(h.image_rows()[0], row(0, 0, 1, 232, 232));
}

#[test]
fn the_list_follows_the_page_being_read_among_the_rows_by_page() {
    let h = Harness::uniform(6, 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    let logo = (area(500.0, 10.0, 60.0, 60.0), 232, 232, 9);
    h.catalogue_from(0, &[&[logo], &[picture(1), picture(2)], &[logo], &[picture(3)], &[], &[]]);
    assert_eq!(h.image_rows().len(), 4);

    // On the first page, which has no image of its own, the list is on the
    // next page's first; on a page with images, on its first; past the
    // last image, on none, never on the repeated images at the end.
    assert_eq!(h.window.get_image_current(), 0);
    h.viewer.nav_to_page(1);
    assert_eq!(h.window.get_image_current(), 0);
    h.viewer.nav_to_page(2);
    assert_eq!(h.window.get_image_current(), 2);
    h.viewer.nav_to_page(3);
    assert_eq!(h.window.get_image_current(), 2);
    h.viewer.nav_to_page(5);
    assert_eq!(h.window.get_image_current(), -1);
}

#[test]
fn a_shown_row_asks_for_its_preview_once_and_keeps_it_through_a_rebuild() {
    let h = Harness::uniform(1, 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    h.catalogue_from(0, &[&[picture(1)]]);
    h.take_thumb_requests();

    h.viewer.request_preview(0);
    assert_eq!(h.take_thumb_requests(), [ThumbRequest::Preview { page: 0, ordinal: 0 }]);

    let preview = slint::Image::from_rgba8(slint::SharedPixelBuffer::new(4, 2));
    h.viewer.on_preview_rendered(0, 0, preview);
    assert!(h.image_rows()[0].6, "the preview is there");
    // With the preview in hand the row asks for nothing more, even once
    // the list has been built again.
    h.set_image_filters(false, false);
    assert!(h.image_rows()[0].6, "the preview survived the rebuild");
    h.viewer.request_preview(0);
    assert!(h.take_thumb_requests().is_empty());
}

#[test]
fn a_click_copies_the_image_as_embedded_and_flashes_it_on_its_page() {
    let h = Harness::uniform(2, 600.0, 800.0);
    h.viewport(1000.0, 900.0);
    let bounds = area(10.0, 100.0, 50.0, 50.0);
    h.catalogue_from(0, &[&[picture(1), (bounds, 64, 64, 2)], &[]]);

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
    let logo = || Embedded::at(500.0, 20.0, 60.0, 60.0, (232, 232)).filled([200, 30, 30]);
    write_image_pdf(
        &path,
        &[
            &[logo()],
            &[
                Embedded::at(72.0, 72.0, 200.0, 100.0, (800, 400)),
                Embedded::at(72.0, 300.0, 50.0, 50.0, (3, 3)),
                logo(),
            ],
            &[Embedded::at(0.0, 0.0, 612.0, 792.0, (1224, 1584)), logo()],
        ],
    );

    let t = Tabs::new();
    t.open_file(&path);
    t.finish_cataloguing();
    // The tiny image is left out, and the logo, the same image on every
    // page, is gathered at the end.
    assert_eq!(
        t.image_rows(),
        [
            (1, 0, 1, 800, 400, 0, false),
            (2, 0, 1, 1224, 1584, 0, false),
            (0, 0, 2, 232, 232, 3, false),
        ]
    );

    let viewer = t.viewer(0);
    viewer.copy_image(0);
    t.finish_screenshot();
    assert_eq!(t.copied_image(), Some((800, 400)));
    viewer.copy_image(2);
    t.finish_screenshot();
    assert_eq!(t.copied_image(), Some((232, 232)));
    // The outline flashes where the image is drawn.
    let shot = t.window.global::<Screenshot>();
    assert!(shot.get_flashing());
    assert_eq!((shot.get_page(), shot.get_x(), shot.get_y()), (0, 500.0, 20.0));
    assert_eq!((shot.get_width(), shot.get_height()), (60.0, 60.0));

    // Shown all and by page, the tiny image is back and the logo is under
    // each page.
    t.set_image_filters(false, false);
    assert_eq!(t.image_rows().len(), 6);
    assert_eq!(t.image_rows()[0], row(0, 0, 1, 232, 232));
}

#[test]
fn an_image_drawn_only_to_mask_another_is_not_listed() {
    let directory = Scratch::new("images-masked");
    let path = directory.join("shadowed.pdf");
    let mut shadowed = Embedded::at(72.0, 72.0, 200.0, 100.0, (80, 40));
    shadowed.masked = true;
    write_image_pdf(&path, &[&[shadowed, Embedded::at(72.0, 300.0, 50.0, 50.0, (33, 33))]]);

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
    t.finish_cataloguing();
    assert_eq!(t.image_rows(), [row(0, 0, 1, 80, 40), row(0, 1, 0, 33, 33)]);
    t.viewer(0).copy_image(1);
    t.finish_screenshot();
    assert_eq!(t.copied_image(), Some((33, 33)));
}
