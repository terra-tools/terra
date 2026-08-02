# terra — common tasks. Run `just` to list.

set quiet

# The socket the *development* build binds. On macOS the socket is the
# single-instance claim: a launch that finds the default `~/.terra/terra.sock`
# alive focuses that instance and exits. Giving the debug build its own address
# is what lets it open beside the terra you actually work in all day — and it is
# also what puts " (dev)" in its title bar (see `dev_suffix` in
# crates/terra-app/src/main.rs). Nothing in the code changes: plain `terra` and
# /Applications/Terra.app still use the default socket.
dev-socket := env('HOME', '/tmp') / '.terra/terra-dev.sock'

# Pattern that matches the dev app and *only* the dev app. The installed build
# runs from `Terra.app/Contents/MacOS/terra-app`, so it can never match this.
dev-pattern := 'target/debug/terra-app'

default:
    just --list

# Build everything (debug)
build:
    cargo build

# Build optimized binaries
release:
    cargo build --release

# Run the GUI app (debug build) beside the installed release
run:
    TERRA_SOCKET='{{dev-socket}}' cargo run -p terra-app

# Kill the running *dev* app (never the installed one) and start a fresh debug build
restart: build
    -pkill -f '{{dev-pattern}}'
    sleep 1
    (TERRA_SOCKET='{{dev-socket}}' nohup ./target/debug/terra-app > /tmp/terra-app.log 2>&1 &)
    echo "terra-app (dev) restarted on {{dev-socket}} (log: /tmp/terra-app.log)"

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

# Plain `terra` in your shell still talks to the installed release.
# Use the CLI against the running *dev* app, e.g. `just t ls`, `just t new -- htop`
t *args:
    TERRA_SOCKET='{{dev-socket}}' cargo run -q -p terra-cli -- {{args}}

# Cross-platform bundles via cargo-packager (.app + .dmg on macOS)
bundle: release
    cargo packager -p terra-app --release

# One-time: mint a stable self-signed "terra-dev" signing identity, so macOS
# recognises the app across upgrades and TCC permission grants survive.
# Ad-hoc signatures (`--sign -`) are per-build hashes: every upgrade looks
# like a brand-new app and re-prompts for Downloads/Music/etc.
setup-signing:
    uv run scripts/release/setup_signing.py

# Bundle + sign + install to /Applications and the CLI to bin, e.g. `just install 1 ~/.local/bin`.
# Signs with the stable terra-dev identity when present (see setup-signing),
# else falls back to ad-hoc.
install force="" bin="/usr/local/bin":
    uv run scripts/release/install.py '{{force}}' '{{bin}}'

# Replace the installed app with a fresh build and relaunch it.
# Closes the running daily instance (its tabs die) — run when you're done for the iteration.
upgrade:
    just install 1
    open /Applications/Terra.app
    echo "terra upgraded and relaunched"

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
