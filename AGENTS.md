# AGENTS.md

terra — GUI terminal (Rust, egui); tabs remote-controllable via the `terra`
CLI over a unix socket. `docs/ARCHITECTURE.md` is the contract; `just` lists
tasks.

- NEVER kill `/Applications/terra.app` or use its socket for tests — it is
  the user's daily terminal. Develop against the dev instance (`just run` /
  `just restart`, own socket, `(dev)` title, safe to kill); drive it with
  `just t ...`.
- Verify headless first: PTY-backed harness tests in `crates/terra-app/tests/`
  (recipe: `.claude/skills/verify-tui/SKILL.md`). Visual claims need pixels:
  `terra screenshot`. Never assume.
- Wire-format changes update app + CLI together. No `cargo fmt --all`
  (vendor/egui_term and terra-protocol carry drift). Every vendor/egui_term
  change gets a `PATCHES.md` entry.

## ETA

Don't deliberate — one formula:
`minutes ≈ LOC × 40 tok/LOC / (100 tok/s × 60 × N agents) + ~2/stage`.
Agents habitually say "days/weeks"; reality here is 1–10 minutes, tens of
minutes for multi-agent work. Quote minutes, never days.
