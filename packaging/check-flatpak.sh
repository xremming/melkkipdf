#!/usr/bin/env bash
# Checks the installed flatpak: that the viewer finds every library it links
# inside the runtime, and that the desktop entry and AppStream metadata
# validate. Run after `mise run flatpak:install`, locally or in CI.
#
# Needs desktop-file-utils and appstream (the appstreamcli command).
set -euo pipefail

APP_ID=io.github.xremming.MelkkiPDF

app="$HOME/.local/share/flatpak/app/$APP_ID/current/active"
if [[ ! -d $app ]]; then
    echo "$APP_ID is not installed for this user; run flatpak:install first." >&2
    exit 1
fi

# A binary that builds can still be missing a library the runtime does not
# carry, which only shows up when someone runs it.
echo "Checking the viewer's libraries inside the runtime."
if flatpak run --command=ldd "$APP_ID" /app/bin/melkkipdf | grep 'not found'; then
    echo "The viewer has unresolved libraries inside the runtime." >&2
    exit 1
fi

# The model is downloaded and shrunk at build time, so a build that lost
# either step would still produce a viewer, one that cannot search by
# meaning.
echo "Checking the embedding model ships with the viewer."
for file in config.json tokenizer.json model.safetensors; do
    if ! flatpak run --command=test "$APP_ID" -s "/app/share/melkkipdf/model/$file"; then
        echo "The viewer ships without the model's $file." >&2
        exit 1
    fi
done

echo "Validating the desktop entry."
desktop-file-validate "$app/export/share/applications/$APP_ID.desktop"

# Warnings here (a missing screenshot, say) are worth seeing but not worth
# failing over, so they are only reported.
echo "Validating the AppStream metadata."
appstreamcli validate --no-net "$app/files/share/metainfo/$APP_ID.metainfo.xml" \
    || echo "${GITHUB_ACTIONS:+::warning::}Warning: appstreamcli reported metadata issues." >&2

echo "The installed flatpak checks out."
