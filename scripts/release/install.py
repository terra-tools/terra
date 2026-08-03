# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Bundle, sign and install Terra.app to /Applications and the CLI to a bin dir.

    uv run scripts/release/install.py [force] [bin]     # `just install 1 ~/.local/bin`

Signs with a Developer ID if one is in the keychain, else the stable terra-dev
identity from setup_signing.py, else ad-hoc.
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import threading
from pathlib import Path

from build import ROOT, run  # sibling script; shared so the commands stay in one place

DEST = Path("/Applications/Terra.app")
APP = ROOT / "target/release/Terra.app"
CLI = ROOT / "target/release/terra"


def signing_identity() -> str:
    """Developer ID first, then terra-dev, else "" for ad-hoc."""
    out = subprocess.run(["security", "find-identity", "-v", "-p", "codesigning"],
                         stdout=subprocess.PIPE, text=True, check=False).stdout
    developer_id = re.search(r'"(Developer ID Application: [^"]*)"', out)
    if developer_id:
        return developer_id.group(1)
    return "terra-dev" if "terra-dev" in out else ""


def prime_sudo(bin_dir: Path) -> threading.Event | None:
    """Ask for the sudo password NOW if the CLI install will need it.

    The build takes minutes; without this the password prompt appears at the
    very end, long after the user has walked away. A background `sudo -n -v`
    keeps the timestamp fresh so the eventual install never re-prompts. The
    returned Event stops the refresher; None means sudo won't be needed.
    """
    if os.access(bin_dir if bin_dir.exists() else bin_dir.parent, os.W_OK):
        return None
    print(f"{bin_dir} needs sudo — asking for your password up front, "
          "so the build can run unattended afterwards.")
    subprocess.run(["sudo", "-v"], check=True)
    stop = threading.Event()

    def refresh() -> None:
        while not stop.wait(60):
            subprocess.run(["sudo", "-n", "-v"], check=False,
                           stderr=subprocess.DEVNULL)

    threading.Thread(target=refresh, daemon=True).start()
    return stop


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("force", nargs="?", default="",
                        help="any non-empty value replaces an existing install")
    parser.add_argument("bin", nargs="?", default="/usr/local/bin",
                        help="where the terra CLI goes (default: /usr/local/bin)")
    args = parser.parse_args()
    bin_dir = Path(args.bin)

    if DEST.exists() and not args.force:
        print(f"{DEST} already exists. Re-run as 'just install 1' to replace it.",
              file=sys.stderr)
        return 1

    sudo_refresher = prime_sudo(bin_dir)

    # Same commands as `just bundle` — app only; the dmg's local build pops a
    # Finder window (see the justfile's `bundle` comment).
    run(["cargo", "build", "--release"])
    run(["cargo", "packager", "-p", "terra-app", "--release", "-f", "app"])

    identity = signing_identity()
    if identity:
        print(f"signing with: {identity}")
        run(["codesign", "--force", "--deep", "--options", "runtime",
             "--sign", identity, str(APP)])
    else:
        print("note: signing ad-hoc; run 'just setup-signing' once to stop macOS",
              file=sys.stderr)
        print("      re-asking for folder permissions after every upgrade",
              file=sys.stderr)
        run(["codesign", "--force", "--deep", "--sign", "-", str(APP)])

    # Deliberate: the bundle being replaced has to let go of the app first. The
    # pattern is bundle-only, so `just run`'s dev instance keeps running.
    run(["pkill", "-f", "Terra.app/Contents/MacOS/terra-app"], allow_fail=True)
    shutil.rmtree(DEST, ignore_errors=True)
    shutil.copytree(APP, DEST, symlinks=True)
    run(["xattr", "-dr", "com.apple.quarantine", str(DEST)], allow_fail=True)

    cli_dest = str(bin_dir / "terra")
    install_cmd = ["install", "-m", "755", str(CLI), cli_dest]
    print("$ " + " ".join(install_cmd), flush=True)
    if subprocess.run(install_cmd, check=False, stderr=subprocess.DEVNULL).returncode != 0:
        run(["sudo", *install_cmd])
    if sudo_refresher is not None:
        sudo_refresher.set()

    print(f"installed {DEST} and {cli_dest} — first launch: right-click the app -> Open")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except subprocess.CalledProcessError as exc:
        raise SystemExit(f"error: {exc.cmd[0]} exited {exc.returncode}") from exc
