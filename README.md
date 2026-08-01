# terra 🌱

A GUI terminal with browser-style tabs and a remote-control CLI — tmux
ergonomics in a Ghostty-like window, built for watching (and driving) coding
agents. Any process can open a tab, type into it, and read it back while you
watch the same tabs live.

## Quick start

```sh
just restart          # build + launch the app
just t learn          # the CLI teaches agents (and you) everything else
```

```sh
id=$(terra new --title "tests" -- bash)   # open a tab
terra send "$id" "cargo test" --enter      # type into it
terra capture "$id"                        # read its screen
```

## Docs

- [docs/AGENTS.md](docs/AGENTS.md) — copy-paste block for your CLAUDE.md / AGENTS.md so agents run their commands in visible terra tabs
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — design, crate layout, wire protocol, CLI contract
- [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) — building, tasks, packaging, project conventions

MIT.
