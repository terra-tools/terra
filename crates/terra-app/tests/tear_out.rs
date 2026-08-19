//! Tearing a tab out into its own OS window: the model semantics only.
//!
//! The window layer is a second dimension on top of editor groups — every
//! window owns a split tree, tabs stay in the one global map, and exactly one
//! tab is active across all of them. These tests pin that contract at the
//! `TabManager` level; the viewport rendering that draws each window is
//! `App::ui`'s business and is not reachable from a test binary (see
//! `groups.rs` for the same split).
//!
//! `terra-app` is a binary crate, so the manager is pulled in by path — the
//! same `tabs.rs` the app compiles, together with the two things it reaches
//! for at the crate root (`config` and `terminal_theme`).
//!
//! Real PTYs are spawned (`/bin/cat`), which makes the whole file Unix-only
//! like `groups.rs`.
#![cfg(unix)]
// The included modules carry plenty the tests here never call.
#![allow(dead_code)]

#[path = "../src/config.rs"]
mod config;
#[path = "../src/ghostty_theme.rs"]
mod ghostty_theme;
#[path = "../src/tabs.rs"]
mod tabs;
#[path = "../src/transcript.rs"]
mod transcript;

use tabs::TabManager;

/// `tabs.rs` calls `crate::terminal_theme()`; in this test crate, this is it —
/// the same construction `main.rs` uses.
fn terminal_theme() -> egui_term::TerminalTheme {
    egui_term::TerminalTheme::new(Box::new(ghostty_theme::palette()))
}

/// A manager with `n` `/bin/cat` tabs, all in one group of the one window.
fn manager_with(ctx: &egui::Context, n: usize) -> (TabManager, Vec<u64>) {
    let (tx, rx) = std::sync::mpsc::channel();
    // The sender must stay connected for the life of the backends; nothing
    // reads the events, so the receiver is parked in a leaked box.
    Box::leak(Box::new(rx));
    let mut tabs = TabManager::new(ctx.clone(), tx);
    let ids = (0..n)
        .map(|_| {
            tabs.open(&["/bin/cat".to_string()], None, None)
                .expect("spawn /bin/cat")
        })
        .collect();
    (tabs, ids)
}

/// The tabs of one window, in no particular order.
fn tabs_of_window(tabs: &TabManager, window: u64) -> Vec<u64> {
    let mut ids: Vec<u64> = tabs
        .ids()
        .into_iter()
        .filter(|id| tabs.window_of_tab(*id) == Some(window))
        .collect();
    ids.sort_unstable();
    ids
}

/// The invariants the window layer adds to the group ones:
/// - every open tab belongs to exactly one *existing* window;
/// - no window is empty, and every window has a layout;
/// - `focused_window` names a real window, and the globally active tab lives
///   in it — so `infos()` still marks exactly one row for IPC;
/// - the focused window's group APIs describe that window and nothing else.
fn assert_invariants(tabs: &TabManager) {
    let windows = tabs.window_ids();
    let mut sorted = windows.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), windows.len(), "a window id appears twice");

    // The last window is never removed: an empty manager still reports the
    // root window (its empty state is the quit condition), so `window_ids`
    // never goes below one.
    let ids = tabs.ids();
    assert!(!windows.is_empty(), "the last window vanished");
    if ids.is_empty() {
        assert_eq!(windows.len(), 1, "an empty manager keeps one window only");
    }

    for id in &ids {
        let window = tabs
            .window_of_tab(*id)
            .unwrap_or_else(|| panic!("tab {id} belongs to no window"));
        assert!(
            windows.contains(&window),
            "tab {id} names window {window}, which does not exist"
        );
    }
    for window in &windows {
        if ids.is_empty() {
            // The lone surviving window is allowed to sit empty, exactly as
            // an empty manager does — and an empty window has no layout.
            continue;
        }
        assert!(
            !tabs_of_window(tabs, *window).is_empty(),
            "empty window {window} survived"
        );
        assert!(
            tabs.window_layout(*window).is_some(),
            "window {window} has no layout"
        );
    }

    if ids.is_empty() {
        assert_eq!(tabs.active_id(), None);
        assert!(tabs.infos().iter().all(|i| !i.active));
        return;
    }

    let focused = tabs.focused_window();
    assert!(
        windows.contains(&focused),
        "focused window {focused} does not exist"
    );
    let active = tabs.active_id().expect("tabs remain, one must be active");
    assert_eq!(
        tabs.window_of_tab(active),
        Some(focused),
        "the globally active tab lives outside the focused window"
    );
    let actives: Vec<u64> = tabs
        .infos()
        .iter()
        .filter(|i| i.active)
        .map(|i| i.id)
        .collect();
    assert_eq!(actives, vec![active], "exactly one IPC-active tab");

    // The group APIs are a view of the focused window alone.
    let count = tabs.group_count();
    assert!(count > 0, "the focused window has no groups");
    assert!(tabs.focused_group() < count, "focused group out of range");
    let mut union: Vec<u64> = Vec::new();
    for group in 0..count {
        let members = tabs.group_tabs(group);
        assert!(!members.is_empty(), "empty group {group} survived");
        let group_active = tabs
            .group_active(group)
            .unwrap_or_else(|| panic!("group {group} has no active tab"));
        assert!(
            members.contains(&group_active),
            "group {group}'s active tab {group_active} is not one of its own"
        );
        union.extend(members);
    }
    union.sort_unstable();
    assert_eq!(
        union,
        tabs_of_window(tabs, focused),
        "the focused window's groups disagree with its tabs"
    );
    assert_eq!(
        tabs.group_active(tabs.focused_group()),
        Some(active),
        "the focused group's active tab is the global one"
    );
}

