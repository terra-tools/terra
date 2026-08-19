//! Tab model: one PTY-backed terminal per tab, owned by a [`TabManager`].

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Line;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::{Color, NamedColor};
use egui_term::{BackendCommand, BackendSettings, PtyEvent, TerminalBackend};
use serde::Serialize;
use terra_protocol::TabInfo;

use crate::config::{BidiMode, Profile};
use crate::transcript::{self, Ring};

/// Fallback when `$SHELL` is not set. Unix only — Windows never sets `SHELL`
/// and has no `/bin`, so it takes [`WINDOWS_FALLBACK_SHELL`] instead.
const FALLBACK_SHELL: &str = "/bin/zsh";

/// Last resort on Windows: the one interpreter guaranteed to be on every
/// install, including a Windows PE recovery image with nothing else in it.
const WINDOWS_FALLBACK_SHELL: &str = "cmd.exe";

/// Interactive shells to prefer on Windows, best first.
///
/// `pwsh.exe` is PowerShell 7: separately installed, actively developed, and
/// the one a developer who went and installed a shell actually wants.
/// `powershell.exe` is Windows PowerShell 5.1, shipped in the box since
/// Windows 7 and what `alacritty_terminal`'s own ConPTY backend defaults to.
///
/// `%COMSPEC%` is deliberately *not* in this list even though it is always
/// set: it names the interpreter Windows uses to run batch files, which is
/// always `cmd.exe`, and it says nothing about what shell the user wants to
/// type at. Consulting it first would mean nobody ever gets PowerShell. It is
/// the fallback below instead, after both PowerShell spellings have missed.
const WINDOWS_SHELLS: [&str; 2] = ["pwsh.exe", "powershell.exe"];

/// The shell dialect a program speaks, which is what decides how an argument
/// has to be quoted before it can be typed at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellFamily {
    /// zsh, bash, sh, fish, nu — `'…'` with `'\''` for an embedded quote.
    Posix,
    /// PowerShell 5 and 7 — `'…'` with `''` for an embedded quote.
    PowerShell,
    /// `cmd.exe` — `"…"`, and no way at all to escape a `"` inside one.
    Cmd,
}

/// A shell's name, normalised for matching: basename, lowercased, `.exe` gone.
///
/// Both separators are split on because a Windows path uses `\` while
/// `%COMSPEC%` and anything typed by hand may use `/`, and Windows filenames
/// are case-insensitive so `POWERSHELL.EXE` has to match too. On Unix this is
/// exactly the old `rsplit('/')`: no `\` appears in a real shell path, and the
/// names are already lowercase and extensionless.
fn shell_name(shell: &str) -> String {
    let base = shell
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(shell)
        .to_lowercase();
    match base.strip_suffix(".exe") {
        Some(stem) => stem.to_string(),
        None => base,
    }
}

fn shell_family(shell: &str) -> ShellFamily {
    match shell_name(shell).as_str() {
        "powershell" | "pwsh" => ShellFamily::PowerShell,
        "cmd" | "command" => ShellFamily::Cmd,
        _ => ShellFamily::Posix,
    }
}

/// Quote one argument so a shell of `family` sees it as a single literal word.
///
/// Each family gets the smallest set of characters it is safe to leave bare —
/// PowerShell's is the tightest, because `@` (splatting), `%` (the
/// `ForEach-Object` alias) and `,` (the array operator) are all live syntax
/// there and only look inert. `:` stays bare despite being a scope separator,
/// because a bare `C:\src` is the single most common argument on the platform
/// and quoting every Windows path would be worse than the risk.
///
/// `cmd.exe` is the one that cannot be made fully correct: a `"` inside a
/// quoted string has no escape the interpreter itself understands. `""` is
/// what the C runtime's own argument parser accepts, so it is right for the
/// overwhelmingly common case of launching an ordinary program, and wrong only
/// for a batch file that re-parses its own command line.
fn shell_quote_for(family: ShellFamily, arg: &str) -> String {
    let bare = match family {
        ShellFamily::Posix => "-_./=:@%+,",
        ShellFamily::PowerShell => "-_./\\=:",
        ShellFamily::Cmd => "-_./\\=:",
    };
    if !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || bare.contains(c))
    {
        return arg.to_string();
    }
    match family {
        ShellFamily::Posix => format!("'{}'", arg.replace('\'', "'\\''")),
        ShellFamily::PowerShell => format!("'{}'", arg.replace('\'', "''")),
        ShellFamily::Cmd => format!("\"{}\"", arg.replace('"', "\"\"")),
    }
}

/// `command` rendered as one line that `shell` will run when it is typed in.
///
/// PowerShell needs one thing the others do not: a quoted first word is a
/// *string literal*, which it would simply echo back, so a program whose path
/// had to be quoted has to be introduced with the call operator `&`. Bare
/// first words — the normal case, `claude` or `git` — are left alone, because
/// `& git status` and `git status` are the same command and the shorter one is
/// what the user expects to see in their scrollback.
fn command_line(shell: &str, command: &[String]) -> String {
    let family = shell_family(shell);
    let quoted: Vec<String> = command
        .iter()
        .map(|arg| shell_quote_for(family, arg))
        .collect();
    let line = quoted.join(" ");
    match (family, quoted.first()) {
        (ShellFamily::PowerShell, Some(first)) if first.starts_with('\'') => format!("& {line}"),
        _ => line,
    }
}

/// Is `program` resolvable through `%PATH%`?
///
/// Only ever called on Windows, and only with candidates that already carry
/// their `.exe`, so there is no `%PATHEXT%` expansion to do. `std::env::split_paths`
/// handles the `;` separator and the quoting Windows allows in `PATH` entries.
fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(program).is_file())
}

/// Which shell a Windows tab runs.
///
/// Kept pure — the environment and the filesystem both arrive as closures — so
/// the preference order can be tested on a Mac, where nothing it names exists.
///
/// `%SHELL%` is not consulted at all. It is not a Windows convention; when it
/// is set it is MSYS2 or Git Bash that set it, to a POSIX path like
/// `/usr/bin/bash` that no Win32 API can open, so honouring it would trade the
/// old "always fails" bug for a subtler one that only fires for the developers
/// most likely to hit it.
fn windows_shell(var: &dyn Fn(&str) -> Option<String>, exists: &dyn Fn(&str) -> bool) -> String {
    for candidate in WINDOWS_SHELLS {
        if exists(candidate) {
            return candidate.to_string();
        }
    }
    var("COMSPEC")
        .filter(|comspec| !comspec.is_empty())
        .unwrap_or_else(|| WINDOWS_FALLBACK_SHELL.to_string())
}

fn default_shell() -> String {
    // `cfg!` rather than `#[cfg]` so both arms are compiled — and therefore
    // type-checked and unit-tested — on every platform.
    if cfg!(windows) {
        windows_shell(&|key| std::env::var(key).ok(), &on_path)
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| FALLBACK_SHELL.to_string())
    }
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
/// `-l` covers zsh, bash and sh, and must not reach the Windows shells. It is
/// not a flag they merely ignore: `powershell.exe` treats an unrecognised
/// leading argument as a script to run, and `cmd.exe` takes `-l` as a stray
/// token — both turn a working tab into an immediate error. They are matched
/// explicitly below rather than left to the catch-all so that the exclusion is
/// visible, and tested.
fn login_args(shell: &str) -> Vec<String> {
    match shell_name(shell).as_str() {
        "zsh" | "bash" | "sh" | "dash" | "ksh" => vec!["-l".to_string()],
        // PowerShell reads its profile on every start, login or not, and
        // Windows hands a GUI process the user's full environment anyway, so
        // there is nothing here for a login flag to fix.
        "powershell" | "pwsh" | "cmd" | "command" => Vec::new(),
        _ => Vec::new(),
    }
}

/// Where a tab starts when the caller did not say.
///
/// Also a launched-from-Finder problem: the app's own working directory is
/// `/`, so without this every tab opens at the filesystem root rather than
/// somewhere anyone wants to be.
fn default_cwd() -> Option<PathBuf> {
    home_dir(&|key| std::env::var_os(key))
}

/// The user's home directory according to the environment.
///
/// Unix sets `HOME` and that is the whole story. Windows does not set it: the
/// equivalent is `%USERPROFILE%`, with `%HOMEDRIVE%%HOMEPATH%` as the older
/// spelling that a domain profile may still be the only one to define. `HOME`
/// comes *last* on Windows rather than first, because when it is set there at
/// all it is usually MSYS2 or Git Bash having set it to `/c/Users/me`, which
/// is not a path any Win32 API can open — preferring it would send every tab
/// to a directory that does not exist.
///
/// Pure, so the ordering is testable without touching the real environment.
pub(crate) fn home_dir(var: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let get = |key: &str| var(key).filter(|v| !v.is_empty()).map(PathBuf::from);
    if !cfg!(windows) {
        return get("HOME");
    }
    get("USERPROFILE")
        .or_else(|| {
            // Concatenated, not `join`ed: `%HOMEPATH%` starts with a separator,
            // and joining an absolute-looking path replaces the drive instead
            // of appending to it.
            let drive = var("HOMEDRIVE").filter(|v| !v.is_empty())?;
            let path = var("HOMEPATH").filter(|v| !v.is_empty())?;
            let mut joined = drive;
            joined.push(path);
            Some(PathBuf::from(joined))
        })
        .or_else(|| get("HOME"))
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
    /// The command this tab was opened with, joined by spaces, or empty for a
    /// plain shell.
    ///
    /// Kept only as a hint for the tab-bar icon (see [`crate::tab_icon`]): a
    /// tab opened as `terra new -- htop` should wear the htop logo from the
    /// first frame, not from whenever the process walk next runs — and it
    /// keeps wearing it while `htop` is what the tab is *for*, even during the
    /// moments the shell is between commands. Not the truth about what is
    /// running; the process table is.
    pub spawn: String,
    /// Per-tab override of `[text] bidi`, set from the palette or
    /// `terra bidi`. `None` means "follow the config".
    ///
    /// It has to be per tab: whether the terminal should reorder depends on
    /// which program is running, and one window routinely has a shell in one
    /// tab and an agent that does its own BiDi in another.
    pub bidi: Option<BidiMode>,
    /// The tab's transcript ring — every byte its child has written, capped at
    /// `[tabs] transcript_kb` and overwritten oldest-first. `None` when
    /// transcripts are switched off, in which case no tap is installed either
    /// and the bytes are never copied at all.
    ///
    /// Shared with the PTY reader thread, which is the only writer; the IPC
    /// threads only ever snapshot it. See [`crate::transcript`].
    pub transcript: Option<transcript::Shared>,
}

impl Tab {
    /// Effective title = custom title, else the shell-reported one.
    pub fn effective_title(&self) -> &str {
        self.title.effective()
    }
}

/// One editor group (a "split leaf"): a tab bar plus the terminal of its
/// active tab.
///
/// Groups hold ids only — the tabs themselves stay in [`TabManager::tabs`],
/// one global map, so the wire protocol keeps addressing tabs by global id.
#[derive(Debug, Clone)]
pub struct Group {
    /// Stable identity of this leaf, minted from [`TabManager::next_leaf`]
    /// and never reused. It is what focus tracks: DFS indices shift when the
    /// tree changes shape, the id never does.
    id: u64,
    /// The group's tabs in visual (bar) order.
    tab_ids: Vec<u64>,
    /// Which of [`Self::tab_ids`] shows its terminal below the bar.
    active: Option<u64>,
}

