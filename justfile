# terra — common tasks. Run `just` to list.

set quiet

default:
    just --list

# Build everything (debug)
build:
    cargo build

# Build optimized binaries
release:
    cargo build --release

# Run the GUI app (debug build)
run:
    cargo run -p terra-app

# Kill any running terra-app and start a fresh debug build
restart: build
    -pkill -f 'target/(debug|release)/terra-app'
    sleep 1
    (nohup ./target/debug/terra-app > /tmp/terra-app.log 2>&1 &)
    echo "terra-app restarted (log: /tmp/terra-app.log)"

# Type-check the whole workspace
check:
    cargo check --workspace

# Run all tests
test:
    cargo test --workspace

# Lint (clippy) the whole workspace
lint:
    cargo clippy --workspace --all-targets

# Format all crates
fmt:
    cargo fmt --all

# fmt + clippy + tests — run before committing
pre-commit: fmt lint test

# Use the CLI against the running app, e.g. `just t ls`, `just t new -- htop`
t *args:
    cargo run -q -p terra-cli -- {{args}}

# Cross-platform bundles via cargo-packager (.app + .dmg on macOS)
bundle: release
    cargo packager -p terra-app --release

# Bundle + ad-hoc sign + install to /Applications and the CLI to bin, e.g. `just install 1 ~/.local/bin`
install force="" bin="/usr/local/bin":
    #!/usr/bin/env bash
    set -euo pipefail
    dest=/Applications/terra.app
    if [ -e "$dest" ] && [ -z "{{force}}" ]; then
        echo "$dest already exists. Re-run as 'just install 1' to replace it." >&2
        exit 1
    fi
    just bundle
    codesign --force --deep --sign - target/release/terra.app
    pkill -f 'terra.app/Contents/MacOS/terra-app' 2>/dev/null || true
    rm -rf "$dest"
    cp -R target/release/terra.app "$dest"
    xattr -dr com.apple.quarantine "$dest" 2>/dev/null || true
    install -m 755 target/release/terra "{{bin}}/terra" 2>/dev/null \
        || sudo install -m 755 target/release/terra "{{bin}}/terra"
    echo "installed $dest and {{bin}}/terra — first launch: right-click the app -> Open"

# Remove build artifacts
clean:
    cargo clean

# Tail the running app's log
log:
    tail -f /tmp/terra-app.log

# --- landing page (site/) -----------------------------------------------
# React + Vite + Tailwind, pnpm. Copy lives in site/src/locales/ (en only).

# Dev server with hot reload, opened in your browser
site:
    cd site && pnpm install --prefer-offline && pnpm dev --open

# Production build into site/dist (BASE_PATH=/ for a custom domain)
site-build:
    cd site && pnpm install --prefer-offline && pnpm build

# Serve the production build exactly as GitHub Pages will
site-preview: site-build
    cd site && pnpm preview --open

# Typecheck + lint the site
site-check:
    cd site && pnpm exec tsc -b --noEmit && pnpm lint
