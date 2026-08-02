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
- `crates/terra-app/src/config.rs` — `~/.terra/config.toml`; see
  `docs/config.example.toml` for every key
- `vendor/egui_term` — vendored terminal widget with terra patches (see its `PATCHES.md`)
- `crates/terra-app/assets/icon/` — `terra.svg` is the icon source of truth;
  PNGs and `terra.icns` are rendered from it (`rsvg-convert` + `iconutil`)
- `plans/` — git-ignored scratch: reference clones (ghostty, harmonics-cli) and
  the `logo-concepts*.html` design galleries that led to the icon

## Debugging a rendering or terminal-behaviour difference

"It looks wrong in terra but right in Ghostty" is three separate questions, and
each has a command that answers it without a screenshot:

- **What did the program actually draw?** `terra capture <tab> --cells` — the
  grid as JSON, run-length encoded by style, with colours left as the program
  named them and the cursor position included. If the styling you expect is not
  in there, the program never asked for it and the renderer is innocent.
- **What does this terminal advertise?** `diff <(terra doctor) <(ssh box terra
  doctor)` — env, size, colour count and decoded DA1/DA2/XTVERSION/DECRQM/CPR
  replies, sorted and free of run-to-run noise, so the diff is the difference.
- **What did the program say and hear?** `terra record --out t.jsonl -- prog` in
  each terminal, then `terra record --decode` both and diff. This is the only
  one of the three that shows the terminal→program direction, which is usually
  where the divergence lives.

Worked example: Codex's composer background was missing in terra. `capture
--cells` showed zero non-default backgrounds, so nothing was being dropped in
rendering — Codex simply never emitted the colour. Diffing a `record` taken in
terra against one taken in Terminal.app showed why: Codex queries the terminal's
foreground and background with OSC 10/11 and derives the composer shade from the
answer, and terra never replied, so it fell back to no shading. The bug was a
missing reply, invisible from the output direction alone.

## Conventions

- Rust 2021, `cargo fmt` defaults; builds and clippy stay warning-free
- macOS is the v1 target; AppKit-specific code lives in `terra-app/src/macos.rs`
  behind `cfg(target_os = "macos")` with no-op fallbacks
- The wire protocol (terra-protocol) is frozen — extend it, don't break it;
  `terra learn`'s command map is generated from clap so it can't drift
- Config loading never fails. A missing, unreadable or malformed
  `config.toml` yields the compiled-in defaults plus a warning — a GUI that
  refuses to start over a typo is a GUI that appears to do nothing. Defaults
  are pinned by test to the values terra hardcoded before config existed.
- Runtime settings are a session layer over the file layer, and are never
  written back; see the module docs in `config.rs` for why.
- Hard-won gotchas are documented where they bit: egui_term patches in
  `vendor/egui_term/PATCHES.md`, the winit-waker warning in `ipc.rs`,
  the epaint color-emoji limitation in `fonts.rs`
