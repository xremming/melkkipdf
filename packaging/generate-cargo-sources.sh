#!/usr/bin/env bash
# Regenerates packaging/cargo-sources.json from Cargo.lock.
#
# The flatpak build has no network access, so every crate has to be declared as
# a source in the manifest. Run this after any change to Cargo.lock and commit
# the result.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

generator_url=https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py
cache=${XDG_CACHE_HOME:-$HOME/.cache}/melkkipdf-packaging

mkdir -p "$cache"
if [[ ! -f $cache/flatpak-cargo-generator.py ]]; then
    echo "Fetching flatpak-cargo-generator.py."
    curl -sSfL -o "$cache/flatpak-cargo-generator.py" "$generator_url"
fi

if [[ ! -x $cache/venv/bin/python ]]; then
    echo "Creating a virtualenv for the generator."
    python3 -m venv "$cache/venv"
    "$cache/venv/bin/pip" --quiet install aiohttp tomlkit
fi

echo "Generating packaging/cargo-sources.json."
"$cache/venv/bin/python" "$cache/flatpak-cargo-generator.py" \
    Cargo.lock -o packaging/cargo-sources.json

echo "Done. Remember to commit packaging/cargo-sources.json."