impl Group {
    fn of(id: u64, tab_id: u64) -> Self {
        Self {
            id,
            tab_ids: vec![tab_id],
            active: Some(tab_id),
        }
    }
}

/// Which way a split lays its children out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Children sit side by side (a row of columns); `split_right`/`left`
    /// create these, and the separators between children are vertical lines.
    Horizontal,
    /// Children are stacked top to bottom; `split_down`/`up` create these.
    Vertical,
}

/// One node of the split tree: a leaf (a [`Group`]) or a run of children
/// along an [`Axis`]. VS Code's 2D grid, exactly.
///
/// Invariants, restored by [`normalized`] after every mutation:
/// - no leaf is empty (a closed-out leaf folds its weight into a sibling);
/// - no split has fewer than two children (a lone child replaces its parent);
/// - no child of a split is a split on the same axis (its children splice in,
///   scaled so they keep the child's share — VS Code's merge behaviour).
#[derive(Debug, Clone)]
struct Node {
    /// Share of the parent split's extent, relative to the siblings' weights.
    /// Meaningless (and forced to 1) on the root.
    weight: f32,
    kind: NodeKind,
}

#[derive(Debug, Clone)]
enum NodeKind {
    Leaf(Group),
    Split { axis: Axis, children: Vec<Node> },
}

/// All leaves under `node`, in DFS order — the order every `group` index in
/// the public API refers to.
fn collect_leaves<'a>(node: &'a Node, out: &mut Vec<&'a Group>) {
    match &node.kind {
        NodeKind::Leaf(group) => out.push(group),
        NodeKind::Split { children, .. } => {
            for child in children {
                collect_leaves(child, out);
            }
        }
    }
}

/// The leaf with id `leaf`, mutable.
fn leaf_mut(node: &mut Node, leaf: u64) -> Option<&mut Group> {
    match &mut node.kind {
        NodeKind::Leaf(group) if group.id == leaf => Some(group),
        NodeKind::Leaf(_) => None,
        NodeKind::Split { children, .. } => {
            children.iter_mut().find_map(|child| leaf_mut(child, leaf))
        }
    }
}

/// Restore the tree invariants (see [`Node`]) bottom-up: drop empty leaves
/// (folding each one's weight into the sibling before it, else the one after),
/// splice same-axis child splits into their parent, and collapse single-child
/// splits into the child. `None` means the whole tree emptied out.
fn normalized(node: Node) -> Option<Node> {
    let Node { weight, kind } = node;
    match kind {
        NodeKind::Leaf(group) if group.tab_ids.is_empty() => None,
        NodeKind::Leaf(group) => Some(Node {
            weight,
            kind: NodeKind::Leaf(group),
        }),
        NodeKind::Split { axis, children } => {
            let mut kids: Vec<Node> = Vec::with_capacity(children.len());
            // Weight of dropped children that had no left sibling yet; it
            // falls forward onto the first survivor.
            let mut orphaned = 0.0;
            for child in children {
                let child_weight = child.weight;
                match normalized(child) {
                    Some(mut child) => {
                        child.weight += std::mem::take(&mut orphaned);
                        match child.kind {
                            NodeKind::Split {
                                axis: child_axis,
                                children: grandchildren,
                            } if child_axis == axis => {
                                // Same-axis nesting splices flat, each spliced
                                // node keeping its share of the child's share.
                                let total: f32 = grandchildren.iter().map(|n| n.weight).sum();
                                let scale = if total > 0.0 {
                                    child.weight / total
                                } else {
                                    child.weight / grandchildren.len().max(1) as f32
                                };
                                for mut grandchild in grandchildren {
                                    grandchild.weight *= scale;
                                    kids.push(grandchild);
                                }
                            }
                            _ => kids.push(child),
                        }
                    }
                    None => match kids.last_mut() {
                        Some(previous) => previous.weight += child_weight,
                        None => orphaned += child_weight,
                    },
                }
            }
            if orphaned > 0.0 {
                if let Some(first) = kids.first_mut() {
                    first.weight += orphaned;
                }
            }
            match kids.len() {
                0 => None,
                1 => {
                    // A split of one is no split: the child takes its place —
                    // and its share of the parent.
                    let mut only = kids.pop().expect("len checked");
                    only.weight = weight;
                    Some(only)
                }
                _ => Some(Node {
                    weight,
                    kind: NodeKind::Split {
                        axis,
                        children: kids,
                    },
                }),
            }
        }
    }
}

/// Replace leaf `leaf` with a two-child split on `axis`: the old leaf and
/// `group` (taken out of the option), the new leaf after or before it. The
/// pair start with equal weights inside a split that keeps the old leaf's
/// share; when the parent already runs on `axis`, [`normalized`] then splices
/// the pair in — which is exactly "the two halves share what the one had".
fn wrap_leaf(
    node: &mut Node,
    leaf: u64,
    group: &mut Option<Group>,
    axis: Axis,
    after: bool,
) -> bool {
    match &mut node.kind {
        NodeKind::Leaf(existing) if existing.id == leaf => {
            let Some(new_group) = group.take() else {
                return false;
            };
            let old_kind = std::mem::replace(
                &mut node.kind,
                NodeKind::Split {
                    axis,
                    children: Vec::new(),
                },
            );
            let old = Node {
                weight: 1.0,
                kind: old_kind,
            };
            let new = Node {
                weight: 1.0,
                kind: NodeKind::Leaf(new_group),
            };
            let children = if after {
                vec![old, new]
            } else {
                vec![new, old]
            };
            node.kind = NodeKind::Split { axis, children };
            true
        }
        NodeKind::Leaf(_) => false,
        NodeKind::Split { children, .. } => children
            .iter_mut()
            .any(|child| wrap_leaf(child, leaf, group, axis, after)),
    }
}

/// Each leaf's share of the whole window (the product of its ancestors'
/// normalised weights), pushed in DFS order. The shares sum to 1.
// Only [`TabManager::group_weights`] calls this — see there for why it stays.
#[allow(dead_code)]
fn leaf_fractions(node: &Node, factor: f32, out: &mut Vec<f32>) {
    match &node.kind {
        NodeKind::Leaf(_) => out.push(factor),
        NodeKind::Split { children, .. } => {
            let total: f32 = children.iter().map(|c| c.weight).sum();
            for child in children {
                let share = if total > 0.0 {
                    child.weight / total
                } else {
                    1.0 / children.len().max(1) as f32
                };
                leaf_fractions(child, factor * share, out);
            }
        }
    }
}

/// The split node at `path` (child indices from the root; `[]` is the root).
fn node_at<'a>(node: &'a Node, path: &[usize]) -> Option<&'a Node> {
    match path.split_first() {
        None => Some(node),
        Some((first, rest)) => match &node.kind {
            NodeKind::Leaf(_) => None,
            NodeKind::Split { children, .. } => node_at(children.get(*first)?, rest),
        },
    }
}

fn node_at_mut<'a>(node: &'a mut Node, path: &[usize]) -> Option<&'a mut Node> {
    match path.split_first() {
        None => Some(node),
        Some((first, rest)) => match &mut node.kind {
            NodeKind::Leaf(_) => None,
            NodeKind::Split { children, .. } => node_at_mut(children.get_mut(*first)?, rest),
        },
    }
}

/// Give `group` a new active tab after the one at `removed_idx` left:
/// the nearest tab to the left, else the first remaining one.
fn reactivate(group: &mut Group, removed_idx: usize) {
    group.active = group.tab_ids.get(removed_idx.saturating_sub(1)).copied();
}

/// The split tree with every leaf reduced to its DFS index and every split's
/// weights normalised — all the renderer needs to lay the window out, and
/// what the tree-shape tests assert on. `Leaf(i)`'s `i` is the same `group`
/// index the rest of the API takes.
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutNode {
    Leaf(usize),
    Split {
        axis: Axis,
        /// One fraction per child, summing to 1.
        weights: Vec<f32>,
        children: Vec<LayoutNode>,
    },
}

fn build_layout(node: &Node, next_leaf: &mut usize) -> LayoutNode {
    match &node.kind {
        NodeKind::Leaf(_) => {
            let index = *next_leaf;
            *next_leaf += 1;
            LayoutNode::Leaf(index)
        }
        NodeKind::Split { axis, children } => {
            let total: f32 = children.iter().map(|c| c.weight).sum();
            let weights = children
                .iter()
                .map(|c| {
                    if total > 0.0 {
                        c.weight / total
                    } else {
                        1.0 / children.len().max(1) as f32
                    }
                })
                .collect();
            LayoutNode::Split {
                axis: *axis,
                weights,
                children: children
                    .iter()
                    .map(|child| build_layout(child, next_leaf))
                    .collect(),
            }
        }
    }
}

/// `h([0,2] v([1] [3]))`-style rendering of the tree, leaves as their tab
/// ids — one line the tree tests can assert whole shapes with. Unix-only
/// like its callers: the shape tests spawn real PTYs.
#[cfg(all(test, unix))]
fn shape_of(node: &Node) -> String {
    match &node.kind {
        NodeKind::Leaf(group) => format!(
            "[{}]",
            group
                .tab_ids
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ),
        NodeKind::Split { axis, children } => {
            let tag = match axis {
                Axis::Horizontal => "h",
                Axis::Vertical => "v",
            };
            let inner = children.iter().map(shape_of).collect::<Vec<_>>().join(" ");
            format!("{tag}({inner})")
        }
    }
}

/// One OS window: its own split tree, and which of its leaves the keyboard
/// belongs to while the window is the focused one.
///
/// Tabs are *not* in here — they stay in the one global [`TabManager::tabs`]
/// map, so a tab torn out into a second window keeps the id the wire protocol
/// already knows it by. A window owns leaves; leaves own tab ids.
struct Window {
    /// Stable identity, minted from [`TabManager::next_window`] and never
    /// reused. The window terra starts in is 0.
    id: u64,
    /// This window's split tree — the same [`Node`] model, with the same
    /// invariants, one per window. `None` while the window holds no tab,
    /// which only the last surviving window is ever allowed to be (an
    /// emptied second window is removed instead; see
    /// [`TabManager::normalize`]).
    tree: Option<Node>,
    /// Leaf *id* (see [`Group::id`]) of this window's focused group.
    ///
    /// Remembered per window rather than globally so that returning to a
    /// window returns to the pane you left it in — switching windows must not
    /// reshuffle where the keyboard lands inside them.
    focused_leaf: u64,
}

/// Does the tree under `node` hold a leaf with id `leaf`?
fn contains_leaf(node: &Node, leaf: u64) -> bool {
    match &node.kind {
        NodeKind::Leaf(group) => group.id == leaf,
        NodeKind::Split { children, .. } => children.iter().any(|c| contains_leaf(c, leaf)),
    }
}

/// The leaves of one window, cloned so no borrow of the window list escapes.
fn leaves_of(window: &Window) -> Vec<Group> {
    let mut refs = Vec::new();
    if let Some(root) = window.tree.as_ref() {
        collect_leaves(root, &mut refs);
    }
    refs.into_iter().cloned().collect()
}

