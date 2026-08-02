//! Tab model: one PTY-backed terminal per tab, owned by a [`TabManager`].

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Line;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::{Color, NamedColor};
use egui_term::{BackendCommand, BackendSettings, PtyEvent, TerminalBackend};
use serde::Serialize;
use terra_protocol::TabInfo;

use crate::config::BidiMode;

/// Fallback when `$SHELL` is not set.
const FALLBACK_SHELL: &str = "/bin/zsh";

/// Quote one argument for POSIX shells: safe single-quote wrapping.
fn shell_quote(arg: &str) -> String {
    if !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c))
    {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| FALLBACK_SHELL.to_string())
}

/// Arguments that make the shell a *login* shell.
///
/// Launched from Finder, an app inherits launchd's minimal environment, not
/// the one a terminal window gets. The PATH additions people actually depend
/// on — Homebrew, nvm, pyenv, cargo — are conventionally set in `.zprofile`
/// or `.profile`, which only a login shell reads; `.zshrc` alone is not
/// enough, and a `.zshrc` that *uses* those tools then fails outright with
/// "command not found".
///
/// `-l` covers zsh, bash and sh. A shell that does not understand it is no
/// worse off than before, because the flag is simply rejected and the user
/// still gets a shell.
fn login_args(shell: &str) -> Vec<String> {
    let name = shell.rsplit('/').next().unwrap_or(shell);
    match name {
        "zsh" | "bash" | "sh" | "dash" | "ksh" => vec!["-l".to_string()],
        _ => Vec::new(),
    }
}

/// Where a tab starts when the caller did not say.
///
/// Also a launched-from-Finder problem: the app's own working directory is
/// `/`, so without this every tab opens at the filesystem root rather than
/// somewhere anyone wants to be.
fn default_cwd() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The two title sources of a tab, kept apart on purpose.
///
/// The shell keeps pushing OSC titles (zsh reports the cwd), so `shell` moves
/// on its own. `custom` is only ever written by an explicit user action —
/// palette rename, `terra rename`, `terra new --title` — and, once set, wins
/// forever: shell updates keep landing in `shell` but stop being displayed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Title {
    /// Title reported by the shell via OSC (`PtyEvent::Title`).
    pub shell: String,
    /// User-set title; overrides `shell` when present.
    pub custom: Option<String>,
}

impl Title {
    /// Effective title = custom title, else the shell-reported one shortened to
    /// its path part (see [`strip_user_host`]).
    pub fn effective(&self) -> &str {
        match self.custom.as_deref() {
            Some(custom) => custom,
            None => strip_user_host(&self.shell),
        }
    }
}

/// Drop a leading `user@host:` from a shell-reported title, Ghostty-style, so
/// the bar shows `~/Documents/terra` rather than
/// `yqbqwlny@MacBook-Pro-sl-yqb:~/Documents/terra`.
///
/// Deliberately conservative: the part before the first `:` must look exactly
/// like `user@host` — one `@`, no whitespace, nothing empty — and something has
/// to be left after the `:`. Anything else is returned untouched, so titles that
/// merely contain a colon (`make: build`) or an `@` keep their full text.
fn strip_user_host(title: &str) -> &str {
    let Some((prefix, rest)) = title.split_once(':') else {
        return title;
    };
    if rest.is_empty() || prefix.contains(char::is_whitespace) {
        return title;
    }
    match prefix.split_once('@') {
        Some((user, host)) if !user.is_empty() && !host.is_empty() && !host.contains('@') => rest,
        _ => title,
    }
}

pub struct Tab {
    pub backend: TerminalBackend,
    pub title: Title,
    /// Per-tab override of `[text] bidi`, set from the palette or
    /// `terra bidi`. `None` means "follow the config".
    ///
    /// It has to be per tab: whether the terminal should reorder depends on
    /// which program is running, and one window routinely has a shell in one
    /// tab and an agent that does its own BiDi in another.
    pub bidi: Option<BidiMode>,
}

impl Tab {
    /// Effective title = custom title, else the shell-reported one.
    pub fn effective_title(&self) -> &str {
        self.title.effective()
    }
}

