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

### Searching by meaning

The search sidebar's "Meaning" mode ranks pages by an embedding model,
[potion-multilingual-128M](https://huggingface.co/minishlab/potion-multilingual-128M),
which is not in the repository. The flatpak and the macOS bundle download
it when they are built and ship it shrunk to int8, about 150 MB. For a
build from source, download it once, half a gigabyte, with:

```sh
mise run model:fetch    # into data/model, where mise points the viewer
```

A build run outside mise looks for the model in `MELKKIPDF_MODEL_DIR`, then
beside its own binary where the flatpak and the bundle put it, then under the
platform's data directory: `~/.local/share/melkkipdf/model` on Linux,
`~/Library/Application Support/melkkipdf/model` on macOS. Without it, the
mode says so, and exact search is unaffected. The model is read into memory
the first time a document is searched by meaning, which takes a few seconds
and about 500 MB. `mise run test:model` runs the tests that need it.

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
- Search by meaning, listing the pages most like the query whatever words
  they use, in the query's language or another, with the passage a page
  matched by outlined on it (see below)
- Text selection with the pointer, across pages and spreads, with a double
  or triple click for a word or a line, and Ctrl+C to copy
- Links followed with a click: one within the document goes to its page,
  dog-earing the page left, and a web or mail address opens outside the
  viewer, while a drag over a link still selects its text
- Screenshots to the clipboard, of a page or of a part of one dragged over,
  rendered at 300 dots per inch whatever the window's size and zoom
- The document's images listed in the sidebar under the page each is on,
  and copied to the clipboard as they are embedded, at their own
  resolution and with their masks applied, with a click
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
| Ctrl+Shift+F                           | Switch the search between exact and meaning   |
| N, Shift+N                             | Next or previous hit                          |
| Enter, Shift+Enter in the search field | Next or previous hit                          |
| Esc in a field                         | Back to the document                          |
| Ctrl+C                                 | Copy the selected text                        |
| Ctrl+A                                 | Select the whole document                     |
| Esc                                    | Let the selection go                          |
| Y, Ctrl+Shift+C                        | Screenshot the next page clicked or part dragged over |
| Esc                                    | Leave screenshot mode without taking one      |
| D                                      | Flag the page, or take its flag away          |
| B, Shift+B                             | Next or previous flag                         |
| M                                      | Dog-ear the page, to flip back to it          |
| Tab, mouse back button                 | Flip between the dog-ear and where you came from |
| O, Ctrl+O                              | Open a PDF                                    |
| T                                      | Show or hide the outline, thumbnails and images       |
| Ctrl+Tab, Ctrl+Shift+Tab               | Next or previous tab                          |
| Ctrl+1 to Ctrl+8, Ctrl+9               | That tab, the last tab                        |
| Ctrl+W                                 | Close the tab                                 |
| ?, F1                                  | Show or hide the keyboard shortcuts           |

Going to a flag, to a link, to a section or page clicked in the sidebar, or
to a page typed after a colon, leaves the dog-ear on the page you came from,
so Tab or the mouse's back button takes you back.

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
