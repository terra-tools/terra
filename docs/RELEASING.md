# Releasing

## The checklist

Every release, in this order — each line has burned us once:

1. **Start the Windows signing server first** (skip only for an unsigned
   release). On the Mac with the YubiKey plugged in:
   `uv run scripts/sign-server/sign_server.py`, and keep the machine awake for
   the whole run — the v1.3.2 Windows job failed with Cloudflare 530/1033
   because the tunnel's host was asleep. Smoke-test from outside:
   `curl -s -o /dev/null -w '%{http_code}' -A terra-release-sign/1.0 -X POST
   <SIGN_TUNNEL_URL>/sign` must print `401` (reachable, secret required) —
   `530` means the tunnel is down. Ctrl+C the server when the run is done.
2. **Bump the workspace version** in the root `Cargo.toml` (one place; every
   crate inherits it) and let `cargo check` refresh `Cargo.lock`.
3. **Branch, commit, open a PR** titled in the house style:
   `release: vX.Y.Z — <what changed, one clause>`.
4. **Wait for the PR's CI to go green before merging.** CI covers what a
   macOS-only local run cannot: the Linux runner (where `command` means Ctrl —
   input semantics genuinely differ) and the Windows build. The v1.4.0 tag was
   pushed on a merge that had never seen Linux CI, and the release run died on
   tests that could not fail on the author's machine.
5. **Squash-merge** with the PR title as the commit message, `git pull`.
6. **Tag the squash commit**: `git tag vX.Y.Z && git push origin vX.Y.Z`.
7. Watch the run: `gh run watch $(gh run list --workflow=release.yml -L1
   --json databaseId -q '.[0].databaseId')`. If one platform fails on
   infrastructure (signing tunnel, runner flake), fix the cause and
   `gh run rerun <id> --failed` — the tag does not need to move. If the
   *commit* is broken: fix on a new PR, then move the tag
   (`git tag -f vX.Y.Z && git push -f origin vX.Y.Z`) only if the release was
   never published; otherwise bump the patch version.

Test-environment note: the PTY harness suites drive a **real tmux**. CI
installs it on Linux (apt) and macOS (brew) — see the workflows — and the
suites are `#![cfg(unix)]`, so Windows skips them.

## The machinery

A release is a tag push. Everything else is automatic:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

`.github/workflows/release.yml` fans out over three pinned runners (macos-15,
ubuntu-22.04, windows-2022), runs `cargo test --workspace`, then hands the
platform-specific work to `scripts/release/build.py` and publishes what lands in
`dist/`. To rehearse without publishing, run the workflow from the Actions tab
(`workflow_dispatch`): every job builds and uploads its artifacts, and only the
publish step is skipped because the ref is not a tag.

## Artifacts

Three files, one installer per platform:

| File | Contents |
| ---- | -------- |
| `terra-macos-universal.dmg` | `Terra.app` (arm64 + x86_64) plus the `/Applications` drop target |
| `terra-linux-x86_64.deb` | `/usr/bin/terra-app` and `/usr/bin/terra`, desktop entry and icons |
| `terra-windows-x86_64-setup.exe` | NSIS installer: `terra-app.exe` and `terra.exe` |

The installers are renamed to stable, versionless names so the site and
`install.sh` never have to parse a version out of a URL. Those names are
produced by `scripts/release/build.py` and listed in the workflow's matrix —
the two places to edit if one ever changes.

Every installer carries both binaries, because the GUI is only half of terra:
`terra` is what `terra ls`, `terra new` and `terra learn` need. That comes from
the `binaries` list in `[package.metadata.packager]`; cargo-packager otherwise
picks up only the bin targets of the `terra-app` crate. On macOS the CLI sits at
`Terra.app/Contents/MacOS/terra` and `install.sh` symlinks it into
`/usr/local/bin`; on Linux and Windows the installer puts it on `PATH` directly.

There are no plain archives any more. Anyone who wants loose binaries can build
them: `cargo build --release -p terra-app -p terra-cli`.

## Running the build locally

