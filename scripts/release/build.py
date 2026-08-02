# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Build and package terra's release artifacts. See docs/RELEASING.md.

    uv run scripts/release/build.py macos|linux|windows [--dry-run]

Signing is decided from the environment, never from a flag: macOS uses a
Developer ID when APPLE_CERTIFICATE and APPLE_ID are both set, Windows uses the
remote YubiKey server when SIGN_TUNNEL_URL and SIGN_TUNNEL_SECRET are both set.
Anything else fails the run rather than falling back to unsigned.
"""

from __future__ import annotations

import argparse
import base64
import os
import secrets
import shutil
import subprocess
import sys
import tempfile
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DIST = ROOT / "dist"
PACKAGER_TARGETS = ["-p", "terra-app", "-p", "terra-cli"]

DRY_RUN = False


def log(msg: str) -> None:
    print(f"[release] {msg}", flush=True)


def fail(msg: str) -> None:
    raise SystemExit(f"[release] error: {msg}")


def run(cmd: list[str], *, env: dict[str, str] | None = None, capture: bool = False,
        mask: list[str] | None = None, allow_fail: bool = False) -> str:
    """Run a command from the repo root, or print it under --dry-run.

    mask: values redacted when the command is echoed (secrets).
    """
    hide = [m for m in (mask or []) if m]
    shown = ["***" if arg in hide else arg for arg in cmd]
    print("$ " + " ".join(shown), flush=True)
    if DRY_RUN:
        return ""
    full_env = {**os.environ, **(env or {})}
    if capture:
        out = subprocess.run(cmd, cwd=ROOT, env=full_env, check=not allow_fail,
                             stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        print(out.stdout, end="", flush=True)
        return out.stdout
    subprocess.run(cmd, cwd=ROOT, env=full_env, check=not allow_fail)
    return ""


def act(desc: str, fn) -> None:
    """A filesystem step: described always, executed unless --dry-run."""
    print(f"* {desc}", flush=True)
    if not DRY_RUN:
        fn()


def runner_temp() -> Path:
    return Path(os.environ.get("RUNNER_TEMP") or tempfile.gettempdir())


def enabled(*names: str) -> bool:
    return all(os.environ.get(n) for n in names)


def need(name: str) -> str:
    value = os.environ.get(name, "")
    if not value and not DRY_RUN:
        fail(f"{name} is required when signing is enabled")
    return value


def sole(pattern: str, where: Path) -> Path:
    """The single file matching a glob, or a clear failure. Newest wins."""
    hits = sorted(where.glob(pattern))
    if not hits:
        if DRY_RUN:
            return where / pattern.replace("*", "Terra_0.0.0_x64")
        fail(f"no file matching {pattern} in {where}")
    return hits[-1]


def tar_gz(archive: Path, base: Path, *names: str) -> None:
    run(["tar", "-czf", str(archive), "-C", str(base), *names])


def list_dist() -> None:
    print(f"* artifacts in {DIST}", flush=True)
    if DRY_RUN:
        return
    for f in sorted(DIST.iterdir()):
        print(f"  {f.stat().st_size / 1e6:8.1f} MB  {f.name}", flush=True)


# --------------------------------------------------------------------------
# macOS
# --------------------------------------------------------------------------

UNIVERSAL = Path("target/universal-apple-darwin/release")


def macos(_args: argparse.Namespace) -> None:
    signing = enabled("APPLE_CERTIFICATE", "APPLE_ID")
    log("macOS: Developer ID signing + notarisation" if signing
        else "macOS: ad-hoc signing (no APPLE_CERTIFICATE/APPLE_ID in the environment)")

    for target in ("aarch64-apple-darwin", "x86_64-apple-darwin"):
        run(["cargo", "build", "--release", "--target", target, *PACKAGER_TARGETS])

    # cargo-packager does not lipo: it just reads target/universal-apple-darwin/.
    act("mkdir -p target/universal-apple-darwin/release",
        lambda: (ROOT / UNIVERSAL).mkdir(parents=True, exist_ok=True))
    for binary in ("terra-app", "terra"):
        run(["lipo", "-create", "-output", str(UNIVERSAL / binary),
             f"target/aarch64-apple-darwin/release/{binary}",
             f"target/x86_64-apple-darwin/release/{binary}"])
        run(["lipo", "-info", str(UNIVERSAL / binary)])

    keychain: Path | None = None
    try:
        if signing:
            keychain = import_certificate()

        # Only the .app: `-f dmg` would rebuild it and discard the signature.
        run(["cargo", "packager", "-p", "terra-app", "--release",
             "--target", "universal-apple-darwin", "-f", "app"])

        app = UNIVERSAL / "Terra.app"
        cli = UNIVERSAL / "terra"
        if signing:
            codesign_developer_id(app, cli)
        else:
            run(["codesign", "--force", "--deep", "--sign", "-", str(app)])
            run(["codesign", "--verify", "--deep", "--strict", "--verbose=2", str(app)])

        dmg = build_dmg(app)
        if signing:
            notarize(dmg, app)

        tar_gz(DIST / "terra-cli-macos-universal.tar.gz", UNIVERSAL, "terra")
        tar_gz(DIST / "terra-macos-universal.app.tar.gz", UNIVERSAL, "Terra.app")
        list_dist()
    finally:
        if keychain is not None:
            # The certificate must not survive a failed run either.
            print(f"$ security delete-keychain {keychain}", flush=True)
            if not DRY_RUN:
                subprocess.run(["security", "delete-keychain", str(keychain)], check=False)
                (runner_temp() / "cert.p12").unlink(missing_ok=True)


def import_certificate() -> Path:
    """Import the .p12 into a throwaway keychain and return its path."""
    keychain = runner_temp() / "terra-signing.keychain-db"
    password = secrets.token_urlsafe(24)

    run(["security", "create-keychain", "-p", password, str(keychain)], mask=[password])
    # -lut 21600: no auto-relock for six hours; the default five minutes is
    # shorter than a notarisation.
    run(["security", "set-keychain-settings", "-lut", "21600", str(keychain)])
    run(["security", "unlock-keychain", "-p", password, str(keychain)], mask=[password])

    p12 = runner_temp() / "cert.p12"
    act(f"decode APPLE_CERTIFICATE into {p12}",
        lambda: p12.write_bytes(base64.b64decode(os.environ["APPLE_CERTIFICATE"])))
    run(["security", "import", str(p12), "-k", str(keychain),
         "-P", os.environ.get("APPLE_CERTIFICATE_PASSWORD", ""),
         "-T", "/usr/bin/codesign", "-T", "/usr/bin/security"],
        mask=[os.environ.get("APPLE_CERTIFICATE_PASSWORD", "")])
    act(f"rm {p12}", lambda: p12.unlink(missing_ok=True))
    # Without set-key-partition-list codesign blocks on a GUI prompt nobody can
    # answer on a headless runner.
    run(["security", "set-key-partition-list", "-S", "apple-tool:,apple:,codesign:",
         "-s", "-k", password, str(keychain)], mask=[password])

    # Prepend rather than replace, so the default keychains still resolve.
    existing = run(["security", "list-keychains", "-d", "user"], capture=True)
    others = [line.strip().strip('"') for line in existing.splitlines() if line.strip()]
    run(["security", "list-keychains", "-d", "user", "-s", str(keychain), *others])

    # "1 matching identity, 0 valid identities" here means the Developer ID G2
    # intermediate is missing from the .p12, not that the ACL is wrong.
    run(["security", "find-identity", "-v", "-p", "codesigning", str(keychain)])
    return keychain


def codesign_developer_id(app: Path, cli: Path) -> None:
    identity = need("APPLE_SIGNING_IDENTITY")
    # --options runtime is mandatory for notarisation; --timestamp keeps the
    # signature valid past certificate expiry. The CLI ships as its own tarball,
    # so it is signed separately from the bundle.
    run(["codesign", "--force", "--timestamp", "--options", "runtime",
         "--sign", identity, str(cli)])
    run(["codesign", "--force", "--deep", "--timestamp", "--options", "runtime",
         "--sign", identity, str(app)])
    run(["codesign", "--verify", "--deep", "--strict", "--verbose=2", str(app)])
    run(["codesign", "--verify", "--strict", "--verbose=2", str(cli)])
    shown = run(["codesign", "--display", "--verbose=4", str(app)], capture=True)
    for line in shown.splitlines():
        if any(k in line for k in ("Authority", "flags", "Timestamp")):
            print(line, flush=True)


def build_dmg(app: Path) -> Path:
    """A plain UDZO image: the .app plus the customary /Applications shortcut."""
    stage = runner_temp() / "dmg"
    dmg = DIST / "terra-macos-universal.dmg"

    def prepare() -> None:
        shutil.rmtree(stage, ignore_errors=True)
        stage.mkdir(parents=True)
        DIST.mkdir(parents=True, exist_ok=True)
        shutil.copytree(ROOT / app, stage / app.name, symlinks=True)
        (stage / "Applications").symlink_to("/Applications")

    act(f"stage {app.name} + /Applications in {stage}", prepare)
    run(["hdiutil", "create", "-volname", "Terra", "-srcfolder", str(stage),
         "-ov", "-format", "UDZO", str(dmg)])
    return dmg


def notarize(dmg: Path, app: Path) -> None:
    """Apple's scan plus a stapled ticket, so a first launch works offline."""
    identity = need("APPLE_SIGNING_IDENTITY")
    password = need("APPLE_PASSWORD")
    # Sign the image too, so the download is tamper-evident before mounting.
    run(["codesign", "--force", "--timestamp", "--sign", identity, str(dmg)])
    # Submitting the .dmg covers the .app inside it; stapler then finds the same
    # ticket by code hash, which is what makes the .app.tar.gz notarised too.
    run(["xcrun", "notarytool", "submit", str(dmg),
         "--apple-id", need("APPLE_ID"),
         "--password", password,
         "--team-id", need("APPLE_TEAM_ID"),
         "--wait", "--timeout", "45m"], mask=[password])
    run(["xcrun", "stapler", "staple", str(dmg)])
    run(["xcrun", "stapler", "staple", str(app)])
    run(["xcrun", "stapler", "validate", str(dmg)])
    # What Gatekeeper will decide on first open.
    run(["spctl", "--assess", "--type", "open",
         "--context", "context:primary-signature", "-v", str(dmg)])