/// Take `tab` out of whichever window's leaf holds it, leaving that leaf the
/// way [`TabManager::close`] leaves it: the nearest tab to the left becomes
/// active. `false` when no window held it.
///
/// The leaf it empties (and the window that leaf may empty) is left standing —
/// [`TabManager::normalize`] is what collapses both, once the caller has also
/// put the tab down somewhere.
fn detach_tab(windows: &mut [Window], tab: u64) -> bool {
    for window in windows.iter_mut() {
        let Some(root) = window.tree.as_mut() else {
            continue;
        };
        let src_leaf = {
            let mut refs = Vec::new();
            collect_leaves(root, &mut refs);
            refs.iter().find(|g| g.tab_ids.contains(&tab)).map(|g| g.id)
        };
        if let Some(src_leaf) = src_leaf {
            let group = leaf_mut(root, src_leaf).expect("leaf just seen");
            let idx = group
                .tab_ids
                .iter()
                .position(|i| *i == tab)
                .expect("tab just seen in this leaf");
            group.tab_ids.remove(idx);
            if group.active == Some(tab) {
                reactivate(group, idx);
            }
            return true;
        }
    }
    false
}

pub struct TabManager {
    tabs: BTreeMap<u64, Tab>,
    /// Every open window, in creation order, each with its own split tree —
    /// the single source of truth for the bars, `⌘1..9` and next/prev.
    /// Never empty: window 0 exists from `new` on, and the last window is
    /// kept even once its last tab closes (that empty state is the app's quit
    /// condition, not a window to remove).
    ///
    /// Behind a `RefCell` because the tab bar reorders tabs while holding only
    /// a `&TabManager` (the bar is drawn from an immutable borrow, mid-frame,
    /// while a tab is dragged). Nothing else in the manager is shared, so the
    /// borrows are short and strictly local to the group methods.
    windows: RefCell<Vec<Window>>,
    /// Which window the keyboard is in. *That* window's focused leaf's active
    /// tab is the globally active tab: keystrokes go there and `terra ls`
    /// marks it — exactly one tab across every window.
    focused_window: std::cell::Cell<u64>,
    /// Next [`Window::id`] to mint. Never reused, like tab and leaf ids.
    next_window: std::cell::Cell<u64>,
    /// A window [`Self::hold_window`] is keeping alive while it has no tabs.
    ///
    /// One slot, because one drag: a tab dragged out of window A and docked
    /// into window B leaves A empty, and an empty window is normally gone the
    /// moment it empties. But A is the window pumping the drag — it owns the
    /// mouse capture the gesture is riding on — so removing it mid-gesture
    /// takes the drag down with it. The hold parks that rule until the button
    /// comes up ([`Self::release_window`]).
    held: std::cell::Cell<Option<u64>>,
    /// Next [`Group::id`] to mint. Never reused, like tab ids. Global across
    /// windows, so a leaf id names a group without also naming its window.
    next_leaf: std::cell::Cell<u64>,
    next_id: u64,
    ctx: egui::Context,
    pty_events: Sender<(u64, PtyEvent)>,
    /// The named ways to open a tab, mirrored here from the config.
    ///
    /// They live on the manager rather than being read from the `ConfigStore`
    /// because both things that need them are off the UI thread's happy path:
    /// an IPC connection thread resolving `terra new --profile`, and the tab
    /// bar, which is drawn from a `&TabManager` and nothing else. The store
    /// stays the owner — `main.rs` pushes a fresh copy on load and on reload.
    profiles: BTreeMap<String, Profile>,
    /// Bytes of PTY output each new tab retains for `terra transcript`; `0`
    /// installs no tap at all. Mirrored from `[tabs] transcript_kb` the same
    /// way (and for the same reason) as `profiles`.
    ///
    /// Read only by [`Self::open`]: a tab's ring is sized once, when it is
    /// created, so a reload changes what the *next* tab gets rather than
    /// silently discarding what an open tab has already recorded.
    transcript_bytes: usize,
}

impl TabManager {
    pub fn new(ctx: egui::Context, pty_events: Sender<(u64, PtyEvent)>) -> Self {
        Self {
            tabs: BTreeMap::new(),
            windows: RefCell::new(vec![Window {
                id: 0,
                tree: None,
                focused_leaf: 0,
            }]),
            focused_window: std::cell::Cell::new(0),
            next_window: std::cell::Cell::new(1),
            held: std::cell::Cell::new(None),
            next_leaf: std::cell::Cell::new(0),
            next_id: 0,
            ctx,
            pty_events,
            profiles: BTreeMap::new(),
            transcript_bytes: crate::config::DEFAULT_TAB_TRANSCRIPT_KB * 1024,
        }
    }

    /// A snapshot of every leaf of the *focused* window in DFS order — the
    /// order all `group` indices in this API refer to. Cloned so no borrow of
    /// the window list escapes.
    fn leaves(&self) -> Vec<Group> {
        self.leaves_in(self.focused_window.get())
    }

    /// [`Self::leaves`] for a named window; empty for an unknown one.
    fn leaves_in(&self, win: u64) -> Vec<Group> {
        let windows = self.windows.borrow();
        windows
            .iter()
            .find(|w| w.id == win)
            .map(leaves_of)
            .unwrap_or_default()
    }

    /// Leaf id of the focused window's focused group.
    fn focused_leaf(&self) -> u64 {
        let windows = self.windows.borrow();
        let focused = self.focused_window.get();
        windows
            .iter()
            .find(|w| w.id == focused)
            .map(|w| w.focused_leaf)
            .unwrap_or(0)
    }

    /// Focus leaf `leaf` *and the window that holds it* — the two are one
    /// action, because a leaf only ever has the keyboard while its window
    /// does. A leaf that is in no tree yet (a group being minted) is recorded
    /// against the currently focused window, which is where `open` and
    /// `split` always put it.
    fn set_focused_leaf(&self, leaf: u64) {
        let owner = {
            let mut windows = self.windows.borrow_mut();
            let focused = self.focused_window.get();
            let idx = windows
                .iter()
                .position(|w| {
                    w.tree
                        .as_ref()
                        .is_some_and(|root| contains_leaf(root, leaf))
                })
                .or_else(|| windows.iter().position(|w| w.id == focused));
            idx.map(|i| {
                windows[i].focused_leaf = leaf;
                windows[i].id
            })
        };
        if let Some(id) = owner {
            self.focused_window.set(id);
        }
    }

    /// Read the focused window's tree. `None` reaches `f` both for a window
    /// with no tabs and (defensively) for a focused id naming no window.
    fn with_tree<R>(&self, f: impl FnOnce(Option<&Node>) -> R) -> R {
        let windows = self.windows.borrow();
        let focused = self.focused_window.get();
        f(windows
            .iter()
            .find(|w| w.id == focused)
            .and_then(|w| w.tree.as_ref()))
    }

    /// [`Self::with_tree`], mutably. Writes to the `None` stand-in that an
    /// unknown focused window gets are simply dropped.
    fn with_tree_mut<R>(&self, f: impl FnOnce(&mut Option<Node>) -> R) -> R {
        let mut windows = self.windows.borrow_mut();
        let focused = self.focused_window.get();
        match windows.iter_mut().find(|w| w.id == focused) {
            Some(window) => f(&mut window.tree),
            None => f(&mut None),
        }
    }

    /// Restore the tree invariants of *every* window after a mutation (a
    /// cross-window move touches two of them), drop the windows that emptied
    /// out, and make sure each window's focus still points at a live leaf.
    ///
    /// When a window's focused leaf collapsed, the leaf now at its old DFS
    /// index takes over — "stay in place" from the user's point of view. That
    /// index is `old_focus` for the focused window (captured by the caller
    /// *before* the mutation); for the others it is read off the tree as it
    /// stands here, which is still pre-collapse: a leaf emptied of tabs is
    /// only removed below.
    ///
    /// The last window is never removed. A terra with no tabs left is one
    /// empty window, not zero windows — that is the state `is_empty` reports
    /// and the frame loop quits on.
    fn normalize(&self, old_focus: usize) {
        {
            let mut windows = self.windows.borrow_mut();
            let focused_window = self.focused_window.get();
            for window in windows.iter_mut() {
                let old_index = if window.id == focused_window {
                    old_focus
                } else {
                    window
                        .tree
                        .as_ref()
                        .map(|root| {
                            let mut refs = Vec::new();
                            collect_leaves(root, &mut refs);
                            refs.iter()
                                .position(|g| g.id == window.focused_leaf)
                                .unwrap_or(0)
                        })
                        .unwrap_or(0)
                };
                if let Some(root) = window.tree.take() {
                    window.tree = normalized(root);
                }
                let Some(root) = window.tree.as_mut() else {
                    continue;
                };
                root.weight = 1.0;
                let mut refs = Vec::new();
                collect_leaves(root, &mut refs);
                if !refs.iter().any(|g| g.id == window.focused_leaf) {
                    window.focused_leaf = refs[old_index.min(refs.len() - 1)].id;
                }
            }
            // An emptied window is gone — except the last one, which is how a
            // tabless terra looks, and except one that is being held (a drag
            // is still pumping its events; see [`Self::hold_window`]).
            let held = self.held.get();
            if windows.len() > 1 {
                if windows.iter().any(|w| w.tree.is_some()) {
                    windows.retain(|w| w.tree.is_some() || Some(w.id) == held);
                } else {
                    // Nothing left anywhere: one empty window survives rather
                    // than a fresh id being minted for the same window — plus
                    // the held one, if that is not it.
                    let first = windows[0].id;
                    windows.retain(|w| w.id == first || Some(w.id) == held);
                }
            }
            if !windows.iter().any(|w| w.id == focused_window) {
                // The focused window closed under us: the keyboard goes to the
                // first survivor, at the leaf that window was last left in.
                self.focused_window.set(windows[0].id);
            }
        }
    }

    /// Keep window `win` alive while it is empty.
    ///
    /// A tab dragged out of a window and docked into another one really does
    /// leave: the model moves it, the source window empties, and an empty
    /// window normally disappears on the spot. That window is also the one
    /// running the drag — the OS delivers the whole gesture to whichever
    /// window saw the button go down — so its viewport closing mid-drag ends
    /// the drag. The hold keeps it (and only it) around until the gesture is
    /// over; nothing else about it changes, and with no tabs it is invisible
    /// to `terra ls` either way.
    ///
    /// One slot: there is one pointer, so there is one drag.
    pub fn hold_window(&self, win: u64) {
        self.held.set(Some(win));
    }

    /// The window [`Self::hold_window`] is keeping alive, if any. The one
    /// window allowed to sit empty while others are open, which is the shape
    /// the model tests check their invariants against — the app itself only
    /// ever sets and clears the hold.
    #[allow(dead_code)]
    pub fn held_window(&self) -> Option<u64> {
        self.held.get()
    }

    /// Drop the hold from [`Self::hold_window`] and settle up: a window that
    /// only the hold was keeping alive collapses now. Safe to call for a
    /// window that was never held, and for one that has since been given tabs.
    pub fn release_window(&mut self, win: u64) {
        if self.held.get() != Some(win) {
            return;
        }
        self.held.set(None);
        self.normalize(self.focused_group());
    }

    /// Replace the profile table. Called with the config's own copy at startup
    /// and after every reload, so the menu and `--profile` never disagree with
    /// the file.
    pub fn set_profiles(&mut self, profiles: BTreeMap<String, Profile>) {
        self.profiles = profiles;
    }

    /// The profile table, alphabetical by name.
    pub fn profiles(&self) -> &BTreeMap<String, Profile> {
        &self.profiles
    }

