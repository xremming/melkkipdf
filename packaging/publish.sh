#!/usr/bin/env bash
# Builds the flatpak into an OSTree repo and lays out the static site that is
# published to GitHub Pages. Used by CI and runnable locally to preview the
# exact tree that gets deployed.
#
# Set GPG_KEY_ID (and optionally GPG_HOMEDIR) to sign the repo. Unsigned repos
# work over HTTPS but clients cannot verify the publisher.
set -euo pipefail

APP_ID=io.github.xremming.MelkkiPDF
BRANCH=stable
BASE_URL=${BASE_URL:-https://xremming.github.io/melkkipdf}

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

repo=${REPO_DIR:-packaging/repo}
site=${SITE_DIR:-packaging/site}

# Distributions increasingly ship flatpak-builder only as a flatpak.
if command -v flatpak-builder >/dev/null; then
    builder=(flatpak-builder)
else
    builder=(flatpak run org.flatpak.Builder)
fi

gpg_args=()
gpg_homedir_args=()
if [[ -n ${GPG_KEY_ID:-} ]]; then
    gpg_args+=(--gpg-sign="$GPG_KEY_ID")
    if [[ -n ${GPG_HOMEDIR:-} ]]; then
        gpg_args+=(--gpg-homedir="$GPG_HOMEDIR")
        gpg_homedir_args+=(--homedir "$GPG_HOMEDIR")
    fi
fi

echo "Building $APP_ID."
"${builder[@]}" --force-clean --disable-rofiles-fuse \
    --repo="$repo" --default-branch="$BRANCH" \
    "${gpg_args[@]}" \
    packaging/build-dir "packaging/$APP_ID.yml"

echo "Updating the repo summary."
flatpak build-update-repo --generate-static-deltas --prune \
    "${gpg_args[@]}" "$repo"

echo "Laying out the site in $site."
rm -rf "$site"
mkdir -p "$site"
cp -a "$repo" "$site/repo"
cp "data/icons/hicolor/scalable/apps/$APP_ID.svg" "$site/icon.svg"

# Both ref files carry the public key inline so a user only needs the one URL.
gpg_key_line=""
if [[ -n ${GPG_KEY_ID:-} ]]; then
    key=$(gpg "${gpg_homedir_args[@]}" --export "$GPG_KEY_ID" | base64 -w0)
    gpg_key_line="GPGKey=$key"
fi

cat >"$site/melkkipdf.flatpakrepo" <<EOF
[Flatpak Repo]
Title=MelkkiPDF
Url=$BASE_URL/repo/
Homepage=https://github.com/xremming/melkkipdf
Comment=A fast, minimal PDF viewer for Linux
Description=A fast, minimal PDF viewer for Linux, inspired by SumatraPDF.
Icon=$BASE_URL/icon.svg
$gpg_key_line
EOF

cat >"$site/melkkipdf.flatpakref" <<EOF
[Flatpak Ref]
Name=$APP_ID
Branch=$BRANCH
Url=$BASE_URL/repo/
Title=MelkkiPDF
Homepage=https://github.com/xremming/melkkipdf
Comment=A fast, minimal PDF viewer for Linux
Description=A fast, minimal PDF viewer for Linux, inspired by SumatraPDF.
Icon=$BASE_URL/icon.svg
RuntimeRepo=https://dl.flathub.org/repo/flathub.flatpakrepo
IsRuntime=false
SuggestRemoteName=melkkipdf
$gpg_key_line
EOF

cat >"$site/index.html" <<EOF
<!doctype html>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>MelkkiPDF</title>
<style>
  :root { color-scheme: light dark; }
  body { max-width: 42rem; margin: 4rem auto; padding: 0 1.5rem;
         font: 16px/1.6 system-ui, sans-serif; }
  pre { background: color-mix(in srgb, currentColor 8%, transparent);
        padding: 1rem; border-radius: .5rem; overflow-x: auto; }
  code { font-family: ui-monospace, monospace; }
  img { width: 96px; height: 96px; }
</style>
<img src="icon.svg" alt="">
<h1>MelkkiPDF</h1>
<p>A fast, minimal PDF viewer for Linux, inspired by SumatraPDF.</p>

<h2>Install</h2>
<p>Requires <a href="https://flathub.org/setup">flatpak</a>.</p>
<pre><code>flatpak install $BASE_URL/melkkipdf.flatpakref</code></pre>
<p>Then run it from your application menu, or with
   <code>flatpak run $APP_ID</code>.</p>

<h2>Updates</h2>
<pre><code>flatpak update $APP_ID</code></pre>

<h2>Source</h2>
<p><a href="https://github.com/xremming/melkkipdf">github.com/xremming/melkkipdf</a></p>
EOF

echo "Site ready: $site"
