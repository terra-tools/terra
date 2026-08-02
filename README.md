<div align="center">

<a href="https://terra-tools.github.io/terra/">
  <img src="crates/terra-app/assets/icon/terra.svg" width="76" alt="Terra">
</a>

<h1>Terra</h1>

<p>
  <b>A terminal your agents can drive.</b><br>
  Browser-style tabs in a clean, dark window. Your coding agent opens tabs, types into them and reads them back — while you watch the same tabs live.
</p>

<p>
  <a href="https://terra-tools.github.io/terra/"><img alt="Website" src="https://img.shields.io/badge/website-terra--tools.github.io-1d4ed8?style=flat-square"></a>
  <a href="https://github.com/terra-tools/terra/releases"><img alt="Download" src="https://img.shields.io/badge/download-macOS%20%7C%20Windows%20%7C%20Linux-1d4ed8?style=flat-square"></a>
  <img alt="License" src="https://img.shields.io/badge/license-MIT-1d4ed8?style=flat-square">
</p>

</div>

## Install

**[Download for your platform →](https://terra-tools.github.io/terra/#install)**

macOS and Linux, from a terminal:

```sh
curl -fsSL https://terra-tools.github.io/terra/install.sh | sh
```

Installers are on the [releases page](https://github.com/terra-tools/terra/releases)
too: `.dmg` on macOS, `-setup.exe` on Windows, `.deb` on Linux. Not signed yet —
macOS: right-click → Open on first launch; Windows: click through SmartScreen.

## Why

Agents run long commands somewhere you cannot see. Terra gives them a real
window instead: every command lands in a tab you can watch, scroll back and
take over at any moment. Nothing is hidden in a log file, and nothing needs a
second terminal multiplexer on top.

- **Tabs, not panes.** Titles, icons and `⌘1`–`⌘9`, the way a browser does it.
- **Driveable.** Anything on your machine can open a tab, send keystrokes and
  read the screen back.
- **Yours.** Free, open source and local — no account, no telemetry.

## Docs

- [Using Terra with agents](docs/AGENTS.md) — the block to paste into your CLAUDE.md or AGENTS.md
- [Architecture](docs/ARCHITECTURE.md) — how the app, the CLI and the protocol fit together
- [Development](docs/DEVELOPMENT.md) — building, testing and packaging from source
- [Configuration](docs/config.example.toml) — the settings file in `~/.terra/config.toml`

## License

MIT
