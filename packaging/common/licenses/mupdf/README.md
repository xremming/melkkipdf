# MuPDF's missing licence texts

The `mupdf-sys` crate carries MuPDF's sources but leaves out some of the
licence texts for what it compiles into the viewer. These are those texts,
taken for MuPDF 1.27.2, the release `mupdf-sys` 0.8.0 builds, so that
`../../generate-licenses.sh` can run without the network.

| File | What it covers | Taken from |
| ---- | -------------- | ---------- |
| `urw-OFL.txt` | The base 14 fonts | MuPDF 1.27.2, `resources/fonts/urw/OFL.txt` |
| `adobe-cmap-resources-LICENSE.md` | The CMaps in `source/pdf/cmaps` | [adobe-type-tools/cmap-resources](https://github.com/adobe-type-tools/cmap-resources), `LICENSE.md` |
| `adobe-mapping-resources-pdf-LICENSE.txt` | The same | [adobe-type-tools/mapping-resources-pdf](https://github.com/adobe-type-tools/mapping-resources-pdf), `LICENSE.txt` |
| `hyphenation.txt` | The hyphenation patterns in `resources/hyphen` | MuPDF 1.27.2, `resources/hyphen/README`, then from each file in `resources/hyphen/license` its `title`, `copyright`, `notice`, `authors`, `licence` and `source` |
| `freetype-FTL.TXT` | FreeType 2.13.3 | FreeType's `VER-2-13-3` tag, `docs/FTL.TXT` |
| `libjpeg-LEGAL.txt` | libjpeg 9f | The LEGAL ISSUES section of the `README` in MuPDF 1.27.2's `thirdparty/libjpeg` submodule, up to the paragraphs on Autoconf, which are not built |

When `mupdf-sys` moves to another release of MuPDF, the generator stops until
these are taken again from that release, and the versions here and
`MUPDF_VERSION` in the generator are changed to match. Check too that its
`thirdparty` directory and the archives its build produces hold the same
libraries as the generator lists.
