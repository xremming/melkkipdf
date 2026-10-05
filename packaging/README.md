# Packaging

MelkkiPDF is distributed as a flatpak from a self-hosted OSTree repository
served by GitHub Pages. An OSTree repo is just static files, so Pages is enough
to host it, and users get automatic updates through `flatpak update`. On macOS
it is a disk image on each GitHub release, signed with a Developer ID and
notarized by Apple. Each platform has a directory, and `common/` holds what
both use.

| File                                    | What it is                                  |
| --------------------------------------- | ------------------------------------------- |
| `common/fetch-model.sh`                  | Downloads the embedding model for searching by meaning into `data/model` |
| `common/release.sh`                      | Checks a release is ready to tag; the `/release` skill's last stop |
| `common/third-party-licenses.txt`        | The licences of everything built into the viewer but its own (generated) |
| `common/generate-licenses.sh`            | Regenerates the above from `Cargo.lock` and `common/licenses/` |
| `flatpak/io.github.xremming.MelkkiPDF.yml` | Flatpak manifest                          |
| `flatpak/io.github.xremming.MelkkiPDF.desktop` | Desktop entry, incl. the `application/pdf` association |
| `flatpak/io.github.xremming.MelkkiPDF.metainfo.xml` | AppStream metadata for software centres |
| `flatpak/cargo-sources.json`             | Every crate as a flatpak source (generated) |
| `flatpak/generate-cargo-sources.sh`      | Regenerates the above from `Cargo.lock`     |
| `flatpak/index.html`                     | Landing page; `@BASE_URL@` and `@APP_ID@` are filled in at publish time |
| `flatpak/publish.sh`                     | Builds the repo and lays out the Pages site |
| `flatpak/check.sh`                       | Checks the installed flatpak runs, ships the model, and its metadata validates |
| `macos/build-app.sh`                     | Builds `target/MelkkiPDF.app`, ad hoc signed unless given a Developer ID |
| `macos/dmg.sh`                           | Packs the app into `target/MelkkiPDF-<version>.dmg`, notarizing both when given a key |
| `macos/check.sh`                         | Checks the app and the image, and Gatekeeper's verdict once they are signed |

Each script has a task in [`mise.toml`](../mise.toml), which is how CI and
the steps below run them, and every script runs from the repository root
wherever it is called from.

## Installing

```sh
flatpak install https://xremming.github.io/melkkipdf/melkkipdf.flatpakref
```