/// Tearing a tab out gives it a window of its own — one group, that one tab,
/// focused — and takes it out of the window it came from without disturbing
/// what stayed behind.
#[test]
fn tearing_a_tab_out_gives_it_a_focused_window_of_its_own() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 3);
    assert_invariants(&tabs);

    let root = tabs.focused_window();
    assert_eq!(tabs.window_ids(), vec![root], "one window to start with");

    let torn = tabs
        .move_tab_to_new_window(ids[0])
        .expect("a tab with siblings can always be torn out");
    assert_ne!(torn, root, "the tear-out made a new window");
    assert_eq!(tabs.window_ids().len(), 2);
    assert_invariants(&tabs);

    // The new window is focused and holds exactly the torn tab.
    assert_eq!(tabs.focused_window(), torn);
    assert_eq!(tabs.window_of_tab(ids[0]), Some(torn));
    assert_eq!(tabs_of_window(&tabs, torn), vec![ids[0]]);
    assert_eq!(tabs.group_count(), 1, "one group in the new window");
    assert_eq!(tabs.group_tabs(0), vec![ids[0]]);
    assert_eq!(tabs.group_active(0), Some(ids[0]));
    // ...and it is the globally active tab: the keyboard followed it out.
    assert_eq!(tabs.active_id(), Some(ids[0]));

    // The window it left keeps the rest, and no id was renumbered.
    assert_eq!(tabs_of_window(&tabs, root), vec![ids[1], ids[2]]);

    tabs.clear();
}

/// The sole tab of the sole window has nowhere to go: tearing it out would
/// leave an empty window behind and move the tab nowhere, so it is refused —
/// `None`, and not a byte of state changes.
#[test]
fn tearing_out_the_sole_tab_of_the_sole_window_is_a_no_op() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 1);
    let root = tabs.focused_window();

    assert_eq!(tabs.move_tab_to_new_window(ids[0]), None);
    assert_eq!(tabs.window_ids(), vec![root]);
    assert_eq!(tabs.window_of_tab(ids[0]), Some(root));
    assert_eq!(tabs.active_id(), Some(ids[0]));
    assert_invariants(&tabs);

    // An unknown tab is refused the same way.
    assert_eq!(tabs.move_tab_to_new_window(u64::MAX), None);
    assert_invariants(&tabs);

    // But the *last tab of a non-last window* is not the sole tab of the sole
    // window: with a second window open, a lone tab may still move out.
    let (mut tabs, ids) = manager_with(&ctx, 2);
    let torn = tabs
        .move_tab_to_new_window(ids[0])
        .expect("two tabs, one window");
    assert_eq!(tabs_of_window(&tabs, torn), vec![ids[0]]);
    // `ids[0]` is now alone in its window, but another window exists, so
    // tearing it out again is still legal — it just lands in a third window
    // and the second one removes itself.
    let again = tabs
        .move_tab_to_new_window(ids[0])
        .expect("another window exists, so this is not the sole tab of the sole window");
    assert_ne!(again, torn);
    assert!(
        !tabs.window_ids().contains(&torn),
        "the emptied window is gone"
    );
    assert_eq!(tabs.window_ids().len(), 2);
    assert_invariants(&tabs);

    tabs.clear();
}

