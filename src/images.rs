//! The images drawn on a page, found by running the page through a device
//! of our own that only takes note of them.
//!
//! MuPDF's text page can list images too, but it lists every image the
//! page draws, including those drawn only to make a soft mask: the gray
//! drop shadow or feathered edge an image is masked with comes through as
//! an image of its own, with nothing to tell it apart. The interpreter
//! brackets a mask's drawing in `begin_mask` and `end_mask`, which the
//! device here counts, so the images it records are the ones the reader
//! sees. It runs on whichever thread owns the page.

use std::cell::RefCell;
use std::rc::Rc;

use mupdf::device::NativeDevice;
use mupdf::{ColorParams, Colorspace, Device, Error, Image, Matrix, Page, Rect};

use crate::search::{Area, ImageSpot};

/// An image as drawn on a page: the image itself and where it landed, in
/// the page's own coordinates.
pub struct Drawn {
    pub image: Image,
    pub bounds: Rect,
}

/// The device: a list of what was drawn, and how deep inside a mask's
/// definition the drawing is.
#[derive(Default)]
struct Catalogue {
    drawn: Vec<Drawn>,
    masking: u32,
}

impl Catalogue {
    fn note(&mut self, image: &Image, ctm: Matrix) {
        if self.masking == 0 {
            let bounds = Rect::new(0.0, 0.0, 1.0, 1.0).transform(&ctm);
            self.drawn.push(Drawn { image: image.clone(), bounds });
        }
    }
}

impl NativeDevice for Catalogue {
    fn fill_image(&mut self, img: &Image, ctm: Matrix, _alpha: f32, _cp: ColorParams) {
        self.note(img, ctm);
    }

    fn fill_image_mask(
        &mut self,
        img: &Image,
        ctm: Matrix,
        _color_space: &Colorspace,
        _color: &[f32],
        _alpha: f32,
        _cp: ColorParams,
    ) {
        self.note(img, ctm);
    }

    fn begin_mask(
        &mut self,
        _area: Rect,
        _luminosity: bool,
        _color_space: &Colorspace,
        _color: &[f32],
        _cp: ColorParams,
    ) {
        self.masking += 1;
    }

    fn end_mask(&mut self, _f: &mupdf::Function) {
        self.masking = self.masking.saturating_sub(1);
    }
}

/// The images drawn on `page`, in drawing order, with where each landed.
pub fn drawn_on(page: &Page) -> Result<Vec<Drawn>, Error> {
    let catalogue = Rc::new(RefCell::new(Catalogue::default()));
    {
        let device = Device::from_native(catalogue.clone())?;
        page.run(&device, &Matrix::IDENTITY)?;
    }
    let catalogue = Rc::try_unwrap(catalogue)
        .unwrap_or_else(|_| panic!("the device was dropped with the page's run"));
    Ok(catalogue.into_inner().drawn)
}

/// The images drawn on `page`, as the index records them: where each is
/// in points from the page's corner, and how many pixels it has.
pub fn spots_on(page: &Page) -> Result<Vec<ImageSpot>, Error> {
    let origin = page.bounds()?;
    Ok(drawn_on(page)?
        .iter()
        .enumerate()
        .map(|(ordinal, drawn)| ImageSpot {
            ordinal,
            bounds: Area {
                x: drawn.bounds.x0 - origin.x0,
                y: drawn.bounds.y0 - origin.y0,
                width: drawn.bounds.width(),
                height: drawn.bounds.height(),
            },
            width: drawn.image.width(),
            height: drawn.image.height(),
        })
        .collect())
}

/// The image drawn `ordinal`-th on `page`, found as [`spots_on`] counted
/// it.
pub fn drawn_nth(page: &Page, ordinal: usize) -> Result<Image, Error> {
    drawn_on(page)?
        .into_iter()
        .nth(ordinal)
        .map(|drawn| drawn.image)
        .ok_or_else(|| Error::InvalidArgument("the image is no longer on the page".into()))
}
