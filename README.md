# MelkkiPDF

A fast, minimal PDF viewer for Linux, inspired by [SumatraPDF][sumatra]. It
opens documents instantly and stays out of the way: no tabs to manage, no
background indexing, just the page you are reading.

Built with [Slint][slint] and [MuPDF][mupdf].

## Install

```sh
flatpak install https://xremming.github.io/melkkipdf/melkkipdf.flatpakref
```

This adds the MelkkiPDF repository and installs the app; `flatpak update` picks
up new versions from then on. If you do not have flatpak yet, see
[flathub.org/setup](https://flathub.org/setup).

## Build from source

Needs a Rust toolchain and MuPDF's build dependencies (a C compiler, `clang`
for bindgen).

```sh
cargo build --release
./target/release/melkkipdf document.pdf
```

To build the flatpak instead, see [packaging/README.md](packaging/README.md).

## Features

- Continuous and single-page reading modes
- Single, odd, and even page spreads
- Zoom, fit-width, and fit-page
- Bookmark sidebar with page thumbnails
- Keyboard-driven navigation

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE).

The toolbar icons come from [pdf.js][pdfjs] and are used under the Apache
License 2.0; see [ui/icons/README.md](ui/icons/README.md).

[sumatra]: https://www.sumatrapdfreader.org/
[slint]: https://slint.dev/
[mupdf]: https://mupdf.com/
[pdfjs]: https://github.com/mozilla/pdf.js
