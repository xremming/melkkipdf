#!/usr/bin/env bash
# Packs target/MelkkiPDF.app, as build-app.sh leaves it, into
# target/MelkkiPDF-<version>.dmg: a disk image holding the app beside a link
# to Applications to drag it onto.
#
# A release also sets what build-app.sh signed the app with,
# MACOS_SIGNING_IDENTITY, to sign the image, and an App Store Connect API key
# to notarize with:
#
#   NOTARY_KEY_PATH          The key's .p8 file.
#   NOTARY_KEY_ID            Its ID.
#   NOTARY_ISSUER_ID         The team's issuer ID.
#
# The app is notarized first and its ticket stapled to it, so it opens without
# asking Apple once copied out of the image, even offline; then the image is,
# so opening the download does not ask either. Without these the image is for
# trying out only, as Gatekeeper refuses it on any other Mac.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

app=target/MelkkiPDF.app
if [[ ! -d $app ]]; then
    echo "There is no $app; build it with build-app.sh first." >&2
    exit 1
fi

version=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')
dmg=target/MelkkiPDF-$version.dmg

notarizing=
if [[ -n ${NOTARY_KEY_PATH:-} ]]; then
    if [[ -z ${MACOS_SIGNING_IDENTITY:-} ]]; then
        echo "Only an app signed with a Developer ID can be notarized; set MACOS_SIGNING_IDENTITY." >&2
        exit 1
    fi
    notarizing=1
fi

staging=$(mktemp -d)
trap 'rm -rf "$staging"' EXIT

# Submits a zip or an image and waits for the verdict. notarytool's own exit
# status does not say whether Apple accepted it, so the status is read, and
# for anything but acceptance the log saying why is printed.
notarize() {
    local file=$1 result id status
    local key=(--key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER_ID")
    echo "Notarizing $file, which takes a few minutes."
    result=$(xcrun notarytool submit "$file" "${key[@]}" --wait --timeout 1h --output-format json)
    id=$(jq -r .id <<<"$result")
    status=$(jq -r .status <<<"$result")
    if [[ $status != Accepted ]]; then
        echo "Apple did not accept $file: $status. Its log:" >&2
        xcrun notarytool log "$id" "${key[@]}" >&2 || true
        exit 1
    fi
}

if [[ -n $notarizing ]]; then
    # The service takes a zip, an image or an installer, not a bare bundle.
    ditto -c -k --keepParent "$app" "$staging/MelkkiPDF.zip"
    notarize "$staging/MelkkiPDF.zip"
    xcrun stapler staple "$app"
fi

echo "Packing $dmg."
mkdir "$staging/image"
ditto "$app" "$staging/image/MelkkiPDF.app"
ln -s /Applications "$staging/image/Applications"
rm -f "$dmg"
# LZFSE, which every macOS the app runs on can read.
hdiutil create -quiet -volname MelkkiPDF -srcfolder "$staging/image" \
    -fs HFS+ -format ULFO "$dmg"

if [[ -n ${MACOS_SIGNING_IDENTITY:-} ]]; then
    echo "Signing $dmg with $MACOS_SIGNING_IDENTITY."
    codesign --force --timestamp --sign "$MACOS_SIGNING_IDENTITY" "$dmg"
fi

if [[ -n $notarizing ]]; then
    notarize "$dmg"
    xcrun stapler staple "$dmg"
fi

echo "Done: $dmg"
