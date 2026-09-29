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

It also builds and runs on macOS. `packaging/build-macos-app.sh` wraps the
release binary in an app bundle that Finder can open PDFs with, signed only for
the machine that built it.

The tests drive the window headlessly, so they need no display:

```sh
cargo test --features testing
```

## Features

- Tabs for keeping several documents open
- Reopens your tabs and each document's view where you left off
- Continuous and single-page reading modes
- Single, odd, and even page spreads
- Zoom, fit-width, and fit-page
- Bookmark sidebar with page thumbnails
- Full-text search, with every hit listed in a sidebar and outlined on its page
- Keyboard-driven navigation

## Keyboard shortcuts

Ctrl is ⌘ on macOS.

| Keys                                   | Action                                        |
| -------------------------------------- | --------------------------------------------- |
| Up, Down                               | Scroll, or turn the page at its edge in paged mode |
| Left, Right, Page Up, Page Down, Space | Previous or next page                         |
| Home, End                              | First or last page                            |
| Ctrl+Plus, Ctrl+Minus, Ctrl+0          | Zoom in, zoom out, 100%                       |
| F, P                                   | Fit the page width, fit the whole page        |
| C                                      | Switch between continuous and paged           |
| 1, 2, 3                                | Single pages, odd spreads, even spreads       |
| Ctrl+F                                 | Search                                        |
| Enter, Shift+Enter in the search field | Next or previous hit                          |
| Esc in the search field                | Back to the document                          |
| Ctrl+Tab, Ctrl+Shift+Tab               | Next or previous tab                          |
| Ctrl+W                                 | Close the tab                                 |

## Where it keeps its state

Each document's view and the tabs to reopen are kept in one file,
`documents.json`:

- Linux: `~/.local/state/melkkipdf/`, or `$XDG_STATE_HOME/melkkipdf/`.
- Flatpak: `~/.var/app/io.github.xremming.MelkkiPDF/.local/state/melkkipdf/`.
- macOS: `~/Library/Application Support/io.github.xremming.MelkkiPDF/`.

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE).

The toolbar icons come from [pdf.js][pdfjs] and are used under the Apache
License 2.0; see [ui/icons/README.md](ui/icons/README.md).

[sumatra]: https://www.sumatrapdfreader.org/
[slint]: https://slint.dev/
[mupdf]: https://mupdf.com/
[pdfjs]: https://github.com/mozilla/pdf.js