# --------------------------------------------------------------------------
# Linux
# --------------------------------------------------------------------------


def linux(_args: argparse.Namespace) -> None:
    log("Linux: unsigned (nothing on Linux is signed)")
    run(["cargo", "build", "--release", *PACKAGER_TARGETS])
    # Overrides the manifest's macOS-only formats list.
    run(["cargo", "packager", "-p", "terra-app", "--release", "-f", "deb"])

    act(f"mkdir -p {DIST}", lambda: DIST.mkdir(parents=True, exist_ok=True))
    tar_gz(DIST / "terra-linux-x86_64.tar.gz", Path("target/release"), "terra-app", "terra")
    deb = sole("*.deb", ROOT / "target/release")
    # Versionless artifact name: the site and install.sh never parse a version.
    act(f"copy {deb.name} -> dist/terra-linux-x86_64.deb",
        lambda: shutil.copy2(deb, DIST / "terra-linux-x86_64.deb"))
    list_dist()


# --------------------------------------------------------------------------
# Windows
# --------------------------------------------------------------------------


def windows(_args: argparse.Namespace) -> None:
    signing = enabled("SIGN_TUNNEL_URL", "SIGN_TUNNEL_SECRET")
    log("Windows: remote YubiKey signing" if signing
        else "Windows: unsigned (no SIGN_TUNNEL_URL/SIGN_TUNNEL_SECRET in the environment)")

    run(["cargo", "build", "--release", *PACKAGER_TARGETS])
    release = ROOT / "target/release"

    # Before packaging: NSIS embeds these two, so signing after would leave the
    # copies users actually run unsigned.
    if signing:
        remote_sign(release / "terra-app.exe")
        remote_sign(release / "terra.exe")

    run(["cargo", "packager", "-p", "terra-app", "--release", "-f", "nsis"])

    # The installer is a fresh .exe and carries no signature until now; it is
    # also the file SmartScreen judges.
    setup = sole("*-setup.exe", release)
    if signing:
        remote_sign(setup)

    zip_path = DIST / "terra-windows-x86_64.zip"

    def stage() -> None:
        DIST.mkdir(parents=True, exist_ok=True)
        with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED) as zf:
            for exe in ("terra-app.exe", "terra.exe"):
                zf.write(release / exe, exe)
        shutil.copy2(setup, DIST / "terra-windows-x86_64-setup.exe")

    act(f"zip terra-app.exe + terra.exe -> {zip_path.name}, copy {setup.name} "
        "-> dist/terra-windows-x86_64-setup.exe", stage)

    if signing:
        verify_signature(DIST / "terra-windows-x86_64-setup.exe")
    list_dist()


