# Code signing

Terra's releases are **unsigned by default**, and that is a supported state —
a tag push with no signing secrets configured produces exactly what it always
has: an ad-hoc signed macOS app and an unsigned Windows installer. Everything
described here is opt-in, gated on repository secrets. Set none, and every
signing step in `.github/workflows/release.yml` is skipped.

The design is lifted from [vibe](https://github.com/thewh1teagle/vibe), which
signs the same way with the same Apple account and the same YubiKey. Each
section below cites the vibe file it came from.

## Contents

- [What users see today (unsigned)](#what-users-see-today-unsigned)
- [How the opt-in works](#how-the-opt-in-works)
- [macOS: Developer ID + notarisation](#macos-developer-id--notarisation)
  - [The CLI has to be signed twice](#the-cli-has-to-be-signed-twice)
- [Windows: YubiKey signing server](#windows-yubikey-signing-server)
- [Verifying a release](#verifying-a-release)

## What users see today (unsigned)

**macOS.** The `.app` gets an ad-hoc signature (`codesign --sign -`). That is
not a certificate; it is a stable code identity and nothing more. Gatekeeper
shows "Terra cannot be opened because it is from an unidentified developer" on
first launch. The way through is right-click (or Control-click) the app →
**Open** → **Open** in the dialog, which records an exception for that copy.
The download also carries a quarantine attribute; `xattr -dr
com.apple.quarantine /Applications/Terra.app` clears it if the prompt is
being stubborn.

**Windows.** The NSIS `setup.exe` is unsigned. SmartScreen shows "Windows
protected your PC" — **More info** → **Run anyway**. This is more painful than
the macOS case because it also affects reputation: an unsigned installer never
accumulates SmartScreen reputation, so the warning never goes away on its own.

**Linux.** Nothing is signed and nothing complains. The `.deb` is unaffected by
any of this; the Linux job is untouched.

The release notes appended by the `support-notes` job state the unsigned
situation in these terms. If signing is turned on permanently, that text is
worth updating.

## How the opt-in works

The workflow passes every signing secret as environment variables to the one
step that builds and packages. Unset secrets arrive as empty strings, so nothing
has to inspect the `secrets` context in an `if:` (which GitHub Actions does not
allow anyway).

**cargo-packager 0.11.8 does the signing itself**, in both directions;
`scripts/release/build.py` only chooses the identity and reports which path it
took. Once a path is enabled a failure in it fails the run — there is no
fallback from real signing to unsigned.

**macOS is packager-native.** `[package.metadata.packager.macos]` in
`crates/terra-app/Cargo.toml` carries `signing-identity = "-"`, and that single
setting is what turns everything on: cargo-packager then signs every Mach-O in
the bundle and the bundle itself, inside-out, with `--options runtime
--timestamp`, signs the `.dmg` afterwards, and notarises when it can find
credentials. `-` is an ad-hoc signature, which is exactly the unsigned default.
For a real release `build.py` rewrites that one line to `$APPLE_SIGNING_IDENTITY`
before packaging and restores it in a `finally`. It has to: the certificate and
the notarisation credentials are read from the environment by cargo-packager,
but the *identity* is config-only — there is no `APPLE_SIGNING_IDENTITY`
fallback anywhere in 0.11.8's source. The environment variables it does read,
from `src/codesign/macos.rs`:

| Variable | Read by | Effect |
| -------- | ------- | ------ |
| `APPLE_CERTIFICATE` | `try_sign()` | base64 `.p12`, imported into a throwaway `cargo-packager.keychain` deleted after signing |
| `APPLE_CERTIFICATE_PASSWORD` | `try_sign()` | the `.p12` password; both must be set or no keychain is created |
| `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` | `notarize_auth()` | notarisation credentials; all three, or notarisation is skipped with a warning |
| `APPLE_KEYCHAIN_PROFILE` | `notarize_auth()` | alternative: a stored `notarytool` profile, checked first |
| `APPLE_API_KEY`, `APPLE_API_ISSUER`, `APPLE_API_KEY_PATH` | `notarize_auth()` | alternative: App Store Connect key |

**Windows goes through `sign-command`.** Also in the manifest:

```toml
[package.metadata.packager.windows]
sign-command = "terra-sign.cmd %1"
```

cargo-packager substitutes the file to sign for `%1` and runs the command for
the main binary, for the uninstaller (as an NSIS `!uninstfinalize` line) and for
the finished installer. `terra-sign.cmd` is a one-line shim that `build.py`
writes into `$RUNNER_TEMP` and prepends to `PATH` for the packaging step; it
calls `scripts/release/sign_client.py`. The shim exists because the hook runs
from three different working directories, so no relative path to the script
would resolve in all of them.

The one gap: 0.11.8 offers the hook only the *main* binary
(`src/package/nsis/mod.rs`), so `terra.exe` would go into the installer
unsigned. `build.py` signs it with the same client before packaging.

vibe instead gates on `workflow_dispatch` inputs (`sign-macos`, `sign-windows`)
in
[`.github/workflows/release.yml`](https://github.com/thewh1teagle/vibe/blob/main/.github/workflows/release.yml).
Terra releases on tag push rather than by hand, so presence-of-secret is the
gate here. The effect is the same and the secret names are identical, so the
same values work in both repositories.

## macOS: Developer ID + notarisation

Source:
[`docs/code-signing/macos.md`](https://github.com/thewh1teagle/vibe/blob/main/docs/code-signing/macos.md).
Read it for the full walkthrough — CSR, certificate creation, and especially
the intermediate-certificate trap. What follows is the short version plus what
is specific to Terra.

### Secrets

| Secret                       | Value                                                    |
| ---------------------------- | -------------------------------------------------------- |
| `APPLE_CERTIFICATE`          | Developer ID Application `.p12`, base64-encoded           |
| `APPLE_CERTIFICATE_PASSWORD` | Password used when exporting the `.p12`                   |
| `APPLE_SIGNING_IDENTITY`     | `Developer ID Application: Your Name (TEAMID)`            |
| `APPLE_ID`                   | Apple ID email                                            |
| `APPLE_PASSWORD`             | App-specific password from appleid.apple.com              |
| `APPLE_TEAM_ID`              | 10-character team ID                                      |

`APPLE_CERTIFICATE` and `APPLE_ID` are the two the workflow checks; set all six
or the run will fail partway through.

### Getting the certificate

1. Enrol in the Apple Developer Program ($99/year). Only the Account Holder can
   create a Developer ID Application certificate.
2. Keychain Access → Certificate Assistant → **Request a Certificate from a
   Certificate Authority**, saved to disk. The private key lands in your login
   keychain.
3. [Certificates, Identifiers & Profiles](https://developer.apple.com/account/resources/certificates/list)
   → **Developer ID Application** → **G2 Sub-CA** → upload the CSR → download
   the `.cer`.
4. Double-click to install into the **login** keychain.
5. **Install the intermediate.** From
   [Apple's Certificate Authority page](https://www.apple.com/certificateauthority/),
   install *Developer ID - G2* and *Apple Root CA - G2*. Skip this and
   `security find-identity -v -p codesigning` reports "1 matching identity, 0
   valid identities", which looks like a permissions problem and is not. vibe's
   macos.md calls this the single most common failure.
6. Export from **My Certificates** as `.p12` with a strong password, then:

```sh
openssl base64 -A -in certificate.p12 -out certificate-base64.txt
gh secret set APPLE_CERTIFICATE -b "$(cat certificate-base64.txt)"
gh secret set APPLE_CERTIFICATE_PASSWORD -b '<p12 password>'
gh secret set APPLE_SIGNING_IDENTITY -b 'Developer ID Application: Your Name (TEAMID)'
gh secret set APPLE_ID -b 'you@example.com'
gh secret set APPLE_PASSWORD -b '<app-specific password>'
gh secret set APPLE_TEAM_ID -b 'TEAMID'
rm certificate.p12 certificate-base64.txt
```

The app-specific password comes from
[appleid.apple.com](https://appleid.apple.com) → Sign-In and Security →
App-Specific Passwords. A regular Apple ID password gets a 401 from
`notarytool`.

### What the workflow then does

vibe gets all of this for free from `tauri-action`; terra gets it from
cargo-packager, which grew the same machinery. Almost all of it happens inside
one `cargo packager -f app -f dmg` call. The exception comes first:

0. **Pre-sign the `terra` CLI**, in `build.py`, in its own throwaway
   `terra-presign.keychain-db`, deleted again before the packager starts. See
   [The CLI has to be signed twice](#the-cli-has-to-be-signed-twice) below —
   without this the release fails outright, roughly half the time.

Then, inside the packager:

1. **Import** the `.p12` into a `cargo-packager.keychain`, with
   `security set-key-partition-list` so `codesign` does not block on a GUI
   prompt no one can click.
2. **Sign** every Mach-O inside the bundle — `terra-app`, the `terra` CLI, any
   framework — deepest path first, then the bundle, with `--options runtime
   --timestamp`. Hardened runtime is mandatory for notarisation; the timestamp
   keeps signatures valid past certificate expiry. This is Apple's recommended
   inside-out order and better than the `--deep` the script used to pass.
3. **Notarise** the `.app`: `ditto` it into a zip, `xcrun notarytool submit
   --wait`, then `xcrun stapler staple` the bundle.
4. **Build the `.dmg`** around the stapled bundle and sign the image.
5. **Delete the keychain**, on success and on failure.

### The CLI has to be signed twice

cargo-packager collects every Mach-O in the bundle and sorts them **by path
depth alone** before signing — `impl Ord for SignTarget` in
`src/codesign/macos.rs` compares `path.components().count()` and nothing else,
and the result goes through a `BinaryHeap`. `Contents/MacOS/terra` and
`Contents/MacOS/terra-app` sit at the same depth, so they tie, and the heap
returns them in whatever order it likes. The `binaries` order in the manifest
has no effect; neither does renaming.

That matters because `codesign`, pointed at a bundle's **main** executable,
validates the whole bundle rather than just that one file. If it reaches
`terra-app` while `terra` is still unsigned, the run dies with:

```
Contents/MacOS/terra-app: code object is not signed at all
In subcomponent: Contents/MacOS/terra
```

which is exactly what failed the v1.0.2 macOS job. So `build.py` signs the CLI
before packaging; the packager then re-signs it with `--force`, harmlessly.

Two traps worth writing down:

- **A local build hides this.** The arm64 linker ad-hoc signs what it produces,
  so a native `cargo packager` run finds the sibling already signed and works.
  `lipo` output carries no signature and neither does a cross-compiled slice,
  so CI hits it and your laptop does not. To reproduce, `codesign
  --remove-signature` both binaries first.
- **Ad-hoc is not a shortcut here.** Notarisation rejects a bundle containing an
  ad-hoc signed binary, so the pre-sign has to use the real identity on the
  signed path. The unsigned path pre-signs ad-hoc, which is all it needs — that
  path fails the same way without it.

### Not covered by a rehearsal

A `workflow_dispatch` run has no secrets, so it exercises only the ad-hoc path.
The Developer ID path — `.p12` import, `set-key-partition-list`, notarisation,
stapling — is first exercised by a real tag push, and the same is true of the
Windows tunnel. Check the first tagged release after any change here.

One difference from the old flow, worth knowing: cargo-packager submits the
`.app`, not the `.dmg`, so the image ends up signed but without a stapled
ticket of its own. The bundle inside it is stapled, which is what Gatekeeper
assesses when the app is launched from `/Applications`, but `xcrun stapler
validate` on the `.dmg` will fail. If that ever matters, staple the image by
hand after the run.

The first notarisation for a new certificate can take hours on Apple's side;
subsequent ones are minutes. If a run times out, the submission is still
progressing — check with `xcrun notarytool history` and staple by hand rather
than re-signing.

## Windows: YubiKey signing server

Sources:
[`docs/code-signing/windows_yubikey.md`](https://github.com/thewh1teagle/vibe/blob/main/docs/code-signing/windows_yubikey.md)
(token setup),
[`scripts/sign_server.py`](https://github.com/thewh1teagle/vibe/blob/main/scripts/sign_server.py)
(server),
[`scripts/sign_windows.py`](https://github.com/thewh1teagle/vibe/blob/main/scripts/sign_windows.py)
(client).

The certificate (SSL.com, individual code signing) is bound to a YubiKey FIPS
by attestation: the private key is generated *inside* the token and cannot be
exported. Unlimited signings, no per-signature fee, no cloud service — but also
nothing that can be handed to a GitHub runner. So the runner uploads binaries
to a small HTTP server running on the machine the token is plugged into.

### Secrets

| Secret               | Value                                     |
| -------------------- | ----------------------------------------- |
| `SIGN_TUNNEL_URL`    | Public URL of the signing server          |
| `SIGN_TUNNEL_SECRET` | Shared secret sent as `X-Tunnel-Secret`   |

Both names are vibe's, unchanged. Set both to enable; unset either to disable.

### Server

`scripts/sign-server/sign_server.py` — Flask, one endpoint, ~50 lines of actual
logic. It shells out to `jsign --storetype YUBIKEY` (PKCS#11 to PIV slot 9c,
DigiCert timestamp) and streams the signed file back. `cloudflared` gives it a
public hostname so no port is opened on the home router; this is how the GitHub
runner reaches a machine sitting on someone's desk.

Setup, prerequisites and env vars are in
[`scripts/sign-server/README.md`](../scripts/sign-server/README.md). In short:
plug in the YubiKey (physical USB — PIV does not work over RDP), then

```sh
uv run scripts/sign-server/sign_server.py
```

and leave it running for the duration of the release job.

### Client

`scripts/release/sign_client.py` — one file, one argument, stdlib only, so the
Windows release job needs no toolchain beyond the `uv` it already uses. The
protocol is vibe's exactly: `POST /sign`, multipart field `file`, secret in
`X-Tunnel-Secret`, signed bytes in the response body. With `SIGN_TUNNEL_URL` or
`SIGN_TUNNEL_SECRET` unset it prints a line and exits 0 without touching the
file, which is what keeps a secretless run unsigned — cargo-packager has no way
to enable the hook conditionally.

**It sets an explicit `User-Agent` and that line must stay.** Cloudflare sits in
front of the tunnel and answers the stdlib default, `Python-urllib/3.x`, with a
403. This is not theoretical: it is what failed the Windows job of the v1.0.1
release, and the same request through the same tunnel succeeds with a
non-urllib UA. If Windows signing starts returning 403, check that header first.

### What gets signed, and in what order

1. `terra.exe`, by `build.py` calling the client directly, **before**
   `cargo packager -f nsis` runs — NSIS embeds it, so signing afterwards would
   leave the copy users actually execute unsigned. This one is manual because
   cargo-packager 0.11.8 only hands the hook the main binary.
2. `terra-app.exe`, by cargo-packager through `sign-command`, at the start of
   NSIS packaging. vibe signs per-binary the same way, via Tauri's
   `signCommand` in
   [`tauri.windows.signing.conf.json`](https://github.com/thewh1teagle/vibe/blob/main/desktop/src-tauri/tauri.windows.signing.conf.json).
3. The uninstaller, by `makensis` invoking the same hook from `!uninstfinalize`.
4. The NSIS `setup.exe`, after packaging. This is the file SmartScreen judges.
5. `signtool verify /pa /v` on the staged installer, as an independent check
   that Windows will accept the result.

## Verifying a release

macOS:

```sh
codesign --verify --deep --strict --verbose=2 /Applications/Terra.app
codesign --display --verbose=4 /Applications/Terra.app   # authority, flags, timestamp
xcrun stapler validate /Applications/Terra.app           # the ticket is on the app
spctl --assess --type execute -v /Applications/Terra.app
```

Check the app, not the image: cargo-packager notarises and staples the `.app`
and only signs the `.dmg`, so `stapler validate` on the image is expected to
fail. `codesign --verify` on the `.dmg` should still pass.

Windows (PowerShell, Windows SDK installed):

```powershell
signtool verify /pa /v terra-windows-x86_64-setup.exe
```

Expect a trusted chain, a timestamp, and zero warnings.

## A note on `plans/`

`plans/` is gitignored. The vibe clone under `plans/vibe` was scratch material
for writing all of the above and is not part of the repository — the links in
this document point at the upstream files on GitHub for that reason.
