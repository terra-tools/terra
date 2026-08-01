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

# Remove build artifacts
clean:
    cargo clean

# Tail the running app's log
log:
    tail -f /tmp/terra-app.log
