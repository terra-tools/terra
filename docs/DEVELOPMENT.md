# Developing terra

## Everyday tasks (justfile)

```sh
just run / restart   # debug build on the dev socket, beside your daily terra
just t <args>        # CLI against the dev app, e.g. `just t ls`
just pre-commit      # fmt + clippy + tests
just log             # tail /tmp/terra-app.log
just bundle          # release build + cargo-packager (.app / .dmg)
just upgrade         # replace /Applications/terra.app and relaunch it
```

## Dev instance vs daily instance

The control socket is the single-instance claim, so `just run`/`restart`/`t`
export `TERRA_SOCKET=~/.terra/terra-dev.sock` and the debug build opens beside
the terra you are working in. The dev window is titled `… (dev)` (`TERRA_DEV=1/0`
forces the mark); plain `terra` still drives the installed app. `just restart`
pkills only `target/debug/terra-app` — it can never hit the installed bundle.
`just upgrade` is the one command that intentionally closes the daily instance.

Note: `cargo packager` does not build; run `cargo build --release` first when
invoking it by hand.

## Layout

- `crates/terra-app` — egui/eframe GUI (tabs, palette, IPC server)
- `crates/terra-cli` — the `terra` command (`terra learn` is self-teaching)
- `crates/terra-protocol` — wire types, socket path, blocking client
- `crates/terra-palette` — reusable command-palette widget
- `crates/terra-app/src/config.rs` — `~/.terra/config.toml`
  (`docs/config.example.toml` lists every key)
- `vendor/egui_term` — vendored terminal widget; every terra patch is logged
  in its `PATCHES.md`
- `crates/terra-app/assets/icon/` — `terra.svg` is the source of truth
- `crates/terra-app/assets/tab-icons/` — the per-tab logos in the tab bar
  (`src/tab_icon.rs`). SVGs are the sources, the checked-in 64px PNGs next to
  them are what ships; that directory's `LICENSE.md` has the provenance and the
  `rsvg-convert` line that regenerates them
- `plans/` — git-ignored scratch (reference clones, design galleries)

## Debugging "looks wrong in terra, right in Ghostty"

Three questions, each answered without a screenshot:

- **What did the program draw?** `terra capture <tab> --cells` — styled grid
  as JSON. If the styling isn't there, the program never emitted it.
- **What does this terminal advertise?** `diff <(terra doctor) <(ssh box
  terra doctor)`.
- **What did the program say and hear?** `terra record --out t.jsonl -- prog`
  in each terminal, `--decode`, diff. The only view of the terminal→program
  direction — usually where the divergence lives (e.g. Codex derives its
  composer shade from an OSC 10/11 reply terra once failed to send).

For pixels: `terra screenshot --out f.png` (add `--pretty` for a shareable
card).

## Conventions

- Rust 2021; builds and clippy stay warning-free; macOS is the v1 target
  (AppKit code in `terra-app/src/macos.rs` behind `cfg`, no-op fallbacks).
- The wire protocol is frozen — extend, don't break; `terra learn`'s command
  map is generated from clap so it can't drift.
- Config loading never fails: missing/malformed `config.toml` yields the
  compiled-in defaults plus a warning. Runtime settings are a session layer
  over the file layer, never written back (see `config.rs` module docs).
- Gotchas are documented where they bit: `vendor/egui_term/PATCHES.md`, the
  winit-waker warning in `ipc.rs`, the epaint colour-emoji notes in `fonts.rs`.
