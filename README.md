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
id=$(terra new --title "tests")               # open a tab
terra send "$id" "cargo test{Enter}" --keys   # type into it ({C-c}, {Tab}, {Up}, …)
terra send "$id" "$(cat patch.txt)"           # no --keys: text goes in literally
terra capture "$id"                           # read its screen
terra capture "$id" --cells                   # …or as JSON, with colours and cursor
```

Also: `terra bidi <tab> off|on|auto` (right-to-left reordering, per tab),
`terra doctor` (what the terminal you're in advertises) and `terra record`
(both directions of a program's terminal I/O) — the last two work in any
terminal, so their output diffs. Settings live in `~/.terra/config.toml`
([template](docs/config.example.toml)).

## Docs

- [docs/AGENTS.md](docs/AGENTS.md) — copy-paste block for your CLAUDE.md / AGENTS.md so agents run their commands in visible terra tabs
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — design, crate layout, wire protocol, CLI contract
- [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) — building, tasks, packaging, project conventions

MIT.