pub struct TabManager {
    tabs: BTreeMap<u64, Tab>,
    /// Visual left-to-right order of [`Self::tabs`]; the single source of truth
    /// for the bar, `⌘1..9` and next/prev. Ids are appended on open and removed
    /// on close, so it always holds exactly the keys of `tabs`.
    ///
    /// Behind a `RefCell` because the tab bar reorders it while holding only a
    /// `&TabManager` (the bar is drawn from an immutable borrow, mid-frame,
    /// while a tab is dragged). Nothing else in the manager is shared, so the
    /// borrows are short and strictly local to the ordering methods.
    order: RefCell<Vec<u64>>,
    active: Option<u64>,
    next_id: u64,
    ctx: egui::Context,
    pty_events: Sender<(u64, PtyEvent)>,
}

impl TabManager {
    pub fn new(ctx: egui::Context, pty_events: Sender<(u64, PtyEvent)>) -> Self {
        Self {
            tabs: BTreeMap::new(),
            order: RefCell::new(Vec::new()),
            active: None,
            next_id: 0,
            ctx,
            pty_events,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// All tab ids in visual order.
    pub fn ids(&self) -> Vec<u64> {
        self.order.borrow().clone()
    }

    /// Position of `id` in the visual order.
    pub fn index_of(&self, id: u64) -> Option<usize> {
        self.order.borrow().iter().position(|i| *i == id)
    }

    /// Move a tab to `new_idx` in the visual order, shifting the rest along.
    /// Indices past the end clamp to the last slot. Returns whether anything moved.
    pub fn move_tab(&self, id: u64, new_idx: usize) -> bool {
        let mut order = self.order.borrow_mut();
        let Some(from) = order.iter().position(|i| *i == id) else {
            return false;
        };
        let to = new_idx.min(order.len() - 1);
        if from == to {
            return false;
        }
        order.remove(from);
        order.insert(to, id);
        true
    }

    /// [`Self::move_tab`] addressed by current position instead of id — the
    /// index-based half of the reorder API, used by the tests and by any caller
    /// that thinks in slots rather than tabs.
    #[cfg(test)]
    pub fn reorder(&self, from_idx: usize, to_idx: usize) -> bool {
        let id = match self.order.borrow().get(from_idx) {
            Some(id) => *id,
            None => return false,
        };
        self.move_tab(id, to_idx)
    }

    pub fn active_id(&self) -> Option<u64> {
        self.active
    }

    pub fn active_mut(&mut self) -> Option<&mut Tab> {
        let id = self.active?;
        self.tabs.get_mut(&id)
    }

    pub fn title(&self, id: u64) -> Option<&str> {
        self.tabs.get(&id).map(|t| t.effective_title())
    }

    /// Tab descriptions in visual order.
    pub fn infos(&self) -> Vec<TabInfo> {
        self.order
            .borrow()
            .iter()
            .filter_map(|id| {
                let tab = self.tabs.get(id)?;
                Some(TabInfo {
                    id: *id,
                    title: tab.effective_title().to_string(),
                    active: Some(*id) == self.active,
                })
            })
            .collect()
    }

    /// Spawn a new tab. `command` empty -> the user's `$SHELL`.
    /// The new tab becomes active.
    pub fn open(
        &mut self,
        command: &[String],
        cwd: Option<&str>,
        title: Option<String>,
    ) -> anyhow::Result<u64> {
        // A command tab is just a default-shell tab with the command typed
        // into it (tmux send-keys style): the user's real prompt renders,
        // the command line is visible, and the shell survives after it.
        let shell = default_shell();
        let args = login_args(&shell);

        let id = self.next_id;
        let mut backend = TerminalBackend::new(
            id,
            self.ctx.clone(),
            self.pty_events.clone(),
            BackendSettings {
                shell,
                args,
                working_directory: cwd.map(PathBuf::from).or_else(default_cwd),
            },
        )?;
        // Answer colour queries from the very first byte the shell writes.
        //
        // Programs ask what the terminal's colours are while they start up,
        // which for a tab opened in the background is long before it is ever
        // drawn — so this cannot wait for the first frame, or the query goes
        // unanswered and the program falls back to unstyled output.
        backend.set_reported_colors(crate::terminal_theme().reported_colors());
        if !command.is_empty() {
            let typed = command
                .iter()
                .map(|a| shell_quote(a))
                .collect::<Vec<_>>()
                .join(" ");
            backend.process_command(egui_term::BackendCommand::Write(
                format!("{typed}\r").into_bytes(),
            ));
        }
        self.next_id += 1;

        self.tabs.insert(
            id,
            Tab {
                backend,
                bidi: None,
                title: Title {
                    shell: format!("terra {id}"),
                    custom: title,
                },
            },
        );
        self.order.borrow_mut().push(id);
        self.active = Some(id);
        self.sync_visibility();
        Ok(id)
    }

    /// Remove a tab (dropping its backend shuts the PTY down).
    pub fn close(&mut self, id: u64) -> bool {
        if self.tabs.remove(&id).is_none() {
            return false;
        }
        let mut order = self.order.borrow_mut();
        let removed = order.iter().position(|i| *i == id);
        if let Some(idx) = removed {
            order.remove(idx);
        }
        if self.active == Some(id) {
            // Prefer the nearest tab to the left, else the first remaining one.
            self.active = removed
                .and_then(|idx| order.get(idx.saturating_sub(1)))
                .copied();
        }
        self.sync_visibility();
        true
    }

    pub fn close_active(&mut self) {
        if let Some(id) = self.active {
            self.close(id);
        }
    }

    pub fn clear(&mut self) {
        self.tabs.clear();
        self.order.borrow_mut().clear();
        self.active = None;
    }

    fn sync_visibility(&self) {
        let active = self.active;
        for (id, tab) in &self.tabs {
            tab.backend.set_visible(Some(*id) == active);
        }
    }

    pub fn select(&mut self, id: u64) -> bool {
        if self.tabs.contains_key(&id) {
            self.active = Some(id);
            true
        } else {
            false
        }
    }

    /// Select the nth tab (0-based) in bar order.
    pub fn select_nth(&mut self, n: usize) {
        if let Some(id) = self.order.borrow().get(n).copied() {
            self.active = Some(id);
        }
    }

    pub fn select_next(&mut self) {
        self.step(1);
    }

    pub fn select_prev(&mut self) {
        self.step(-1);
    }

    fn step(&mut self, delta: isize) {
        if self.tabs.is_empty() {
            return;
        }
        let ids = self.ids();
        let current = self
            .active
            .and_then(|id| ids.iter().position(|i| *i == id))
            .unwrap_or(0) as isize;
        let len = ids.len() as isize;
        let next = (current + delta).rem_euclid(len) as usize;
        self.active = Some(ids[next]);
    }

    /// Record what the shell reports. Never clobbers a custom title: it only
    /// writes the shell slot, which a custom title shadows.
    pub fn set_shell_title(&mut self, id: u64, title: String) {
        if let Some(tab) = self.tabs.get_mut(&id) {
            tab.title.shell = title;
        }
    }

    /// Pin a user-chosen title. From here on the shell can no longer change
    /// what this tab displays.
    /// The tab's BiDi override, or `None` when it follows the config.
    pub fn bidi(&self, id: u64) -> Option<Option<BidiMode>> {
        self.tabs.get(&id).map(|t| t.bidi)
    }

    /// Override a tab's BiDi mode. `None` returns it to the config value.
    pub fn set_bidi(&mut self, id: u64, mode: Option<BidiMode>) -> bool {
        match self.tabs.get_mut(&id) {
            Some(tab) => {
                tab.bidi = mode;
                true
            }
            None => false,
        }
    }

    /// The pid of the tab's shell, for looking up what is running in it.
    pub fn shell_pid(&self, id: u64) -> Option<u32> {
        self.tabs.get(&id).map(|t| t.backend.pty_id())
    }

    pub fn set_custom_title(&mut self, id: u64, title: String) -> bool {
        match self.tabs.get_mut(&id) {
            Some(tab) => {
                tab.title.custom = Some(title);
                true
            }
            None => false,
        }
    }

    /// Write `text` to the tab's PTY, appending CR when `enter` is set.
    pub fn send(&mut self, id: u64, text: &str, enter: bool) -> bool {
        let Some(tab) = self.tabs.get_mut(&id) else {
            return false;
        };
        let mut bytes = text.as_bytes().to_vec();
        if enter {
            bytes.push(b'\r');
        }
        tab.backend.process_command(BackendCommand::Write(bytes));
        self.sync_visibility();
        true
    }

    /// Plain-text dump of the visible screen plus up to `scrollback` lines above it.
    pub fn capture(&mut self, id: u64, scrollback: usize) -> Option<String> {
        let tab = self.tabs.get_mut(&id)?;
        let grid = &tab.backend.sync().grid;

        let display_offset = grid.display_offset() as i32;
        let screen_lines = grid.screen_lines() as i32;
        let history = grid.history_size() as i32;
        let scrollback = scrollback.min(i32::MAX as usize) as i32;

        let top_visible = -display_offset;
        let bottom_visible = top_visible + screen_lines - 1;
        let start = (top_visible - scrollback).max(-history);

        let mut lines: Vec<String> = Vec::new();
        for line in start..=bottom_visible {
            let mut text = String::new();
            for cell in &grid[Line(line)] {
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                text.push(cell.c);
            }
            while text.ends_with(' ') {
                text.pop();
            }
            lines.push(text);
        }

        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        Some(lines.join("\n"))
    }

    /// The visible grid with styling, as a JSON string.
    ///
    /// Same rows as [`Self::capture`] — the viewport plus up to `scrollback`
    /// lines above it — but every cell's colours and attributes come along,
    /// run-length encoded by style. See [`GridDump`] for the shape.
    pub fn capture_cells(&mut self, id: u64, scrollback: usize) -> Option<String> {
        let tab = self.tabs.get_mut(&id)?;
        let content = tab.backend.sync();
        let grid = &content.grid;

        let display_offset = grid.display_offset() as i32;
        let screen_lines = grid.screen_lines() as i32;
        let history = grid.history_size() as i32;
        let scrollback = scrollback.min(i32::MAX as usize) as i32;

        let top_visible = -display_offset;
        let bottom_visible = top_visible + screen_lines - 1;
        let start = (top_visible - scrollback).max(-history);

        let rows_data = (start..=bottom_visible)
            .map(|line| RowDump {
                y: viewport_row(line, display_offset),
                runs: encode_row(grid[Line(line)].into_iter()),
            })
            .collect();

        let dump = GridDump {
            cols: grid.columns(),
            rows: grid.screen_lines(),
            cursor: CursorDump {
                row: viewport_row(grid.cursor.point.line.0, display_offset),
                col: grid.cursor.point.column.0,
                visible: content.terminal_mode.contains(TermMode::SHOW_CURSOR),
            },
            rows_data,
        };
        serde_json::to_string(&dump).ok()
    }
}

/// A styled dump of the grid, as returned by [`TabManager::capture_cells`].
///
/// ```json
/// {
///   "cols": 120,
///   "rows": 40,
///   "cursor": {"row": 38, "col": 4, "visible": true},
///   "rows_data": [
///     {"y": 0, "runs": [
///       {"x": 0, "text": "> hello", "fg": {"named": "Foreground"},
///        "bg": {"named": "Background"}},
///       {"x": 7, "text": "  ", "fg": {"named": "Foreground"},
///        "bg": {"indexed": 236}, "flags": ["INVERSE"]}
///     ]}
///   ]
/// }
/// ```
///
/// `row`/`y` are viewport coordinates: 0 is the topmost visible row, exactly
/// the space `view.rs` computes its `line_num` in, so scrollback rows are
/// negative.
#[derive(Serialize)]
struct GridDump {
    cols: usize,
    rows: usize,
    cursor: CursorDump,
    rows_data: Vec<RowDump>,
}

#[derive(Serialize)]
struct CursorDump {
    row: i32,
    col: usize,
    visible: bool,
}

#[derive(Serialize)]
struct RowDump {
    y: i32,
    runs: Vec<Run>,
}

/// A maximal span of adjacent cells sharing one style, starting at column `x`.
#[derive(Serialize)]
struct Run {
    x: usize,
    text: String,
    fg: ColorDump,
    bg: ColorDump,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    flags: Vec<&'static str>,
}

/// A colour exactly as the application asked for it — deliberately *not*
/// resolved against the theme.
///
/// `{"named": "Background"}`, `{"indexed": 236}` or `"#3a3a3a"`. Resolving to
/// RGB would bake the current theme into the dump and destroy its whole point:
/// "this row asked for indexed 236" is the fact worth diffing, and it survives
/// a theme change.
#[derive(Serialize)]
#[serde(untagged)]
enum ColorDump {
    Named { named: String },
    Indexed { indexed: u8 },
    Spec(String),
}

impl From<Color> for ColorDump {
    fn from(color: Color) -> Self {
        match color {
            // `NamedColor` exposes no name accessor, but its variants are
            // fieldless, so the derived `Debug` output *is* the name.
            Color::Named(named) => ColorDump::Named {
                named: format!("{named:?}"),
            },
            Color::Indexed(indexed) => ColorDump::Indexed { indexed },
            Color::Spec(rgb) => {
                ColorDump::Spec(format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b))
            }
        }
    }
}

/// Grid line -> viewport row, the same arithmetic `view.rs` does for `line_num`.
fn viewport_row(line: i32, display_offset: i32) -> i32 {
    line + display_offset
}

/// Every set [`Flags`] bit, uppercase, in declaration order.
fn flag_names(flags: Flags) -> Vec<&'static str> {
    flags.iter_names().map(|(name, _)| name).collect()
}

