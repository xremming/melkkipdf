#!/usr/bin/env bash
# Checks target/MelkkiPDF.app, and the disk image if dmg.sh has made one:
# that the signature holds and uses the hardened runtime, that the version,
# the architectures in MACOS_ARCHS, the model and the licences are all in the
# bundle, and, once it is signed with a Developer ID, that Gatekeeper accepts
# both on the strength of their stapled tickets. Run after build-app.sh, or
# dmg.sh, locally or in CI.
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

echo "Checking the signature of $app."
codesign --verify --deep --strict "$app"
signature=$(codesign --display --verbose=2 "$app" 2>&1)
if ! grep -q '^CodeDirectory .*flags=.*runtime' <<<"$signature"; then
    echo "The bundle is not signed with the hardened runtime, which notarization requires." >&2
    exit 1
fi

echo "Checking the bundle is version $version."
bundled=$(plutil -extract CFBundleShortVersionString raw "$app/Contents/Info.plist")
if [[ $bundled != "$version" ]]; then
    echo "The bundle says $bundled, but Cargo.toml says $version." >&2
    exit 1
fi

echo "Checking the architectures."
archs=$(lipo -archs "$app/Contents/MacOS/melkkipdf")
for arch in ${MACOS_ARCHS:-}; do
    # lipo calls by Apple's name what Rust calls aarch64.
    [[ $arch == aarch64 ]] && arch=arm64
    if [[ " $archs " != *" $arch "* ]]; then
        echo "The binary is for $archs, without $arch." >&2
        exit 1
    fi
done
echo "The binary is for $archs."

# A bundle missing either would still open, but could not search by meaning
# or would be distributed without the notices its licences ask for.
echo "Checking the model and the licences ship with the viewer."
resources=$app/Contents/Resources
for file in model/config.json model/tokenizer.json model/model.safetensors \
    LICENSE third-party-licenses.txt; do
    if [[ ! -s $resources/$file ]]; then
        echo "The bundle ships without $file." >&2
        exit 1
    fi
done

if [[ -f $dmg ]]; then
    echo "Checking $dmg."
    hdiutil verify -quiet "$dmg"
fi

if ! grep -q '^Authority=Developer ID Application' <<<"$signature"; then
    echo "The bundle is not signed with a Developer ID, so Gatekeeper is not asked."
    echo "The bundle checks out."
    exit 0
fi

# Gatekeeper's verdict on a download, read from the stapled tickets rather
# than by asking Apple, as on a Mac that is offline.
echo "Checking Gatekeeper accepts the bundle."
xcrun stapler validate "$app"
spctl --assess --type execute --verbose=2 "$app"
if [[ -f $dmg ]]; then
    echo "Checking Gatekeeper accepts $dmg."
    xcrun stapler validate "$dmg"
    spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"
fi

echo "The bundle checks out."
