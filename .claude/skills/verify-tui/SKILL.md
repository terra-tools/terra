---
name: verify-tui
description: Verify terra's terminal behaviour (input translation, focus, mouse reporting, escape sequences) without a human at the GUI — headless egui harness first, live dev instance second.
---

# Verifying terminal behaviour in terra

"Does the wheel/keyboard/focus behave right inside program X?" is testable
without opening a window. Work down this list; the first layer that can
express the question is the right one.

## Layer 1 — headless harness (default, CI-able)

An `egui::Context` renders a terra-shaped frame over a **real PTY**; you
inject synthetic egui events and read what bytes reached the program (or what
the grid shows). Existing examples to copy from, both in
`crates/terra-app/tests/`:

- `tab_focus.rs` — clicks, typing, focus/palette interplay.
- `mouse_reporting.rs` — wheel events vs terminal modes (repro for #21).

The pattern:

1. Spawn the PTY with the modes the real program would set. `/bin/cat` echoes
   every byte back through the tty, so the *screen becomes the assertion*:
   `sh -c "printf '\033[?1049h\033[?1000h\033[?1006h'; cat"` behaves like
   claude code (alt screen + SGR mouse) but prints what it receives.
   Control bytes echo caret-style: arrow-up shows as `^[[A` (or `^[OA` in
   application cursor mode), an SGR wheel report as `^[[<64;…M`.
2. Drive frames with `ctx.run_ui(RawInput { events, .. }, |ui| …)` rendering
   `TerminalView` the way `main.rs` does (`set_focus`, panels).
3. Order matters: send `Event::PointerMoved` into the grid on an earlier
   frame before pointer-gated events (wheel, clicks). Poll
   `backend.sync().terminal_mode` until the program's mode changes land
   before injecting the event under test.
4. Assert on the grid text (`display_iter`) with a deadline loop — the PTY is
   asynchronous.

Known-bug repros live as `#[ignore = "reproduces issue #N"]` tests: the
normal suite stays green, `cargo test -- --ignored` demonstrates the bug, and
the fix un-ignores them. PTY tests are `#![cfg(unix)]`.

## Layer 2 — the live dev instance (terra drives terra)

For behaviour that needs the real app (window focus, IPC, rendering,
config), run the dev build beside the daily one and script it:

    just restart            # dev instance on ~/.terra/terra-dev.sock
    just t new -- <program> # open the program under test, prints tab id
    just t send <id> "text" --keys --enter
    just t capture <id>     # what the screen actually shows
    just log                # the dev instance's stderr/log

`terra send --keys` can type anything (`{C-c}`, `{Delay 300}`…), and
`capture` reads the result — a full interactive session without a human.
Compare against another terminal by running the same program there and
diffing `terra doctor` / `terra record` output.

## Layer 3 — what neither layer can do

Real OS-level mouse wheel/IME/gesture events entering the window. Synthetic
CGEvent injection (small Swift tool + Accessibility permission) is possible
but fragile — prefer restating the question as Layer 1 ("which bytes should
this event become?") which is almost always the actual bug.
