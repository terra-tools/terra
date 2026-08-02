# Windows signing server (YubiKey)

`sign_server.py` signs Windows binaries with a code-signing certificate held on
a YubiKey. The client is built into the release script,
[`scripts/release/build.py`](../release/build.py) (`remote_sign`).

Both are adapted from vibe, which does the same thing for the same YubiKey:

- server — <https://github.com/thewh1teagle/vibe/blob/main/scripts/sign_server.py>
- client — <https://github.com/thewh1teagle/vibe/blob/main/scripts/sign_windows.py>
- token setup — <https://github.com/thewh1teagle/vibe/blob/main/docs/code-signing/windows_yubikey.md>

## Why

The private key is generated inside the YubiKey and cannot be exported, which
is the point — but it also means a GitHub runner can never hold it. So the
runner uploads the unsigned binary to whatever machine the token is plugged
into, that machine signs it, and the signed bytes come back. The key stays put.

This is entirely opt-in. With no `SIGN_TUNNEL_URL` / `SIGN_TUNNEL_SECRET` repo
secrets set, the release workflow skips the upload and publishes an unsigned
installer exactly as it does today.

## Prerequisites

On the machine with the YubiKey:

- **Physical USB access.** PIV does not work over RDP.
- [`jsign`](https://ebourg.github.io/jsign/) on `PATH` — does the signing, via
  PKCS#11 against the token. (`brew install jsign`, or the .jar plus a wrapper.)
- [`cloudflared`](https://developers.cloudflare.com/cloudflare-tunnel/) on
  `PATH` — publishes a URL for the local server without opening a router port.
- [`uv`](https://docs.astral.sh/uv/) — the script declares its own deps inline
  (Flask, python-dotenv), so `uv run` needs no virtualenv setup.
- A code-signing certificate imported into PIV slot 9c. Follow vibe's
  [`windows_yubikey.md`](https://github.com/thewh1teagle/vibe/blob/main/docs/code-signing/windows_yubikey.md)
  end to end for that: generate the key on the token, CSR, attestation to the
  CA, re-issued cert, `ykman piv certificates import 9c`.

## Environment

Put these in `.env` next to the script, or export them:

| Variable          | What it is                                                      |
| ----------------- | --------------------------------------------------------------- |
| `TUNNEL_URL`      | Public URL of the tunnel, e.g. `https://signing.example.com`      |
| `CF_TUNNEL_TOKEN` | Cloudflare tunnel token (`cloudflared tunnel token <name>`)       |
| `PIV_PIN`         | YubiKey PIV PIN — jsign's `--storepass`                           |
| `TUNNEL_SECRET`   | Optional. Shared secret. Generated per run if unset.              |

Set `TUNNEL_SECRET` to whatever is already stored in the repo's
`SIGN_TUNNEL_SECRET` secret, otherwise each restart mints a new one and you
have to update the repo secret before every release.

## Running

```sh
uv run scripts/sign-server/sign_server.py
```

It prints the URL, the secret, and a ready-made `curl` to smoke-test with.
Leave it running for the duration of the release job and Ctrl+C afterwards —
there is no reason for it to be reachable when no release is building.

## How the release job reaches it

`.github/workflows/release.yml` reads two repo secrets:

```sh
gh secret set SIGN_TUNNEL_URL    -b https://signing.example.com
gh secret set SIGN_TUNNEL_SECRET -b '<TUNNEL_SECRET>'
```

With both set, the Windows job signs `terra-app.exe` and `terra.exe` before
packaging (so the installer embeds signed binaries) and then the NSIS
`setup.exe` after. With either unset every signing step is skipped.

## Protocol

```
POST /sign
  X-Tunnel-Secret: <shared secret>
  multipart/form-data, field "file"
→ 200  signed binary as the response body
  401  wrong or missing secret
  500  jsign failed (stdout/stderr in the JSON body)

GET /  → {"status":"ok"}   (unauthenticated health check)
```

## Notes

- The server is deliberately small. It shells out to one pinned command line
  and streams one file back; there is no queue, no persistence, no state.
- Uploaded files land in a `TemporaryDirectory` that is removed on return, so
  nothing accumulates on the signing machine.
- Timestamps come from DigiCert's TSA, so signatures stay valid after the
  certificate expires.
- Verify a result with `signtool verify /pa /v <file>` on Windows.
