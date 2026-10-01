//! The system clipboard, for the text the reader copies.
//!
//! Slint hands the clipboard only to its own text fields, so the viewer
//! writes it through arboard, which is what Slint's winit backend does
//! underneath. The connection is kept for the life of the app: on X11 the
//! copied text is served by whoever copied it, and goes away with them.

use std::cell::RefCell;

/// The system clipboard, reached when first written to, or a detached one
/// that keeps only what was copied, for the tests, which must not write over
/// the clipboard of whoever runs them.
pub struct Clipboard {
    system: RefCell<Connection>,
    #[cfg(feature = "testing")]
    copied: RefCell<String>,
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
        }
    }

    /// Puts `text` on the clipboard.
    pub fn set_text(&self, text: &str) {
        #[cfg(feature = "testing")]
        {
            *self.copied.borrow_mut() = text.to_string();
        }
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
            && let Err(err) = clipboard.set_text(text)
        {
            eprintln!("Copying to the clipboard failed: {err}.");
        }
    }

    /// The text last copied.
    #[cfg(feature = "testing")]
    pub fn copied(&self) -> String {
        self.copied.borrow().clone()
    }
}
