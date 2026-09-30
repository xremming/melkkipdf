---
name: release
description: Cut a MelkkiPDF release - bump the version, write the release notes, commit, tag and push, then watch the build. Use when asked to release, cut a version or tag a release.
argument-hint: major | minor | patch | x.y.z
arguments: [bump]
disable-model-invocation: true
allowed-tools: Bash(git:*), Bash(cargo:*), Bash(mise:*), Bash(gh:*), Bash(packaging/release.sh:*), Read, Edit, AskUserQuestion
---

Release the version asked for as `$bump`: `major`, `minor` or `patch` bumps
the current version, and an explicit `x.y.z` is used as given. Everything a
release needs is in this file; `packaging/README.md` has the background.

## Where things stand

- Branch: !`git branch --show-current`
- HEAD: !`git rev-parse --short HEAD`
- Uncommitted: !`git status --short | grep . || echo none`
- Unpushed: !`git fetch --quiet origin main && git log --oneline origin/main..HEAD | grep . || echo none`
- Last CI run on main: !`gh run list --workflow CI --branch main --limit 1 --json headSha,status,conclusion --jq '.[] | "\(.headSha[0:7]) \(.status) \(.conclusion)"'`
- Version now: !`cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version'`
- Last release: !`git describe --tags --abbrev=0 --match 'v*'`
- Since then: !`git log --format='- %s' "$(git describe --tags --abbrev=0 --match 'v*')..HEAD"`
- Cargo.lock changed since then: !`git diff --quiet "$(git describe --tags --abbrev=0 --match 'v*')..HEAD" -- Cargo.lock && echo no || echo yes`

## Steps

Stop and say why at the first thing that is not right; a release is nothing
to push through.

1. **Check the ground.** The branch must be `main`, with nothing uncommitted
   and nothing unpushed. The last CI run on `main` must be for HEAD and have
   succeeded: if it is still running, wait for it with `gh run watch`
   rather than releasing what CI has not yet passed; if it failed, or is
   for another commit, stop. If `Cargo.lock` changed since the last release, run
   `mise run flatpak:sources` and make sure `packaging/cargo-sources.json`
   comes out unchanged; if it changes, that is a missing commit, so stop and
   say so. A version that is not `major`, `minor`, `patch` or `x.y.z` is a
   question back to the user, not a guess.

2. **Work out the version.** Bump the part asked for and zero the parts
   below it: `patch` on 0.6.1 gives 0.6.2, `minor` gives 0.7.0, `major`
   gives 1.0.0. The tag is `v` and the version.

3. **Draft the release notes** from the commits since the last release, and
   show the draft before writing anything. The notes are one paragraph of two
   to five full sentences for the reader of the app, not of the code: what
   they can now do or what stopped bothering them, in the voice of the
   entries already in `packaging/io.github.xremming.MelkkiPDF.metainfo.xml`
   (read the top two). Name keys and buttons as the README does. Leave out
   refactors, CI and tooling unless they change what the reader gets. If
   `Cargo.lock` did not change, the commit body may say so, as earlier
   release commits do, but the notes need not.

   Wait for the user to approve the draft, or to change it, before going on.

4. **Write the release.**
   - `Cargo.toml`: the `version = "…"` line under `[package]`.
   - `cargo check`, so `Cargo.lock` carries the new version too.
   - The metainfo: a new `<release>` first inside `<releases>`, shaped like
     the ones there, with today's date as `YYYY-MM-DD` and the approved notes
     as its `<p>`, wrapped like its neighbours.
   - `mise run release:check` must then pass; it verifies the version and
     the entry agree and that the tag is new, and only complains about the
     uncommitted changes, which the next step makes.

5. **Commit** the three files as `Release x.y.z`, with the approved notes as
   the body, wrapped at 72 columns, and the usual trailer. Nothing else goes
   into this commit.

6. **Tag and push, after asking.** Say what is about to happen: the commit
   is tagged `vx.y.z` and `main` and the tag are pushed, and the tag is what
   builds and publishes the release. On a yes:

   ```sh
   git tag -a vx.y.z -m "MelkkiPDF x.y.z"
   git push origin main vx.y.z
   ```

7. **Watch the build.** Find the Flatpak workflow run the tag started with
   `gh run list --workflow Flatpak --limit 1` and follow it with
   `gh run watch <id> --exit-status`. When it has deployed, confirm the
   signed ref published:

   ```sh
   curl -s https://xremming.github.io/melkkipdf/melkkipdf.flatpakref | grep -c GPGKey
   ```

   A `1` means it did. Report the outcome plainly, and if the build failed,
   what failed, without retrying anything: the tag is out, and what to do
   about a failed release is the user's call.
