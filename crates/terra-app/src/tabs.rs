//! Tab model: one PTY-backed terminal per tab, owned by a [`TabManager`].

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Line;
use alacritty_terminal::term::cell::Flags;
use egui_term::{BackendCommand, BackendSettings, PtyEvent, TerminalBackend};
use terra_protocol::TabInfo;

/// Fallback when `$SHELL` is not set.
const FALLBACK_SHELL: &str = "/bin/zsh";

fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| FALLBACK_SHELL.to_string())
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
        let (shell, args) = match command.split_first() {
            Some((program, rest)) => (program.clone(), rest.to_vec()),
            None => (default_shell(), Vec::new()),
        };

        let id = self.next_id;
        let backend = TerminalBackend::new(
            id,
            self.ctx.clone(),
            self.pty_events.clone(),
            BackendSettings {
                shell,
                args,
                working_directory: cwd.map(PathBuf::from),
            },
        )?;
        self.next_id += 1;

        self.tabs.insert(
            id,
            Tab {
                backend,
                title: Title {
                    shell: format!("terra {id}"),
                    custom: title,
                },
            },
        );
        self.order.borrow_mut().push(id);
        self.active = Some(id);
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
}
