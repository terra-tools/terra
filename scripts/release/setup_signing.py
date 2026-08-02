# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""One-time: mint a stable self-signed "terra-dev" codesigning identity.

    uv run scripts/release/setup_signing.py

Ad-hoc signatures are per-build hashes, so every upgrade looks like a brand-new
app to macOS and re-prompts for Downloads/Music/etc. A stable identity keeps
the TCC grants. Run by `just setup-signing`; idempotent.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import tempfile
from pathlib import Path


def identity_exists() -> bool:
    out = subprocess.run(["security", "find-identity", "-v", "-p", "codesigning"],
                         stdout=subprocess.PIPE, text=True, check=False).stdout
    return "terra-dev" in out


def main() -> int:
    # Takes no arguments, but parse anyway so `--help` does not mint a keypair.
    argparse.ArgumentParser(description=__doc__,
                            formatter_class=argparse.RawDescriptionHelpFormatter).parse_args()

    if identity_exists():
        print("terra-dev identity already exists")
        return 0

    with tempfile.TemporaryDirectory() as tmp:
        tmpdir = Path(tmp)
        key, cert, p12 = tmpdir / "key.pem", tmpdir / "cert.pem", tmpdir / "terra-dev.p12"
        subprocess.run([
            "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "3650",
            "-subj", "/CN=terra-dev",
            "-addext", "keyUsage=digitalSignature",
            "-addext", "extendedKeyUsage=codeSigning",
            "-keyout", str(key), "-out", str(cert),
        ], check=True)
        subprocess.run([
            "openssl", "pkcs12", "-export", "-inkey", str(key), "-in", str(cert),
            "-passout", "pass:terra", "-out", str(p12),
        ], check=True)
        subprocess.run(["security", "import", str(p12), "-P", "terra",
                        "-T", "/usr/bin/codesign"], check=True)

    print("terra-dev identity created. macOS may show one keychain prompt on the")
    print("next 'just upgrade' — choose Always Allow. Permissions will then")
    print("survive upgrades (one final round of TCC prompts, then never again).")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except subprocess.CalledProcessError as exc:
        raise SystemExit(f"error: {exc.cmd[0]} exited {exc.returncode}") from exc
