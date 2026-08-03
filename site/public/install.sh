#!/bin/sh
# Terra installer — https://github.com/terra-tools/terra
#
#   curl -fsSL https://terra-tools.github.io/terra/install.sh | sh
#   curl -fsSL https://terra-tools.github.io/terra/install.sh | sh -s v1.0.0
#
# Installs from the release installers: the .dmg on macOS, the .deb on Linux.
set -eu

REPO="https://github.com/terra-tools/terra"
TAG="${1:-}"

if [ -n "$TAG" ]; then
  BASE="$REPO/releases/download/$TAG"
else
  BASE="$REPO/releases/latest/download"
fi

TMPDIR_TERRA=""
MOUNT=""
cleanup() {
  [ -z "$MOUNT" ] || hdiutil detach "$MOUNT" -quiet 2>/dev/null || true
  [ -z "$TMPDIR_TERRA" ] || rm -rf "$TMPDIR_TERRA"
}
trap cleanup EXIT INT TERM

die() { echo "error: $*" >&2; exit 1; }
info() { echo "$*"; }

if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -q "$1" -O "$2"; }
else
  die "neither curl nor wget is available; please install one and retry."
fi

download() {
  info "  downloading $1"
  fetch "$BASE/$1" "$TMPDIR_TERRA/$1" || die "failed to download $BASE/$1"
}

path_hint() {
  case ":$PATH:" in
    *":$1:"*) ;;
    *) info ""
       info "note: $1 is not on your PATH. Add this to your shell profile:"
       info "      export PATH=\"$1:\$PATH\"" ;;
  esac
}

TMPDIR_TERRA="$(mktemp -d)" || die "could not create a temporary directory."
OS="$(uname -s)"

case "$OS" in
  Darwin)
    info "Installing Terra for macOS..."
    DMG="terra-macos-universal.dmg"
    download "$DMG"

    MOUNT="$TMPDIR_TERRA/mnt"
    mkdir -p "$MOUNT"
    hdiutil attach -nobrowse -readonly -mountpoint "$MOUNT" "$TMPDIR_TERRA/$DMG" -quiet ||
      die "could not mount $DMG"
    [ -d "$MOUNT/Terra.app" ] || die "$DMG did not contain Terra.app"

    if [ -w /Applications ]; then
      rm -rf /Applications/Terra.app
      cp -R "$MOUNT/Terra.app" /Applications/ || die "could not copy Terra.app into /Applications"
    elif command -v sudo >/dev/null 2>&1; then
      info "  /Applications needs elevated permissions; you may be asked for your password."
      sudo rm -rf /Applications/Terra.app || die "could not remove the previous /Applications/Terra.app"
      sudo cp -R "$MOUNT/Terra.app" /Applications/ || die "could not copy Terra.app into /Applications"
    else
      die "/Applications is not writable and sudo is unavailable."
    fi

    hdiutil detach "$MOUNT" -quiet 2>/dev/null || true
    MOUNT=""
    xattr -dr com.apple.quarantine /Applications/Terra.app 2>/dev/null || true
    info "  installed /Applications/Terra.app"

    # The CLI ships inside the bundle, so it is a symlink rather than a copy:
    # it then tracks whatever version of the app is installed.
    CLI=/Applications/Terra.app/Contents/MacOS/terra
    [ -x "$CLI" ] || die "Terra.app does not contain the terra CLI"
    if [ -w /usr/local/bin ]; then
      ln -sf "$CLI" /usr/local/bin/terra && info "  linked /usr/local/bin/terra"
    elif command -v sudo >/dev/null 2>&1 &&
         sudo mkdir -p /usr/local/bin && sudo ln -sf "$CLI" /usr/local/bin/terra; then
      info "  linked /usr/local/bin/terra"
    else
      mkdir -p "$HOME/.local/bin"
      ln -sf "$CLI" "$HOME/.local/bin/terra" || die "could not link the terra CLI anywhere."
      info "  linked $HOME/.local/bin/terra"
      path_hint "$HOME/.local/bin"
    fi

    info ""
    info "Done. Terra is in your Applications folder."
    info "If macOS warns on first launch, right-click Terra.app and choose Open."
    info "From the terminal: terra"
    ;;

  Linux)
    ARCH="$(uname -m)"
    case "$ARCH" in
      x86_64|amd64) ;;
      *) echo "Sorry, Terra does not have a Linux $ARCH build yet." >&2
         echo "See $REPO/releases for available downloads." >&2
         exit 1 ;;
    esac

    info "Installing Terra for Linux (x86_64)..."
    DEB="terra-linux-x86_64.deb"
    download "$DEB"

    if [ "$(id -u)" = 0 ]; then
      SUDO=""
    elif command -v sudo >/dev/null 2>&1; then
      SUDO="sudo"
      info "  installing the package needs root; you may be asked for your password."
    else
      die "installing the .deb needs root, and sudo is unavailable. Run this as root."
    fi

    if command -v apt-get >/dev/null 2>&1; then
      $SUDO apt-get install -y "$TMPDIR_TERRA/$DEB" || die "apt-get could not install $DEB"
    elif command -v dpkg >/dev/null 2>&1; then
      $SUDO dpkg -i "$TMPDIR_TERRA/$DEB" ||
        die "dpkg could not install $DEB; try '$SUDO apt-get -f install' to pull in what it needs."
    else
      echo "error: this installer needs apt-get or dpkg, and neither is available." >&2
      echo "Download a package by hand from $REPO/releases" >&2
      exit 1
    fi

    info ""
    info "Done. Launch Terra with: terra-app"
    info "The terra CLI is installed too: terra ls"
    ;;

  *)
    echo "This installer supports macOS and Linux only." >&2
    echo "On Windows, grab the installer from $REPO/releases" >&2
    exit 1
    ;;
esac
