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

Needs MuPDF's build dependencies (a C compiler, `clang` and `libclang` for
bindgen, and `fontconfig` on Linux) and the tools in [`mise.toml`](mise.toml),
which [mise](https://mise.jdx.dev/) installs:

```sh
mise trust && mise install
mise run build
./target/release/melkkipdf document.pdf
```

`mise tasks` lists every command; `mise run check` runs what CI runs. The
tasks are thin wrappers, so with a Rust toolchain of your own the plain
commands work too:

```sh
cargo build --release
```

To build the flatpak instead, see [packaging/README.md](packaging/README.md).

It also builds and runs on macOS, where `mise run macos:app` wraps the release
binary in an app bundle that Finder can open PDFs with, signed only for the
machine that built it.

The tests drive the window headlessly, so they need no display:

```sh
mise run test    # cargo test --features testing
```

## Features

- Tabs for keeping several documents open, with documents opened while the
  viewer runs added as tabs to its window
- Reopens your tabs and each document's view where you left off
- Continuous and single-page reading modes
- Single, odd, and even page spreads
- Zoom, fit-width, and fit-page
- Outline sidebar with page thumbnails
- Bookmarks that hang from the page edge like flags on a book, so every
  marked page is in view and one press flips there and back, each in the
  next colour and shape in turn, or ones picked from its right-click menu
- Full-text search that ignores case, accents, and most punctuation, with every
  hit listed in a sidebar and outlined on its page
- Text selection with the pointer, across pages and spreads, with a double
  or triple click for a word or a line, and Ctrl+C to copy
- Keyboard-driven navigation

## Keyboard shortcuts

Laid out as in Vim and zathura, so the viewer reads well from the keyboard
alone. Ctrl is ⌘ on macOS.

| Keys                                   | Action                                        |
| -------------------------------------- | --------------------------------------------- |
| J, K, Down, Up                         | Scroll, or turn the page at its edge in paged mode |
| H, L, Left, Right                      | Previous or next page                         |
| Shift+K, Shift+J, Page Up, Page Down   | Previous or next page                         |
| Space, Shift+Space                     | Next or previous page                         |
| Home, End                              | First or last page                            |
| :                                      | Type a page number, then Enter to go there    |
| Plus, Minus, =                         | Zoom in, zoom out, 100%                       |
| Ctrl+Plus, Ctrl+Minus, Ctrl+0          | The same                                      |
| S, A                                   | Fit the page width, fit the whole page        |
| C                                      | Switch between continuous and paged           |
| 1, 2, 3                                | Single pages, odd spreads, even spreads       |
| /, Ctrl+F                              | Search                                        |
| N, Shift+N                             | Next or previous hit                          |
| Enter, Shift+Enter in the search field | Next or previous hit                          |
| Esc in a field                         | Back to the document                          |
| Ctrl+C                                 | Copy the selected text                        |
| Ctrl+A                                 | Select the whole document                     |
| Esc                                    | Let the selection go                          |
| D                                      | Flag the page, or take its flag away          |
| B, Shift+B                             | Next or previous flag                         |
| M                                      | Dog-ear the page, to flip back to it          |
| Tab                                    | Flip between the dog-ear and where you came from |
| O, Ctrl+O                              | Open a PDF                                    |
| T                                      | Show or hide the outline and thumbnails       |
| Ctrl+Tab, Ctrl+Shift+Tab               | Next or previous tab                          |
| Ctrl+1 to Ctrl+8, Ctrl+9               | That tab, the last tab                        |
| Ctrl+W                                 | Close the tab                                 |
| ?, F1                                  | Show or hide the keyboard shortcuts           |

Going to a flag, or to a page typed after a colon, leaves the dog-ear on the
page you came from, so Tab takes you back.

## Where it keeps its state

Each document's view and bookmarks, and the tabs to reopen, are kept in one
file, `documents.json`:

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