/// Exactly one tab is active across every window, and `focus_window` is what
/// moves it: focusing a window makes that window's focused group's active tab
/// the global one, which is the tab `terra ls` marks and the keyboard reaches.
#[test]
fn the_globally_active_tab_follows_focus_window() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 3);
    let root = tabs.focused_window();
    // Give the root window two groups, so "the window's active tab" is really
    // "the window's focused group's active tab".
    assert!(tabs.split_right(ids[2]));
    let root_active = tabs.active_id().expect("a tab is active");

    let torn = tabs.move_tab_to_new_window(ids[0]).expect("tearable");
    assert_eq!(tabs.active_id(), Some(ids[0]));

    // Back to the root window: its own active tab takes over again, and the
    // torn tab stops being active without moving anywhere.
    tabs.focus_window(root);
    assert_eq!(tabs.focused_window(), root);
    assert_eq!(tabs.active_id(), Some(root_active));
    assert_eq!(tabs.window_of_tab(ids[0]), Some(torn));
    assert_invariants(&tabs);

    // ...and back again. Focus is remembered per window.
    tabs.focus_window(torn);
    assert_eq!(tabs.active_id(), Some(ids[0]));
    assert_invariants(&tabs);

    // Focusing a window that does not exist changes nothing.
    tabs.focus_window(u64::MAX);
    assert_eq!(tabs.focused_window(), torn);
    assert_eq!(tabs.active_id(), Some(ids[0]));
    assert_invariants(&tabs);

    tabs.clear();
}

/// A non-last window that loses its last tab removes itself — whether the tab
/// was closed or moved away — and focus lands on a survivor. The app quits
/// only when no tabs remain anywhere.
#[test]
fn an_emptied_window_removes_itself() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 3);
    let root = tabs.focused_window();

    // Closing the torn tab empties its window.
    let _torn = tabs.move_tab_to_new_window(ids[0]).expect("tearable");
    assert!(tabs.close(ids[0]));
    assert_eq!(tabs.window_ids(), vec![root], "the emptied window is gone");
    assert_eq!(tabs.focused_window(), root);
    assert!(!tabs.is_empty(), "two tabs remain: no quit");
    assert_invariants(&tabs);

    // Moving the torn tab back empties its window just the same.
    let torn = tabs.move_tab_to_new_window(ids[1]).expect("tearable");
    tabs.move_tab_to_window(ids[1], root);
    assert_eq!(tabs.window_ids(), vec![root]);
    assert!(!tabs.window_ids().contains(&torn));
    assert_eq!(tabs.window_of_tab(ids[1]), Some(root));
    assert_invariants(&tabs);

    // The last window is not removed when it empties — the app quits instead.
    for id in [ids[1], ids[2]] {
        assert!(tabs.close(id));
        assert_invariants(&tabs);
    }
    assert!(tabs.is_empty(), "the quit condition");
    assert_eq!(tabs.window_ids().len(), 1, "the last window sits empty");
    assert_eq!(tabs.active_id(), None);
}

/// `move_tab_to_window` is the way back: it lands the tab in the target
/// window's focused group, keeping its global id, and leaves the *focused*
/// window alone (like `move_tab`, it is a membership change, not a selection).
#[test]
fn move_tab_to_window_lands_the_tab_in_that_window() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 3);
    let root = tabs.focused_window();

    let torn = tabs.move_tab_to_new_window(ids[0]).expect("tearable");
    // A second tab joins the torn window, so moving one out does not remove it.
    tabs.move_tab_to_window(ids[1], torn);
    assert_eq!(tabs.window_of_tab(ids[1]), Some(torn));
    assert_eq!(tabs_of_window(&tabs, torn), vec![ids[0], ids[1]]);
    assert_eq!(tabs_of_window(&tabs, root), vec![ids[2]]);
    assert_invariants(&tabs);

    // The torn window is still the focused one, and its tabs are what the
    // group APIs describe.
    assert_eq!(tabs.focused_window(), torn);
    let mut union: Vec<u64> = (0..tabs.group_count())
        .flat_map(|g| tabs.group_tabs(g))
        .collect();
    union.sort_unstable();
    assert_eq!(union, vec![ids[0], ids[1]]);

    // Send one back; ids are untouched by every hop.
    tabs.move_tab_to_window(ids[1], root);
    assert_eq!(tabs.window_of_tab(ids[1]), Some(root));
    let mut all = tabs.ids();
    all.sort_unstable();
    assert_eq!(all, vec![ids[0], ids[1], ids[2]]);
    assert_invariants(&tabs);

    // Refused moves leave the state untouched: an unknown tab, an unknown
    // window, and a tab already in the target.
    tabs.move_tab_to_window(u64::MAX, root);
    tabs.move_tab_to_window(ids[0], u64::MAX);
    tabs.move_tab_to_window(ids[2], root);
    assert_eq!(tabs.window_of_tab(ids[0]), Some(torn));
    assert_eq!(tabs.window_of_tab(ids[2]), Some(root));
    assert_invariants(&tabs);

    tabs.clear();
}

