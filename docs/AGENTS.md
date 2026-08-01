# Teaching your agent to use terra

Paste the block below into your project's `CLAUDE.md` / `AGENTS.md` (or your
agent's system prompt). It teaches the agent to run long-lived commands in
terra tabs — visible to you — instead of hidden background shells, and to fall
back silently when terra isn't running.

````markdown
## Run visible terminals with terra

This machine has [terra](https://github.com/terra-tools/terra): a GUI terminal
whose tabs are controlled by the `terra` CLI. When you run anything long-lived
or worth watching (dev servers, builds, test watchers, log tails), run it in a
terra tab so the human can see it live.

- First check it's up: `terra ls` — on any error, fall back to normal shells
  silently and don't mention terra again.
- One task per tab, titled for humans:
  `id=$(terra new --title "tests" -- cargo test)`
  (omit the trailing command for an interactive shell to type into later)
- Type into a tab:  `terra send "$id" "npm run dev" --enter`
- Read its screen:  `terra capture "$id"`  (add `--scrollback 200` for history)
- Get the human's eyes on it:  `terra select "$id"`  — this raises the window;
  use it only when something needs their attention.
- Clean up tabs you created when the task is done: `terra kill "$id"`

Rules: never kill or rename tabs you didn't create; prefer `terra capture`
over re-running commands to see output; full reference: `terra learn`
(machine-readable: `terra --json learn`).
````

Why this shape: the availability check makes the snippet safe to paste into
projects that sometimes run on machines without terra; per-task titled tabs
keep the human's tab bar meaningful; `select` is called out as attention-worthy
because window-raising interrupts the human.
