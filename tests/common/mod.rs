//! Helpers shared by the integration tests that work with real files.
//!
//! Each test file compiles its own copy of this module and uses only some of
//! it, so what one file leaves unused is not dead code.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use mupdf::Size;
use mupdf::pdf::PdfDocument;

/// A fresh, empty directory for one test's files, removed again when the test
/// is done with it.
pub struct Scratch(PathBuf);

impl Scratch {
    /// A directory named after the test binary, `name` and the process, so
    /// tests running at the same time never share one.
    pub fn new(name: &str) -> Self {
        let binary = std::env::current_exe()
            .ok()
            .and_then(|path| path.file_stem().map(|stem| stem.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let directory =
            std::env::temp_dir().join(format!("melkkipdf-{binary}-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        Self(directory)
    }

    pub fn join(&self, path: impl AsRef<Path>) -> PathBuf {
        self.0.join(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Writes a PDF of `pages` blank A4 pages.
pub fn write_pdf(path: &Path, pages: usize) {
    let mut document = PdfDocument::new();
    for _ in 0..pages {
        document.new_page(Size::A4).unwrap();
    }
    document.save(path.to_str().unwrap()).unwrap();
}

/// Writes a PDF of US Letter pages with the given text, one page per entry,
/// each line of it set in Helvetica 12pt, the first line 72pt from the top
/// and each next one 20pt below. The text must not contain parentheses or
/// backslashes.
pub fn write_text_pdf(path: &Path, pages: &[&[&str]]) {
    write_linked_pdf(path, pages, &[]);
}

/// The object number of the 0-based `page` in a PDF [`write_linked_pdf`]
/// writes, for a destination to name.
pub fn page_object(page: usize) -> usize {
    4 + page * 2
}

/// A link annotation over `rect`, in PDF user space with its origin at the
/// bottom-left corner of the page, to `page` shown from `top` points up
/// from the bottom, the way a PDF `/XYZ` destination says it.
pub fn link_to_page(rect: [f32; 4], page: usize, top: f32) -> String {
    let [x0, y0, x1, y1] = rect;
    format!(
        "<< /Type /Annot /Subtype /Link /Rect [{x0} {y0} {x1} {y1}] \
         /Dest [{} 0 R /XYZ null {top} null] >>",
        page_object(page)
    )
}

/// A link annotation over `rect` to `uri`.
pub fn link_to_uri(rect: [f32; 4], uri: &str) -> String {
    let [x0, y0, x1, y1] = rect;
    format!(
        "<< /Type /Annot /Subtype /Link /Rect [{x0} {y0} {x1} {y1}] \
         /A << /S /URI /URI ({uri}) >> >>"
    )
}

/// A link annotation over `rect` whose destination names an object the
/// file does not have.
pub fn broken_link(rect: [f32; 4]) -> String {
    let [x0, y0, x1, y1] = rect;
    format!(
        "<< /Type /Annot /Subtype /Link /Rect [{x0} {y0} {x1} {y1}] \
         /Dest [999 0 R /Fit] >>"
    )
}

/// Like [`write_text_pdf`], with link annotations: `links[page]` holds that
/// page's annotation dictionaries as PDF syntax, from [`link_to_page`] and
/// its kin.
pub fn write_linked_pdf(path: &Path, pages: &[&[&str]], links: &[Vec<String>]) {
    let mut objects: Vec<String> = Vec::new();
    let font = 3;
    let first_page = 4;
    let kids: Vec<String> =
        (0..pages.len()).map(|index| format!("{} 0 R", first_page + index * 2)).collect();
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
    objects.push(format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len()));
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
    for (index, lines) in pages.iter().enumerate() {
        let contents = first_page + index * 2 + 1;
        let annots = match links.get(index) {
            Some(page_links) if !page_links.is_empty() => {
                format!(" /Annots [{}]", page_links.join(" "))
            }
            _ => String::new(),
        };
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 {font} 0 R >> >> /Contents {contents} 0 R{annots} >>"
        ));
        let mut stream = String::from("BT /F1 12 Tf 14 TL 72 708 Td");
        for (line, text) in lines.iter().enumerate() {
            if line > 0 {
                stream.push_str(" 0 -20 Td");
            }
            stream.push_str(&format!(" ({text}) Tj"));
        }
        stream.push_str(" ET");
        objects.push(format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()));
    }

    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{object}\nendobj\n", index + 1));
    }
    let xref = pdf.len();
    pdf.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1));
    for offset in offsets {
        pdf.push_str(&format!("{offset:010} 00000 n \n"));
    }
    pdf.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    ));
    std::fs::write(path, pdf).unwrap();
}

