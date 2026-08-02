# AGENTS.md

terra — GUI terminal (Rust, egui); tabs remote-controllable via the `terra`
CLI over a unix socket. `docs/ARCHITECTURE.md` is the contract; `just` lists
tasks (`just pre-commit` before committing).

## Working style

- For a multi-part prompt (a feature request, a release-polish list, anything
  with more than one deliverable): state the plan first, briefly — a few
  bullets of what you would do and in what order — and STOP there. Do not
  start working until the user okays the plan: besides redirecting the scope,
  that is where they choose *how* to run it (e.g. fan out to many subagents,
  reorder, drop items). Simple one-step asks don't need this.

## Two instances

- NEVER kill `/Applications/Terra.app` or test against its socket — it is
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

## Orchestration

- The main agent is the MANAGER: it plans, briefs, reviews and integrates —
  it does not implement. Delegate all implementation to background
  subagents, always on the Opus 5 model (`model: "opus"`), so the manager
  stays free to talk to the user and steer. Trivial one-file touch-ups the
  manager may do inline; anything more goes to a subagent.
- Subagents run in git worktrees by default (`isolation: "worktree"`); the
  manager collects each worktree's diff, applies it to the main tree, and
  removes the worktree. Every brief names the exact files the agent owns.
  Caveat: a worktree branches from HEAD, so an agent that must build on
  uncommitted changes instead works in the main tree with exclusive
  ownership of those files (or the manager commits first).
- Plain subagents usually beat a workflow — reach for workflows only when
  staged fan-out genuinely pays.
- Every live-verifying agent gets its own socket
  (`TERRA_SOCKET=~/.terra/terra-<task>.sock`) and kills only its own app
  pid — never a shared `pkill` pattern.

## ETA

Don't deliberate — one formula:
`minutes ≈ LOC × 40 tok/LOC / (100 tok/s × 60 × N agents) + ~2 min/stage`.
Agents habitually answer "days/weeks"; reality here is 1–10 minutes, tens of
minutes for multi-agent work. Quote minutes, never days.
