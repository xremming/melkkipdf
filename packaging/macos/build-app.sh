#!/usr/bin/env bash
# Builds target/MelkkiPDF.app.
#
# By default the bundle is for the host's architecture and signed ad hoc,
# which is enough to run it on the machine that built it. A release sets:
#
#   MACOS_ARCHS              The architectures to build, as Rust names them,
#                            such as "aarch64 x86_64" for a universal binary.
#                            Each needs its target: rustup target add
#                            x86_64-apple-darwin aarch64-apple-darwin.
#   MACOS_SIGNING_IDENTITY   The Developer ID Application certificate to sign
#                            with, by name or SHA-1, from the keychain.
#
# dmg.sh then packs the bundle into a disk image and notarizes both.
#
# Needs rsvg-convert (brew install librsvg) to render the icon, and downloads
# the embedding model, half a gigabyte, the first time.
set -euo pipefail

APP_ID=io.github.xremming.MelkkiPDF

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

# The C in MuPDF and the Rust alike are built for the oldest macOS the bundle
# declares in LSMinimumSystemVersion below, not for the one building it.
export MACOSX_DEPLOYMENT_TARGET=11.0

binaries=()
if [[ -n ${MACOS_ARCHS:-} ]]; then
    for arch in $MACOS_ARCHS; do
        echo "Building the release binary for $arch."
        cargo build --release --bins --target "$arch-apple-darwin"
        binaries+=("target/$arch-apple-darwin/release/melkkipdf")
    done
else
    echo "Building the release binary."
    cargo build --release --bins
    binaries+=(target/release/melkkipdf)
fi
# Runs here, at build time, so it is only ever built for the host.
cargo build --release --example quantize_model

# The model for searching by meaning, shrunk to int8 from the download,
# which fetch-model.sh keeps in data/model.
packaging/common/fetch-model.sh

version=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')
app=target/MelkkiPDF.app

echo "Assembling $app."
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create -output "$app/Contents/MacOS/melkkipdf" "${binaries[@]}"
# Where the viewer looks for the model, beside its binary's directory.
target/release/examples/quantize_model data/model "$app/Contents/Resources/model"
# Everything built into the viewer that is not its own, whose licences ask
# for their notices to travel with the binary.
cp LICENSE packaging/common/third-party-licenses.txt "$app/Contents/Resources/"

# iconutil only accepts a directory of PNGs at fixed sizes, not an SVG.
iconset=$(mktemp -d)/MelkkiPDF.iconset
trap 'rm -rf "$(dirname "$iconset")"' EXIT
mkdir "$iconset"
svg=data/icons/hicolor/scalable/apps/$APP_ID.svg
for size in 16 32 128 256 512; do
    rsvg-convert -w "$size" -h "$size" "$svg" -o "$iconset/icon_${size}x${size}.png"
    rsvg-convert -w $((size * 2)) -h $((size * 2)) "$svg" -o "$iconset/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/MelkkiPDF.icns"

# The Alternate rank lists the viewer under Open With without taking over as
# the default for PDFs; the user can still choose it as the default in Finder.
cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>MelkkiPDF</string>
    <key>CFBundleDisplayName</key><string>MelkkiPDF</string>
    <key>CFBundleIdentifier</key><string>$APP_ID</string>
    <key>CFBundleExecutable</key><string>melkkipdf</string>
    <key>CFBundleIconFile</key><string>MelkkiPDF</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
    <key>NSHumanReadableCopyright</key><string>Copyright © 2026 Maximilian Remming. GNU AGPL 3.0 or later.</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>CFBundleDocumentTypes</key>
    <array>
        <dict>
            <key>CFBundleTypeName</key><string>PDF Document</string>
            <key>CFBundleTypeRole</key><string>Viewer</string>
            <key>LSHandlerRank</key><string>Alternate</string>
            <key>LSItemContentTypes</key><array><string>com.adobe.pdf</string></array>
        </dict>
    </array>
</dict>
</plist>
EOF

# The hardened runtime is what notarization requires; an ad hoc build runs
# under it too, so whatever it would stop shows up before a release does.
if [[ -n ${MACOS_SIGNING_IDENTITY:-} ]]; then
    echo "Signing the bundle with $MACOS_SIGNING_IDENTITY."
    codesign --force --options runtime --timestamp --sign "$MACOS_SIGNING_IDENTITY" "$app"
else
    echo "Signing the bundle ad hoc."
    codesign --force --options runtime --sign - "$app"
fi

echo "Done. Run it with: open $app"
