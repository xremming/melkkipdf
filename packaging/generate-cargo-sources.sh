#!/usr/bin/env bash
# Regenerates packaging/cargo-sources.json from Cargo.lock.
#
# The flatpak build has no network access, so every crate has to be declared as
# a source in the manifest. Run this after any change to Cargo.lock and commit
# the result. `mise run flatpak:sources` does so only when the lock file is
# newer than the list.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

# The generator is pinned to a commit, so the same Cargo.lock always gives the
# same sources and a change upstream cannot slip into the build unreviewed.
# Bump it deliberately, and check the regenerated file's diff when you do.
generator_commit=41c20aa10819cdb2a4f3ca171758a96d1955c018
generator_url=https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/$generator_commit/cargo/flatpak-cargo-generator.py
cache=${XDG_CACHE_HOME:-$HOME/.cache}/melkkipdf-packaging
# Named after the commit, so bumping the pin fetches the new version.
generator=$cache/flatpak-cargo-generator-$generator_commit.py

mkdir -p "$cache"
if [[ ! -f $generator ]]; then
    echo "Fetching flatpak-cargo-generator.py at $generator_commit."
    curl -sSfL -o "$generator" "$generator_url"
fi

# uv brings the generator's two dependencies along, and a Python if there is
# none, without a virtualenv to keep.
echo "Generating packaging/cargo-sources.json."
uv run --quiet --no-project --with aiohttp --with tomlkit \
    "$generator" Cargo.lock -o packaging/cargo-sources.json

echo "Done. Remember to commit packaging/cargo-sources.json."
