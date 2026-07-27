//! Headless render check: dumps a page of a PDF to a PNG.
//!
//! Useful on Wayland where grabbing the live window is awkward. It exercises the
//! same MuPDF path the viewer uses.
//! Run: `cargo run --example render_png -- in.pdf out.png [page]` (page is 0-based).

use std::error::Error;

use mupdf::{Colorspace, Document, Matrix};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let input = args.next().ok_or("usage: render_png <in.pdf> <out.png> [page]")?;
    let output = args.next().ok_or("usage: render_png <in.pdf> <out.png> [page]")?;
    let page_no: i32 = args.next().map(|p| p.parse()).transpose()?.unwrap_or(0);

    let document = Document::open(&input)?;
    let page = document.load_page(page_no)?;
    // Match the viewer: render opaque on white (no alpha).
    let pixmap = page.to_pixmap(
        &Matrix::new_scale(1.5, 1.5),
        &Colorspace::device_rgb(),
        false,
        true,
    )?;
    pixmap.save_as(&output, mupdf::pixmap::ImageFormat::PNG)?;

    println!(
        "Rendered {input} page {page_no} to {output} ({}x{}).",
        pixmap.width(),
        pixmap.height()
    );
    Ok(())
}