    /// Set the per-tab transcript cap in bytes, `0` for off. Pushed from the
    /// config at startup and after every reload, like [`Self::set_profiles`];
    /// it takes effect for tabs opened from here on.
    pub fn set_transcript_bytes(&mut self, bytes: usize) {
        self.transcript_bytes = bytes;
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// All tab ids in visual order: the windows in creation order, each
    /// window's leaves in DFS order, each group's tabs in bar order.
    ///
    /// Every window, deliberately: `terra ls` and `terra kill` address the
    /// whole app, and a tab that was torn out into a second window has not
    /// left it.
    pub fn ids(&self) -> Vec<u64> {
        let windows = self.windows.borrow();
        windows
            .iter()
            .flat_map(leaves_of)
            .flat_map(|g| g.tab_ids)
            .collect()
    }

    /// Position of `id` in the global visual order (see [`Self::ids`]).
    // The drag path is group-scoped now ([`Self::group_of`]); kept because the
    // PTY tests and the cross-group drag feature address slots globally.
    #[allow(dead_code)]
    pub fn index_of(&self, id: u64) -> Option<usize> {
        self.ids().iter().position(|i| *i == id)
    }

    // -- groups --------------------------------------------------------------

    /// How many groups (leaves) the window currently shows.
    pub fn group_count(&self) -> usize {
        self.leaves().len()
    }

    /// DFS index of the focused group. Its active tab is the globally active
    /// tab.
    pub fn focused_group(&self) -> usize {
        let focused = self.focused_leaf();
        self.leaves()
            .iter()
            .position(|g| g.id == focused)
            .unwrap_or(0)
    }

    /// The group `id` lives in, if it is open.
    pub fn group_of(&self, id: u64) -> Option<usize> {
        self.leaves().iter().position(|g| g.tab_ids.contains(&id))
    }

    /// The stable leaf id of group `group` — what survives the tree changing
    /// shape. DFS indices renumber every leaf after an insertion; per-group
    /// UI state (bar animations, widget ids) must key on this instead, or a
    /// split in one place makes every bar to its right inherit a
    /// neighbour's state.
    pub fn group_leaf_id(&self, group: usize) -> Option<u64> {
        self.leaves().get(group).map(|g| g.id)
    }

    /// The tab ids of group `group` in bar order (empty for an unknown group).
    pub fn group_tabs(&self, group: usize) -> Vec<u64> {
        self.leaves()
            .get(group)
            .map(|g| g.tab_ids.clone())
            .unwrap_or_default()
    }

    /// The active tab of group `group`.
    pub fn group_active(&self, group: usize) -> Option<u64> {
        self.leaves().get(group).and_then(|g| g.active)
    }

    /// Each group's share of the window area, in DFS order, summing to 1
    /// (empty when there are no groups). For a single row of columns these
    /// are the column widths; in a nested tree, the product of the
    /// normalised weights down the leaf's path.
    // The renderer reads per-split weights now ([`Self::split_weights`]);
    // this stays for the invariant tests, which assert every leaf's share.
    #[allow(dead_code)]
    pub fn group_weights(&self) -> Vec<f32> {
        self.with_tree(|tree| {
            let mut out = Vec::new();
            if let Some(root) = tree {
                leaf_fractions(root, 1.0, &mut out);
            }
            out
        })
    }

    /// The split tree of the focused window for rendering: leaves as DFS
    /// indices, weights normalised per split. `None` while the window holds no
    /// tab. A lone group comes back as `Leaf(0)` — the renderer needs no
    /// special case.
    // The renderer names its window explicitly now ([`Self::window_layout`]);
    // this stays as the "whatever has the keyboard" spelling the tests and the
    // group API are written in.
    #[allow(dead_code)]
    pub fn layout(&self) -> Option<LayoutNode> {
        self.window_layout(self.focused_window.get())
    }

    /// [`Self::layout`] for a named window — what the renderer of a *second*
    /// OS window walks. `None` for an unknown window and for one with no tab
    /// in it.
    pub fn window_layout(&self, win: u64) -> Option<LayoutNode> {
        let windows = self.windows.borrow();
        let root = windows.iter().find(|w| w.id == win)?.tree.as_ref()?;
        let mut next = 0usize;
        Some(build_layout(root, &mut next))
    }

    /// The focused window's tree as a string — `h([0,2] v([1] [3]))`, leaves
    /// as tab ids — for the shape tests (unix-only, like [`shape_of`]).
    #[cfg(all(test, unix))]
    pub fn shape(&self) -> String {
        self.window_shape(self.focused_window.get())
    }

    /// [`Self::shape`] for a named window; `-` for an empty or unknown one.
    #[cfg(all(test, unix))]
    pub fn window_shape(&self, win: u64) -> String {
        let windows = self.windows.borrow();
        match windows
            .iter()
            .find(|w| w.id == win)
            .and_then(|w| w.tree.as_ref())
        {
            Some(root) => shape_of(root),
            None => "-".to_string(),
        }
    }

    /// The normalised child weights of the split node at `path` (child
    /// indices from the root, `[]` = root). Empty unless `path` names a
    /// split.
    pub fn split_weights(&self, path: &[usize]) -> Vec<f32> {
        self.with_tree(|tree| {
            let Some(NodeKind::Split { children, .. }) =
                tree.and_then(|root| node_at(root, path)).map(|n| &n.kind)
            else {
                return Vec::new();
            };
            let total: f32 = children.iter().map(|c| c.weight).sum();
            children
                .iter()
                .map(|c| {
                    if total > 0.0 {
                        c.weight / total
                    } else {
                        1.0 / children.len().max(1) as f32
                    }
                })
                .collect()
        })
    }

    /// Replace the child weights of the split at `path` (the separator
    /// resize drag in `main.rs` writes them back). Rejected unless `path`
    /// names a split and there is one strictly positive weight per child.
    pub fn set_split_weights(&self, path: &[usize], weights: &[f32]) -> bool {
        self.with_tree_mut(|tree| {
            let Some(NodeKind::Split { children, .. }) = tree
                .as_mut()
                .and_then(|root| node_at_mut(root, path))
                .map(|n| &mut n.kind)
            else {
                return false;
            };
            if weights.len() != children.len()
                || weights.iter().any(|w| !w.is_finite() || *w <= 0.0)
            {
                return false;
            }
            for (child, weight) in children.iter_mut().zip(weights) {
                child.weight = *weight;
            }
            true
        })
    }

    /// Focus group `idx` (DFS order) of the focused window; its active tab
    /// becomes the globally active tab.
    pub fn focus_group(&mut self, idx: usize) -> bool {
        let Some(leaf) = self.leaves().get(idx).map(|g| g.id) else {
            return false;
        };
        self.set_focused_leaf(leaf);
        self.sync_visibility();
        true
    }

    /// Focus the next group in DFS order (wrapping).
    pub fn next_group(&mut self) {
        self.step_group(1);
    }

    /// Focus the previous group in DFS order (wrapping).
    pub fn prev_group(&mut self) {
        self.step_group(-1);
    }

    fn step_group(&mut self, delta: isize) {
        let leaves = self.leaves();
        if leaves.is_empty() {
            return;
        }
        let len = leaves.len() as isize;
        let current = self.focused_group() as isize;
        let next = (current + delta).rem_euclid(len) as usize;
        self.set_focused_leaf(leaves[next].id);
    }

    /// Move `id` out of its group into a *new* group immediately to the right
    /// of it; the moved tab is the new group's active tab and the new group
    /// takes focus. A tab that is alone in its group has nothing to split away
    /// from, so that is a `false` no-op — as is an unknown id.
    pub fn split_right(&mut self, id: u64) -> bool {
        self.split(id, Axis::Horizontal, true)
    }

    /// [`Self::split_right`], but the new group lands on the left.
    pub fn split_left(&mut self, id: u64) -> bool {
        self.split(id, Axis::Horizontal, false)
    }

    /// [`Self::split_right`] turned 90°: the new group opens *below* the
    /// tab's old one.
    pub fn split_down(&mut self, id: u64) -> bool {
        self.split(id, Axis::Vertical, true)
    }

    /// [`Self::split_down`], but the new group lands on top.
    pub fn split_up(&mut self, id: u64) -> bool {
        self.split(id, Axis::Vertical, false)
    }

    fn split(&mut self, id: u64, axis: Axis, after: bool) -> bool {
        let old_focus = self.focused_group();
        let new_leaf = self.next_leaf.get();
        let split = self.with_tree_mut(|tree| {
            let Some(root) = tree.as_mut() else {
                return false;
            };
            let src_leaf = {
                let mut refs = Vec::new();
                collect_leaves(root, &mut refs);
                match refs.iter().find(|g| g.tab_ids.contains(&id)) {
                    // Splitting a lone tab would collapse its old group and
                    // put the new one in the same slot: pure churn, so refuse.
                    Some(src) if src.tab_ids.len() >= 2 => src.id,
                    _ => return false,
                }
            };
            let src = leaf_mut(root, src_leaf).expect("leaf just seen");
            src.tab_ids.retain(|i| *i != id);
            if src.active == Some(id) {
                src.active = src.tab_ids.first().copied();
            }
            let mut group = Some(Group::of(new_leaf, id));
            let wrapped = wrap_leaf(root, src_leaf, &mut group, axis, after);
            debug_assert!(wrapped, "the source leaf cannot have vanished");
            true
        });
        if !split {
            return false;
        }
        self.next_leaf.set(new_leaf + 1);
        // The wrap may have nested a split inside a same-axis parent;
        // normalize splices it in (and the halved weights come out right).
        self.normalize(old_focus);
        self.set_focused_leaf(new_leaf);
        self.sync_visibility();
        true
    }

    /// Move a tab to slot `index` of group `target_group` (its own or another),
    /// shifting the rest along. `index` past the end clamps to the last slot;
    /// the moved tab becomes its new group's active tab; a source group left
    /// empty collapses (its weight folds into a sibling). Unknown ids/groups
    /// and no-op moves return `false`.
    ///
    /// Both groups are the *focused window's* — dropping a pill into another
    /// window is [`Self::move_tab_to_window`].
    ///
    /// `&self` on purpose: the bar reorders mid-drag while holding only an
    /// immutable borrow (see [`Self::windows`]). Focus does *not* follow the
    /// tab — use [`Self::select`] for that.
    pub fn move_tab(&self, id: u64, target_group: usize, index: usize) -> bool {
        let old_focus = self.focused_group();
        let moved = self.with_tree_mut(|tree| {
            let Some(root) = tree.as_mut() else {
                return false;
            };
            let (src_leaf, target_leaf, from, to) = {
                let mut refs = Vec::new();
                collect_leaves(root, &mut refs);
                let Some(target) = refs.get(target_group) else {
                    return false;
                };
                let Some(src) = refs.iter().find(|g| g.tab_ids.contains(&id)) else {
                    return false;
                };
                let from = src.tab_ids.iter().position(|i| *i == id).unwrap();
                let to = if src.id == target.id {
                    index.min(src.tab_ids.len() - 1)
                } else {
                    index.min(target.tab_ids.len())
                };
                if src.id == target.id && from == to {
                    return false;
                }
                (src.id, target.id, from, to)
            };
            let src = leaf_mut(root, src_leaf).expect("leaf just seen");
            src.tab_ids.remove(from);
            if src.active == Some(id) && src_leaf != target_leaf {
                reactivate(src, from);
            }
            let target = leaf_mut(root, target_leaf).expect("leaf just seen");
            target.tab_ids.insert(to, id);
            target.active = Some(id);
            true
        });
        if !moved {
            return false;
        }
        self.normalize(old_focus);
        self.sync_visibility();
        true
    }

    /// [`Self::move_tab`] within the focused group, addressed by current
    /// position instead of id — the index-based half of the reorder API, used
    /// by the tests and by any caller that thinks in slots rather than tabs.
    #[cfg(test)]
    // Only the Unix-gated PTY tests reach this today — the tab-drag UI in
    // `ui.rs` does not call it yet, which is worth fixing separately.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub fn reorder(&self, from_idx: usize, to_idx: usize) -> bool {
        let group = self.focused_group();
        let id = match self.group_tabs(group).get(from_idx) {
            Some(id) => *id,
            None => return false,
        };
        self.move_tab(id, group, to_idx)
    }