On macOS, download `MelkkiPDF-<version>.dmg` from the
[latest release](https://github.com/xremming/melkkipdf/releases/latest),
open it, and drag MelkkiPDF onto Applications.

## Building locally

Needs `flatpak-builder` (or the `org.flatpak.Builder` flatpak, which
`publish.sh` falls back to) and the runtime:

```sh
mise run flatpak:runtime
mise run flatpak:build
```

That leaves an OSTree repo in `packaging/flatpak/repo` and the site that gets
deployed in `packaging/flatpak/site`. To try the result:

```sh
mise run flatpak:install
flatpak run io.github.xremming.MelkkiPDF
```

`mise run flatpak:check` then checks the installed app the way CI does, given
`desktop-file-utils` and `appstream`.

## The embedding model

Searching by meaning needs
[potion-multilingual-128M](https://huggingface.co/minishlab/potion-multilingual-128M),
half a gigabyte of weights that are not in the repository. The manifest lists
its three files as sources at the revision `common/fetch-model.sh` pins, so
flatpak-builder fetches and checks them like the crates, and the build shrinks
them to int8 with the `quantize_model` example before installing them under
`/app/share/melkkipdf/model`. `macos/build-app.sh` does the same from
`data/model` into the bundle's `Resources/model`, running `fetch-model.sh`
first. Both places are where the viewer looks for the model beside its
binary. To move to another revision, change it and the hashes in
`fetch-model.sh` and the manifest together.

## macOS

`mise run macos:dmg` builds the app for this Mac's architecture, signs it ad
hoc, and packs it into a disk image; `mise run macos:check` checks both. That
is what CI does on every push, and it runs only on the machine that built it.
Needs `rsvg-convert` (`brew install librsvg`) and Xcode's command line tools.

A release is built by `.github/workflows/macos.yml` instead, which sets what
the scripts read to make one anyone can open:

- `MACOS_ARCHS="aarch64 x86_64"` builds the binary for both and joins them
  into a universal one, for Apple silicon and Intel Macs alike. Each needs
  its Rust target, which `rustup target add` installs.
- `MACOS_SIGNING_IDENTITY` signs the app, with the hardened runtime, and the
  image with that Developer ID Application certificate from the keychain.
- `NOTARY_KEY_PATH`, `NOTARY_KEY_ID` and `NOTARY_ISSUER_ID` give
  `notarytool` an App Store Connect API key. The app is notarized and its
  ticket stapled before it goes into the image, and then the image is, so
  Gatekeeper opens both without asking Apple, offline too.

The bundle is built for macOS 11 and later, which is what
`LSMinimumSystemVersion` says and what `MACOSX_DEPLOYMENT_TARGET` builds the
C and the Rust for.

To try a signed build before tagging, run the macOS workflow by hand
(`gh workflow run macOS`): it does everything but publish, and keeps the
image as the run's artifact.

## After changing dependencies

The flatpak build has no network access, so every crate has to be listed as a
source. Whenever `Cargo.lock` changes:

```sh
mise run flatpak:sources
git add packaging/flatpak/cargo-sources.json
```

The task only runs when the lock file is newer than the list, and
`flatpak:build` runs it first, so the flatpak is never built from a stale
list.

The licences shipped with the packages are generated from the lock file too,
and need the same:

```sh
mise run licenses
git add packaging/common/third-party-licenses.txt
```

Both package builds and the release check run it first, and CI fails when
the committed file is not what it would write.

## Licences

The flatpak and the macOS bundle ship `LICENSE` and
`common/third-party-licenses.txt`, in `/app/share/licenses/<app id>/` and in
`Contents/Resources/`. The latter is what the MIT, BSD, Apache and font
licences of the viewer's dependencies ask a binary to carry, and
`generate-licenses.sh` puts it together from three places:

- The crates, from [cargo-about](https://github.com/EmbarkStudios/cargo-about)
  with `common/licenses/about.toml`, for every platform there is a package
  for. A crate offering a choice of licences is listed under the first one
  in `accepted` it allows, which is how Slint comes under the GPL rather
  than its own licences.
- The libraries MuPDF compiles in, from the licence files in the sources
  `mupdf-sys` unpacks.
- What neither carries, kept in `common/licenses/`: the pdf.js icons' and the
  embedding model's licences, and in `common/licenses/mupdf/` the texts the
  `mupdf-sys` crate leaves out of MuPDF's sources. The script stops when
  `mupdf-sys` moves to another MuPDF than those texts were taken for; the
  README there says how to refresh them.

A new dependency under a licence not in `accepted` fails the generator until
it is added there, after checking it allows the viewer's use. One whose
sources MuPDF compiles in, or a new data file built into the viewer, has to
be added to the script by hand.

## Releasing

A `v*` tag is the only thing that publishes. CI builds the flatpak and the
macOS disk image on every push and throws them away, so both are known to
work before a release; nothing reaches the repository or a release until a
version is tagged. The tag's Flatpak workflow publishes the flatpak, and its
macOS workflow makes the GitHub release, with the release notes from the
metainfo, and puts the signed and notarized image on it.

In Claude Code, `/release patch` (or `minor`, `major`, or an explicit
`x.y.z`) does the whole checklist below, showing the release notes for
approval and asking before it pushes. By hand:

1. Bump `version` in `Cargo.toml` and run `cargo check` so `Cargo.lock` picks
   up the new number.

2. Add a `<release>` entry at the top of `<releases>` in
   `io.github.xremming.MelkkiPDF.metainfo.xml`:

   ```xml
   <release version="0.2.0" date="2026-08-16">
     <description>
       <p>What changed, in a sentence or two.</p>
     </description>
   </release>
   ```

   The manifest declares no version of its own, so this entry is what
   `flatpak list` and the software centres report, and the description is the
   changelog they show. Keep it equal to the `Cargo.toml` version so the two
   cannot drift. Dates are `YYYY-MM-DD`.

3. Commit the bump as `Release 0.2.0`, with the notes as the body. If
   dependencies changed, `mise run flatpak:sources` regenerates the vendored
   crate list to commit with it; a bare version bump leaves it alone, since
   the generator lists only crates fetched from a registry, and the viewer's
   own package is not one.

4. Check, tag and push, on `main`:

   ```sh
   mise run release:check
   git tag -a v0.2.0 -m "MelkkiPDF 0.2.0"
   git push origin main v0.2.0
   ```

   The check verifies that the metainfo's top release matches `Cargo.toml`,
   that the tag is new, and that nothing is uncommitted, a stale crate list
   included. The tag is what triggers the build; `main` goes too so the
   published commit is on the branch.

5. Confirm the deployment:

   ```sh
   gh run watch --exit-status
   curl -s https://xremming.github.io/melkkipdf/melkkipdf.flatpakref | grep -c GPGKey
   ```

   A `1` means the signed ref published. Existing installs then pick the build
   up with `flatpak update`; nothing about the remote changes. Only replacing
   the signing key would force users to act, by removing and re-adding the
   remote.

   The macOS workflow takes longer, notarizing twice. Once it is done the
   release has the image:

   ```sh
   gh release view v0.2.0 --json assets --jq '.assets[].name'
   ```

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

3. Give the macOS workflow what it signs and notarizes with, from an Apple
   Developer Program membership:

   - A Developer ID Application certificate, which only the team's Account
     Holder can create, exported from Keychain Access as a `.p12` with its
     private key.
   - A Team API key from App Store Connect, under Users and Access →
     Integrations, with the Developer role; notarytool cannot use an
     Individual key. Its `.p8` file can be downloaded only once.

   ```sh
   base64 < DeveloperID.p12 | gh secret set MACOS_CERTIFICATE_P12
   gh secret set MACOS_CERTIFICATE_PASSWORD
   gh secret set NOTARY_KEY_P8 < AuthKey_XXXXXXXXXX.p8
   gh secret set NOTARY_KEY_ID
   gh secret set NOTARY_ISSUER_ID
   ```

   Notarizing stops working whenever Apple publishes a new Program License
   Agreement until it is accepted on developer.apple.com, and the
   certificate expires after five years.
