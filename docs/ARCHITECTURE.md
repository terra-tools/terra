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
  Per connection: read JSON lines -> `Request`; forward
  `(Request, mpsc::Sender<Response>)` to the UI thread over an
  `mpsc::Sender<IpcMsg>`; call `egui::Context::request_repaint()` so the UI
  wakes; block (with ~2s timeout) for the Response; write JSON line back.
  UI thread drains requests in `update()` and executes them on TabManager.
- Capture: `backend.sync()` then walk `last_content().grid.display_iter()`,
  build lines for the visible screen; include up to `scrollback` lines above
  via grid indexing if feasible, else visible-only is acceptable for v1.
  Trim trailing whitespace / blank tail lines.
- Send: `process_command(BackendCommand::Write(text.into_bytes()))`, append
  `\r` if `enter`.

## terra CLI (tmux-flavored)

```
terra ls                          # table: id  active  title
terra new [--title T] [--cwd D] [-- cmd args...]   # prints new tab id
terra kill <tab>
terra send <tab> "text" [--enter]  # --enter appends CR (like tmux send-keys ... Enter)
terra capture <tab> [--scrollback N]  # prints text to stdout
terra rename <tab> "new title"
terra select <tab>
terra --json ...                  # raw JSON Response on stdout for scripting
```

Exit codes: 0 ok, 1 error (message on stderr). `<tab>` is the numeric id from `ls`.

## Conventions

- Rust 2021, `cargo fmt` defaults, no new heavyweight deps without need.
- Everything must pass `cargo build` + `cargo clippy` warnings-free-ish.
- macOS is the target platform for v1.