    // -- windows -------------------------------------------------------------

    /// Every open window's id, ascending — the render loop's list of viewports.
    /// Never empty: a terra with no tabs is still one (empty) window, so a
    /// fresh manager answers `[0]`.
    pub fn window_ids(&self) -> Vec<u64> {
        let windows = self.windows.borrow();
        let mut ids: Vec<u64> = windows.iter().map(|w| w.id).collect();
        ids.sort_unstable();
        ids
    }

    /// The window tab `tab` lives in, if it is open.
    pub fn window_of_tab(&self, tab: u64) -> Option<u64> {
        let windows = self.windows.borrow();
        windows
            .iter()
            .find(|w| leaves_of(w).iter().any(|g| g.tab_ids.contains(&tab)))
            .map(|w| w.id)
    }

    /// The window holding the leaf (group) with id `leaf_id`.
    ///
    /// The counterpart of [`Self::group_leaf_id`] for callers that hold a
    /// stable leaf id — per-group UI state, an IPC request naming a group —
    /// and need to know which viewport to draw it in.
    // Not on the render path (it walks windows outward, so it always knows
    // which one it is in); this is for the callers that arrive holding only
    // the leaf id.
    #[allow(dead_code)]
    pub fn window_of_group_leaf(&self, leaf_id: u64) -> Option<u64> {
        let windows = self.windows.borrow();
        windows
            .iter()
            .find(|w| {
                w.tree
                    .as_ref()
                    .is_some_and(|root| contains_leaf(root, leaf_id))
            })
            .map(|w| w.id)
    }

    /// The window the keyboard is in. Every group-index API on this manager
    /// (`focused_group`, `group_tabs`, `split_*`, `layout`, …) is scoped to it.
    pub fn focused_window(&self) -> u64 {
        self.focused_window.get()
    }

    /// Give window `win` the keyboard, at the leaf it was last left in — the
    /// per-window focus memory is the point: switching windows must not
    /// reshuffle which pane you land in. `false` for an unknown window.
    pub fn focus_window(&mut self, win: u64) -> bool {
        if !self.windows.borrow().iter().any(|w| w.id == win) {
            return false;
        }
        self.focused_window.set(win);
        self.sync_visibility();
        true
    }

    /// Tear `tab` out into a brand-new window holding one group, which takes
    /// the keyboard; returns the new window's id.
    ///
    /// `None` — a no-op, nothing moved — for an unknown tab and for the sole
    /// tab of a sole window: there the tab's old window would collapse the
    /// moment the new one appeared, so the whole operation is the app
    /// renumbering its only window for nothing.
    ///
    /// The source leaf is left the way [`Self::close`] leaves it (the nearest
    /// tab to the left becomes active) and collapses if it emptied, taking its
    /// window with it if that emptied too.
    pub fn move_tab_to_new_window(&mut self, tab: u64) -> Option<u64> {
        if !self.tabs.contains_key(&tab) {
            return None;
        }
        let old_focus = self.focused_group();
        let new_leaf = self.next_leaf.get();
        let new_window = self.next_window.get();
        {
            let mut windows = self.windows.borrow_mut();
            if windows.len() == 1 && self.tabs.len() == 1 {
                return None;
            }
            if !detach_tab(&mut windows, tab) {
                return None;
            }
            windows.push(Window {
                id: new_window,
                tree: Some(Node {
                    weight: 1.0,
                    kind: NodeKind::Leaf(Group::of(new_leaf, tab)),
                }),
                focused_leaf: new_leaf,
            });
        }
        self.next_leaf.set(new_leaf + 1);
        self.next_window.set(new_window + 1);
        // Focus moves with the tab: a torn-out tab is one the user is looking
        // at. Set before `normalize`, so the emptied source window (if it
        // emptied) is removed without the keyboard ever pointing at it.
        self.focused_window.set(new_window);
        self.normalize(old_focus);
        self.sync_visibility();
        Some(new_window)
    }

    /// Move `tab` into window `win`: its focused group, after that group's
    /// active tab, becoming that group's active tab — the same landing spot
    /// [`Self::open`] uses, because dropping a pill on a window and opening a
    /// tab in it should put it in the same place.
    ///
    /// Focus does *not* follow (like [`Self::move_tab`]; the drag path decides
    /// that for itself) — use [`Self::focus_window`] or [`Self::select`].
    /// `false` for an unknown tab or window, and for a tab that is already the
    /// only occupant of `win`, where the move has nothing to do but destroy
    /// and rebuild a window.
    // The drop-on-another-window half of the drag is the UI's to wire up; the
    // model side is complete and tested either way.
    #[allow(dead_code)]
    pub fn move_tab_to_window(&mut self, tab: u64, win: u64) -> bool {
        if !self.tabs.contains_key(&tab) {
            return false;
        }
        let old_focus = self.focused_group();
        let new_leaf = self.next_leaf.get();
        let minted = {
            let mut windows = self.windows.borrow_mut();
            let Some(target) = windows.iter().position(|w| w.id == win) else {
                return false;
            };
            let Some(source) = windows
                .iter()
                .position(|w| leaves_of(w).iter().any(|g| g.tab_ids.contains(&tab)))
            else {
                return false;
            };
            if source == target
                && leaves_of(&windows[target])
                    .iter()
                    .map(|g| g.tab_ids.len())
                    .sum::<usize>()
                    == 1
            {
                return false;
            }
            detach_tab(&mut windows, tab);
            // Two separate index borrows: the source is written first, then
            // the target, because both are `&mut` into the same vector.
            let window = &mut windows[target];
            match window.tree.as_mut() {
                Some(root) => {
                    let leaf = {
                        let mut refs = Vec::new();
                        collect_leaves(root, &mut refs);
                        refs.iter()
                            .find(|g| g.id == window.focused_leaf)
                            .or(refs.first())
                            .map(|g| g.id)
                            .expect("a tree has leaves")
                    };
                    let group = leaf_mut(root, leaf).expect("leaf just seen");
                    let at = group
                        .active
                        .and_then(|a| group.tab_ids.iter().position(|i| *i == a))
                        .map(|i| i + 1)
                        .unwrap_or(group.tab_ids.len());
                    group.tab_ids.insert(at, tab);
                    group.active = Some(tab);
                    window.focused_leaf = group.id;
                    false
                }
                None => {
                    // The target had emptied out (only the last window is ever
                    // allowed to sit like that): it gets a fresh root leaf.
                    window.tree = Some(Node {
                        weight: 1.0,
                        kind: NodeKind::Leaf(Group::of(new_leaf, tab)),
                    });
                    window.focused_leaf = new_leaf;
                    true
                }
            }
        };
        if minted {
            self.next_leaf.set(new_leaf + 1);
        }
        self.normalize(old_focus);
        self.sync_visibility();
        true
    }

    /// Close window `win` and every tab in it, returning those tab ids in
    /// visual order.
    ///
    /// The tabs are dropped here — a `Tab` owns its `TerminalBackend`, so
    /// there is no other way to shut their PTYs down — and the ids come back
    /// for the caller's own bookkeeping: the confirm-close question it asked
    /// beforehand, the viewport it now has to tear down, any per-tab UI state
    /// keyed by id. An unknown window returns an empty vector and changes
    /// nothing.
    ///
    /// Closing the *last* window empties it rather than removing it, which is
    /// exactly the state a tabless terra is already in (and what the frame
    /// loop quits on).
    pub fn close_window(&mut self, win: u64) -> Vec<u64> {
        let doomed: Vec<u64> = {
            let windows = self.windows.borrow();
            let Some(window) = windows.iter().find(|w| w.id == win) else {
                return Vec::new();
            };
            leaves_of(window)
                .into_iter()
                .flat_map(|g| g.tab_ids)
                .collect()
        };
        for id in &doomed {
            self.tabs.remove(id);
        }
        {
            let mut windows = self.windows.borrow_mut();
            if windows.len() > 1 {
                windows.retain(|w| w.id != win);
            } else {
                windows[0].tree = None;
            }
            if !windows.iter().any(|w| w.id == self.focused_window.get()) {
                self.focused_window.set(windows[0].id);
            }
        }
        // Nothing else was reshaped, so this only re-checks the invariants —
        // cheap, and the one place they are guaranteed.
        self.normalize(0);
        self.sync_visibility();
        doomed
    }

    // -- the active tab ------------------------------------------------------

    /// The globally active tab: the focused window's focused group's active
    /// tab — one tab across every window.
    pub fn active_id(&self) -> Option<u64> {
        let focused = self.focused_leaf();
        self.leaves()
            .iter()
            .find(|g| g.id == focused)
            .and_then(|g| g.active)
    }

    // The render path now walks groups, but this stays the natural spelling
    // for "the tab the keyboard talks to".
    #[allow(dead_code)]
    pub fn active_mut(&mut self) -> Option<&mut Tab> {
        let id = self.active_id()?;
        self.tabs.get_mut(&id)
    }

    /// The tab itself, for rendering a group's active terminal.
    pub fn get_mut(&mut self, id: u64) -> Option<&mut Tab> {
        self.tabs.get_mut(&id)
    }

    pub fn title(&self, id: u64) -> Option<&str> {
        self.tabs.get(&id).map(|t| t.effective_title())
    }

