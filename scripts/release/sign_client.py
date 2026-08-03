# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Sign one Windows file on the YubiKey server, in place. See docs/SIGNING.md.

    uv run scripts/release/sign_client.py <file>

cargo-packager calls this through its `%1` hook — `sign-command` under
`[package.metadata.packager.windows]` in crates/terra-app/Cargo.toml — once for
each file it wants signed, and `scripts/release/build.py` calls it directly for
the one file cargo-packager does not offer (the `terra` CLI; 0.11.8 only signs
the *main* binary, the uninstaller and the installer).

With SIGN_TUNNEL_URL or SIGN_TUNNEL_SECRET unset this exits 0 without touching
the file. That is deliberate: cargo-packager has no way to enable the hook
conditionally, so a run with no secrets has to pass straight through and leave
the artifacts unsigned.
"""

from __future__ import annotations

import os
import secrets
import sys
import urllib.error
import urllib.request
from pathlib import Path

# Cloudflare sits in front of the tunnel and answers the stdlib's default
# "Python-urllib/3.x" with a 403 (confirmed on the v1.0.1 release: the identical
# request is served with a browser-ish UA and blocked with the urllib one). Do
# not remove this header.
USER_AGENT = "terra-release-sign/1.0 (+https://github.com/terra-tools/terra)"


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {Path(sys.argv[0]).name} <file>", file=sys.stderr)
        return 2

    path = Path(sys.argv[1])
    url = os.environ.get("SIGN_TUNNEL_URL", "")
    secret = os.environ.get("SIGN_TUNNEL_SECRET", "")
    if not url or not secret:
        print(f"[sign] no SIGN_TUNNEL_URL/SIGN_TUNNEL_SECRET; leaving {path.name} unsigned",
              flush=True)
        return 0

    if not path.is_file():
        print(f"[sign] error: {path} does not exist", file=sys.stderr)
        return 1

    endpoint = url.rstrip("/") + "/sign"
    data = path.read_bytes()
    print(f"[sign] {path.name} ({len(data)} bytes) via {endpoint}", flush=True)

    # vibe's protocol: POST /sign, multipart field "file", shared secret in
    # X-Tunnel-Secret, the signed file back in the 200 body.
    boundary = "----terra" + secrets.token_hex(16)
    body = (
        f"--{boundary}\r\n"
        f'Content-Disposition: form-data; name="file"; filename="{path.name}"\r\n'
        "Content-Type: application/octet-stream\r\n\r\n"
    ).encode() + data + f"\r\n--{boundary}--\r\n".encode()

    request = urllib.request.Request(
        endpoint, data=body, method="POST",
        headers={"Content-Type": f"multipart/form-data; boundary={boundary}",
                 "X-Tunnel-Secret": secret,
                 "User-Agent": USER_AGENT,
                 "Content-Length": str(len(body))})
    try:
        with urllib.request.urlopen(request, timeout=300) as response:
            signed = response.read()
    except urllib.error.HTTPError as exc:
        # A 403 here is usually Cloudflare, not the server: see USER_AGENT.
        print(f"[sign] error: {endpoint} returned {exc.code} {exc.reason}", file=sys.stderr)
        print(exc.read(2000).decode("utf-8", "replace"), file=sys.stderr)
        return 1
    except urllib.error.URLError as exc:
        print(f"[sign] error: could not reach {endpoint}: {exc.reason}", file=sys.stderr)
        return 1
    if not signed:
        print(f"[sign] error: empty response body for {path.name}", file=sys.stderr)
        return 1

    # Via a side file: a half-written response must not leave a truncated
    # executable behind for the packaging step to pick up.
    tmp = path.with_suffix(path.suffix + ".signed")
    tmp.write_bytes(signed)
    os.replace(tmp, path)
    print(f"[sign] signed {path.name} ({len(signed)} bytes)", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