def remote_sign(path: Path) -> None:
    """POST the file to the YubiKey signing server, write the signed bytes back.

    Protocol is vibe's: POST <url>/sign, multipart field "file", shared secret
    in X-Tunnel-Secret, signed file in the 200 body. A server that is not
    running fails the build, so a signing-enabled run cannot ship unsigned.
    """
    url = os.environ["SIGN_TUNNEL_URL"].rstrip("/") + "/sign"
    secret = os.environ["SIGN_TUNNEL_SECRET"]
    print(f"* sign {path.name} via {url}", flush=True)
    if DRY_RUN:
        return

    data = path.read_bytes()
    print(f"  uploading {len(data)} bytes", flush=True)
    boundary = "----terra" + secrets.token_hex(16)
    body = (
        f"--{boundary}\r\n"
        f'Content-Disposition: form-data; name="file"; filename="{path.name}"\r\n'
        "Content-Type: application/octet-stream\r\n\r\n"
    ).encode() + data + f"\r\n--{boundary}--\r\n".encode()

    request = urllib.request.Request(
        url, data=body, method="POST",
        headers={"Content-Type": f"multipart/form-data; boundary={boundary}",
                 "X-Tunnel-Secret": secret, "Content-Length": str(len(body))})
    with urllib.request.urlopen(request, timeout=300) as response:
        signed = response.read()
    if not signed:
        fail(f"sign server returned an empty body for {path.name}")

    # Via a side file: a half-written response must not leave a truncated
    # executable behind for the packaging step to pick up.
    tmp = path.with_suffix(path.suffix + ".signed")
    tmp.write_bytes(signed)
    os.replace(tmp, path)
    print(f"  signed {path.name} ({len(signed)} bytes)", flush=True)


