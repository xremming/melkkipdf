//! The images drawn on a page, found by running the page through a device
//! of our own that only takes note of them, and the cataloguer that reads
//! every page's images on a thread of its own for the sidebar's list.
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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use mupdf::device::NativeDevice;
use mupdf::{ColorParams, Colorspace, Device, Document, Error, Image, Matrix, Page, Pixmap, Rect};
use slint::Weak;

use crate::MainWindow;
use crate::search::Area;

/// An image embedded in the document and drawn on a page: where it is
/// drawn, how many pixels it has of its own, which is what copying it
/// gives, and a key the same image drawn elsewhere shares. `ordinal` is its
/// place among the page's images, by which the render worker finds it
/// again.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageSpot {
    pub ordinal: usize,
    pub bounds: Area,
    pub width: u32,
    pub height: u32,
    pub key: ImageKey,
}

/// Tells one image from another: its size in pixels and a digest of it
/// drawn small, with its mask applied. MuPDF gives no way to the PDF
/// object behind an image, so the same image on two pages is known by
/// looking the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImageKey {
    pub width: u32,
    pub height: u32,
    pub digest: [u8; 16],
}

/// The longer side, in pixels, an image is drawn at for its key: small
/// enough to be cheap for thousands of images, large enough that two
/// images are not taken for one another when they merely resemble.
const KEY_PX: f32 = 48.0;

/// An image drawn with a shorter side under this many points is small: a
/// bullet, a rule or an ornament rather than a picture.
pub const SMALL_PT: f32 = 20.0;

/// An image with a shorter side under this many pixels of its own is small
/// too: there is nothing in it worth pasting.
pub const SMALL_PX: u32 = 32;

impl ImageSpot {
    /// Whether the image is small, by [`SMALL_PT`] or [`SMALL_PX`].
    pub fn is_small(&self) -> bool {
        self.bounds.width.min(self.bounds.height) < SMALL_PT
            || self.width.min(self.height) < SMALL_PX
    }
}

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

/// The images drawn on `page`, as the catalogue records them: where each
/// is in points from the page's corner, how many pixels it has, and its
/// key.
pub fn spots_on(page: &Page) -> Result<Vec<ImageSpot>, Error> {
    let origin = page.bounds()?;
    drawn_on(page)?
        .iter()
        .enumerate()
        .map(|(ordinal, drawn)| {
            Ok(ImageSpot {
                ordinal,
                bounds: Area {
                    x: drawn.bounds.x0 - origin.x0,
                    y: drawn.bounds.y0 - origin.y0,
                    width: drawn.bounds.width(),
                    height: drawn.bounds.height(),
                },
                width: drawn.image.width(),
                height: drawn.image.height(),
                key: key_of(&drawn.image)?,
            })
        })
        .collect()
}

/// The key of an image: its size and the digest of it drawn [`KEY_PX`]
/// across.
fn key_of(image: &Image) -> Result<ImageKey, Error> {
    let (width, height) = (image.width(), image.height());
    let longer = width.max(height).max(1) as f32;
    let scale = (KEY_PX / longer).min(1.0);
    let pixmap = draw(image, (width as f32 * scale).max(1.0), (height as f32 * scale).max(1.0))?;
    Ok(ImageKey { width, height, digest: *pixmap.digest()?.as_bytes() })
}

/// Draws an embedded image into a pixmap of `width`×`height` pixels, as
/// MuPDF draws it on a page: through its colour space, with its soft mask
/// applied, which leaves the pixmap transparent where the mask hides it,
/// and a stencil mask painted black. The mask is applied the way the page
/// interpreter applies it, as a clip, since decoding alone leaves it out.
/// Drawing the image smaller than it is has MuPDF decode it smaller, which
/// keeps a huge scan affordable. The alpha comes out premultiplied, as
/// MuPDF keeps it.
pub fn draw(image: &Image, width: f32, height: f32) -> Result<Pixmap, Error> {
    let bbox = Rect::new(0.0, 0.0, width, height).round();
    let mut pixmap = Pixmap::new_with_rect(&Colorspace::device_rgb(), bbox, true)?;
    pixmap.clear()?;
    {
        let device = Device::from_pixmap(&pixmap)?;
        let ctm = Matrix::new(width, 0.0, 0.0, height, 0.0, 0.0);
        if image.color_space().is_some() {
            let mask = image.mask();
            if let Some(mask) = &mask {
                device.clip_image_mask(mask, &ctm)?;
            }
            device.fill_image(image, &ctm, 1.0, ColorParams::default())?;
            if mask.is_some() {
                device.pop_clip()?;
            }
        } else {
            let black = [0.0, 0.0, 0.0];
            device.fill_image_mask(
                image,
                &ctm,
                &Colorspace::device_rgb(),
                &black,
                1.0,
                ColorParams::default(),
            )?;
        }
    }
    Ok(pixmap)
}

/// How often the cataloguer hands over the pages it has read, so the
/// sidebar can say how far it has got.
const CATALOGUE_BATCH: Duration = Duration::from_millis(100);

/// Pages a document's cataloguer has read: the images of each of the pages
/// from `first_page` on.
pub struct Catalogued {
    pub doc: i32,
    pub first_page: usize,
    pub pages: Vec<Vec<ImageSpot>>,
}

/// Keeps a document's cataloguer running. Dropping it, as closing the tab
/// does, stops the cataloguer at the next page.
pub struct CatalogueHandle {
    stop: Arc<AtomicBool>,
}

impl Drop for CatalogueHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Starts reading the images of every page of the document at `path` on a
/// thread of its own, sending the pages to `sender` in batches and calling
/// the window's `images-catalogued` callback after each, tagged with `doc`.
/// A page whose images cannot be read counts as having none. It is a
/// thread apart from the indexer so that searching never waits on it.
pub fn spawn_cataloguer(
    path: String,
    doc: i32,
    window: Weak<MainWindow>,
    sender: Sender<Catalogued>,
) -> CatalogueHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    thread::spawn(move || {
        let document = match Document::open(&path) {
            Ok(document) => document,
            Err(err) => {
                eprintln!("Failed to open {path} for its images: {err}.");
                return;
            }
        };
        let count = document.page_count().unwrap_or(0).max(0);
        let mut batch = Vec::new();
        let mut first_page = 0;
        let mut last_sent = Instant::now();
        for index in 0..count {
            if stopped.load(Ordering::Relaxed) {
                return;
            }
            let spots = document.load_page(index).and_then(|page| spots_on(&page));
            batch.push(spots.unwrap_or_else(|err| {
                eprintln!("Failed to read the images of page {} of {path}: {err}.", index + 1);
                Vec::new()
            }));
            if last_sent.elapsed() >= CATALOGUE_BATCH || index + 1 == count {
                let pages = std::mem::take(&mut batch);
                let sent = pages.len();
                if sender.send(Catalogued { doc, first_page, pages }).is_err() {
                    return;
                }
                let _ = window.upgrade_in_event_loop(|window| window.invoke_images_catalogued());
                first_page += sent;
                last_sent = Instant::now();
            }
        }
    });
    CatalogueHandle { stop }
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
