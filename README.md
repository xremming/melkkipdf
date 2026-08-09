# MelkkiPDF

A fast, minimal PDF viewer for Linux, inspired by [SumatraPDF][sumatra].

```sh
flatpak install https://xremming.github.io/melkkipdf/melkkipdf.flatpakref
```

This adds the MelkkiPDF repository and installs the viewer; `flatpak update`
picks up new versions. The repository is signed and the public key travels with
the ref, so flatpak verifies each build. Requires
[flatpak](https://flathub.org/setup).

Launch it from your application menu, or:

```sh
flatpak run io.github.xremming.MelkkiPDF document.pdf
```

Built with [Slint][slint] and [MuPDF][mupdf].

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
