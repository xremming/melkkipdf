//! melkkipdf — a fast, minimal PDF viewer for Linux.
//!
//! The binary is a thin wrapper around [`run`]. The viewer state lives in
//! [`Viewer`]; the `testing` feature exposes a headless [`testing::Harness`] that
//! drives it without an event loop for integration tests.

mod render;
mod viewer;

use std::error::Error;
use std::rc::Rc;

use mupdf::Document;
use slint::ComponentHandle;

pub use viewer::Viewer;

slint::include_modules!();

/// Reads every page's size in points, used to lay out the scrollable document
/// before any page is rendered. This is fast even for large documents.
pub fn read_page_sizes(path: &str) -> Result<Vec<(f32, f32)>, mupdf::Error> {
    let document = Document::open(path)?;
    let count = document.page_count()?;
    let mut sizes = Vec::with_capacity(count.max(0) as usize);
    for index in 0..count {
        let bounds = document.load_page(index)?.bounds()?;
        sizes.push((bounds.width(), bounds.height()));
    }
    Ok(sizes)
}

/// Reads the document outline (bookmarks) as a flat list of
/// `(title, 0-based page, depth)`, depth-first. Returns empty on any error or
/// when the document has no outline.
pub fn read_outline(path: &str) -> Vec<(String, i32, i32)> {
    fn flatten(outlines: &[mupdf::Outline], depth: i32, out: &mut Vec<(String, i32, i32)>) {
        for outline in outlines {
            let page = outline.dest.as_ref().map_or(-1, |dest| dest.loc.page_number as i32);
            out.push((outline.title.clone(), page, depth));
            flatten(&outline.down, depth + 1, out);
        }
    }
    let Ok(document) = Document::open(path) else {
        return Vec::new();
    };
    let Ok(outlines) = document.outlines() else {
        return Vec::new();
    };
    let mut items = Vec::new();
    flatten(&outlines, 0, &mut items);
    items
}

/// Opens the window for `path` (or a usage message when `None`) and runs the
/// event loop until the window closes.
pub fn run(path: Option<String>) -> Result<(), Box<dyn Error>> {
    let window = MainWindow::new()?;

    let Some(path) = path else {
        window.set_status("Usage: melkkipdf <file.pdf>".into());
        window.run()?;
        return Ok(());
    };

    let pages_pt = match read_page_sizes(&path) {
        Ok(sizes) if !sizes.is_empty() => sizes,
        Ok(_) => {
            window.set_status("Document has no pages.".into());
            window.run()?;
            return Ok(());
        }
        Err(err) => {
            window.set_status(format!("Failed to open {path}: {err}").into());
            window.run()?;
            return Ok(());
        }
    };

    // Populate the outline (bookmarks) sidebar.
    let outline: Vec<OutlineItem> = read_outline(&path)
        .into_iter()
        .map(|(title, page, depth)| OutlineItem { title: title.into(), page, depth })
        .collect();
    window.set_outline(slint::ModelRc::new(slint::VecModel::from(outline)));

    let scale_factor = window.window().scale_factor();
    let (sender, control) = render::spawn(path.clone(), window.as_weak());
    let thumb_sender = render::spawn_thumbnails(path, window.as_weak());
    let viewer = Viewer::new(&window, pages_pt, scale_factor, sender, thumb_sender, control);
    wire_callbacks(&window, &viewer);

    window.run()?;
    Ok(())
}

/// Connects the window's callbacks to the viewer.
fn wire_callbacks(window: &MainWindow, viewer: &Rc<Viewer>) {
    window.on_request_render_row({
        let viewer = viewer.clone();
        move |row| viewer.request_render_row(row)
    });
    window.on_page_rendered({
        let viewer = viewer.clone();
        move |page, image| viewer.on_page_rendered(page, image)
    });
    window.on_viewport_resized({
        let viewer = viewer.clone();
        move |width, height| viewer.set_viewport(width, height)
    });
    window.on_zoom_in({
        let viewer = viewer.clone();
        move || viewer.zoom_in()
    });
    window.on_zoom_out({
        let viewer = viewer.clone();
        move || viewer.zoom_out()
    });
    window.on_zoom_reset({
        let viewer = viewer.clone();
        move || viewer.zoom_reset()
    });
    window.on_fit_width({
        let viewer = viewer.clone();
        move || viewer.fit_width()
    });
    window.on_fit_page({
        let viewer = viewer.clone();
        move || viewer.fit_page()
    });
    window.on_toggle_continuous({
        let viewer = viewer.clone();
        move || viewer.toggle_continuous()
    });
    window.on_set_continuous({
        let viewer = viewer.clone();
        move |continuous| viewer.set_continuous(continuous)
    });
    window.on_scrolled({
        let viewer = viewer.clone();
        move |offset| viewer.scrolled(offset)
    });
    window.on_go_to_page({
        let viewer = viewer.clone();
        move |text| viewer.go_to_page(text.as_str())
    });
    window.on_set_spread({
        let viewer = viewer.clone();
        move |mode| viewer.set_spread(mode)
    });
    window.on_nav_line({
        let viewer = viewer.clone();
        move |dir| viewer.nav_line(dir)
    });
    window.on_nav_page({
        let viewer = viewer.clone();
        move |dir| viewer.nav_page(dir)
    });
    window.on_nav_home({
        let viewer = viewer.clone();
        move || viewer.nav_home()
    });
    window.on_nav_end({
        let viewer = viewer.clone();
        move || viewer.nav_end()
    });
    window.on_paged_scroll({
        let viewer = viewer.clone();
        move |delta_x, delta_y, shift| viewer.paged_scroll(delta_x, delta_y, shift)
    });
    window.on_go_to_page_index({
        let viewer = viewer.clone();
        move |page| viewer.nav_to_page(page)
    });
    window.on_request_thumbnail_row({
        let viewer = viewer.clone();
        move |row| viewer.request_thumbnail_row(row)
    });
    window.on_thumbnail_rendered({
        let viewer = viewer.clone();
        move |page, image| viewer.on_thumbnail_rendered(page, image)
    });
    window.on_toggle_sidebar({
        let window = window.as_weak();
        move || {
            if let Some(window) = window.upgrade() {
                window.set_sidebar_open(!window.get_sidebar_open());
            }
        }
    });
}

#[cfg(feature = "testing")]
pub mod testing;
