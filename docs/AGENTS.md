# Teaching your agent to use terra

Paste this into your project's `CLAUDE.md` / `AGENTS.md`:

````markdown
## terra: visible terminals

Run long-lived commands (servers, builds, watchers) in [terra](https://github.com/terra-tools/terra)
tabs the human can watch, via the `terra` CLI. If `terra ls` errors, silently use normal shells.

- `id=$(terra new --title "tests" -- cargo test)` — one titled tab per task
- `terra send "$id" "cmd{Enter}" --keys` — type into it; `{C-c}`, `{Tab}`, `{Up}`,
  `{Delay 300}` too. Drop `--keys` to paste text literally.
- `terra capture "$id"` reads its screen; `--cells` adds colours and cursor as JSON
- `terra select "$id"` raises the window (only when the human's attention is needed);
  `terra kill "$id"` when done. Never touch tabs you didn't create. Details: `terra learn`.
````
