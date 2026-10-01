//! The system clipboard, for the text the reader copies and the screenshots
//! they take.
//!
//! Slint hands the clipboard only to its own text fields, so the viewer
//! writes it through arboard, which is what Slint's winit backend does
//! underneath. The connection is kept for the life of the app: on X11 the
//! copied text is served by whoever copied it, and goes away with them.

use std::borrow::Cow;
use std::cell::RefCell;

use arboard::ImageData;

/// The system clipboard, reached when first written to, or a detached one
/// that keeps only what was copied, for the tests, which must not write over
/// the clipboard of whoever runs them.
pub struct Clipboard {
    system: RefCell<Connection>,
    #[cfg(feature = "testing")]
    copied: RefCell<String>,
    #[cfg(feature = "testing")]
    copied_image: RefCell<Option<(u32, u32)>>,
}

enum Connection {
    Pending,
    Open(arboard::Clipboard),
    /// The clipboard could not be reached, which has been reported once.
    Unreachable,
    #[cfg(feature = "testing")]
    Detached,
}

impl Clipboard {
    /// The system clipboard.
    pub fn system() -> Self {
        Self::new(Connection::Pending)
    }

    /// A clipboard that goes nowhere.
    #[cfg(feature = "testing")]
    pub fn detached() -> Self {
        Self::new(Connection::Detached)
    }

    fn new(connection: Connection) -> Self {
        Self {
            system: RefCell::new(connection),
            #[cfg(feature = "testing")]
            copied: RefCell::new(String::new()),
            #[cfg(feature = "testing")]
            copied_image: RefCell::new(None),
        }
    }

    /// Puts `text` on the clipboard.
    pub fn set_text(&self, text: &str) {
        #[cfg(feature = "testing")]
        {
            *self.copied.borrow_mut() = text.to_string();
        }
        self.write(|clipboard| clipboard.set_text(text));
    }

    /// Puts an image of `width`×`height` pixels on the clipboard, from its
    /// tightly packed RGB bytes.
    pub fn set_image(&self, width: u32, height: u32, rgb: &[u8]) {
        #[cfg(feature = "testing")]
        {
            *self.copied_image.borrow_mut() = Some((width, height));
        }
        // The clipboard takes RGBA, so the paper gets an opaque alpha.
        let mut rgba = Vec::with_capacity(rgb.len() / 3 * 4);
        for pixel in rgb.chunks_exact(3) {
            rgba.extend_from_slice(pixel);
            rgba.push(0xff);
        }
        let image =
            ImageData { width: width as usize, height: height as usize, bytes: Cow::Owned(rgba) };
        self.write(|clipboard| clipboard.set_image(image));
    }

    /// Opens the connection if it has not been, then writes through it.
    fn write(&self, put: impl FnOnce(&mut arboard::Clipboard) -> Result<(), arboard::Error>) {
        let mut system = self.system.borrow_mut();
        if matches!(*system, Connection::Pending) {
            *system = match arboard::Clipboard::new() {
                Ok(clipboard) => Connection::Open(clipboard),
                Err(err) => {
                    eprintln!("The clipboard could not be reached: {err}.");
                    Connection::Unreachable
                }
            };
        }
        if let Connection::Open(clipboard) = &mut *system
            && let Err(err) = put(clipboard)
        {
            eprintln!("Copying to the clipboard failed: {err}.");
        }
    }

    /// The text last copied.
    #[cfg(feature = "testing")]
    pub fn copied(&self) -> String {
        self.copied.borrow().clone()
    }

    /// The width and height of the image last copied, if any.
    #[cfg(feature = "testing")]
    pub fn copied_image(&self) -> Option<(u32, u32)> {
        *self.copied_image.borrow()
    }
}