/// Is this a cell nothing was ever written to? Trailing runs of these are
/// dropped so a blank row costs `"runs": []`.
fn is_default(cell: &Cell) -> bool {
    cell.c == ' '
        && cell.flags.is_empty()
        && matches!(cell.fg, Color::Named(NamedColor::Foreground))
        && matches!(cell.bg, Color::Named(NamedColor::Background))
}

/// Run-length encode one row by style.
///
/// Adjacent cells join a run while `fg`, `bg` and `flags` all match; any
/// difference starts a new one. Wide-character spacers contribute their column
/// to `x` but no character to `text` — the glyph is already there from the cell
/// before them. Trailing default cells are dropped entirely.
fn encode_row<'a>(cells: impl Iterator<Item = &'a Cell>) -> Vec<Run> {
    let cells: Vec<&Cell> = cells.collect();
    let end = cells
        .iter()
        .rposition(|cell| !is_default(cell))
        .map_or(0, |i| i + 1);

    let mut runs: Vec<Run> = Vec::new();
    let mut style: Option<(Color, Color, Flags)> = None;
    for (x, cell) in cells[..end].iter().enumerate() {
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let here = (cell.fg, cell.bg, cell.flags);
        if style == Some(here) {
            if let Some(run) = runs.last_mut() {
                run.text.push(cell.c);
                continue;
            }
        }
        runs.push(Run {
            x,
            text: cell.c.to_string(),
            fg: cell.fg.into(),
            bg: cell.bg.into(),
            flags: flag_names(cell.flags),
        });
        style = Some(here);
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tab with no custom title mirrors whatever the shell reports (zsh
    /// pushes the cwd, so this is how the bar tracks directories).
    #[test]
    fn shell_title_drives_the_displayed_title() {
        let mut title = Title {
            shell: "terra 0".to_string(),
            custom: None,
        };
        assert_eq!(title.effective(), "terra 0");

        title.shell = "~/src/terra".to_string();
        assert_eq!(title.effective(), "~/src/terra");

        title.shell = "~/src/terra/crates".to_string();
        assert_eq!(title.effective(), "~/src/terra/crates");
    }

    /// Once renamed, the tab keeps that name for good: the shell keeps
    /// reporting, but nothing it says is displayed again.
    #[test]
    fn a_custom_title_wins_over_later_shell_titles() {
        let mut title = Title {
            shell: "~/src/terra".to_string(),
            custom: None,
        };

        title.custom = Some("build".to_string());
        assert_eq!(title.effective(), "build");

        title.shell = "~/elsewhere".to_string();
        assert_eq!(title.effective(), "build");
        // The shell slot is still updated underneath, just shadowed.
        assert_eq!(title.shell, "~/elsewhere");
    }

    /// `terra new --title T` pins the title from birth, same as a rename.
    #[test]
    fn a_title_given_at_open_is_custom() {
        let mut title = Title {
            shell: "terra 3".to_string(),
            custom: Some("logs".to_string()),
        };
        title.shell = "~/var/log".to_string();
        assert_eq!(title.effective(), "logs");
    }

    /// The same two rules, exercised through `TabManager` with a real PTY.
    #[test]
    fn tab_manager_title_sync() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut tabs = TabManager::new(egui::Context::default(), tx);

        let plain = tabs
            .open(&["/bin/cat".to_string()], None, None)
            .expect("spawn /bin/cat");
        let named = tabs
            .open(&["/bin/cat".to_string()], None, Some("pinned".to_string()))
            .expect("spawn /bin/cat");

        // Shell updates flow through to the displayed title...
        tabs.set_shell_title(plain, "~/src/terra".to_string());
        assert_eq!(tabs.title(plain), Some("~/src/terra"));

        // ...until a custom title is set, after which they are ignored.
        assert!(tabs.set_custom_title(plain, "build".to_string()));
        tabs.set_shell_title(plain, "~/elsewhere".to_string());
        assert_eq!(tabs.title(plain), Some("build"));

        // A title passed to `open` behaves exactly like a rename.
        tabs.set_shell_title(named, "~/var/log".to_string());
        assert_eq!(tabs.title(named), Some("pinned"));

        tabs.clear();
    }

    /// zsh reports `user@host:path`; only the path is worth showing.
    #[test]
    fn a_user_host_prefix_is_stripped_from_shell_titles() {
        let title = Title {
            shell: "yqbqwlny@MacBook-Pro-sl-yqb:~/Documents/terra".to_string(),
            custom: None,
        };
        assert_eq!(title.effective(), "~/Documents/terra");
    }

    /// Anything that is not exactly `user@host:rest` is left alone.
    #[test]
    fn titles_without_the_pattern_are_untouched() {
        let plain = |shell: &str| Title {
            shell: shell.to_string(),
            custom: None,
        };
        assert_eq!(plain("~/src/terra").effective(), "~/src/terra");
        assert_eq!(plain("make: build").effective(), "make: build");
        assert_eq!(plain("mail@example.com").effective(), "mail@example.com");
        // Two `@` before the colon, an empty user, an empty tail: all bail out.
        assert_eq!(plain("a@b@c:~/x").effective(), "a@b@c:~/x");
        assert_eq!(plain("@host:~/x").effective(), "@host:~/x");
        assert_eq!(plain("user@host:").effective(), "user@host:");
    }

    /// A user-chosen title is shown verbatim, even if it looks like `user@host:`.
    #[test]
    fn custom_titles_are_never_shortened() {
        let title = Title {
            shell: "yqbqwlny@MacBook-Pro-sl-yqb:~/Documents/terra".to_string(),
            custom: Some("root@box:/etc".to_string()),
        };
        assert_eq!(title.effective(), "root@box:/etc");
    }

    /// Tabs live in an explicit order: new ones land at the end, and `ids`,
    /// `infos` and `⌘n` all read that order rather than the id order.
    #[test]
    fn tabs_keep_an_explicit_visual_order() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut tabs = TabManager::new(egui::Context::default(), tx);
        let ids: Vec<u64> = (0..3)
            .map(|_| {
                tabs.open(&["/bin/cat".to_string()], None, None)
                    .expect("spawn /bin/cat")
            })
            .collect();
        assert_eq!(tabs.ids(), ids);

        // Drag the last tab to the front.
        assert!(tabs.move_tab(ids[2], 0));
        assert_eq!(tabs.ids(), vec![ids[2], ids[0], ids[1]]);
        assert_eq!(tabs.index_of(ids[2]), Some(0));
        assert_eq!(
            tabs.infos().iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![ids[2], ids[0], ids[1]]
        );

        // ⌘1..9 and next/prev follow the visual order, not the ids.
        tabs.select_nth(0);
        assert_eq!(tabs.active_id(), Some(ids[2]));
        tabs.select_next();
        assert_eq!(tabs.active_id(), Some(ids[0]));
        tabs.select_prev();
        assert_eq!(tabs.active_id(), Some(ids[2]));
        tabs.select_prev();
        assert_eq!(tabs.active_id(), Some(ids[1]));

        tabs.clear();
        assert!(tabs.ids().is_empty());
    }

    /// Reordering is total and stable: no id is lost or duplicated, out-of-range
    /// targets clamp, and unknown ids or no-op moves report `false`.
    #[test]
    fn reordering_preserves_every_tab() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut tabs = TabManager::new(egui::Context::default(), tx);
        let ids: Vec<u64> = (0..4)
            .map(|_| {
                tabs.open(&["/bin/cat".to_string()], None, None)
                    .expect("spawn /bin/cat")
            })
            .collect();

        assert!(tabs.reorder(0, 3));
        assert_eq!(tabs.ids(), vec![ids[1], ids[2], ids[3], ids[0]]);
        // Past the end clamps to the last slot instead of panicking.
        assert!(tabs.move_tab(ids[1], 99));
        assert_eq!(tabs.ids(), vec![ids[2], ids[3], ids[0], ids[1]]);
        assert!(!tabs.move_tab(ids[1], 3));
        assert!(!tabs.move_tab(u64::MAX, 0));
        assert!(!tabs.reorder(9, 0));

        let mut sorted = tabs.ids();
        sorted.sort_unstable();
        assert_eq!(sorted, ids);

        tabs.clear();
    }

    /// Closing keeps the order intact and hands focus to the left neighbour in
    /// *visual* order — after a drag that is not the neighbour by id.
    #[test]
    fn closing_a_tab_removes_it_from_the_order() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut tabs = TabManager::new(egui::Context::default(), tx);
        let ids: Vec<u64> = (0..3)
            .map(|_| {
                tabs.open(&["/bin/cat".to_string()], None, None)
                    .expect("spawn /bin/cat")
            })
            .collect();

        // Order: [2, 0, 1], active = 2.
        assert!(tabs.move_tab(ids[2], 0));
        tabs.select(ids[0]);
        assert!(tabs.close(ids[0]));
        assert_eq!(tabs.ids(), vec![ids[2], ids[1]]);
        assert_eq!(tabs.active_id(), Some(ids[2]));

        // Closing the leftmost falls forward to the new leftmost.
        assert!(tabs.close(ids[2]));
        assert_eq!(tabs.ids(), vec![ids[1]]);
        assert_eq!(tabs.active_id(), Some(ids[1]));

        assert!(tabs.close(ids[1]));
        assert!(tabs.ids().is_empty());
        assert_eq!(tabs.active_id(), None);
        assert!(!tabs.close(ids[1]));
        assert!(tabs.is_empty());
    }

    /// A cell carrying `c` in the default style.
    fn cell(c: char) -> Cell {
        Cell {
            c,
            ..Default::default()
        }
    }

    /// The row `text` in the default style, padded with default cells to
    /// `cols` — what an untouched grid row of that width looks like.
    fn row(text: &str, cols: usize) -> Vec<Cell> {
        let mut cells: Vec<Cell> = text.chars().map(cell).collect();
        cells.resize_with(cols, Cell::default);
        cells
    }

    /// The whole point of the format: uniform text costs one run, not one
    /// entry per column.
    #[test]
    fn adjacent_cells_with_the_same_style_merge_into_one_run() {
        let runs = encode_row(row("hello", 20).iter());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].x, 0);
        assert_eq!(runs[0].text, "hello");
        assert!(runs[0].flags.is_empty());
    }

    /// Any of the three style axes splits a run, which is what makes "is that
    /// row's background actually gray?" a `jq` query.
    #[test]
    fn a_style_change_starts_a_new_run() {
        let split = |mutate: &dyn Fn(&mut Cell)| {
            let mut cells = row("abcd", 10);
            mutate(&mut cells[2]);
            encode_row(cells.iter())
        };

        let runs = split(&|c| c.fg = Color::Indexed(9));
        assert_eq!(runs.len(), 3);
        assert_eq!((runs[0].x, runs[0].text.as_str()), (0, "ab"));
        assert_eq!((runs[1].x, runs[1].text.as_str()), (2, "c"));
        assert_eq!((runs[2].x, runs[2].text.as_str()), (3, "d"));

        let runs = split(&|c| c.bg = Color::Named(NamedColor::Red));
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[1].text, "c");

        let runs = split(&|c| c.flags = Flags::BOLD);
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[1].flags, vec!["BOLD"]);
        assert!(runs[0].flags.is_empty());
    }

    /// Blank rows are the common case in a mostly-empty screen; they must not
    /// cost a run each.
    #[test]
    fn a_blank_row_encodes_as_no_runs() {
        assert!(encode_row(row("", 80).iter()).is_empty());
        assert!(encode_row(row("   ", 80).iter()).is_empty());
    }

    /// Padding to the right edge is not content — but a styled trailing space
    /// (a highlighted row, say) is, and stays.
    #[test]
    fn trailing_default_cells_are_not_emitted() {
        let runs = encode_row(row("hi", 80).iter());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "hi");

        let mut cells = row("hi", 80);
        cells[40].bg = Color::Indexed(236);
        let runs = encode_row(cells.iter());
        // The padding up to the styled cell is real content now, and merges
        // into the leading run; only what follows the styled cell is dropped.
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, format!("hi{}", " ".repeat(38)));
        assert_eq!(runs[1].x, 40);
        assert_eq!(runs[1].text, " ");
    }

    /// `y` and the cursor share one space: the topmost visible row is 0, so
    /// scrollback is negative — exactly `view.rs`'s `line_num`.
    #[test]
    fn the_cursor_is_reported_in_viewport_coordinates() {
        // Scrolled to the bottom: grid line and viewport row coincide.
        assert_eq!(viewport_row(0, 0), 0);
        assert_eq!(viewport_row(38, 0), 38);
        // Scrolled up by 10: the cursor's grid line 38 is now 10 rows lower in
        // the viewport, and the rows above the viewport top go negative.
        assert_eq!(viewport_row(38, 10), 48);
        assert_eq!(viewport_row(-10, 10), 0);
        assert_eq!(viewport_row(-15, 10), -5);
    }

    /// A wide glyph occupies two columns; the second holds a spacer, whose
    /// blank must not land in the run's text and whose style must not split it.
    #[test]
    fn a_wide_character_and_its_spacer_are_not_double_counted() {
        let mut cells = row("a世 b", 10);
        cells[1].flags = Flags::WIDE_CHAR;
        cells[2].flags = Flags::WIDE_CHAR_SPACER;

        let runs = encode_row(cells.iter());
        assert_eq!(runs.len(), 3);
        assert_eq!((runs[0].x, runs[0].text.as_str()), (0, "a"));
        assert_eq!((runs[1].x, runs[1].text.as_str()), (1, "世"));
        assert_eq!(runs[1].flags, vec!["WIDE_CHAR"]);
        // The spacer's column is skipped entirely: `b` reports column 3.
        assert_eq!((runs[2].x, runs[2].text.as_str()), (3, "b"));
    }

    /// Colours travel unresolved, so a dump still says what the app asked for.
    #[test]
    fn colours_are_dumped_in_the_form_the_app_asked_for() {
        let named = serde_json::to_string(&ColorDump::from(Color::Named(NamedColor::Background)));
        assert_eq!(named.unwrap(), r#"{"named":"Background"}"#);
        let indexed = serde_json::to_string(&ColorDump::from(Color::Indexed(236)));
        assert_eq!(indexed.unwrap(), r#"{"indexed":236}"#);
        let rgb = alacritty_terminal::vte::ansi::Rgb {
            r: 58,
            g: 58,
            b: 58,
        };
        let spec = serde_json::to_string(&ColorDump::from(Color::Spec(rgb)));
        assert_eq!(spec.unwrap(), r##""#3a3a3a""##);
    }
}

#[cfg(test)]
mod launch_env_tests {
    use super::*;

    /// Launched from Finder the app inherits launchd's environment, so the
    /// shell has to be a login shell or none of the user's PATH setup runs.
    #[test]
    fn the_common_shells_are_started_as_login_shells() {
        for shell in ["/bin/zsh", "/bin/bash", "/bin/sh", "/opt/homebrew/bin/bash"] {
            assert_eq!(login_args(shell), vec!["-l".to_string()], "{shell}");
        }
    }

    /// A shell we do not recognise still gets a working session; we just do
    /// not guess a flag it may reject.
    #[test]
    fn an_unrecognised_shell_is_launched_without_extra_flags() {
        assert!(login_args("/usr/bin/fish").is_empty());
        assert!(login_args("/usr/local/bin/nu").is_empty());
    }

    /// An explicit cwd always wins; the fallback only fills in the blank.
    #[test]
    fn an_explicit_directory_is_never_overridden_by_the_default() {
        let explicit = Some(PathBuf::from("/tmp"));
        assert_eq!(explicit.clone().or_else(default_cwd), explicit);
    }
}