def verify_signature(path: Path) -> None:
    """jsign exiting 0 is not proof that Windows accepts the result."""
    kits = Path(r"C:\Program Files (x86)\Windows Kits\10\bin")
    tools = sorted(kits.glob("*/x64/signtool.exe"), reverse=True)
    if not tools:
        if DRY_RUN:
            tools = [kits / "*/x64/signtool.exe"]
        else:
            fail("signtool.exe not found on this runner")
    run([str(tools[0]), "verify", "/pa", "/v", str(path)])


# --------------------------------------------------------------------------


def main() -> None:
    global DRY_RUN
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--dry-run", action="store_true",
                        help="print the commands instead of running them")
    subs = parser.add_subparsers(dest="platform", required=True)
    for name, fn, help_text in (
        ("macos", macos, "universal .dmg, .app tarball and CLI tarball"),
        ("linux", linux, "x86_64 .deb and tarball"),
        ("windows", windows, "x86_64 NSIS setup.exe and zip"),
    ):
        sub = subs.add_parser(name, parents=[common], help=help_text)
        sub.set_defaults(func=fn)

    args = parser.parse_args()
    DRY_RUN = args.dry_run
    if DRY_RUN:
        log("dry run: no commands are executed")
    try:
        args.func(args)
    except subprocess.CalledProcessError as exc:
        raise SystemExit(f"[release] error: {exc.cmd[0]} exited {exc.returncode}") from exc
    log(f"done: {args.platform}")


if __name__ == "__main__":
    sys.exit(main())
