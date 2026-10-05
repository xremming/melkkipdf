#!/usr/bin/env bash
# Checks that the tree is ready to tag as a release: the version in Cargo.toml
# has its release entry in the metainfo, the tag is new, the branch is main,
# and nothing is uncommitted, which covers a stale cargo-sources.json once it
# has been regenerated. The /release skill runs this on the release it has
# prepared, before tagging it. With notes, it prints the release notes instead.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

metainfo=packaging/flatpak/io.github.xremming.MelkkiPDF.metainfo.xml

version=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')
tag=v$version

check() {
    echo "Checking release $version."
    local failed=0

    # The manifest declares no version of its own, so the top release entry
    # is what flatpak list and the software centres report.
    local released
    released=$(grep -o -m1 '<release version="[^"]*"' "$metainfo" | cut -d'"' -f2)
    if [[ $released != "$version" ]]; then
        echo "Cargo.toml says $version but the top release in $metainfo is ${released:-missing}." >&2
        failed=1
    fi

    if git rev-parse --verify --quiet "refs/tags/$tag" >/dev/null; then
        echo "The tag $tag already exists." >&2
        failed=1
    fi

    local branch
    branch=$(git branch --show-current)
    if [[ $branch != main ]]; then
        echo "Releases are tagged on main, not on $branch." >&2
        failed=1
    fi

    # Covers the version bump, the release entry and a regenerated
    # cargo-sources.json alike: whatever is not committed is not released.
    if [[ -n $(git status --porcelain) ]]; then
        echo "The tree has uncommitted changes:" >&2
        git status --short >&2
        failed=1
    fi

    if (( failed )); then
        exit 1
    fi
    echo "Release $version is ready to tag."
}

# Prints the release notes of the version in Cargo.toml, the paragraphs of its
# entry in the metainfo, for the GitHub release the macOS workflow makes.
notes() {
    local entry="//releases/release[@version='$version']/description/p"
    local count i
    count=$(xmllint --xpath "count($entry)" "$metainfo")
    if (( count == 0 )); then
        echo "The metainfo has no release notes for $version." >&2
        exit 1
    fi
    for (( i = 1; i <= count; i++ )); do
        (( i > 1 )) && echo
        # The XML wraps the text; Markdown on GitHub is better off without.
        xmllint --xpath "string(${entry}[$i])" "$metainfo" | tr -s '[:space:]' ' ' | sed 's/^ //; s/ $//'
        echo
    done
}

case ${1:-check} in
    check) check ;;
    notes) notes ;;
    *)
        echo "usage: $0 [check | notes]" >&2
        exit 2
        ;;
esac
