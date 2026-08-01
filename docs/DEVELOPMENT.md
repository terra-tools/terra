# Developing terra

## Everyday tasks (justfile)

```sh
just restart      # kill any running terra-app, rebuild debug, relaunch
just t <args>     # run the CLI against the live app, e.g. `just t ls`
just pre-commit   # fmt + clippy + tests — run before committing
just test / lint / fmt / check
just log          # tail the running app's log (/tmp/terra-app.log)
just bundle       # release build + cargo-packager (.app / .dmg)
```

Note: run `cargo build --release` before `cargo packager` if invoking the
packager by hand — it does not build the binary itself.

## Layout

- `crates/terra-app` — egui/eframe GUI (tabs, palette, IPC server, scrollbar)
- `crates/terra-cli` — the `terra` command (`terra learn` is self-teaching)
- `crates/terra-protocol` — shared wire types, socket path, blocking client
- `crates/terra-palette` — reusable command-palette widget
- `vendor/egui_term` — vendored terminal widget with terra patches (see its `PATCHES.md`)
- `crates/terra-app/assets/icon/` — `terra.svg` is the icon source of truth;
  PNGs and `terra.icns` are rendered from it (`rsvg-convert` + `iconutil`)
- `plans/` — git-ignored scratch: reference clones (ghostty, harmonics-cli) and
  the `logo-concepts*.html` design galleries that led to the icon

## Conventions

- Rust 2021, `cargo fmt` defaults; builds and clippy stay warning-free
- macOS is the v1 target; AppKit-specific code lives in `terra-app/src/macos.rs`
  behind `cfg(target_os = "macos")` with no-op fallbacks
- The wire protocol (terra-protocol) is frozen — extend it, don't break it;
  `terra learn`'s command map is generated from clap so it can't drift
- Hard-won gotchas are documented where they bit: egui_term patches in
  `vendor/egui_term/PATCHES.md`, the winit-waker warning in `ipc.rs`,
  the epaint color-emoji limitation in `fonts.rs`