/// `close_window` hands back the tabs it took, so the caller can tell what a
/// "Close Window?" actually killed — and closing a non-last window leaves the
/// app running on the rest.
#[test]
fn close_window_returns_the_tabs_it_took() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 4);
    let root = tabs.focused_window();

    let torn = tabs.move_tab_to_new_window(ids[0]).expect("tearable");
    tabs.move_tab_to_window(ids[1], torn);

    // Closing the *focused* window: its two tabs come back, the survivors
    // stay open, and focus lands on the window that is left.
    let mut closed = tabs.close_window(torn);
    closed.sort_unstable();
    assert_eq!(closed, vec![ids[0], ids[1]]);
    assert_eq!(tabs.window_ids(), vec![root]);
    assert_eq!(tabs.focused_window(), root);
    assert_eq!(tabs_of_window(&tabs, root), vec![ids[2], ids[3]]);
    assert!(!tabs.is_empty(), "the app survives a closed window");
    assert_invariants(&tabs);

    // Closing a window that does not exist takes nothing.
    assert!(tabs.close_window(u64::MAX).is_empty());
    assert_invariants(&tabs);

    // Closing the last window empties the app: the quit condition, reached
    // through the window door rather than the tab one.
    let mut closed = tabs.close_window(root);
    closed.sort_unstable();
    assert_eq!(closed, vec![ids[2], ids[3]]);
    assert!(tabs.is_empty());
    assert_eq!(tabs.window_ids().len(), 1, "the last window sits empty");
    assert_eq!(tabs.active_id(), None);
    assert_invariants(&tabs);
}

/// The group API is a window-local view: `group_count`, `group_tabs`,
/// `split_*`, `move_tab` and `focus_group` all talk about the focused window's
/// tree, which is what lets the tab bar and the keybindings stay
/// window-agnostic. Splitting in one window is invisible from the other.
#[test]
fn the_group_apis_scope_to_the_focused_window() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 4);
    let root = tabs.focused_window();

    let torn = tabs.move_tab_to_new_window(ids[0]).expect("tearable");
    tabs.move_tab_to_window(ids[1], torn);
    assert_eq!(tabs.focused_window(), torn);

    // In the torn window: one group, two tabs. Split it there.
    assert_eq!(tabs.group_count(), 1);
    assert!(tabs.split_right(ids[1]));
    assert_eq!(tabs.group_count(), 2, "the split happened in this window");
    assert_eq!(tabs.group_tabs(0), vec![ids[0]]);
    assert_eq!(tabs.group_tabs(1), vec![ids[1]]);
    assert_invariants(&tabs);

    // The root window never heard about it: still one group of two tabs.
    tabs.focus_window(root);
    assert_eq!(tabs.group_count(), 1);
    assert_eq!(tabs.group_tabs(0), vec![ids[2], ids[3]]);
    assert_eq!(tabs.focused_group(), 0);
    assert_invariants(&tabs);

    // A split here is equally invisible over there.
    assert!(tabs.split_down(ids[3]));
    assert_eq!(tabs.group_count(), 2);
    assert_invariants(&tabs);
    tabs.focus_window(torn);
    assert_eq!(tabs.group_count(), 2, "still the torn window's own two");
    assert_eq!(tabs.group_tabs(0), vec![ids[0]]);
    assert_invariants(&tabs);

    // `move_tab` addresses groups of the focused window, so it can never drag
    // a tab across windows by index.
    assert!(tabs.move_tab(ids[0], 1, 0));
    assert_eq!(tabs.window_of_tab(ids[0]), Some(torn));
    assert_eq!(tabs.window_of_tab(ids[2]), Some(root));
    assert_invariants(&tabs);

    // Group focus is per window too: focusing group 0 here, then hopping
    // away and back, comes back to group 0.
    assert!(tabs.focus_group(0));
    tabs.focus_window(root);
    tabs.focus_window(torn);
    assert_eq!(tabs.focused_group(), 0);
    assert_invariants(&tabs);

    // A leaf index out of range for *this* window is refused, even if the
    // other window has that many groups.
    assert!(!tabs.focus_group(9));
    assert_invariants(&tabs);

    tabs.clear();
}

/// `window_layout` describes each window's tree independently — a lone group
/// still comes back as `Leaf(0)`, so a torn-out window needs no special case
/// in the renderer.
#[test]
fn each_window_has_its_own_layout() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 3);
    let root = tabs.focused_window();

    let torn = tabs.move_tab_to_new_window(ids[0]).expect("tearable");
    assert_eq!(
        tabs.window_layout(torn),
        Some(tabs::LayoutNode::Leaf(0)),
        "a torn-out tab is one leaf"
    );
    assert_eq!(tabs.window_layout(root), Some(tabs::LayoutNode::Leaf(0)));
    assert_eq!(tabs.window_layout(u64::MAX), None);

    // Splitting the root window changes its layout and only its layout.
    tabs.focus_window(root);
    assert!(tabs.split_right(ids[2]));
    assert!(
        matches!(
            tabs.window_layout(root),
            Some(tabs::LayoutNode::Split { .. })
        ),
        "the root window is split"
    );
    assert_eq!(tabs.window_layout(torn), Some(tabs::LayoutNode::Leaf(0)));
    assert_invariants(&tabs);

    tabs.clear();
}
