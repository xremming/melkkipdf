# Packaging

MelkkiPDF is distributed as a flatpak from a self-hosted OSTree repository
served by GitHub Pages. An OSTree repo is just static files, so Pages is enough
to host it, and users get automatic updates through `flatpak update`.

| File                                    | What it is                                  |
| --------------------------------------- | ------------------------------------------- |
| `io.github.xremming.MelkkiPDF.yml`       | Flatpak manifest                            |
| `io.github.xremming.MelkkiPDF.desktop`   | Desktop entry, incl. the `application/pdf` association |
| `io.github.xremming.MelkkiPDF.metainfo.xml` | AppStream metadata for software centres  |
| `cargo-sources.json`                     | Every crate as a flatpak source (generated) |
| `generate-cargo-sources.sh`              | Regenerates the above from `Cargo.lock`     |
| `publish.sh`                             | Builds the repo and lays out the Pages site |

## Installing

```sh
flatpak install https://xremming.github.io/melkkipdf/melkkipdf.flatpakref
```

## Building locally

Needs `flatpak-builder` (or the `org.flatpak.Builder` flatpak, which
`publish.sh` falls back to) and the runtime:

```sh
flatpak remote-add --if-not-exists --user \
    flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user flathub \
    org.freedesktop.Platform//25.08 \
    org.freedesktop.Sdk//25.08 \
    org.freedesktop.Sdk.Extension.rust-stable//25.08 \
    org.freedesktop.Sdk.Extension.llvm21//25.08

./packaging/publish.sh
```

That leaves an OSTree repo in `packaging/repo` and the site that gets deployed
in `packaging/site`. To try the result:

```sh
flatpak install --user --reinstall packaging/repo io.github.xremming.MelkkiPDF
flatpak run io.github.xremming.MelkkiPDF
```

## After changing dependencies

The flatpak build has no network access, so every crate has to be listed as a
source. Whenever `Cargo.lock` changes:

```sh
./packaging/generate-cargo-sources.sh
git add packaging/cargo-sources.json
```

## Releasing

Pushing to `main` or a `v*` tag builds and deploys. Add the new version to the
`<releases>` list in the metainfo file first — software centres show it as the
changelog.

## One-time repository setup

1. **Settings → Pages → Source: GitHub Actions.**
2. Optionally sign the repo, so clients can verify the publisher rather than
   trusting HTTPS alone:

   ```sh
   gpg --quick-generate-key "MelkkiPDF <you@example.com>" default default never
   gpg --list-secret-keys --keyid-format=long     # note the key ID
   gpg --export-secret-keys --armor <KEY_ID>      # paste into the secret below
   ```

   Add repository secrets `FLATPAK_GPG_PRIVATE_KEY` (the armoured private key)
   and `FLATPAK_GPG_KEY_ID`. The workflow picks them up automatically and
   embeds the public key in the `.flatpakref`. Without them the repo is
   published unsigned, which works but warns.

   Keep the key: re-publishing under a *different* key breaks updates for
   everyone who already installed.

   To test signing locally, put `GPG_HOMEDIR` somewhere under `$HOME`. A
   flatpak gets its own `/tmp`, so a keyring (or repo) under `/tmp` is
   invisible to the `org.flatpak.Builder` sandbox and the export fails with
   `mkdirat: No such file or directory`. CI is unaffected: it uses the
   distribution's flatpak-builder, which is not sandboxed.