    /// Tab descriptions in global visual order (see [`Self::ids`]). `active`
    /// marks the globally active tab — the focused window's focused group's
    /// active one — exactly one across every window.
    pub fn infos(&self) -> Vec<TabInfo> {
        let active = self.active_id();
        self.ids()
            .iter()
            .filter_map(|id| {
                let tab = self.tabs.get(id)?;
                Some(TabInfo {
                    id: *id,
                    title: tab.effective_title().to_string(),
                    active: Some(*id) == active,
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
        // `shell` is moved into the backend settings below, but the quoting of
        // the typed command line depends on which shell it names.
        let typed_into = shell.clone();

        let id = self.next_id;
        // The transcript ring, and the tap that fills it. Both are skipped
        // entirely when the cap is 0, so the disabled case copies nothing and
        // allocates nothing — and the ring itself allocates only once the tab
        // first prints something (see `transcript::Ring`).
        let transcript: Option<transcript::Shared> = (self.transcript_bytes > 0)
            .then(|| Arc::new(Mutex::new(Ring::new(self.transcript_bytes))));
        let output_tap: Option<egui_term::OutputTap> = transcript.clone().map(|ring| {
            Arc::new(move |bytes: &[u8]| transcript::lock(&ring).push(bytes))
                as egui_term::OutputTap
        });
        let mut backend = TerminalBackend::new(
            id,
            self.ctx.clone(),
            self.pty_events.clone(),
            BackendSettings {
                shell,
                args,
                working_directory: cwd.map(PathBuf::from).or_else(default_cwd),
                output_tap,
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
            let typed = command_line(&typed_into, command);
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
                transcript,
                spawn: command.join(" "),
                title: Title {
                    shell: format!("terra {id}"),
                    custom: title,
                },
            },
        );
        {
            // New tabs land in the focused *window's* focused group, right
            // after its active tab, and become that group's — and thus the
            // globally — active tab. A second window opening a tab therefore
            // keeps it, rather than posting it back to the root window.
            let mut windows = self.windows.borrow_mut();
            let focused_window = self.focused_window.get();
            // The focused id always names a live window; index 0 only covers
            // a bug.
            let at = windows
                .iter()
                .position(|w| w.id == focused_window)
                .unwrap_or(0);
            let window = &mut windows[at];
            match window.tree.as_mut() {
                Some(root) => {
                    // The focused id always names a live leaf (`normalize`
                    // guarantees it); the first-leaf fallback only covers a bug.
                    let target = {
                        let mut refs = Vec::new();
                        collect_leaves(root, &mut refs);
                        refs.iter()
                            .find(|g| g.id == window.focused_leaf)
                            .or(refs.first())
                            .map(|g| g.id)
                            .expect("a tree has leaves")
                    };
                    let group = leaf_mut(root, target).expect("leaf just seen");
                    let at = group
                        .active
                        .and_then(|a| group.tab_ids.iter().position(|i| *i == a))
                        .map(|i| i + 1)
                        .unwrap_or(group.tab_ids.len());
                    group.tab_ids.insert(at, id);
                    group.active = Some(id);
                    window.focused_leaf = group.id;
                }
                None => {
                    // First tab of this window: a fresh root leaf.
                    let leaf = self.next_leaf.get();
                    self.next_leaf.set(leaf + 1);
                    window.tree = Some(Node {
                        weight: 1.0,
                        kind: NodeKind::Leaf(Group::of(leaf, id)),
                    });
                    window.focused_leaf = leaf;
                }
            }
            let id = window.id;
            self.focused_window.set(id);
        }
        self.sync_visibility();
        Ok(id)
    }

    /// Spawn a tab from a named profile — the chevron menu and the palette's
    /// `tab.new.<name>` entries. Unknown names error, naming the known ones,
    /// so a stale menu entry says so instead of opening a bare shell.
    pub fn open_profile(&mut self, name: &str) -> anyhow::Result<u64> {
        let profile = crate::config::resolve_profile(&self.profiles, name)
            .map_err(anyhow::Error::msg)?
            .clone();
        self.open(&profile.command, profile.cwd.as_deref(), profile.title)
    }

    /// Remove a tab (dropping its backend shuts the PTY down). A group whose
    /// last tab closes collapses with it, and a *window* whose last tab closes
    /// goes with it unless it is the only one left; the app quits when no tab
    /// is left anywhere (`is_empty`, checked by the frame loop).
    ///
    /// Every window is searched, not just the focused one: `terra kill` and a
    /// `PtyEvent::Exit` name a tab, and a tab is wherever it is.
    pub fn close(&mut self, id: u64) -> bool {
        if self.tabs.remove(&id).is_none() {
            return false;
        }
        let old_focus = self.focused_group();
        {
            let mut windows = self.windows.borrow_mut();
            for window in windows.iter_mut() {
                let Some(root) = window.tree.as_mut() else {
                    continue;
                };
                let src_leaf = {
                    let mut refs = Vec::new();
                    collect_leaves(root, &mut refs);
                    refs.iter().find(|g| g.tab_ids.contains(&id)).map(|g| g.id)
                };
                if let Some(src_leaf) = src_leaf {
                    let group = leaf_mut(root, src_leaf).expect("leaf just seen");
                    let idx = group.tab_ids.iter().position(|i| *i == id).unwrap();
                    group.tab_ids.remove(idx);
                    if group.active == Some(id) {
                        // Prefer the nearest tab to the left, else the first.
                        reactivate(group, idx);
                    }
                    break;
                }
            }
        }
        self.normalize(old_focus);
        self.sync_visibility();
        true
    }

    // (No `close_active` wrapper: the app resolves the active id itself, so
    // it can ask the confirm-close question before the close happens — see
    // `App::close_tab`.)

    /// Drop every tab and every window but the root one — the teardown the
    /// tests use, and the same shape [`Self::new`] hands out.
    pub fn clear(&mut self) {
        self.tabs.clear();
        self.held.set(None);
        *self.windows.borrow_mut() = vec![Window {
            id: 0,
            tree: None,
            focused_leaf: 0,
        }];
        self.focused_window.set(0);
    }

    /// Every group's active tab is on screen — in *every* window, since a
    /// background window is still drawing — so all of them stay "visible" to
    /// their backends; only the hidden tabs coast.
    fn sync_visibility(&self) {
        let shown: Vec<u64> = {
            let windows = self.windows.borrow();
            windows
                .iter()
                .flat_map(leaves_of)
                .filter_map(|g| g.active)
                .collect()
        };
        for (id, tab) in &self.tabs {
            tab.backend.set_visible(shown.contains(id));
        }
    }

    /// Make `id` its group's active tab *and* focus that group — and, when the
    /// tab lives in another window, that window — so `id` becomes the globally
    /// active tab (keyboard input, the IPC active flag).
    pub fn select(&mut self, id: u64) -> bool {
        let found = {
            let mut windows = self.windows.borrow_mut();
            let mut found = None;
            for window in windows.iter_mut() {
                let Some(root) = window.tree.as_mut() else {
                    continue;
                };
                let src_leaf = {
                    let mut refs = Vec::new();
                    collect_leaves(root, &mut refs);
                    refs.iter().find(|g| g.tab_ids.contains(&id)).map(|g| g.id)
                };
                if let Some(src_leaf) = src_leaf {
                    let group = leaf_mut(root, src_leaf).expect("leaf just seen");
                    group.active = Some(id);
                    window.focused_leaf = src_leaf;
                    found = Some(window.id);
                    break;
                }
            }
            found
        };
        let Some(win) = found else {
            return false;
        };
        self.focused_window.set(win);
        self.sync_visibility();
        true
    }

    /// Select the nth tab (0-based) in the focused group's bar order.
    pub fn select_nth(&mut self, n: usize) {
        if let Some(id) = self.group_tabs(self.focused_group()).get(n).copied() {
            self.select(id);
        }
    }

    pub fn select_next(&mut self) {
        self.step(1);
    }

    pub fn select_prev(&mut self) {
        self.step(-1);
    }

    /// Cycle within the focused group — each group has its own bar, so its
    /// bar order is what next/prev walk.
    fn step(&mut self, delta: isize) {
        let ids = self.group_tabs(self.focused_group());
        if ids.is_empty() {
            return;
        }
        let current = self
            .active_id()
            .and_then(|id| ids.iter().position(|i| *i == id))
            .unwrap_or(0) as isize;
        let len = ids.len() as isize;
        let next = (current + delta).rem_euclid(len) as usize;
        self.select(ids[next]);
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

    /// The command the tab was opened with — see [`Tab::spawn`].
    pub fn spawn(&self, id: u64) -> Option<&str> {
        self.tabs.get(&id).map(|t| t.spawn.as_str())
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

    /// Everything the tab's child has written, oldest byte first, capped at
    /// `[tabs] transcript_kb`.
    ///
    /// Two layers of `Option`, like [`Self::bidi`]: `None` is "no such tab",
    /// `Some(None)` is "this tab keeps no transcript" — the two need different
    /// answers on the wire, and only the caller knows how to word them.
    pub fn transcript(&self, id: u64) -> Option<Option<Vec<u8>>> {
        let tab = self.tabs.get(&id)?;
        Some(
            tab.transcript
                .as_ref()
                .map(|ring| transcript::lock(ring).snapshot()),
        )
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
    // Spawns a real PTY running `/bin/cat`, so it is Unix-only. The
    // manager logic under test is portable; only the fixture is not.
    #[cfg(unix)]
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
    // Spawns a real PTY running `/bin/cat`, so it is Unix-only. The
    // manager logic under test is portable; only the fixture is not.
    #[cfg(unix)]
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
        assert!(tabs.move_tab(ids[2], 0, 0));
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
    // Spawns a real PTY running `/bin/cat`, so it is Unix-only. The
    // manager logic under test is portable; only the fixture is not.
    #[cfg(unix)]
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
        assert!(tabs.move_tab(ids[1], 0, 99));
        assert_eq!(tabs.ids(), vec![ids[2], ids[3], ids[0], ids[1]]);
        assert!(!tabs.move_tab(ids[1], 0, 3));
        assert!(!tabs.move_tab(u64::MAX, 0, 0));
        assert!(!tabs.reorder(9, 0));

        let mut sorted = tabs.ids();
        sorted.sort_unstable();
        assert_eq!(sorted, ids);

        tabs.clear();
    }

    /// Closing keeps the order intact and hands focus to the left neighbour in
    /// *visual* order — after a drag that is not the neighbour by id.
    // Spawns a real PTY running `/bin/cat`, so it is Unix-only. The
    // manager logic under test is portable; only the fixture is not.
    #[cfg(unix)]
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
        assert!(tabs.move_tab(ids[2], 0, 0));
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

    /// A manager with `n` `/bin/cat` tabs, all in one group.
    #[cfg(unix)]
    fn manager_with(n: usize) -> (TabManager, Vec<u64>) {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut tabs = TabManager::new(egui::Context::default(), tx);
        let ids = (0..n)
            .map(|_| {
                tabs.open(&["/bin/cat".to_string()], None, None)
                    .expect("spawn /bin/cat")
            })
            .collect();
        (tabs, ids)
    }

    /// Splitting moves the tab into a fresh group beside its old one, focuses
    /// it, and shares the old column's width between the two.
    // Spawns real PTYs, so Unix-only; the group logic under test is portable.
    #[cfg(unix)]
    #[test]
    fn split_right_moves_the_tab_into_a_new_focused_group() {
        let (mut tabs, ids) = manager_with(3);
        assert_eq!(tabs.group_count(), 1);

        assert!(tabs.split_right(ids[1]));
        assert_eq!(tabs.group_count(), 2);
        assert_eq!(tabs.group_tabs(0), vec![ids[0], ids[2]]);
        assert_eq!(tabs.group_tabs(1), vec![ids[1]]);
        assert_eq!(tabs.focused_group(), 1);
        assert_eq!(tabs.group_active(1), Some(ids[1]));
        // The source group's own selection (the last-opened tab) is untouched
        // — the split took a different tab — and global active follows focus.
        assert_eq!(tabs.group_active(0), Some(ids[2]));
        assert_eq!(tabs.active_id(), Some(ids[1]));
        // The two columns share what the one had; the weights still sum to 1.
        let weights = tabs.group_weights();
        assert_eq!(weights.len(), 2);
        assert!((weights.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        assert!((weights[0] - weights[1]).abs() < 1e-5);

        tabs.clear();
    }

    /// `split_left` is the mirror image, and a lone tab cannot split at all.
    #[cfg(unix)]
    #[test]
    fn split_left_and_the_lone_tab_no_op() {
        let (mut tabs, ids) = manager_with(2);

        assert!(tabs.split_left(ids[1]));
        assert_eq!(tabs.group_tabs(0), vec![ids[1]]);
        assert_eq!(tabs.group_tabs(1), vec![ids[0]]);
        assert_eq!(tabs.focused_group(), 0);

        // Both groups now hold one tab each: nothing left to split away from.
        assert!(!tabs.split_right(ids[0]));
        assert!(!tabs.split_left(ids[1]));
        assert!(!tabs.split_right(u64::MAX));
        assert_eq!(tabs.group_count(), 2);

        tabs.clear();
    }

    /// Every tab lives in exactly one group; moving across groups keeps that
    /// true, activates the moved tab in its new group, and collapses a group
    /// whose last tab left.
    #[cfg(unix)]
    #[test]
    fn moving_a_tab_across_groups_keeps_the_invariants() {
        let (mut tabs, ids) = manager_with(3);
        assert!(tabs.split_right(ids[2]));
        // Groups: [0, 1] | [2], focused = 1.

        assert!(tabs.move_tab(ids[0], 1, 0));
        assert_eq!(tabs.group_tabs(0), vec![ids[1]]);
        assert_eq!(tabs.group_tabs(1), vec![ids[0], ids[2]]);
        assert_eq!(tabs.group_active(1), Some(ids[0]));
        // `move_tab` does not steal focus; the focused group merely changed
        // members. `select` is what follows a tab.
        assert_eq!(tabs.focused_group(), 1);

        // The last tab leaving group 0 collapses it, and the focused index
        // shifts down with the groups to its left disappearing.
        assert!(tabs.move_tab(ids[1], 1, 2));
        assert_eq!(tabs.group_count(), 1);
        assert_eq!(tabs.focused_group(), 0);
        assert_eq!(tabs.group_tabs(0), vec![ids[0], ids[2], ids[1]]);
        let weights = tabs.group_weights();
        assert!((weights[0] - 1.0).abs() < 1e-5);

        // Unknown target groups and unknown ids are refused.
        assert!(!tabs.move_tab(ids[0], 5, 0));
        assert!(!tabs.move_tab(u64::MAX, 0, 0));

        tabs.clear();
    }

    /// `select` focuses the tab's group; focus_group/next/prev walk columns.
    #[cfg(unix)]
    #[test]
    fn selecting_a_tab_focuses_its_group() {
        let (mut tabs, ids) = manager_with(3);
        assert!(tabs.split_right(ids[2]));
        assert_eq!(tabs.focused_group(), 1);

        // Selecting a tab in the other group moves focus there with it.
        assert!(tabs.select(ids[0]));
        assert_eq!(tabs.focused_group(), 0);
        assert_eq!(tabs.active_id(), Some(ids[0]));
        // The unfocused group keeps its own active tab.
        assert_eq!(tabs.group_active(1), Some(ids[2]));

        // Explicit group focus flips the globally active tab without touching
        // any group's own selection.
        assert!(tabs.focus_group(1));
        assert_eq!(tabs.active_id(), Some(ids[2]));
        assert!(!tabs.focus_group(9));
        assert_eq!(tabs.focused_group(), 1);

        tabs.next_group();
        assert_eq!(tabs.focused_group(), 0);
        tabs.prev_group();
        assert_eq!(tabs.focused_group(), 1);

        // ⌘n and next/prev act on the focused group's bar.
        tabs.focus_group(0);
        tabs.select_nth(1);
        assert_eq!(tabs.active_id(), Some(ids[1]));
        tabs.select_next();
        assert_eq!(tabs.active_id(), Some(ids[0]));

        tabs.clear();
    }

    /// New tabs open in the focused group, right after its active tab.
    #[cfg(unix)]
    #[test]
    fn new_tabs_open_after_the_focused_groups_active_tab() {
        let (mut tabs, ids) = manager_with(3);
        assert!(tabs.split_right(ids[1]));
        // Groups: [0, 2] | [1], focused = 1.

        let in_split = tabs
            .open(&["/bin/cat".to_string()], None, None)
            .expect("spawn /bin/cat");
        assert_eq!(tabs.group_tabs(1), vec![ids[1], in_split]);
        assert_eq!(tabs.active_id(), Some(in_split));

        // Back in group 0 with its first tab active: the new tab lands right
        // after it, not at the end.
        assert!(tabs.select(ids[0]));
        let after_first = tabs
            .open(&["/bin/cat".to_string()], None, None)
            .expect("spawn /bin/cat");
        assert_eq!(tabs.group_tabs(0), vec![ids[0], after_first, ids[2]]);

        tabs.clear();
    }

    /// Closing the last tab of a group collapses the group and hands focus to
    /// a neighbour; closing the last tab of all empties the manager (the app
    /// quits on that).
    #[cfg(unix)]
    #[test]
    fn closing_a_groups_last_tab_collapses_the_group() {
        let (mut tabs, ids) = manager_with(2);
        assert!(tabs.split_right(ids[1]));
        assert_eq!(tabs.group_count(), 2);

        assert!(tabs.close(ids[1]));
        assert_eq!(tabs.group_count(), 1);
        assert_eq!(tabs.focused_group(), 0);
        assert_eq!(tabs.active_id(), Some(ids[0]));
        let weights = tabs.group_weights();
        assert!((weights[0] - 1.0).abs() < 1e-5);

        assert!(tabs.close(ids[0]));
        assert!(tabs.is_empty());
        assert_eq!(tabs.group_count(), 0);
        assert_eq!(tabs.active_id(), None);
    }

    /// `split_down` stacks; a second split on the same axis merges into the
    /// existing run instead of nesting (VS Code behaviour), and the spliced
    /// pair share what the one leaf had.
    #[cfg(unix)]
    #[test]
    fn same_axis_splits_merge_into_one_run() {
        let (mut tabs, ids) = manager_with(3);

        assert!(tabs.split_down(ids[1]));
        assert_eq!(
            tabs.shape(),
            format!("v([{},{}] [{}])", ids[0], ids[2], ids[1])
        );

        // Splitting the top leaf down again wraps it in a nested vertical
        // split — which normalisation splices into the parent, flat.
        assert!(tabs.split_down(ids[2]));
        assert_eq!(
            tabs.shape(),
            format!("v([{}] [{}] [{}])", ids[0], ids[2], ids[1])
        );
        // The first leaf's share was halved; the untouched leaf kept its.
        let weights = tabs.split_weights(&[]);
        assert_eq!(weights.len(), 3);
        assert!((weights[0] - 0.25).abs() < 1e-5);
        assert!((weights[1] - 0.25).abs() < 1e-5);
        assert!((weights[2] - 0.5).abs() < 1e-5);

        tabs.clear();
    }

    /// Mixed axes nest: a split down inside a column of a split right makes
    /// a 2D grid, and the DFS order of the leaves is the group order.
    #[cfg(unix)]
    #[test]
    fn mixed_axis_splits_nest_and_dfs_order_is_the_group_order() {
        let (mut tabs, ids) = manager_with(4);

        assert!(tabs.split_right(ids[1]));
        assert!(tabs.split_down(ids[3]));
        // ids[3] split away from the left column, which nests vertically.
        assert_eq!(
            tabs.shape(),
            format!("h(v([{},{}] [{}]) [{}])", ids[0], ids[2], ids[3], ids[1])
        );

        // DFS order: top-left, bottom-left, right.
        assert_eq!(tabs.group_count(), 3);
        assert_eq!(tabs.group_tabs(0), vec![ids[0], ids[2]]);
        assert_eq!(tabs.group_tabs(1), vec![ids[3]]);
        assert_eq!(tabs.group_tabs(2), vec![ids[1]]);
        // ...and it is exactly what next/prev walk, wrapping.
        assert_eq!(tabs.focused_group(), 1);
        tabs.next_group();
        assert_eq!(tabs.focused_group(), 2);
        tabs.next_group();
        assert_eq!(tabs.focused_group(), 0);
        tabs.prev_group();
        assert_eq!(tabs.focused_group(), 2);

        // The layout tree the renderer gets mirrors the shape.
        use super::LayoutNode::{Leaf, Split};
        match tabs.layout() {
            Some(Split {
                axis: Axis::Horizontal,
                children,
                ..
            }) => {
                assert_eq!(children.len(), 2);
                match &children[0] {
                    Split {
                        axis: Axis::Vertical,
                        children,
                        ..
                    } => {
                        assert_eq!(children, &vec![Leaf(0), Leaf(1)]);
                    }
                    other => panic!("left child should be a vertical split, got {other:?}"),
                }
                assert_eq!(children[1], Leaf(2));
            }
            other => panic!("root should be a horizontal split, got {other:?}"),
        }

        tabs.clear();
    }

    /// Removing a leaf folds its weight into a sibling, and a split left with
    /// one child collapses into that child — all the way to a lone leaf.
    #[cfg(unix)]
    #[test]
    fn closing_folds_weights_and_collapses_single_child_splits() {
        let (mut tabs, ids) = manager_with(4);
        assert!(tabs.split_right(ids[1]));
        assert!(tabs.split_down(ids[3]));
        // h(v([0,2] [3]) [1]), weights: left column 0.5, right 0.5.

        // Closing the right column's only tab leaves the h-split with one
        // child: the vertical pair takes over as the root.
        assert!(tabs.close(ids[1]));
        assert_eq!(
            tabs.shape(),
            format!("v([{},{}] [{}])", ids[0], ids[2], ids[3])
        );
        let weights = tabs.split_weights(&[]);
        assert!((weights[0] - 0.5).abs() < 1e-5);
        assert!((weights[1] - 0.5).abs() < 1e-5);

        // Growing the top leaf, then closing the bottom one: the survivor
        // absorbs the closed leaf's share (the root collapses to a leaf, so
        // the fold shows as the whole window).
        assert!(tabs.set_split_weights(&[], &[0.7, 0.3]));
        assert!(tabs.close(ids[3]));
        assert_eq!(tabs.shape(), format!("[{},{}]", ids[0], ids[2]));
        assert_eq!(tabs.group_weights(), vec![1.0]);
        // A leaf root has no split to address.
        assert!(tabs.split_weights(&[]).is_empty());
        assert!(!tabs.set_split_weights(&[], &[1.0]));

        tabs.clear();
    }

    /// Weight folding *within* a surviving split: the sibling before the
    /// removed leaf inherits its share, and the others keep theirs.
    #[cfg(unix)]
    #[test]
    fn a_removed_leaf_folds_its_weight_into_the_sibling_before_it() {
        let (mut tabs, ids) = manager_with(4);
        assert!(tabs.split_right(ids[1]));
        assert!(tabs.split_right(ids[2]));
        assert!(tabs.split_right(ids[3]));
        // h([0] [3] [2] [1]) — every split halved its source's share.
        assert_eq!(
            tabs.shape(),
            format!("h([{}] [{}] [{}] [{}])", ids[0], ids[3], ids[2], ids[1])
        );
        assert!(tabs.set_split_weights(&[], &[0.4, 0.3, 0.2, 0.1]));

        assert!(tabs.close(ids[2]));
        let weights = tabs.split_weights(&[]);
        assert_eq!(weights.len(), 3);
        assert!((weights[0] - 0.4).abs() < 1e-5);
        assert!((weights[1] - 0.5).abs() < 1e-5, "0.3 absorbed 0.2");
        assert!((weights[2] - 0.1).abs() < 1e-5);

        // The *first* leaf going folds forward instead: nobody sits before it.
        assert!(tabs.close(ids[0]));
        let weights = tabs.split_weights(&[]);
        assert_eq!(weights.len(), 2);
        assert!((weights[0] - 0.9).abs() < 1e-5);
        assert!((weights[1] - 0.1).abs() < 1e-5);

        tabs.clear();
    }

    /// Focus tracks the leaf, not its DFS index: reshaping the tree elsewhere
    /// leaves the focused group focused, and only the focused leaf itself
    /// collapsing moves focus (to the leaf now at its old index).
    #[cfg(unix)]
    #[test]
    fn focus_rides_the_leaf_through_tree_reshapes() {
        let (mut tabs, ids) = manager_with(4);
        assert!(tabs.split_right(ids[1]));
        assert!(tabs.select(ids[1])); // focus the rightmost leaf
        assert_eq!(tabs.focused_group(), 1);

        // A split in the *other* column shifts that leaf's DFS index up by
        // one; focus stays on the same leaf, at its new index.
        assert!(tabs.split_down(ids[3]));
        assert_eq!(tabs.focused_group(), 1); // the new [3] leaf took focus
        assert!(tabs.select(ids[1]));
        assert_eq!(tabs.focused_group(), 2);
        assert!(tabs.split_up(ids[2]));
        // shape: h(v([2] [0] [3]) [1]) — the focused leaf slid to index 3...
        assert!(tabs.select(ids[1]));
        assert_eq!(tabs.focused_group(), 3);
        assert_eq!(tabs.active_id(), Some(ids[1]));

        // ...and closing tabs before it keeps it focused throughout.
        assert!(tabs.close(ids[0]));
        assert_eq!(tabs.active_id(), Some(ids[1]));

        // The focused leaf collapsing hands focus to the leaf at its old
        // DFS index, clamped: here the last one.
        assert!(tabs.close(ids[1]));
        assert_eq!(tabs.focused_group(), tabs.group_count() - 1);
        assert!(tabs.active_id().is_some());

        tabs.clear();
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

    /// `-l` is not a flag PowerShell or cmd shrug off — it turns the first tab
    /// into an error message — so the Windows shells must never receive it.
    #[test]
    fn the_windows_shells_are_never_started_as_login_shells() {
        for shell in [
            "powershell.exe",
            "pwsh.exe",
            "cmd.exe",
            r"C:\Windows\system32\cmd.exe",
            r"C:\Program Files\PowerShell\7\pwsh.exe",
            // Windows filenames are case-insensitive, so the match has to be.
            r"C:\Windows\System32\WindowsPowerShell\v1.0\POWERSHELL.EXE",
        ] {
            assert!(login_args(shell).is_empty(), "{shell}");
        }
    }

    /// Normalisation is what lets one table cover both platforms: strip the
    /// directory (either separator), the case, and the extension.
    #[test]
    fn a_shell_path_reduces_to_a_bare_lowercase_name() {
        assert_eq!(shell_name("/bin/zsh"), "zsh");
        assert_eq!(shell_name("zsh"), "zsh");
        assert_eq!(shell_name(r"C:\Windows\system32\cmd.exe"), "cmd");
        assert_eq!(shell_name("C:/Program Files/PowerShell/7/pwsh.exe"), "pwsh");
        assert_eq!(shell_name("POWERSHELL.EXE"), "powershell");
        // Only a trailing `.exe` is an extension; a dot elsewhere is a name.
        assert_eq!(shell_name("/usr/bin/python3.11"), "python3.11");
    }

    /// Which dialect a shell speaks decides how its arguments are quoted.
    #[test]
    fn each_shell_is_matched_to_its_quoting_dialect() {
        for shell in [
            "/bin/zsh",
            "/bin/bash",
            "/usr/bin/fish",
            "/usr/local/bin/nu",
        ] {
            assert_eq!(shell_family(shell), ShellFamily::Posix, "{shell}");
        }
        for shell in ["powershell.exe", "pwsh.exe", r"C:\pwsh\PWSH.EXE"] {
            assert_eq!(shell_family(shell), ShellFamily::PowerShell, "{shell}");
        }
        assert_eq!(shell_family(r"C:\Windows\cmd.exe"), ShellFamily::Cmd);
    }

    /// Terra types the command into a real shell, so an argument that carries a
    /// space or a quote has to survive that shell's own parser — and the three
    /// dialects disagree about how to escape the quote character itself.
    #[test]
    fn an_argument_is_quoted_the_way_its_own_shell_expects() {
        use ShellFamily::{Cmd, Posix, PowerShell};

        // Ordinary words pass through untouched in every dialect.
        for family in [Posix, PowerShell, Cmd] {
            assert_eq!(shell_quote_for(family, "git"), "git");
            assert_eq!(shell_quote_for(family, "--depth=1"), "--depth=1");
        }

        // A space forces quoting everywhere.
        assert_eq!(shell_quote_for(Posix, "hello world"), "'hello world'");
        assert_eq!(shell_quote_for(PowerShell, "hello world"), "'hello world'");
        assert_eq!(shell_quote_for(Cmd, "hello world"), "\"hello world\"");

        // The embedded quote is where they part company.
        assert_eq!(shell_quote_for(Posix, "it's"), r"'it'\''s'");
        assert_eq!(shell_quote_for(PowerShell, "it's"), "'it''s'");
        assert_eq!(shell_quote_for(Cmd, "say \"hi\""), "\"say \"\"hi\"\"\"");

        // Empty stays representable rather than vanishing from the line.
        assert_eq!(shell_quote_for(Posix, ""), "''");
        assert_eq!(shell_quote_for(Cmd, ""), "\"\"");

        // PowerShell's sigils only look inert: `@` splats, `%` is an alias
        // and `,` builds an array, so they are quoted where POSIX leaves them
        // bare. `:` is the exception — see `shell_quote_for`.
        assert_eq!(shell_quote_for(Posix, "a@b"), "a@b");
        assert_eq!(shell_quote_for(PowerShell, "a@b"), "'a@b'");
        assert_eq!(shell_quote_for(PowerShell, "50%"), "'50%'");
        // Backslash paths must not be quoted into oblivion on Windows.
        assert_eq!(shell_quote_for(PowerShell, r"C:\src"), r"C:\src");
    }

    /// The whole typed line, per shell.
    #[test]
    fn a_command_line_is_assembled_for_the_shell_that_will_read_it() {
        let cmd: Vec<String> = ["git", "commit", "-m", "hello world"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            command_line("/bin/zsh", &cmd),
            "git commit -m 'hello world'"
        );
        assert_eq!(
            command_line("pwsh.exe", &cmd),
            "git commit -m 'hello world'"
        );
        assert_eq!(
            command_line("cmd.exe", &cmd),
            "git commit -m \"hello world\""
        );
    }

    /// A quoted first word is a *string literal* to PowerShell — it would echo
    /// the path instead of running it — so the call operator has to lead.
    #[test]
    fn powershell_gets_the_call_operator_when_the_program_needs_quoting() {
        let spaced = vec![
            r"C:\Program Files\tool\run.exe".to_string(),
            "-v".to_string(),
        ];
        assert_eq!(
            command_line("pwsh.exe", &spaced),
            r"& 'C:\Program Files\tool\run.exe' -v"
        );
        // A bare program name is left alone: `& git` and `git` are the same
        // command, and the shorter one is what belongs in the scrollback.
        let bare = vec!["git".to_string(), "status".to_string()];
        assert_eq!(command_line("pwsh.exe", &bare), "git status");
        // No other shell has the operator, so no other shell gets it.
        assert_eq!(
            command_line("/bin/zsh", &spaced),
            r"'C:\Program Files\tool\run.exe' -v"
        );
    }

    /// PowerShell first because it is the shell people want, `%COMSPEC%` last
    /// because it always exists and always says `cmd.exe` regardless of what
    /// the user would have chosen.
    #[test]
    fn the_windows_shell_prefers_powershell_and_falls_back_to_comspec() {
        let comspec = |key: &str| match key {
            "COMSPEC" => Some(r"C:\Windows\system32\cmd.exe".to_string()),
            _ => None,
        };
        let nothing = |_: &str| None;

        // Both PowerShells present: the newer one wins.
        let all = |_: &str| true;
        assert_eq!(windows_shell(&comspec, &all), "pwsh.exe");

        // Only Windows PowerShell 5, the in-the-box case.
        let boxed = |program: &str| program == "powershell.exe";
        assert_eq!(windows_shell(&comspec, &boxed), "powershell.exe");

        // A stripped image with no PowerShell at all still gets a shell.
        let none = |_: &str| false;
        assert_eq!(
            windows_shell(&comspec, &none),
            r"C:\Windows\system32\cmd.exe"
        );

        // ...and one that has not even got `%COMSPEC%` gets the literal name,
        // which `CreateProcess` resolves through `%PATH%`.
        assert_eq!(windows_shell(&nothing, &none), "cmd.exe");
        let empty = |_: &str| Some(String::new());
        assert_eq!(windows_shell(&empty, &none), "cmd.exe");
    }

    /// An explicit cwd always wins; the fallback only fills in the blank.
    #[test]
    fn an_explicit_directory_is_never_overridden_by_the_default() {
        let explicit = Some(PathBuf::from("/tmp"));
        assert_eq!(explicit.clone().or_else(default_cwd), explicit);
    }

    /// The home directory is spelled differently per platform, and on Windows
    /// a `HOME` that *is* set is usually a POSIX path no Win32 call can open.
    #[test]
    fn the_home_directory_is_found_under_whichever_name_the_platform_uses() {
        use std::ffi::OsString;
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| OsString::from(*v))
            }
        };

        // Unix: `HOME`, and nothing else is even looked at.
        let unix = env(&[("HOME", "/Users/me"), ("USERPROFILE", r"C:\Users\me")]);
        let expected = if cfg!(windows) {
            PathBuf::from(r"C:\Users\me")
        } else {
            PathBuf::from("/Users/me")
        };
        assert_eq!(home_dir(&unix), Some(expected));

        // Nothing set at all is "no opinion", not an empty path.
        assert_eq!(home_dir(&env(&[])), None);
        assert_eq!(home_dir(&env(&[("HOME", ""), ("USERPROFILE", "")])), None);
    }

    /// The Windows-only half of the home lookup, exercised on every platform by
    /// calling the fallback chain directly — `home_dir` itself is `cfg!`-gated
    /// and so only reaches these branches on Windows.
    #[test]
    fn windows_falls_back_from_userprofile_to_homedrive_and_only_then_to_home() {
        use std::ffi::OsString;
        // The chain, written out the way `home_dir` walks it, so the ordering
        // is asserted rather than assumed.
        let resolve = |pairs: &[(&str, &str)]| -> Option<PathBuf> {
            let var = |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| OsString::from(*v))
                    .filter(|v| !v.is_empty())
            };
            var("USERPROFILE")
                .map(PathBuf::from)
                .or_else(|| {
                    let mut joined = var("HOMEDRIVE")?;
                    joined.push(var("HOMEPATH")?);
                    Some(PathBuf::from(joined))
                })
                .or_else(|| var("HOME").map(PathBuf::from))
        };

        assert_eq!(
            resolve(&[("USERPROFILE", r"C:\Users\me"), ("HOME", "/c/Users/me")]),
            Some(PathBuf::from(r"C:\Users\me"))
        );
        // `%HOMEPATH%` is drive-relative and starts with a separator, so the
        // two halves are concatenated, never `join`ed.
        assert_eq!(
            resolve(&[("HOMEDRIVE", "C:"), ("HOMEPATH", r"\Users\me")]),
            Some(PathBuf::from(r"C:\Users\me"))
        );
        // A half-set pair is not a home directory.
        assert_eq!(resolve(&[("HOMEDRIVE", "C:")]), None);
        // MSYS's POSIX `HOME` is the last resort, never the first choice.
        assert_eq!(
            resolve(&[("HOME", "/c/Users/me")]),
            Some(PathBuf::from("/c/Users/me"))
        );
    }
}
