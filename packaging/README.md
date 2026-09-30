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
| `index.html`                             | Landing page; `@BASE_URL@` and `@APP_ID@` are filled in at publish time |
| `publish.sh`                             | Builds the repo and lays out the Pages site |
| `check-flatpak.sh`                       | Checks the installed flatpak runs and its metadata validates |
| `release.sh`                             | Checks a release is ready to tag; the `/release` skill's last stop |
| `build-macos-app.sh`                     | Builds an ad-hoc signed `target/MelkkiPDF.app` for local use on macOS |

Each script has a task in [`mise.toml`](../mise.toml), which is how CI and
the steps below run them.

## Installing

```sh
flatpak install https://xremming.github.io/melkkipdf/melkkipdf.flatpakref
```

## Building locally

Needs `flatpak-builder` (or the `org.flatpak.Builder` flatpak, which
`publish.sh` falls back to) and the runtime:

```sh
mise run flatpak:runtime
mise run flatpak:build
```

That leaves an OSTree repo in `packaging/repo` and the site that gets deployed
in `packaging/site`. To try the result:

```sh
mise run flatpak:install
flatpak run io.github.xremming.MelkkiPDF
```

`mise run flatpak:check` then checks the installed app the way CI does, given
`desktop-file-utils` and `appstream`.

## After changing dependencies

The flatpak build has no network access, so every crate has to be listed as a
source. Whenever `Cargo.lock` changes:

```sh
mise run flatpak:sources
git add packaging/cargo-sources.json
```

The task only runs when the lock file is newer than the list, and
`flatpak:build` runs it first, so the flatpak is never built from a stale
list.

## Releasing

A `v*` tag is the only thing that publishes. CI builds the flatpak on every
push and throws the result away, so the manifest is known to work before a
release; nothing reaches the repository until a version is tagged.

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
