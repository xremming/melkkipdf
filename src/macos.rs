//! Receives the documents macOS asks the app bundle to open.
//!
//! Finder, `open` and the Dock do not pass a document on the command line; they
//! send an "open documents" Apple Event to the running app instead. winit does
//! not handle that event, so without this the viewer would start empty.

use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::NSApplicationWillFinishLaunchingNotification;
use objc2_foundation::{
    NSAppleEventDescriptor, NSAppleEventManager, NSNotification, NSNotificationCenter,
};

// Four-character codes from <CoreServices/AE/AppleEvents.h>. They are spelled
// out here because the typed bindings for them would pull in the whole of
// objc2-core-services.
const CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
const OPEN_DOCUMENTS: u32 = u32::from_be_bytes(*b"odoc");
const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

struct Ivars {
    on_open: Box<dyn Fn(PathBuf)>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and the class does not
    // implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct OpenDocumentsHandler;

    impl OpenDocumentsHandler {
        #[unsafe(method(applicationWillFinishLaunching:))]
        fn will_finish_launching(&self, _notification: &NSNotification) {
            let manager = NSAppleEventManager::sharedAppleEventManager();
            // SAFETY: The selector names a method of this class with the
            // signature the Apple Event Manager calls it with.
            unsafe {
                let _: () = msg_send![
                    &manager,
                    setEventHandler: self,
                    andSelector: sel!(handleOpenDocuments:withReplyEvent:),
                    forEventClass: CORE_EVENT_CLASS,
                    andEventID: OPEN_DOCUMENTS,
                ];
            }
        }

        #[unsafe(method(handleOpenDocuments:withReplyEvent:))]
        fn handle_open_documents(
            &self,
            event: &NSAppleEventDescriptor,
            _reply: &NSAppleEventDescriptor,
        ) {
            // SAFETY: paramDescriptorForKeyword: takes an AEKeyword, which is
            // a u32, and returns a possibly nil descriptor.
            let documents: Option<Retained<NSAppleEventDescriptor>> =
                unsafe { msg_send![event, paramDescriptorForKeyword: DIRECT_OBJECT] };
            // The viewer has a single window, so of several documents selected
            // together only the first is opened. Apple Event lists are 1-based.
            let path = documents.and_then(|documents| {
                (1..=documents.numberOfItems())
                    .filter_map(|index| documents.descriptorAtIndex(index))
                    .find_map(|document| document.fileURLValue()?.to_file_path())
            });
            if let Some(path) = path {
                (self.ivars().on_open)(path);
            }
        }
    }

    unsafe impl NSObjectProtocol for OpenDocumentsHandler {}
);

/// Calls `on_open` with each document macOS asks the app to open, both the one
/// it was launched with and any sent to it while running.
///
/// Must be called before the event loop starts. Apple's documented place to
/// install the handler is when launching is about to finish: after AppKit has
/// installed its default handlers and before it delivers the launch document.
pub fn on_open_document(on_open: impl Fn(PathBuf) + 'static) {
    let mtm = MainThreadMarker::new().expect("must be called on the main thread");
    let handler = OpenDocumentsHandler::alloc(mtm).set_ivars(Ivars { on_open: Box::new(on_open) });
    let handler: Retained<OpenDocumentsHandler> = unsafe { msg_send![super(handler), init] };

    // SAFETY: The selector names a method of this class that takes the
    // notification as its only argument.
    unsafe {
        NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
            &handler,
            sel!(applicationWillFinishLaunching:),
            Some(NSApplicationWillFinishLaunchingNotification),
            None,
        );
    }

    // Neither the notification centre nor the Apple Event Manager retains its
    // target, and the handler is needed for as long as the app runs.
    std::mem::forget(handler);
}