/// An image to embed in a PDF: where it is drawn on the page, in points
/// from the top-left corner, how many pixels it has of its own, whether it
/// is drawn through a soft mask made from a gray image of its own, as a
/// drop shadow or a feathered edge is, and which colour fills it, so two
/// entries with the same fill and pixels are the same image.
pub struct Embedded {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub pixels: (u32, u32),
    pub masked: bool,
    pub fill: [u8; 3],
}

impl Embedded {
    pub fn at(x: f32, y: f32, width: f32, height: f32, pixels: (u32, u32)) -> Self {
        Self { x, y, width, height, pixels, masked: false, fill: [0, 0, 0] }
    }

    pub fn filled(mut self, fill: [u8; 3]) -> Self {
        self.fill = fill;
        self
    }
}

/// Writes a PDF of US Letter pages with the given images drawn on them, one
/// page per entry. Every image is uncompressed and filled with its colour,
/// or with one of its own when it was given none, and the first of every
/// page is drawn first.
pub fn write_image_pdf(path: &Path, pages: &[&[Embedded]]) {
    let mut objects: Vec<Vec<u8>> = Vec::new();
    let mut add = |object: Vec<u8>| -> usize {
        objects.push(object);
        objects.len()
    };
    let image_object = |width: u32, height: u32, fill: &[u8], space: &str| -> Vec<u8> {
        let mut samples = Vec::with_capacity((width * height) as usize * fill.len());
        for _ in 0..width * height {
            samples.extend_from_slice(fill);
        }
        let mut object = format!(
            "<< /Type /XObject /Subtype /Image /Width {width} /Height {height} \
             /ColorSpace /{space} /BitsPerComponent 8 /Length {} >>\nstream\n",
            samples.len()
        )
        .into_bytes();
        object.extend_from_slice(&samples);
        object.extend_from_slice(b"\nendstream");
        object
    };
    add(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    add(Vec::new()); // The pages object, filled in once the pages exist.
    let mut kids = Vec::new();
    for (index, images) in pages.iter().enumerate() {
        let mut xobjects = String::new();
        let mut states = String::new();
        let mut stream = String::new();
        for (ordinal, image) in images.iter().enumerate() {
            let (width, height) = image.pixels;
            let fill = if image.fill == [0, 0, 0] {
                [(40 * (index + 1)) as u8, (60 * (ordinal + 1)) as u8, 200]
            } else {
                image.fill
            };
            let number = add(image_object(width, height, &fill, "DeviceRGB"));
            xobjects.push_str(&format!("/Im{ordinal} {number} 0 R "));
            // PDF places images from the bottom-left corner, y upwards.
            let placement = format!(
                "{} 0 0 {} {} {} cm",
                image.width,
                image.height,
                image.x,
                792.0 - image.y - image.height
            );
            if image.masked {
                // A luminosity soft mask: a form drawing a gray image over
                // the same place, set as the graphics state's mask.
                let shade = add(image_object(width, height, &[0x80], "DeviceGray"));
                let form_stream = format!("q {placement} /Sh Do Q");
                let form = add(format!(
                    "<< /Type /XObject /Subtype /Form /BBox [0 0 612 792] \
                         /Group << /S /Transparency /CS /DeviceGray >> \
                         /Resources << /XObject << /Sh {shade} 0 R >> >> /Length {} >>\n\
                         stream\n{form_stream}\nendstream",
                    form_stream.len()
                )
                .into_bytes());
                let state = add(format!(
                    "<< /Type /ExtGState /SMask << /S /Luminosity /G {form} 0 R >> >>"
                )
                .into_bytes());
                states.push_str(&format!("/GS{ordinal} {state} 0 R "));
                stream.push_str(&format!("q /GS{ordinal} gs {placement} /Im{ordinal} Do Q\n"));
            } else {
                stream.push_str(&format!("q {placement} /Im{ordinal} Do Q\n"));
            }
        }
        let contents = add(
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()).into_bytes()
        );
        let page = add(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Resources << /XObject << {xobjects}>> /ExtGState << {states}>> >> \
                 /Contents {contents} 0 R >>"
        )
        .into_bytes());
        kids.push(format!("{page} 0 R"));
    }
    objects[1] = format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len())
        .into_bytes();

    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        pdf.extend_from_slice(object);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    std::fs::write(path, pdf).unwrap();
}
