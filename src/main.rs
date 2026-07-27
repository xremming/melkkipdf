mod render;

use std::cell::Cell;
use std::error::Error;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use render::RenderRequest;

slint::include_modules!();

/// Fixed render scale until zoom lands in the next step.
const SCALE: f32 = 1.5;

fn main() -> Result<(), Box<dyn Error>> {
    let window = MainWindow::new()?;

    let Some(path) = std::env::args().nth(1) else {
        window.set_status("Usage: melkkipdf <file.pdf>".into());
        window.run()?;
        return Ok(());
    };

    // Shared with the render worker: it fills this in once the document is open,
    // letting the UI clamp navigation without touching a MuPDF handle.
    let page_count = Arc::new(AtomicI32::new(0));
    // The page currently on screen, owned by the UI thread.
    let current_page = Rc::new(Cell::new(0i32));

    let sender = render::spawn(path, window.as_weak(), page_count.clone());
    let _ = sender.send(RenderRequest { page: 0, scale: SCALE });

    window.on_navigate({
        let sender = sender.clone();
        let current_page = current_page.clone();
        let page_count = page_count.clone();
        move |delta| {
            let count = page_count.load(Ordering::Relaxed);
            if count == 0 {
                return;
            }
            let target = (current_page.get() + delta).clamp(0, count - 1);
            if target != current_page.get() {
                current_page.set(target);
                let _ = sender.send(RenderRequest { page: target, scale: SCALE });
            }
        }
    });

    window.run()?;
    Ok(())
}