The scripts are plain Python with [PEP 723](https://peps.python.org/pep-0723/)
inline metadata and no third-party dependencies, run by
[uv](https://docs.astral.sh/uv/) (`brew install uv`):

```sh
uv run scripts/release/build.py macos     # or linux, windows
uv run scripts/release/build.py macos --dry-run   # print the commands only
```

CI runs exactly these commands. `--dry-run` echoes the whole sequence without
touching anything, which is the cheapest way to check a change to the script.

Each platform is now one `cargo packager` call: `-f app -f dmg` on macOS (one
invocation — the dmg format reuses the `.app` already in `out_dir` instead of
rebuilding it), `-f deb` on Linux, `-f nsis` on Windows. The script's remaining
work is the `lipo` merge for the universal build, the rename into `dist/`, and
telling cargo-packager which identity to sign with.

Prerequisites are the same as the runners': a Rust toolchain (plus both
`*-apple-darwin` targets on macOS) and `cargo install cargo-packager --locked
--version 0.11.8`. The version is pinned because the `.app` layout, the dmg
behaviour and the signing hooks were verified against exactly that one. CI
caches the installed binary, keyed on that version, rather than rebuilding it
each run.

`cargo packager -p terra-app --release` on its own (the `just bundle` path)
produces an ad-hoc signed `.app` and `.dmg` with no notarisation, which is what
a release with no secrets configured produces too.

Set `CI=true` locally too if you want the runner's behaviour: cargo-packager
passes `--skip-jenkins` to create-dmg when it is set, which skips Finder window
styling that needs a real session.

## Signing

Off by default and a supported state: with no secrets configured the macOS app
is ad-hoc signed and the Windows installer is unsigned. The build script decides
from the environment — `APPLE_CERTIFICATE` + `APPLE_ID` for the Developer ID and
notarisation path, `SIGN_TUNNEL_URL` + `SIGN_TUNNEL_SECRET` for the Windows
YubiKey server — prints which path it took, and fails the run rather than
falling back to unsigned once a path is enabled.

**cargo-packager does the signing now**, not the build script: it imports the
`.p12`, signs the bundle inside-out, notarises and staples on macOS, and shells
out to `scripts/release/sign_client.py` for each Windows file. See
[SIGNING.md](SIGNING.md) for the secrets, the wiring and the setup behind them.

## Packaging decisions

**NSIS, not WiX, on Windows.** It is what the Tauri ecosystem ships, the
toolchain download is small and reliable on the GitHub runner, and for an
unsigned binary an `.msi` buys nothing — SmartScreen warns either way.
Enterprise MSI deployment is the one reason to add `-f wix` later, and that is a
one-flag change.

**A `.deb`, no AppImage, on Linux.** The `.deb` comes from the same
cargo-packager manifest as everything else. AppImage tooling (linuxdeploy) is a
build-time download and one more unattended moving part on a tag push, for a
format nothing here needs yet.

**The `.dmg` comes from `cargo packager -f dmg`.** It used to be built by hand
with `hdiutil`, because the script signed the `.app` between the two steps and a
second `cargo packager` invocation would have rebuilt the bundle and discarded
that signature. Now cargo-packager signs during bundling, and a single
`-f app -f dmg` call is safe: `src/package/mod.rs` packages the `app` format for
a dmg only when no app output exists yet, so within one invocation the dmg is
built around the bundle that was just signed.

**Installers only.** The `.app.tar.gz`, the CLI tarball, the Linux `.tar.gz` and
the Windows `.zip` are gone. They existed to carry the `terra` CLI, which the
installers now ship themselves, and each one was a second way to get terra onto
a machine that nothing verified.

## TODO: updater feed (`latest.json`)

Deliberately not implemented. What is already established, from cargo-packager
0.11.8's own source:

- There is no macOS "updater" package format. `cargo packager --help` lists
  exactly: all, default, app, dmg, wix, nsis, deb, appimage, pacman.
- The `.app.tar.gz` that Tauri users expect is produced only as a side effect of
  signing. `sign_outputs()` in `src/lib.rs` walks the package outputs and, for
  any output that is a directory (i.e. the `.app`), does
  `path.with_additional_extension("tar.gz")`, tars and gzips it, appends it to
  the output list and signs it. With no key passed, neither tarball nor
  signature is ever written.
- Signing is opt-in via `-k/--private-key` or
  `CARGO_PACKAGER_SIGN_PRIVATE_KEY`. `src/sign.rs` writes `<file>.sig` holding a
  minisign signature box that is then base64-encoded, with a trusted comment of
  `timestamp:<unix>\tfile:<filename>`.
- So the updater pair on macOS would be `terra.app.tar.gz` and
  `terra.app.tar.gz.sig`, and the `.sig` content is the base64 text that goes in
  the feed's `signature` field.
- cargo-packager never generates `latest.json`. The feed is the consumer's job
  (see the separate `cargo-packager-updater` crate).

What must be answered before writing a feed:

1. What exact JSON schema does `cargo-packager-updater` expect? Field names and
   nesting (`platforms` vs top-level, `url` / `signature` / `version` /
   `pub_date`), and the target keys — is a universal build addressed as
   `darwin-universal`, or must both `darwin-aarch64` and `darwin-x86_64` be
   listed pointing at the same universal tarball? Read the
   cargo-packager-updater source; do not infer it from Tauri.
2. Does the updater verify the signature against the tarball bytes or against
   the base64 wrapper? Signing here is a two-layer encoding.
3. The `.app` is ad-hoc signed with a throwaway identity by default. An in-place
   update replaces a bundle signed by one ad-hoc identity with one signed by
   another; confirm that does not trip Gatekeeper before shipping updates.

Wiring it up, once those are answered: add a `packager-signing` repo secret
holding the key from `cargo packager signer generate` (generated with no
password, since the runner cannot type one), pass it as
`CARGO_PACKAGER_SIGN_PRIVATE_KEY`, re-run the packager step with the key so the
`.app.tar.gz` and `.sig` are produced, then write `latest.json` and add it to
the artifact list.

Until then there is no feed on purpose. A malformed feed is worse than none:
clients would either reject every update or, worse, accept an unverified one.
