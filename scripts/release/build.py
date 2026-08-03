# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Build and package terra's release artifacts. See docs/RELEASING.md.

    uv run scripts/release/build.py macos|linux|windows [--dry-run]

One installer per platform, and cargo-packager does the signing: it signs and
notarises the macOS bundle itself, and it calls scripts/release/sign_client.py
for every Windows file it produces. This script only decides *what* to sign
with, from the environment and never from a flag: a Developer ID when
APPLE_CERTIFICATE and APPLE_ID are both set, ad-hoc otherwise; the remote
YubiKey server when SIGN_TUNNEL_URL and SIGN_TUNNEL_SECRET are both set,
unsigned otherwise. Once a signing path is enabled a failure in it fails the
run rather than falling back to unsigned.
"""

from __future__ import annotations

import argparse
import base64
import contextlib
import os
import secrets
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DIST = ROOT / "dist"
PACKAGER_TARGETS = ["-p", "terra-app", "-p", "terra-cli"]
MANIFEST = ROOT / "crates/terra-app/Cargo.toml"
SIGN_CLIENT = ROOT / "scripts/release/sign_client.py"

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


def sole(pattern: str, where: Path) -> Path:
    """The single file matching a glob, or a clear failure. Newest wins."""
    hits = sorted(where.glob(pattern))
    if not hits:
        if DRY_RUN:
            return where / pattern.replace("*", "Terra_0.0.0_x64")
        fail(f"no file matching {pattern} in {where}")
    return hits[-1]


def stage(src: Path, name: str) -> Path:
    """Copy a packager output into dist/ under its stable, versionless name."""
    dest = DIST / name
    act(f"copy {src.name} -> dist/{name}",
        lambda: (DIST.mkdir(parents=True, exist_ok=True), shutil.copy2(src, dest)))
    return dest


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
AD_HOC = 'signing-identity = "-"'


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

    presign_cli(signing)

    # One invocation for both formats. The dmg packager does not rebuild the
    # .app — it reuses the one already in out_dir (verified against 0.11.8's
    # src/package/mod.rs, which only packages `app` for a dmg when no app output
    # exists yet) — so the signature applied during bundling survives into the
    # image, and the image itself is signed with the same identity afterwards.
    with developer_id(signing):
        run(["cargo", "packager", "-p", "terra-app", "--release",
             "--target", "universal-apple-darwin", "-f", "app", "-f", "dmg"])

    app = UNIVERSAL / "Terra.app"
    # Both paths land here: the ad-hoc default is a real signature too, applied
    # by cargo-packager inside-out (each Mach-O, then the bundle).
    run(["codesign", "--verify", "--deep", "--strict", "--verbose=2", str(app)])
    shown = run(["codesign", "--display", "--verbose=4", str(app)], capture=True)
    for line in shown.splitlines():
        if any(k in line for k in ("Authority", "Signature", "flags", "Timestamp")):
            print(line, flush=True)

    stage(sole("*.dmg", ROOT / UNIVERSAL), "terra-macos-universal.dmg")
    list_dist()


def presign_cli(signing: bool) -> None:
    """Sign the universal `terra` before cargo-packager touches the bundle.

    cargo-packager signs every Mach-O in the .app, sorted by path depth alone
    (`impl Ord for SignTarget`, src/codesign/macos.rs) and pushed through a
    BinaryHeap. `Contents/MacOS/terra` and `Contents/MacOS/terra-app` are the
    same depth, so the tie breaks whichever way the heap feels like — and half
    the time that is terra-app first. codesign, pointed at a bundle's *main*
    executable, validates the whole bundle, so an unsigned sibling aborts the
    run with "code object is not signed at all / In subcomponent:
    Contents/MacOS/terra". That is what failed the v1.0.2 macOS job.

    Signing the CLI here means the copy the packager puts in the bundle is
    already valid whichever order it then picks; the packager re-signs it with
    --force afterwards, which is a no-op in effect.

    This is not a CI-only quirk that a local run would catch: the arm64 linker
    ad-hoc signs what it produces, so a native build hides the problem. `lipo`
    output does not carry a signature, and neither does a cross-compiled slice.

    Ad-hoc is not a shortcut for the signed path — notarisation rejects a bundle
    with an ad-hoc signed binary inside it — so this signs with the real
    identity, from its own keychain, when there is one.
    """
    cli = UNIVERSAL / "terra"
    # The same flags cargo-packager uses for a native binary, so the signature
    # it finds is the signature it would have made.
    flags = ["codesign", "--force", "--options", "runtime", "--timestamp"]

    if not signing:
        run([*flags, "--sign", "-", str(cli)])
    else:
        identity = os.environ.get("APPLE_SIGNING_IDENTITY", "")
        if not identity and not DRY_RUN:
            fail("APPLE_SIGNING_IDENTITY is required when signing is enabled")
        with presign_keychain() as keychain:
            run([*flags, "--sign", identity, "--keychain", str(keychain), str(cli)])

    run(["codesign", "--verify", "--strict", "--verbose=2", str(cli)])


@contextlib.contextmanager
def presign_keychain():
    """A throwaway keychain holding $APPLE_CERTIFICATE, for one codesign call.

    cargo-packager builds its own `cargo-packager.keychain` when it signs, and
    deletes it afterwards; this one is separate and separately named so the two
    cannot collide, and it is gone again before the packager starts.
    """
    keychain = runner_temp() / "terra-presign.keychain-db"
    p12 = runner_temp() / "presign.p12"
    password = secrets.token_urlsafe(24)
    certificate_password = os.environ.get("APPLE_CERTIFICATE_PASSWORD", "")
    others: list[str] = []

    try:
        run(["security", "create-keychain", "-p", password, str(keychain)], mask=[password])
        # -lut 21600: no auto-relock for six hours; the default five minutes is
        # shorter than the build around it.
        run(["security", "set-keychain-settings", "-lut", "21600", str(keychain)])
        run(["security", "unlock-keychain", "-p", password, str(keychain)], mask=[password])

        act(f"decode APPLE_CERTIFICATE into {p12}",
            lambda: p12.write_bytes(base64.b64decode(os.environ["APPLE_CERTIFICATE"])))
        run(["security", "import", str(p12), "-k", str(keychain),
             "-P", certificate_password,
             "-T", "/usr/bin/codesign", "-T", "/usr/bin/security"],
            mask=[certificate_password])
        act(f"rm {p12}", lambda: p12.unlink(missing_ok=True))
        # Without set-key-partition-list codesign blocks on a GUI prompt nobody
        # can answer on a headless runner.
        run(["security", "set-key-partition-list", "-S", "apple-tool:,apple:,codesign:",
             "-s", "-k", password, str(keychain)], mask=[password])

        # Prepend rather than replace, so the default keychains still resolve.
        existing = run(["security", "list-keychains", "-d", "user"], capture=True)
        others = [line.strip().strip('"') for line in existing.splitlines() if line.strip()]
        run(["security", "list-keychains", "-d", "user", "-s", str(keychain), *others])

        # "1 matching identity, 0 valid identities" here means the Developer ID
        # G2 intermediate is missing from the .p12, not that the ACL is wrong.
        run(["security", "find-identity", "-v", "-p", "codesigning", str(keychain)])
        yield keychain
    finally:
        # The certificate must not survive a failed run either, and the search
        # list must not keep pointing at a keychain that is about to be gone.
        if others:
            run(["security", "list-keychains", "-d", "user", "-s", *others],
                allow_fail=True)
        print(f"$ security delete-keychain {keychain}", flush=True)
        if not DRY_RUN:
            subprocess.run(["security", "delete-keychain", str(keychain)], check=False)
            p12.unlink(missing_ok=True)


@contextlib.contextmanager
def developer_id(signing: bool):
    """Point the manifest's signing identity at $APPLE_SIGNING_IDENTITY.

    cargo-packager 0.11.8 reads the identity from the config only — unlike the
    certificate and the notarisation credentials, there is no
    APPLE_SIGNING_IDENTITY fallback anywhere in its source. The manifest carries
    the ad-hoc `-` so an unsigned run and a local `just bundle` behave the same,
    and a signing run swaps that one line for the real identity and puts it
    back, whatever happens.
    """
    if not signing:
        yield
        return

    identity = os.environ.get("APPLE_SIGNING_IDENTITY", "")
    if not identity and not DRY_RUN:
        fail("APPLE_SIGNING_IDENTITY is required when signing is enabled")

    print(f"* set {MANIFEST.relative_to(ROOT)} signing-identity to $APPLE_SIGNING_IDENTITY",
          flush=True)
    if DRY_RUN:
        yield
        return

    original = MANIFEST.read_text()
    if original.count(AD_HOC) != 1:
        fail(f"expected exactly one {AD_HOC!r} line in {MANIFEST}")
    try:
        MANIFEST.write_text(original.replace(AD_HOC, f'signing-identity = "{identity}"'))
        yield
    finally:
        print(f"* restore {MANIFEST.relative_to(ROOT)}", flush=True)
        MANIFEST.write_text(original)


# --------------------------------------------------------------------------
# Linux
# --------------------------------------------------------------------------


def linux(_args: argparse.Namespace) -> None:
    log("Linux: unsigned (nothing on Linux is signed)")
    run(["cargo", "build", "--release", *PACKAGER_TARGETS])
    # Overrides the manifest's macOS-only formats list.
    run(["cargo", "packager", "-p", "terra-app", "--release", "-f", "deb"])

    deb = stage(sole("*.deb", ROOT / "target/release"), "terra-linux-x86_64.deb")
    # The .deb is now the only Linux artifact, so prove both binaries are in it.
    run(["dpkg-deb", "--contents", str(deb)])
    list_dist()


# --------------------------------------------------------------------------
# Windows
# --------------------------------------------------------------------------

SHIM = "terra-sign.cmd"


def windows(_args: argparse.Namespace) -> None:
    signing = enabled("SIGN_TUNNEL_URL", "SIGN_TUNNEL_SECRET")
    log("Windows: remote YubiKey signing" if signing
        else "Windows: unsigned (no SIGN_TUNNEL_URL/SIGN_TUNNEL_SECRET in the environment)")

    run(["cargo", "build", "--release", *PACKAGER_TARGETS])
    release = ROOT / "target/release"

    # cargo-packager signs terra-app.exe, the uninstaller and setup.exe through
    # the sign-command hook, but 0.11.8 only ever offers it the *main* binary
    # (src/package/nsis/mod.rs), so the CLI is ours to do — and it has to happen
    # before packaging, because NSIS embeds a copy.
    sign_file(release / "terra.exe")

    run(["cargo", "packager", "-p", "terra-app", "--release", "-f", "nsis"],
        env={"PATH": f"{write_shim()}{os.pathsep}{os.environ.get('PATH', '')}"})

    setup = stage(sole("*-setup.exe", release), "terra-windows-x86_64-setup.exe")
    if signing:
        verify_signature(setup)
    list_dist()


def write_shim() -> Path:
    """A PATH-resolvable `terra-sign.cmd` for cargo-packager's `%1` hook.

    The hook is invoked from three different working directories — the crate
    directory for the binary and the installer, and the NSIS intermediates
    directory for `!uninstfinalize` — so no relative path to the script works
    for all of them. A shim on PATH does, and stays a no-op when the tunnel
    secrets are absent because sign_client.py is.
    """
    directory = runner_temp() / "terra-sign"
    script = f'@echo off\r\nuv run --script "{SIGN_CLIENT}" %*\r\nexit /b %ERRORLEVEL%\r\n'

    def write() -> None:
        directory.mkdir(parents=True, exist_ok=True)
        (directory / SHIM).write_text(script)

    act(f"write {directory / SHIM} (uv run --script {SIGN_CLIENT.name} %*)", write)
    return directory


def sign_file(path: Path) -> None:
    """Sign one file with the same client cargo-packager's hook uses.

    A no-op that exits 0 when the tunnel secrets are unset, so this is
    unconditional — the decision lives in one place, sign_client.py.
    """
    run(["uv", "run", "--script", str(SIGN_CLIENT), str(path)])


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
        ("macos", macos, "universal .dmg (Terra.app + the terra CLI inside it)"),
        ("linux", linux, "x86_64 .deb (terra-app + terra in /usr/bin)"),
        ("windows", windows, "x86_64 NSIS setup.exe (terra-app.exe + terra.exe)"),
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
