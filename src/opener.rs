//! Opening an address a link points to outside the document, with whatever
//! the system opens it with: the browser for a web address, the mail program
//! for a mail one.

use std::io;

/// The system's opener, or a detached one that keeps only what it was asked
/// to open, for the tests, which must not open a browser on whoever runs
/// them.
pub struct Opener {
    #[cfg(feature = "testing")]
    detached: bool,
    #[cfg(feature = "testing")]
    opened: std::cell::RefCell<Vec<String>>,
}

impl Opener {
    /// The system's opener.
    pub fn system() -> Self {
        Self::new(false)
    }

    /// An opener that opens nothing.
    #[cfg(feature = "testing")]
    pub fn detached() -> Self {
        Self::new(true)
    }

    #[allow(unused_variables)]
    fn new(detached: bool) -> Self {
        Self {
            #[cfg(feature = "testing")]
            detached,
            #[cfg(feature = "testing")]
            opened: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// Hands `uri` to the system to open, without waiting on what opens it.
    /// Inside the flatpak this goes through xdg-open, which the runtime
    /// forwards to the portal, so the sandbox needs no permission for it.
    pub fn open(&self, uri: &str) -> io::Result<()> {
        #[cfg(feature = "testing")]
        {
            self.opened.borrow_mut().push(uri.to_string());
            if self.detached {
                return Ok(());
            }
        }
        open::that_detached(uri)
    }

    /// Every address asked to be opened so far, in order.
    #[cfg(feature = "testing")]
    pub fn opened(&self) -> Vec<String> {
        self.opened.borrow().clone()
    }
}
