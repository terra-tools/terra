# /// script
# requires-python = ">=3.12"
# dependencies = [
#     "flask==3.1.2",
#     "python-dotenv==1.2.1",
# ]
# ///

"""
Terra's Windows code-signing server.

Adapted from vibe's scripts/sign_server.py:
https://github.com/thewh1teagle/vibe/blob/main/scripts/sign_server.py

Why a server at all: the code-signing certificate lives inside a YubiKey and
the private key never leaves it. A GitHub runner cannot hold that key, so the
runner ships the *binary* to the machine the token is plugged into, that
machine signs it, and the signed bytes come back. Nothing secret crosses the
wire in either direction — only an executable, and a shared secret proving the
caller is our own workflow.

Run it on the machine with the YubiKey attached (physical USB — PIV does not
work over RDP):

    uv run scripts/sign-server/sign_server.py

See README.md in this directory for prerequisites and env vars.
"""

import logging
import os
import secrets
import shutil
import subprocess
import sys
import tempfile
import threading
from pathlib import Path

from dotenv import load_dotenv
from flask import Flask, jsonify, request, send_file

load_dotenv()

# Werkzeug's per-request log line adds nothing over our own [SIGN]/[OK] lines,
# and the point of running this by hand is to be able to watch what it does.
logging.getLogger("werkzeug").setLevel(logging.ERROR)

# jsign does the signing (Java, talks PKCS#11 to the token); cloudflared gives
# the GitHub runner a public URL without opening a port on the home router.
REQUIRED_TOOLS = ["jsign", "cloudflared"]
REQUIRED_ENV = ["TUNNEL_URL", "CF_TUNNEL_TOKEN", "PIV_PIN"]

# Slot 9c ("Digital Signature") is where the certificate is imported. This is
# the label jsign's YUBIKEY store reports for that slot.
JSIGN_ALIAS = "X.509 Certificate for Digital Signature"
TSA_URL = "http://timestamp.digicert.com"


def check_prerequisites() -> None:
    """Fail loudly at startup rather than on the first real signing request."""
    missing_tools = [t for t in REQUIRED_TOOLS if not shutil.which(t)]
    if missing_tools:
        print(f"Missing tools: {', '.join(missing_tools)}")
        print("Install them and make sure they're on PATH.")
        sys.exit(1)

    missing_env = [v for v in REQUIRED_ENV if not os.environ.get(v)]
    if missing_env:
        print(f"Missing env vars: {', '.join(missing_env)}")
        print("Add them to .env or export them.")
        sys.exit(1)


check_prerequisites()

# A generated secret is fine for a one-off session, but then it has to be
# copied into the repo secret before the workflow runs. Set TUNNEL_SECRET to
# the value already stored as the SIGN_TUNNEL_SECRET repo secret to skip that.
SECRET = os.environ.get("TUNNEL_SECRET") or secrets.token_urlsafe(32)
TUNNEL_URL = os.environ["TUNNEL_URL"]
CF_TOKEN = os.environ["CF_TUNNEL_TOKEN"]
PIV_PIN = os.environ["PIV_PIN"]

app = Flask(__name__)


@app.route("/")
def index():
    """Unauthenticated health check — the workflow pings this before uploading."""
    print(f"[INFO] health check from {request.remote_addr}")
    return jsonify({"status": "ok"})


@app.route("/sign", methods=["POST"])
def sign():
    # Constant-time compare: the secret is the only thing standing between the
    # open internet and a free Authenticode signature carrying our name.
    provided = request.headers.get("X-Tunnel-Secret", "")
    if not secrets.compare_digest(provided, SECRET):
        print(f"[DENIED] unauthorized request from {request.remote_addr}")
        return jsonify({"error": "unauthorized"}), 401

    file = request.files.get("file")
    if not file or not file.filename:
        print(f"[ERROR] no file in request from {request.remote_addr}")
        return jsonify({"error": "no file provided"}), 400

    # Never trust the client-supplied name as a path — only as a leaf name.
    filename = Path(file.filename).name
    print(f"[SIGN] {filename} ({request.content_length} bytes) from {request.remote_addr}")

    with tempfile.TemporaryDirectory() as tmp:
        filepath = Path(tmp) / filename
        file.save(filepath)

        # jsign signs in place. --storepass is the PIV PIN, not a keystore
        # password; the key itself stays on the token.
        result = subprocess.run(
            [
                "jsign",
                "--storetype", "YUBIKEY",
                "--storepass", PIV_PIN,
                "--alias", JSIGN_ALIAS,
                "--tsaurl", TSA_URL,
                str(filepath),
            ],
            capture_output=True,
            text=True,
        )

        if result.returncode != 0:
            print(f"[FAIL] jsign failed: {result.stderr.strip()}")
            return jsonify({
                "error": "signing failed",
                "stderr": result.stderr,
                "stdout": result.stdout,
            }), 500

        print(f"[OK] signed {filename}")
        return send_file(filepath, as_attachment=True, download_name=filename)


def start_tunnel() -> subprocess.Popen:
    return subprocess.Popen(
        ["cloudflared", "tunnel", "run", "--token", CF_TOKEN],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def main() -> None:
    tunnel = None
    try:
        # Flask in a daemon thread, the tunnel in the foreground: Ctrl+C then
        # tears down the public URL first, which is the part that matters.
        threading.Thread(
            target=lambda: app.run(port=8080, use_reloader=False),
            daemon=True,
        ).start()
        print("Starting tunnel...")
        tunnel = start_tunnel()
        print(
            f"\nSign server ready at: {TUNNEL_URL}\n"
            f"\n"
            f"Endpoint: POST /sign (multipart file upload, field name 'file')\n"
            f"\n"
            f"  gh secret set SIGN_TUNNEL_URL -b {TUNNEL_URL}\n"
            f"  gh secret set SIGN_TUNNEL_SECRET -b {SECRET}\n"
            f"\n"
            f"  curl -X POST {TUNNEL_URL}/sign \\\n"
            f'    -H "X-Tunnel-Secret: {SECRET}" \\\n'
            f"    -F 'file=@terra-app.exe' -o signed.exe\n"
            f"\n"
            f"Press Ctrl+C to stop\n"
        )
        tunnel.wait()
    except KeyboardInterrupt:
        print("\nShutting down...")
    finally:
        if tunnel:
            tunnel.kill()
            tunnel.wait()
        print("Cleaned up")


if __name__ == "__main__":
    main()
