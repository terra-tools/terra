# AGENTS.md

terra — GUI terminal (Rust, egui); tabs remote-controllable via the `terra`
CLI over a unix socket. `docs/ARCHITECTURE.md` is the contract; `just` lists
tasks (`just pre-commit` before committing).

## Two instances

- NEVER kill `/Applications/terra.app` or test against its socket — it is
  the user's daily terminal. Only `just upgrade` replaces it; only the user
  runs that.
- Develop against the dev instance: `just run` / `just restart` — own socket,
  `(dev)` window title, safe to kill. Drive it with `just t ls|new|send|capture`.

## Verification

- Headless first: PTY-backed harness tests in `crates/terra-app/tests/` —
  inject egui events, assert on the bytes/grid. Recipe:
  `.claude/skills/verify-tui/SKILL.md`.
- Visual claims need pixels: `terra screenshot --out f.png`, then look at it.
  Never assume.

## Conventions

- Wire-format (`terra-protocol`) changes update app + CLI together.
- No `cargo fmt --all` — vendor/egui_term and terra-protocol carry drift;
  fmt only the crates you touched (match CI's gate).
- Every `vendor/egui_term` change gets an entry in its `PATCHES.md`.

## ETA

Don't deliberate — one formula:
`minutes ≈ LOC × 40 tok/LOC / (100 tok/s × 60 × N agents) + ~2 min/stage`.
Agents habitually answer "days/weeks"; reality here is 1–10 minutes, tens of
minutes for multi-agent work. Quote minutes, never days.
