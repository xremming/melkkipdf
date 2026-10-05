#!/usr/bin/env bash
# Writes packaging/common/third-party-licenses.txt, the licences of everything
# built into the viewer that is not its own: MuPDF and the libraries and data
# it compiles in, the toolbar icons, the embedding model, and every crate. The
# flatpak and the macOS bundle ship it beside LICENSE, as the MIT, BSD, Apache
# and font licences ask of a binary.
#
# The crates' part comes from cargo-about and the rest from the sources
# mupdf-sys unpacks and from packaging/common/licenses, so whenever Cargo.lock
# changes, run this again and commit what it writes.
set -euo pipefail

# The release of MuPDF the texts in packaging/common/licenses/mupdf were taken
# for; see the README there.
MUPDF_VERSION=1.27.2

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

licenses=packaging/common/licenses
out=packaging/common/third-party-licenses.txt

# Also downloads every crate cargo-about reads below, so that it can then run
# without the network and always read the same files.
mupdf_sys=$(cargo metadata --locked --format-version 1 \
    | jq -r '.packages[] | select(.name == "mupdf-sys") | .manifest_path')
mupdf=$(dirname "$mupdf_sys")/mupdf
third=$mupdf/thirdparty

version=$(sed -n 's/^#define FZ_VERSION "\(.*\)"$/\1/p' "$mupdf/include/mupdf/fitz/version.h")
if [[ $version != "$MUPDF_VERSION" ]]; then
    echo "mupdf-sys now builds MuPDF $version, but $licenses/mupdf is for $MUPDF_VERSION." >&2
    echo "Refresh it as its README says, then set MUPDF_VERSION in $0." >&2
    exit 1
fi

rule=--------------------------------------------------------------------------------

# A heading in the shape cargo-about's entries take, then the files given.
section() {
    local title=$1
    shift
    printf '%s\n%s\n\n' "$rule" "$title"
    local file
    for file in "$@"; do
        cat "$file"
        echo
    done
    echo
}

echo "Gathering the licences into $out."
{
    cat <<EOF
MelkkiPDF is licensed under the GNU Affero General Public License, version 3
or later, which is in LICENSE beside this file. Its source code, and the
source of every version released, is at https://github.com/xremming/melkkipdf.

It is built from the works below, each under the licence given with it. Where
one offers a choice, the licence given is the one it is used under. Where a
work is under the GNU AGPL 3.0 and its text is not repeated, it is the one in
LICENSE.


EOF

    section "MuPDF $MUPDF_VERSION" /dev/stdin \
        <<<"Copyright Artifex Software, Inc., under the GNU AGPL 3.0."

    section "MuPDF's base 14 fonts, from URW++" "$licenses/mupdf/urw-OFL.txt"

    section "MuPDF's CMap and PDF mapping resources, from Adobe" \
        "$licenses/mupdf/adobe-cmap-resources-LICENSE.md" \
        "$licenses/mupdf/adobe-mapping-resources-pdf-LICENSE.txt"

    section "MuPDF's hyphenation patterns, from tex-hyphen" \
        "$licenses/mupdf/hyphenation.txt"

    section "Brotli, built into MuPDF" "$third/brotli/LICENSE"

    section "Extract, built into MuPDF" /dev/stdin \
        <<<"Copyright Artifex Software, Inc., under the GNU AGPL 3.0."

    # FreeType offers the FTL or the GPL 2, and the FTL asks for this credit.
    section "FreeType, built into MuPDF, under the FreeType License" /dev/stdin \
        "$third/freetype/LICENSE.TXT" "$licenses/mupdf/freetype-FTL.TXT" <<'EOF'
Portions of this software are copyright © The FreeType Project
(https://freetype.org). All rights reserved.
EOF

    section "Gumbo, built into MuPDF" "$third/gumbo-parser/COPYING"

    section "HarfBuzz, built into MuPDF" "$third/harfbuzz/COPYING"

    section "jbig2dec, built into MuPDF" "$third/jbig2dec/LICENSE"

    section "Little CMS, built into MuPDF" "$third/lcms2/LICENSE"

    # Leptonica's licence is only in its headers, as a C comment.
    sed -n '2,/\*=====/p' "$third/leptonica/src/allheaders.h" \
        | sed -e '$d' -e 's/^ - \{0,2\}//' \
        | section "Leptonica, built into MuPDF" /dev/stdin

    # The first line is what the licence asks of a binary.
    section "libjpeg, built into MuPDF" /dev/stdin "$licenses/mupdf/libjpeg-LEGAL.txt" \
        <<<"This software is based in part on the work of the Independent JPEG Group."

    section "MuJS, built into MuPDF" "$third/mujs/COPYING"

    section "OpenJPEG, built into MuPDF" "$third/openjpeg/LICENSE"

    section "Tesseract, built into MuPDF" "$third/tesseract/LICENSE"

    section "zlib, built into MuPDF" "$third/zlib/LICENSE"

    section "Toolbar icons, from pdf.js" /dev/stdin "$licenses/pdfjs-LICENSE" <<'EOF'
Copyright Mozilla Foundation. The icons taken are listed in ui/icons/README.md
in the source.
EOF

    section "potion-multilingual-128M, from the Minish Lab" /dev/stdin \
        "$licenses/model-LICENSE.txt" <<'EOF'
The model for searching by meaning, shrunk to int8 when the viewer is built.
It is distilled from BAAI's bge-m3, which is also under the MIT License.
https://huggingface.co/minishlab/potion-multilingual-128M
EOF

    cargo about generate --frozen --config "$licenses/about.toml" "$licenses/about.hbs"
} >"$out.part"
mv "$out.part" "$out"

echo "Wrote $out."
