#!/usr/bin/env bash
# Builds target/MelkkiPDF.app for the host architecture.
#
# The bundle is only ad-hoc signed, which is enough to run it on the machine
# that built it. Distributing it would need a Developer ID and notarization.
#
# Needs rsvg-convert (brew install librsvg) to render the icon.
set -euo pipefail

APP_ID=io.github.xremming.MelkkiPDF

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

echo "Building the release binary."
cargo build --release

version=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')
app=target/MelkkiPDF.app

echo "Assembling $app."
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/melkkipdf "$app/Contents/MacOS/"

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

# No CFBundleDocumentTypes yet: Finder hands documents over as Apple Events
# rather than arguments, and the app does not handle those, so declaring PDF
# support would open an empty window.
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
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF

echo "Signing the bundle ad hoc."
codesign --force --sign - "$app"

echo "Done. Run it with: open $app"
