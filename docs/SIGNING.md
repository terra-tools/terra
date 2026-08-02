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

**Linux.** Nothing is signed and nothing complains. The `.deb` and the tarball
are unaffected by any of this; the Linux job is untouched.

The release notes appended by the `support-notes` job state the unsigned
situation in these terms. If signing is turned on permanently, that text is
worth updating.

## How the opt-in works

The workflow passes every signing secret as environment variables to the one
step that builds and packages, and `scripts/release/build.py` decides from what
it finds there: a Developer ID when `APPLE_CERTIFICATE` and `APPLE_ID` are both
non-empty, ad-hoc otherwise; the YubiKey server when `SIGN_TUNNEL_URL` and
`SIGN_TUNNEL_SECRET` are both non-empty, unsigned otherwise. Unset secrets
arrive as empty strings, so nothing has to inspect the `secrets` context in an
`if:` (which GitHub Actions does not allow anyway).

The script prints which path it took, and once a path is enabled a failure in it
fails the run — there is no fallback from real signing to unsigned.

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

vibe gets all of this for free from `tauri-action`. Terra has no bundler doing
it, so the release workflow performs the same sequence explicitly:

1. **Import** the `.p12` into a temporary keychain in `$RUNNER_TEMP`, with
   `security set-key-partition-list` so `codesign` does not block on a GUI
   prompt no one can click, and `security find-identity` as an early failure
   check.
2. **Sign** the universal `Terra.app` and the standalone `terra` CLI with
   `--options runtime --timestamp`. Hardened runtime is mandatory for
   notarisation; the timestamp keeps signatures valid past certificate expiry.
3. **Build the `.dmg`** from the already-signed app, then sign the image too.
4. **Notarise** with `xcrun notarytool submit --wait`, then `stapler staple`
   both the `.dmg` and the `.app` (one submission covers both — the ticket is
   looked up by code hash), so the `.app.tar.gz` artifact is notarised as well.
5. **Delete the keychain** in an `always()` step.

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

The `remote_sign()` function in `scripts/release/build.py`, so the Windows
release job needs no toolchain beyond the `uv` it already uses. The protocol is
vibe's exactly: `POST /sign`, multipart field `file`, secret in
`X-Tunnel-Secret`, signed bytes in the response body.

### What gets signed, and in what order

1. `terra-app.exe` and `terra.exe`, **before** `cargo packager -f nsis` runs —
   NSIS embeds them, so signing after packaging would leave the copies users
   actually execute unsigned. vibe signs per-binary the same way, via Tauri's
   `signCommand` in
   [`tauri.windows.signing.conf.json`](https://github.com/thewh1teagle/vibe/blob/main/desktop/src-tauri/tauri.windows.signing.conf.json).
2. The NSIS `setup.exe`, after packaging. This is the file SmartScreen judges.
3. `signtool verify /pa /v` on the staged installer, as an independent check
   that Windows will accept the result.

The zip artifact is built from the same `target\release` binaries as step 1, so
its contents are signed too.

## Verifying a release

macOS:

```sh
codesign --verify --deep --strict --verbose=2 /Applications/Terra.app
codesign --display --verbose=4 /Applications/Terra.app   # authority, flags, timestamp
spctl --assess --type open --context context:primary-signature -v terra-macos-universal.dmg
xcrun stapler validate terra-macos-universal.dmg
```

Windows (PowerShell, Windows SDK installed):

```powershell
signtool verify /pa /v terra-windows-x86_64-setup.exe
```

Expect a trusted chain, a timestamp, and zero warnings.

## A note on `plans/`

`plans/` is gitignored. The vibe clone under `plans/vibe` was scratch material
for writing all of the above and is not part of the repository — the links in
this document point at the upstream files on GitHub for that reason.
