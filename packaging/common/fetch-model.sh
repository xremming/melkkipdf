#!/usr/bin/env bash
# Downloads the embedding model that searching by meaning uses into
# data/model, where the viewer finds it through MELKKIPDF_MODEL_DIR (which
# mise.toml sets for every task and every shell it activates). Pinned to one
# revision and checked against its hashes, so every checkout vectorizes the
# same way. Run again to check an existing download; nothing is fetched twice.
#
# Needs curl and shasum.
set -euo pipefail

REPO=minishlab/potion-multilingual-128M
REVISION=73908c3438cf03b6a01bcb9611d62b23d0726f08
HASHES='595e4cab2093732efd5dbe084fd5c1826b5eea693b73b4c1fd971672867d2e54  config.json
19f1909063da3cfe3bd83a782381f040dccea475f4816de11116444a73e1b6a1  tokenizer.json
14b5eb39cb4ce5666da8ad1f3dc6be4346e9b2d601c073302fa0a31bf7943397  model.safetensors'

cd "$(dirname "$0")/../.."
mkdir -p data/model
cd data/model

while read -r hash file; do
    if [[ -f $file ]] && echo "$hash  $file" | shasum -a 256 --check --status; then
        echo "$file is already here."
        continue
    fi
    echo "Downloading $file."
    curl --fail --location --progress-bar \
        --output "$file.part" "https://huggingface.co/$REPO/resolve/$REVISION/$file"
    echo "$hash  $file.part" | shasum -a 256 --check --status || {
        echo "$file did not match its hash; the download is kept as $file.part." >&2
        exit 1
    }
    mv "$file.part" "$file"
done <<< "$HASHES"

echo "The model is in $(pwd)."
