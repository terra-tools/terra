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
- Editor groups (VS Code's full 2D "splits"): the window is a *split tree* —
  a node is either a leaf (`Group { id, tab_ids: Vec<u64>, active }`) or a
  split `{ axis: Horizontal | Vertical, children: Vec<Node> }`, each child
  carrying a weight (its share of the split's extent). Leaves hold tab ids
  only — tabs stay in the one global map, so IPC keeps addressing them by
  global id and the wire protocol is unchanged. The public API addresses
  groups by *DFS leaf index*; focus internally tracks the leaf's stable `id`,
  so reshaping the tree elsewhere never moves it. Invariants, restored by
  every mutation: every open tab is in exactly one leaf, a leaf's `active`
  names one of its own tabs, no leaf is empty (a closed-out leaf folds its
  weight into the sibling before it, else the one after), no split has a
  single child (the child replaces it), and no child of a split runs on its
  parent's axis (its children splice in, scaled — VS Code's merge, which is
  also what makes "split a leaf whose parent already runs that way" insert a
  sibling sharing the leaf's old extent). One leaf is focused; its active tab
  is the *globally* active tab — keyboard input goes there and `terra ls`
  marks it. `select <tab>` activates the tab in its leaf *and* focuses that
  leaf. New tabs land in the focused leaf after its active tab. Splitting a
  lone tab is a no-op (the old leaf would collapse into the new one).
  `split_right/left` make side-by-side columns (`Horizontal`),
  `split_down/up` stack rows (`Vertical`); the renderer walks
  `TabManager::layout()` recursively — rows within columns within rows —
  with a draggable hairline between siblings on both axes (resize cursor per
  axis, no child below 0.15 of its split, weights per split via
  `split_weights`/`set_split_weights(path, …)`).
- Tab bar: one per group, across the top of the group's column —
  Ghostty-like rounded "pill" buttons, active tab highlighted (the *group's*
  active tab; the focused group's bar is the brighter one), close on
  middle-click, trailing `+` button, and a `⌄` beside it opening that group's
  profile menu (`ui::chevron_menu` — ui + rect + salt + `[MenuEntry]` in,
  chosen `AppAction` out, so it re-anchors to any `+`; the rows open in the
  group whose bar was clicked). Keep it clean/dark. A single group
  holding a single tab shows its bar unless `[tabs] bar_with_one_tab = false`
  turns that off; only the lone *empty* group is ever bare unconditionally,
  and a second tab or a second group brings every bar back either way
  (`ui::bar_visible`).
- Keybindings: Cmd+T new tab, Cmd+W close active, Cmd+Shift+P open palette,
  Cmd+, open the config file in the OS's editor (the macOS app menu's
  "Settings…" row and palette `config.open` do the same; a missing file is
  seeded from docs/config.example.toml first),
  Cmd+\ split right (move the active tab into a new group),
  Cmd+Alt+Left/Up and Cmd+Alt+Right/Down focus the previous/next leaf in DFS
  order (order-based, not spatial). Tab-scoped bindings act on the *focused
  group*: Cmd+1..9 select nth in its bar, Cmd+Shift+[ / ] cycle its bar.
- Palette actions (ids): `tab.new`, `tab.new.<name>`
  (one per config profile, label "New Tab: <name>", opens in the focused
  group), `tab.close`, `tab.rename` (opens prompt
  mode, prompt id "rename"), `tab.next`, `tab.prev`, `tab.select.<id>` (one per
  open tab across all groups, label = title, prefixed with the group ordinal —
  "2: htop" — when there is more than one group), `split.right`, `split.left`,
  `split.down`, `split.up`, `group.next`, `group.prev`, `app.quit`,
  `config.edit.<slug>` (one per installed agent/editor — `claude`, `codex`,
  `vscode`, `cursor`).
- "Edit Settings With" (`edit_tools.rs`): probes once at launch, on a
  background thread, for `claude`/`codex`/`code`/`cursor` on the *login
  shell's* PATH (a Finder-launched app has launchd's, which has none of them)
  plus `~/.local/bin`, `~/bin`, `/opt/homebrew/bin`, `/usr/local/bin`; on
  macOS an editor also counts if LaunchServices knows its application bundle
  (`open -Ra`), so a user without the `code` shim still gets the row. An agent
  row opens a `config · <cli>` tab running the CLI with one positional prompt
  (`edit_tools::EDIT_PROMPT`, the same sentence for both); an editor row just
  opens the file. Rows appear in the palette and in the macOS application
  menu's "Edit Settings With ▸" submenu.
- macOS application menu (`macos::install_app_menu`): winit installs none, so
  terra builds its own — About, Settings… (Cmd+,), "Edit Settings With ▸",
  Hide/Show All, Quit. Built on the first frame after the tool probe lands
  (its contents depend on it). Rows carry a tag; `macos::take_menu_actions`
  drains the choices on the next frame, because AppKit dispatches between
  frames. Only the application menu exists: every extra key equivalent is a
  key the terminal stops receiving.
- `TERRA_NO_ACTIVATE=1` starts the window without stealing focus (winit's
  `with_activate_ignoring_other_apps(false)`; the activation policy stays
  `Regular`, since an `Accessory` app owns no menu bar). `just run`/`just
  restart` set it — a dev instance opening on top of what you were typing in
  is the single most disruptive thing about working on terra.
- Drag & drop: a pill dragged within a bar reorders that bar; dropped on
  another group's bar it *moves* there (slot under the cursor, becomes that
  group's active tab); dropped on a terminal it splits — four drop zones per
  leaf, whichever edge is proportionally nearest wins: left/right make a new
  side-by-side leaf, top/bottom a stacked one, each shown as the same
  translucent half-overlay. Dragging the hairline divider between two
  siblings resizes them (rewrites the two weights in their split; the other
  children keep their share).
- IPC server (`ipc.rs`): thread with an `interprocess::local_socket::Listener`
  on `terra_protocol::socket_address()` — a unix socket on Unix, a named pipe
  on Windows (create parent dir 0700 where there is one; reclaim a stale
  socket only after probing that nobody answers; remove on exit). A single
  atomic instance claim lives here too, so a second launch focuses the first
  and exits.
  Connection threads execute requests directly against the shared
  `Arc<Mutex<TabManager>>` — never via the UI thread, which eframe parks
  entirely while the window is occluded. Repaint is requested after mutating
  requests; `Select` also summons the window (thread-safe NSRunningApplication;
  never activate from inside the frame callback — it wedges winit's waker).
- Capture: `backend.sync()` then walk `last_content().grid.display_iter()`,
  build lines for the visible screen; include up to `scrollback` lines above
  via grid indexing if feasible, else visible-only is acceptable for v1.
  Trim trailing whitespace / blank tail lines.
- Transcripts (`transcript.rs`): a bounded per-tab ring of the raw bytes the
  child wrote, so `terra transcript` can read back what a full-screen program
  painted — output that exists nowhere else, since the alternate screen has no
  scrollback and is discarded when the program leaves it. The bytes are tapped
  in the one place they pass through: `BackendSettings.output_tap`, a terra
  patch on vendored egui_term that wraps alacritty's PTY (`backend/tap.rs`) and
  hands every chunk to a closure before the parser sees it. `Ring` is a plain
  byte ring — push, snapshot, overwrite oldest — that allocates on the first
  push rather than at tab creation, and `render` strips escape sequences back
  out. **In memory only**: never written to disk, dies with the tab. Sized by
  `[tabs] transcript_kb` (0 = no ring, no tap, nothing copied), mirrored onto
  the `TabManager` like the profile table and read when a tab is opened, so a
  reload sizes the *next* tab rather than discarding an open one's history.
  `render` is a stripper, not a terminal: cursor motion is not replayed, so a
  repainting program leaves one copy per frame — the history, not the screen.
- Send: `process_command(BackendCommand::Write(text.into_bytes()))`, append
  `\r` if `enter`. With `keys`, the text is first parsed by
  `terra-protocol::keys` into `Bytes`/`Delay` chunks and written in order.
- Config: `~/.terra/config.toml` (`TERRA_CONFIG` overrides), read once at
  startup into `config.rs`. `[font] size, line_height`, `[text] bidi,
  bidi_base, [text.bidi_quirks]`, `[tabs] icons, bar_with_one_tab,
  transcript_kb`, `[profile.<name>] command, cwd, title`.
  Loading never fails — see `docs/config.example.toml`. Profiles are
  deserialized one at a time, so one broken profile is skipped with a warning
  rather than costing the rest; `command` is a string and is split into argv,
  so a profile reaches `TabManager::open` in exactly the shape `terra new --
  cmd` does. The resolved table is mirrored onto the `TabManager` (`main.rs`
  pushes it at startup and on reload) because both consumers are off the UI
  thread's path: the IPC threads answering `--profile`, and the tab bar, which
  is drawn from a `&TabManager` alone.

## terra CLI (tmux-flavored)

```
terra ls                          # table: id  active  title
terra new [--title T] [--cwd D] [--profile P] [-- cmd args...]  # prints new tab id
terra kill <tab>
terra send <tab> "text" [--keys] [--enter]  # --enter appends CR (like tmux send-keys ... Enter)
terra capture <tab> [--scrollback N] [--cells]  # text, or the styled grid as JSON
terra transcript <tab> [--tail N] [--raw]  # what the tab's program wrote, alt screen included
terra rename <tab> "new title"
terra select <tab>
terra screenshot --out F [--pretty] [--bg hex1,hex2]  # PNG of the window
terra bidi <tab> [off|on|auto]    # per-tab RTL reordering; prints the mode
terra learn                       # self-teaching prompt for agents
terra doctor                      # probe the terminal this CLI runs inside
terra record --out F -- cmd | terra record --decode F   # both I/O directions
terra --json ...                  # raw JSON Response on stdout for scripting
```

Exit codes: 0 ok, 1 error (message on stderr). `<tab>` is the numeric id from `ls`.

`--profile P` opens the tab a `[profile.P]` section describes. The *app*
resolves the name, not the CLI: the config belongs to the app, and the CLI may
be on the far end of an ssh-forwarded socket with no config file at all. An
unknown name comes back as an error listing the profiles that do exist, and
`--profile` with a trailing `-- cmd` is refused by clap, because a profile
already is the command. `--title`/`--cwd` still override the profile's own.
On the wire this is one added optional `profile` field on `Request::New`,
`skip_serializing_if = "Option::is_none"` — a request that names no profile
serialises byte-identically to what it always did, and one written by an older
client still parses.

`--keys` opts the text into the key notation in `terra-protocol::keys`
(`{Enter}`, `{C-c}`, `{S-Tab}`, `{F5}`, `{M-b}`, `{Delay 300}`, `{{` for a
literal brace); it is opt-in because `{Home}` and `${HOME}` collide, and text
without it reaches the PTY byte for byte. `--cells` returns run-length-encoded
runs carrying `fg`/`bg`/`flags` plus the cursor, with colours left as the
program named them (`{"indexed":236}`, `{"named":"Background"}`, `"#3a3a3a"`)
rather than resolved against the theme.

`transcript` adds one request (`{"cmd":"transcript","tab":N}`, with `tail` and
`raw` skipped when unset) and one additive `bytes` field on `Response::Ok` —
base64, for the `--raw` payload, which is neither newline-free nor guaranteed
valid UTF-8. Rendering happens app-side so `--tail` costs one round trip and
the escape-stripper has a single implementation; `--tail N` is lines of the
rendered form but *bytes* of `--raw`, since a full-screen program can repaint
for minutes without emitting a newline. A tab whose transcript is switched off
answers with an error naming `[tabs] transcript_kb`, never with silence.

`screenshot` is the only request answered *by the UI thread*: the pixels exist
because a frame was drawn. The IPC thread summons the window (the same
`activate_app` + `Focus` that `select` uses), posts
`ViewportCommand::Screenshot`, and blocks on a rendezvous
(`terra-app/src/screenshot.rs`) until the frame's `Event::Screenshot` reaches
`App::ui`, or 2s pass — an occluded window that will not come forward has to
fail with a message rather than hang. The app encodes PNG; the CLI decodes it
and, for `--pretty`, composites a rounded card, traffic lights, drop shadow and
gradient in pure pixel arithmetic (`terra-cli/src/pretty.rs`).

`doctor` and `record` never open the socket: they talk to the terminal the CLI
is running inside, so the same binary run under terra and under any other
terminal produces two outputs that diff.

## Conventions

- Rust 2021, `cargo fmt` defaults, no new heavyweight deps without need.
- Everything must pass `cargo build` + `cargo clippy` warnings-free-ish.
- macOS is the target platform for v1.
