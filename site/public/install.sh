#!/bin/sh
# Terra installer — https://github.com/terra-tools/terra
#
#   curl -fsSL https://terra-tools.github.io/terra/install.sh | sh
#   curl -fsSL https://terra-tools.github.io/terra/install.sh | sh -s v1.0.0
#
set -eu

REPO="https://github.com/terra-tools/terra"
TAG="${1:-}"

if [ -n "$TAG" ]; then
  BASE="$REPO/releases/download/$TAG"
else
  BASE="$REPO/releases/latest/download"
fi

TMPDIR_TERRA=""
cleanup() { [ -z "$TMPDIR_TERRA" ] || rm -rf "$TMPDIR_TERRA"; }
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

# Install $1 (a file) as an executable named $2 into $3, using sudo if needed.
install_bin() {
  chmod +x "$1"
  if [ -w "$3" ]; then
    mv -f "$1" "$3/$2"
  elif command -v sudo >/dev/null 2>&1; then
    sudo mv -f "$1" "$3/$2" || die "could not install $2 into $3"
  else
    return 1
  fi
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
    APP_TGZ="terra-macos-universal.app.tar.gz"
    CLI_TGZ="terra-cli-macos-universal.tar.gz"

    download "$APP_TGZ"
    tar -xzf "$TMPDIR_TERRA/$APP_TGZ" -C "$TMPDIR_TERRA" || die "could not extract $APP_TGZ"
    [ -d "$TMPDIR_TERRA/Terra.app" ] || die "$APP_TGZ did not contain Terra.app"

    if [ -w /Applications ]; then
      rm -rf /Applications/Terra.app
      mv "$TMPDIR_TERRA/Terra.app" /Applications/ || die "could not move Terra.app into /Applications"
    elif command -v sudo >/dev/null 2>&1; then
      info "  /Applications needs elevated permissions; you may be asked for your password."
      sudo rm -rf /Applications/Terra.app || die "could not remove the previous /Applications/Terra.app"
      sudo mv "$TMPDIR_TERRA/Terra.app" /Applications/ || die "could not move Terra.app into /Applications"
    else
      die "/Applications is not writable and sudo is unavailable."
    fi
    xattr -dr com.apple.quarantine /Applications/Terra.app 2>/dev/null || true
    info "  installed /Applications/Terra.app"

    download "$CLI_TGZ"
    tar -xzf "$TMPDIR_TERRA/$CLI_TGZ" -C "$TMPDIR_TERRA" || die "could not extract $CLI_TGZ"
    [ -f "$TMPDIR_TERRA/terra" ] || die "$CLI_TGZ did not contain the terra binary"
    if install_bin "$TMPDIR_TERRA/terra" terra /usr/local/bin; then
      info "  installed /usr/local/bin/terra"
    else
      mkdir -p "$HOME/.local/bin"
      install_bin "$TMPDIR_TERRA/terra" terra "$HOME/.local/bin" ||
        die "could not install the terra CLI anywhere."
      info "  installed $HOME/.local/bin/terra"
      path_hint "$HOME/.local/bin"
    fi

    info ""
    info "Done. Terra is in your Applications folder."
    info "First launch: right-click Terra.app and choose Open (the app is not notarized yet)."
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
    TGZ="terra-linux-x86_64.tar.gz"
    download "$TGZ"
    tar -xzf "$TMPDIR_TERRA/$TGZ" -C "$TMPDIR_TERRA" || die "could not extract $TGZ"
    [ -f "$TMPDIR_TERRA/terra-app" ] && [ -f "$TMPDIR_TERRA/terra" ] ||
      die "$TGZ did not contain the expected terra-app and terra binaries"

    if [ -w /usr/local/bin ] || command -v sudo >/dev/null 2>&1; then
      DEST=/usr/local/bin
    else
      DEST="$HOME/.local/bin"
      mkdir -p "$DEST"
    fi
    install_bin "$TMPDIR_TERRA/terra-app" terra-app "$DEST" || die "could not install terra-app into $DEST"
    install_bin "$TMPDIR_TERRA/terra" terra "$DEST" || die "could not install terra into $DEST"
    info "  installed $DEST/terra-app and $DEST/terra"
    path_hint "$DEST"

    info ""
    info "Done. Launch Terra with: terra-app"
    ;;

  *)
    echo "This installer supports macOS and Linux only." >&2
    echo "On Windows, grab the installer from $REPO/releases" >&2
    exit 1
    ;;
esac
