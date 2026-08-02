# Releasing

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

| File | Contents |
| ---- | -------- |
| `terra-macos-universal.dmg` | `Terra.app` (arm64 + x86_64) plus the `/Applications` drop target |
| `terra-macos-universal.app.tar.gz` | the same universal `.app` |
| `terra-cli-macos-universal.tar.gz` | the `terra` CLI |
| `terra-linux-x86_64.deb` | GUI + CLI, desktop entry and icons |
| `terra-linux-x86_64.tar.gz` | `terra-app` and `terra`, flat |
| `terra-windows-x86_64-setup.exe` | NSIS installer |
| `terra-windows-x86_64.zip` | `terra-app.exe` and `terra.exe` |

The installers are renamed to stable, versionless names so the site and
`install.sh` never have to parse a version out of a URL. Those names are
produced by `scripts/release/build.py` and listed in the workflow's matrix —
the two places to edit if one ever changes.

Every archive holds two binaries because the GUI is only half of terra: `terra`
is what `terra ls`, `terra new` and `terra learn` need on `PATH`.

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

Prerequisites are the same as the runners': a Rust toolchain (plus both
`*-apple-darwin` targets on macOS) and `cargo install cargo-packager --locked
--version 0.11.8`. The version is pinned because the `.app` layout and the dmg
behaviour were verified against exactly that one.

Set `CI=true` locally too if you want the runner's behaviour: cargo-packager
passes `--skip-jenkins` to create-dmg when it is set, which skips Finder window
styling that needs a real session.

## Signing

Off by default and a supported state: with no secrets configured the macOS app
is ad-hoc signed and the Windows installer is unsigned. The build script decides
from the environment — `APPLE_CERTIFICATE` + `APPLE_ID` for the Developer ID and
notarisation path, `SIGN_TUNNEL_URL` + `SIGN_TUNNEL_SECRET` for the Windows
YubiKey server — prints which path it took, and fails the run rather than
falling back to unsigned once a path is enabled. See
[SIGNING.md](SIGNING.md) for the secrets and the setup behind them.

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

**The `.dmg` is built by hand**, not by `cargo packager -f dmg`: the dmg format
re-creates the `.app` from scratch first, which would discard the signature
applied in between. So the script packages the `.app`, signs it, and only then
builds a plain UDZO image around it.

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
