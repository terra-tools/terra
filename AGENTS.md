# AGENTS.md

terra — a GUI terminal (Rust, egui) whose tabs are remote-controllable via the
`terra` CLI over a unix socket. Read `docs/ARCHITECTURE.md` before touching
code; it is the contract.

## Commands

- `just check` / `just lint` / `just test`; `just pre-commit` before committing.
- Dev instance: `just run` or `just restart` — own socket, window titled
  `(dev)`, safe to kill. Drive it: `just t ls|new|send|capture ...`.
- NEVER kill `/Applications/terra.app` — it is the user's daily terminal.
  Only `just upgrade` may replace it, and only the user runs that.

## Verification

- Headless first: PTY-backed egui harness tests in `crates/terra-app/tests/`
  — inject egui events, assert on the bytes that reach the PTY. Recipe:
  `.claude/skills/verify-tui/SKILL.md`.
- Live second: dev instance + `just t`. Visual claims need pixels
  (`terra screenshot` once available), not assumptions.

## Conventions

- Wire format (`terra-protocol`) changes must update app + CLI together.
- Don't `cargo fmt --all`: `vendor/egui_term` and `terra-protocol` carry
  pre-existing drift; fmt only the gated crates (match CI).
- Every change to `vendor/egui_term` gets an entry in its `PATCHES.md`.

## ETA for plans

Do not deliberate over estimates. One formula, ten seconds, done:

    minutes ≈ LOC × 40 tok/LOC (incl. thinking) / (100 tok/s × 60 × N subagents)
              + ~2 min overhead per sequential stage (reading, building, tests)

So: 300 LOC ≈ 2–4 min solo; a 4-agent parallel feature ≈ 5–15 min.

Agents habitually get this wrong and answer in human units — "a few days",
"1–2 weeks", "several hours". Reality: most tasks in this repo finish in
**1–10 minutes**, large multi-agent work in tens of minutes. Never quote
days or weeks; quote minutes.
