#!/usr/bin/env bash
# The release checklist from packaging/README.md, as two commands:
#
#   release.sh check   Checks that the tree is ready to tag: the version in
#                      Cargo.toml has its release entry in the metainfo, the
#                      tag is new, and nothing is uncommitted, which covers a
#                      stale cargo-sources.json once it has been regenerated.
#   release.sh tag     Runs the check, then tags the release and pushes main
#                      and the tag, which is what publishes it.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

metainfo=packaging/io.github.xremming.MelkkiPDF.metainfo.xml

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

tag() {
    check
    echo
    echo "About to tag $(git rev-parse --short HEAD) as $tag and push main and the tag,"
    echo "which builds and publishes the release."
    read -r -p "Continue? [y/N] " answer
    if [[ $answer != [yY] ]]; then
        echo "Nothing tagged."
        exit 1
    fi

    git tag -a "$tag" -m "MelkkiPDF $version"
    git push origin main "$tag"

    cat <<EOF

Tagged and pushed $tag. To confirm the deployment:

    gh run watch --exit-status
    curl -s https://xremming.github.io/melkkipdf/melkkipdf.flatpakref | grep -c GPGKey

A 1 from the last command means the signed ref published.
EOF
}

case ${1:-} in
    check) check ;;
    tag) tag ;;
    *)
        echo "usage: $0 check|tag" >&2
        exit 2
        ;;
esac
