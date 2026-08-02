# terra — architecture & v1 contract

terra is a GUI terminal (Rust, egui/eframe + egui_term) with browser-style
tabs whose killer feature is remote control: a `terra` CLI can list/create/
kill/send-to/capture tabs over a unix socket — tmux ergonomics, GUI window.

## Workspace

- `crates/terra-protocol` — wire types + blocking client. **DONE, do not change
  the wire format without updating all consumers.**
- `crates/terra-palette` — reusable egui command-palette widget. Public API is
  frozen (see its lib.rs docs); implement `show()`.
- `crates/terra-app` — the GUI app (binary `terra-app`). Owns tabs, UI, IPC server.
- `crates/terra-cli` — the `terra` CLI (binary `terra`). Thin client over terra-protocol.

egui_term is pinned by git rev in the root Cargo.toml. Its API (all we need):
`TerminalBackend::new(id, egui::Context, Sender<(u64, PtyEvent)>, BackendSettings{shell, args, working_directory})`,
`backend.process_command(BackendCommand::Write(Vec<u8>))`,
`backend.last_content() -> &RenderableContent` (pub `grid: Grid<Cell>` from
alacritty_terminal 0.26; iterate `grid.display_iter()` for capture),
`backend.sync()`, `TerminalView::new(ui, &mut backend).set_focus(bool).set_size(vec2)`,
`PtyEvent::{Exit, Title(String)}`.

## terra-app design

- `TabManager`: `BTreeMap<u64, Tab>`, monotonic `next_id` (never reuse ids).
  `Tab { backend: TerminalBackend, shell_title: String, custom_title: Option<String> }`.
  Effective title = custom_title.or(shell_title). PtyEvent::Title updates
  shell_title; PtyEvent::Exit removes the tab (app quits when last tab closes).
- Tab bar: top panel, Ghostty-like rounded "pill" buttons, active tab
  highlighted, close on middle-click, trailing `+` button. Keep it clean/dark.
- Keybindings: Cmd+T new tab, Cmd+W close active, Cmd+1..9 select nth,
  Cmd+Shift+P open palette, Cmd+Shift+[ / ] prev/next tab.
- Palette actions (ids): `tab.new`, `tab.close`, `tab.rename` (opens prompt
  mode, prompt id "rename"), `tab.next`, `tab.prev`, `tab.select.<id>` (one per
  open tab, label = title), `app.quit`.
- IPC server (`ipc.rs`): thread with `UnixListener` on `terra_protocol::socket_path()`
  (create parent dir 0700; remove stale socket on startup; remove on exit).
  Connection threads execute requests directly against the shared
  `Arc<Mutex<TabManager>>` — never via the UI thread, which eframe parks
  entirely while the window is occluded. Repaint is requested after mutating
  requests; `Select` also summons the window (thread-safe NSRunningApplication;
  never activate from inside the frame callback — it wedges winit's waker).
- Capture: `backend.sync()` then walk `last_content().grid.display_iter()`,
  build lines for the visible screen; include up to `scrollback` lines above
  via grid indexing if feasible, else visible-only is acceptable for v1.
  Trim trailing whitespace / blank tail lines.
- Send: `process_command(BackendCommand::Write(text.into_bytes()))`, append
  `\r` if `enter`. With `keys`, the text is first parsed by
  `terra-protocol::keys` into `Bytes`/`Delay` chunks and written in order.
- Config: `~/.terra/config.toml` (`TERRA_CONFIG` overrides), read once at
  startup into `config.rs`. `[font] size, line_height`, `[text] bidi,
  bidi_base, [text.bidi_quirks]`. Loading never fails — see
  `docs/config.example.toml`.

## terra CLI (tmux-flavored)

```
terra ls                          # table: id  active  title
terra new [--title T] [--cwd D] [-- cmd args...]   # prints new tab id
terra kill <tab>
terra send <tab> "text" [--keys] [--enter]  # --enter appends CR (like tmux send-keys ... Enter)
terra capture <tab> [--scrollback N] [--cells]  # text, or the styled grid as JSON
terra rename <tab> "new title"
terra select <tab>
terra bidi <tab> [off|on|auto]    # per-tab RTL reordering; prints the mode
terra learn                       # self-teaching prompt for agents
terra doctor                      # probe the terminal this CLI runs inside
terra record --out F -- cmd | terra record --decode F   # both I/O directions
terra --json ...                  # raw JSON Response on stdout for scripting
```

Exit codes: 0 ok, 1 error (message on stderr). `<tab>` is the numeric id from `ls`.

`--keys` opts the text into the key notation in `terra-protocol::keys`
(`{Enter}`, `{C-c}`, `{S-Tab}`, `{F5}`, `{M-b}`, `{Delay 300}`, `{{` for a
literal brace); it is opt-in because `{Home}` and `${HOME}` collide, and text
without it reaches the PTY byte for byte. `--cells` returns run-length-encoded
runs carrying `fg`/`bg`/`flags` plus the cursor, with colours left as the
program named them (`{"indexed":236}`, `{"named":"Background"}`, `"#3a3a3a"`)
rather than resolved against the theme.

`doctor` and `record` never open the socket: they talk to the terminal the CLI
is running inside, so the same binary run under terra and under any other
terminal produces two outputs that diff.

## Conventions

- Rust 2021, `cargo fmt` defaults, no new heavyweight deps without need.
- Everything must pass `cargo build` + `cargo clippy` warnings-free-ish.
- macOS is the target platform for v1.
