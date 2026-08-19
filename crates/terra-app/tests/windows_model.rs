//! Multiple OS windows: one split tree per window, one global tab map.
//!
//! The model half of "tear a tab out into its own window". Everything here is
//! `TabManager` alone — no frame is composed, because a second window is a
//! second *viewport* and the app's window plumbing is not reachable from a
//! test crate. What is testable, and is what the rest of the app leans on, is
//! that the model stays coherent across windows: every group-index API keeps
//! answering for the focused window, ids stay global and unique, exactly one
//! tab is active in the whole app, and no window is ever left empty except the
//! last one.
//!
//! `terra-app` is a binary crate, so the manager is pulled in by path, with
//! the two things it reaches for at the crate root (`config` and
//! `terminal_theme`) — the same arrangement `groups.rs` uses.
//!
//! Real PTYs are spawned (`/bin/cat`), which makes the whole file Unix-only.
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

/// `tabs.rs` calls `crate::terminal_theme()`; in this test crate, this is it.
fn terminal_theme() -> egui_term::TerminalTheme {
    egui_term::TerminalTheme::new(Box::new(ghostty_theme::palette()))
}

/// A manager with `n` `/bin/cat` tabs, all in one group of the root window.
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

/// The invariants every mutation must restore, now across windows:
/// - windows exist, their ids are unique and ascending, and only the *last*
///   remaining one may hold no tab;
/// - within each window, the group invariants of `groups.rs` hold (no empty
///   group, each group's active tab is one of its own);
/// - a tab id appears in exactly one group of exactly one window, and `ids()`
///   is the concatenation of every window's groups in order;
/// - the focused window is a real one, and `infos()` marks exactly one tab
///   active across the whole app — the focused window's focused group's.
///
/// Takes `&mut` because the group API is scoped to the focused window: the
/// only way to look inside the others is to focus them. The focus is put back
/// before returning, so a check is invisible to the test around it.
fn assert_invariants(tabs: &mut TabManager) {
    let windows = tabs.window_ids();
    assert!(!windows.is_empty(), "there is always a window");
    let mut sorted = windows.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted, windows, "window ids are unique and ascending");

    let focused_window = tabs.focused_window();
    assert!(
        windows.contains(&focused_window),
        "the focused window {focused_window} is not open"
    );

    let ids = tabs.ids();
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "a tab id appears twice");

    let mut union: Vec<u64> = Vec::new();
    for win in &windows {
        assert!(tabs.focus_window(*win));
        let count = tabs.group_count();
        if count == 0 {
            assert!(
                windows.len() == 1 || tabs.held_window() == Some(*win),
                "window {win} sits empty while another window is open"
            );
            assert!(tabs.layout().is_none());
            continue;
        }
        for group in 0..count {
            let members = tabs.group_tabs(group);
            assert!(!members.is_empty(), "empty group {group} in window {win}");
            let active = tabs
                .group_active(group)
                .unwrap_or_else(|| panic!("group {group} of window {win} has no active tab"));
            assert!(
                members.contains(&active),
                "group {group} of window {win} activates a tab it does not hold"
            );
            for id in &members {
                assert_eq!(
                    tabs.window_of_tab(*id),
                    Some(*win),
                    "window_of_tab disagrees about tab {id}"
                );
            }
            union.extend(members);
        }
        assert!(
            tabs.focused_group() < count,
            "window {win}'s focused group is out of range"
        );
        // The focused window's `layout()` is precisely its `window_layout`.
        assert_eq!(tabs.layout(), tabs.window_layout(*win));
        let weights = tabs.group_weights();
        assert_eq!(weights.len(), count);
        assert!(weights.iter().all(|w| *w > 0.0), "non-positive weight");
        assert!((weights.iter().sum::<f32>() - 1.0).abs() < 1e-4);
    }
    assert!(tabs.focus_window(focused_window));

    assert_eq!(union, ids, "ids() disagrees with the windows' union");

    let infos = tabs.infos();
    assert_eq!(infos.iter().map(|i| i.id).collect::<Vec<_>>(), ids);
    let actives: Vec<u64> = infos.iter().filter(|i| i.active).map(|i| i.id).collect();
    match tabs.active_id() {
        Some(active) => assert_eq!(actives, vec![active], "exactly one active tab, app-wide"),
        None => assert!(actives.is_empty()),
    }
}

/// A fresh manager is one window — the root — even before the first tab, and
/// the group API answers for it.
#[test]
fn a_fresh_manager_is_one_empty_root_window() {
    let (tx, rx) = std::sync::mpsc::channel();
    Box::leak(Box::new(rx));
    let mut tabs = TabManager::new(egui::Context::default(), tx);

    assert_eq!(tabs.window_ids(), vec![0]);
    assert_eq!(tabs.focused_window(), 0);
    assert!(tabs.is_empty());
    assert_eq!(tabs.group_count(), 0);
    assert!(tabs.layout().is_none());
    assert!(tabs.window_layout(0).is_none());
    assert!(!tabs.focus_window(7), "an unknown window cannot be focused");
    assert_invariants(&mut tabs);
}

/// Tearing a tab out: a new window with one group holding it, the keyboard
/// goes with it, and the tab keeps the global id the wire protocol knows.
#[test]
fn tearing_a_tab_out_makes_a_window_and_takes_the_keyboard() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 3);
    assert_eq!(tabs.window_ids(), vec![0]);

    let win = tabs
        .move_tab_to_new_window(ids[0])
        .expect("three tabs in one window: there is something to tear out");
    assert_ne!(win, 0, "window ids are minted, never reused");
    assert_eq!(tabs.window_ids(), vec![0, win]);
    assert_eq!(tabs.focused_window(), win);
    assert_eq!(tabs.window_of_tab(ids[0]), Some(win));
    assert_eq!(tabs.window_of_tab(ids[1]), Some(0));

    // The new window is one group holding exactly the torn-out tab, and it is
    // the globally active one.
    assert_eq!(tabs.group_count(), 1);
    assert_eq!(tabs.group_tabs(0), vec![ids[0]]);
    assert_eq!(tabs.active_id(), Some(ids[0]));
    assert_eq!(tabs.shape(), "[0]");
    assert_eq!(tabs.window_shape(0), "[1,2]");
    // Ids stay global and unique: `terra ls` still sees all three.
    assert_eq!(tabs.ids(), vec![ids[1], ids[2], ids[0]]);
    assert_invariants(&mut tabs);

    tabs.clear();
}

/// The sole tab of a sole window has nowhere to go: the operation is refused
/// outright rather than rebuilding the same window under a new id.
#[test]
fn the_last_tab_of_the_last_window_cannot_be_torn_out() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 1);

    assert_eq!(tabs.move_tab_to_new_window(ids[0]), None);
    assert_eq!(tabs.window_ids(), vec![0]);
    assert_eq!(tabs.group_tabs(0), vec![ids[0]]);
    // An unknown tab is refused the same way.
    assert_eq!(tabs.move_tab_to_new_window(u64::MAX), None);
    assert_invariants(&mut tabs);

    tabs.clear();
}

/// Each window remembers which pane the keyboard was in, so coming back to a
/// window comes back to where you left it — not to its first group.
#[test]
fn every_window_remembers_its_own_focused_group() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 4);

    // Root window: two groups, focus deliberately left on the *first*.
    assert!(tabs.split_right(ids[3]));
    assert!(tabs.focus_group(0));
    assert_eq!(tabs.focused_group(), 0);

    // A second window, itself split, focused on its second group.
    let win = tabs
        .move_tab_to_new_window(ids[1])
        .expect("more than one tab");
    assert!(tabs.move_tab_to_window(ids[2], win));
    assert!(tabs.focus_window(win));
    assert!(tabs.split_right(ids[2]));
    let torn_focus = tabs.focused_group();
    assert_eq!(tabs.group_count(), 2);
    assert_eq!(tabs.active_id(), Some(ids[2]));

    // Back to the root: its own focus, untouched by anything the other window
    // did — including the leaf ids minted in between.
    assert!(tabs.focus_window(0));
    assert_eq!(tabs.focused_group(), 0);
    assert_eq!(tabs.active_id(), tabs.group_active(0));

    // ...and back again lands where that window was left.
    assert!(tabs.focus_window(win));
    assert_eq!(tabs.focused_group(), torn_focus);
    assert_eq!(tabs.active_id(), Some(ids[2]));
    assert_invariants(&mut tabs);

    tabs.clear();
}

/// Every group-index API is scoped to the focused window: the same index means
/// a different group depending on which window has the keyboard, and a tab in
/// another window is simply not addressable by group.
#[test]
fn the_group_api_is_scoped_to_the_focused_window() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 4);
    assert!(tabs.split_right(ids[3]));
    // Root: [0,1,2] | [3].
    let win = tabs
        .move_tab_to_new_window(ids[0])
        .expect("three tabs left");

    // Focused = the torn-out window: one group, one tab, no idea about the
    // root window's groups.
    assert_eq!(tabs.group_count(), 1);
    assert_eq!(tabs.group_tabs(0), vec![ids[0]]);
    assert_eq!(tabs.group_of(ids[0]), Some(0));
    assert_eq!(tabs.group_of(ids[3]), None, "tab {} is elsewhere", ids[3]);
    assert_eq!(tabs.layout(), tabs.window_layout(win));

    // Focused = the root window: index 0 now names an entirely different group.
    assert!(tabs.focus_window(0));
    assert_eq!(tabs.group_count(), 2);
    assert_eq!(tabs.group_tabs(0), vec![ids[1], ids[2]]);
    assert_eq!(tabs.group_of(ids[3]), Some(1));
    assert_eq!(tabs.group_of(ids[0]), None);
    assert_eq!(tabs.layout(), tabs.window_layout(0));
    // Leaf ids stay globally unique, so a leaf names its window unambiguously.
    let root_leaf = tabs.group_leaf_id(0).unwrap();
    assert_eq!(tabs.window_of_group_leaf(root_leaf), Some(0));
    assert!(tabs.focus_window(win));
    assert_eq!(
        tabs.window_of_group_leaf(tabs.group_leaf_id(0).unwrap()),
        Some(win)
    );
    assert_invariants(&mut tabs);

    tabs.clear();
}

/// Splits, reorders and weights inside one window leave the other alone: two
/// trees, no shared shape.
#[test]
fn splitting_one_window_does_not_touch_the_other() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 4);
    let win = tabs.move_tab_to_new_window(ids[0]).expect("four tabs");
    assert!(tabs.move_tab_to_window(ids[1], win));
    // Root: [2,3]; torn: [0,1].
    assert_eq!(tabs.window_shape(0), "[2,3]");
    assert_eq!(tabs.window_shape(win), "[0,1]");

    // Split the torn-out window in two, twice over.
    assert!(tabs.focus_window(win));
    assert!(tabs.split_down(ids[1]));
    assert_eq!(tabs.window_shape(win), "v([0] [1])");
    assert_eq!(tabs.window_shape(0), "[2,3]", "the root window reshaped");
    assert_eq!(tabs.group_count(), 2);
    assert_eq!(tabs.split_weights(&[]).len(), 2);

    // And the root window splits on its own axis, independently.
    assert!(tabs.focus_window(0));
    assert!(tabs.split_right(ids[3]));
    assert_eq!(tabs.window_shape(0), "h([2] [3])");
    assert_eq!(tabs.window_shape(win), "v([0] [1])");
    // Weights are per window: rewriting the root's split leaves the other's.
    assert!(tabs.set_split_weights(&[], &[3.0, 1.0]));
    let root_weights = tabs.split_weights(&[]);
    assert!((root_weights[0] - 0.75).abs() < 1e-4);
    assert!(tabs.focus_window(win));
    assert!((tabs.split_weights(&[])[0] - 0.5).abs() < 1e-4);
    assert_invariants(&mut tabs);

    tabs.clear();
}

/// A tab moved into a window lands in that window's focused group, right
/// after its active tab, and becomes that group's active tab — the same
/// landing spot `open` uses. Focus does not follow it.
#[test]
fn a_tab_moved_into_a_window_lands_after_its_active_tab() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 4);
    // Root: [0,1,2,3]; tear 0 out, then send 3 after it.
    let win = tabs.move_tab_to_new_window(ids[0]).expect("four tabs");
    assert!(tabs.focus_window(0));
    let focused_before = tabs.focused_window();

    assert!(tabs.move_tab_to_window(ids[3], win));
    assert_eq!(tabs.window_shape(win), "[0,3]");
    assert_eq!(tabs.window_of_tab(ids[3]), Some(win));
    assert_eq!(
        tabs.focused_window(),
        focused_before,
        "moving a tab must not steal the keyboard"
    );
    // It is the target group's active tab, though — the globally active one is
    // still the focused window's.
    assert!(tabs.focus_window(win));
    assert_eq!(tabs.group_active(0), Some(ids[3]));
    assert_eq!(tabs.active_id(), Some(ids[3]));

    // Another one lands after *that*, not at the end of some fixed order.
    assert!(tabs.select(ids[0]));
    assert!(tabs.move_tab_to_window(ids[2], win));
    assert_eq!(tabs.window_shape(win), "[0,2,3]");
    assert_invariants(&mut tabs);

    tabs.clear();
}

/// The refusals: an unknown tab, an unknown window, and a tab that is already
/// the only thing in the window it is being moved to.
#[test]
fn moving_into_a_window_refuses_the_no_ops() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 3);
    let win = tabs.move_tab_to_new_window(ids[0]).expect("three tabs");

    assert!(!tabs.move_tab_to_window(u64::MAX, win));
    assert!(!tabs.move_tab_to_window(ids[1], u64::MAX));
    assert!(
        !tabs.move_tab_to_window(ids[0], win),
        "{} is already the whole of window {win}",
        ids[0]
    );
    assert_eq!(tabs.window_ids(), vec![0, win]);
    assert_eq!(tabs.window_shape(win), "[0]");
    assert_invariants(&mut tabs);

    // Moving the last tab *out* of a window closes it — the same rule as
    // closing that tab.
    assert!(tabs.move_tab_to_window(ids[0], 0));
    assert_eq!(tabs.window_ids(), vec![0]);
    assert_eq!(tabs.window_of_tab(ids[0]), Some(0));
    assert_invariants(&mut tabs);

    tabs.clear();
}

/// Closing the last tab of a second window removes the window with it; the
/// last window is the one exception and is allowed to sit empty, which is the
/// app's quit condition.
#[test]
fn a_window_whose_last_tab_closes_goes_with_it() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 3);
    let win = tabs.move_tab_to_new_window(ids[0]).expect("three tabs");
    assert_eq!(tabs.window_ids(), vec![0, win]);

    // The focused window empties: it goes, and the keyboard lands in the
    // survivor at the leaf that window was last left in.
    assert!(tabs.close(ids[0]));
    assert_eq!(tabs.window_ids(), vec![0]);
    assert_eq!(tabs.focused_window(), 0);
    let active = tabs.active_id().expect("tabs remain, one must be active");
    assert!(tabs.ids().contains(&active));
    assert_invariants(&mut tabs);

    // The last window survives its last tab, empty.
    assert!(tabs.close(ids[1]));
    assert!(tabs.close(ids[2]));
    assert!(tabs.is_empty(), "the quit condition");
    assert_eq!(tabs.window_ids(), vec![0]);
    assert_eq!(tabs.group_count(), 0);
    assert_eq!(tabs.active_id(), None);
    assert_invariants(&mut tabs);

    // ...and opening a tab refills it rather than minting a window.
    let id = tabs
        .open(&["/bin/cat".to_string()], None, None)
        .expect("spawn /bin/cat");
    assert_eq!(tabs.window_ids(), vec![0]);
    assert_eq!(tabs.window_of_tab(id), Some(0));
    assert_invariants(&mut tabs);

    tabs.clear();
}

/// A held window outlives its last tab: the drag that emptied it is still
/// running in it, and killing its viewport mid-gesture would kill the drag.
/// Releasing the hold settles up on the spot.
#[test]
fn a_held_window_survives_being_emptied_until_it_is_released() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 2);
    let win = tabs.move_tab_to_new_window(ids[0]).expect("two tabs");
    assert_eq!(tabs.window_ids(), vec![0, win]);

    // The dock: the torn-out window is held, then its only tab moves back to
    // the root window. Without the hold that window would be gone right here.
    tabs.hold_window(win);
    assert!(tabs.move_tab_to_window(ids[0], 0));
    assert_eq!(tabs.window_ids(), vec![0, win]);
    assert_eq!(tabs.window_of_tab(ids[0]), Some(0));
    // Empty, so it owns nothing: `terra ls` cannot see it.
    assert_eq!(tabs.ids().len(), 2);
    assert!(tabs
        .infos()
        .iter()
        .all(|info| tabs.window_of_tab(info.id) == Some(0)));
    assert_invariants(&mut tabs);

    // Further mutations keep it alive, hold still standing.
    assert!(tabs.focus_window(0));
    assert!(tabs.split_right(ids[1]));
    assert_eq!(tabs.window_ids(), vec![0, win]);

    // Released, it collapses — nothing else has to happen for it to go.
    tabs.release_window(win);
    assert_eq!(tabs.window_ids(), vec![0]);
    assert_invariants(&mut tabs);

    tabs.clear();
}

/// Undocking hands the tab back, and then the release is a no-op: a held
/// window that has tabs again is an ordinary window.
#[test]
fn releasing_a_hold_on_a_refilled_window_keeps_it() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 2);
    let win = tabs.move_tab_to_new_window(ids[0]).expect("two tabs");

    tabs.hold_window(win);
    assert!(tabs.move_tab_to_window(ids[0], 0));
    // ...and back out again, the way pulling the pill out of the bar does.
    assert!(tabs.move_tab_to_window(ids[0], win));
    tabs.release_window(win);
    assert_eq!(tabs.window_ids(), vec![0, win]);
    assert_eq!(tabs.window_of_tab(ids[0]), Some(win));
    assert_invariants(&mut tabs);

    tabs.clear();
}

/// Teardown drops the hold with everything else, so a manager reused after
/// `clear` cannot be haunted by a window that no longer exists.
#[test]
fn clear_forgets_the_hold() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 2);
    let win = tabs.move_tab_to_new_window(ids[0]).expect("two tabs");
    tabs.hold_window(win);
    assert_eq!(tabs.held_window(), Some(win));

    tabs.clear();
    assert_eq!(tabs.window_ids(), vec![0]);
    assert_eq!(tabs.held_window(), None, "a hold cannot outlive its window");
}

/// A tab opened while a torn-out window has the keyboard stays in *that*
/// window — a `⌘T` in the second window must not post the tab back to the
/// first.
#[test]
fn new_tabs_land_in_the_focused_window() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 2);
    let win = tabs.move_tab_to_new_window(ids[0]).expect("two tabs");

    let fresh = tabs
        .open(&["/bin/cat".to_string()], None, None)
        .expect("spawn /bin/cat");
    assert_eq!(tabs.window_of_tab(fresh), Some(win));
    assert_eq!(tabs.window_shape(win), "[0,2]");
    assert_eq!(tabs.active_id(), Some(fresh));

    assert!(tabs.focus_window(0));
    let other = tabs
        .open(&["/bin/cat".to_string()], None, None)
        .expect("spawn /bin/cat");
    assert_eq!(tabs.window_of_tab(other), Some(0));
    assert_invariants(&mut tabs);

    tabs.clear();
}

/// `close_window` hands back the tabs it took, and takes them out of the model
/// (their backends die with them). The last window empties instead of closing.
#[test]
fn closing_a_window_returns_the_tabs_it_held() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 4);
    let win = tabs.move_tab_to_new_window(ids[0]).expect("four tabs");
    assert!(tabs.move_tab_to_window(ids[1], win));
    // Torn-out window: [0,1]; root: [2,3].
    assert!(tabs.focus_window(win));

    assert!(
        tabs.close_window(u64::MAX).is_empty(),
        "an unknown window changes nothing"
    );
    assert_eq!(tabs.window_ids(), vec![0, win]);

    let mut closed = tabs.close_window(win);
    closed.sort_unstable();
    assert_eq!(closed, vec![ids[0], ids[1]]);
    assert_eq!(tabs.window_ids(), vec![0]);
    assert_eq!(
        tabs.focused_window(),
        0,
        "the keyboard follows the survivor"
    );
    assert_eq!(tabs.ids(), vec![ids[2], ids[3]]);
    assert_eq!(tabs.title(ids[0]), None, "the tab is gone, not just hidden");
    assert_invariants(&mut tabs);

    // Closing the only window leaves an empty one — the quit condition again.
    let mut rest = tabs.close_window(0);
    rest.sort_unstable();
    assert_eq!(rest, vec![ids[2], ids[3]]);
    assert!(tabs.is_empty());
    assert_eq!(tabs.window_ids(), vec![0]);
    assert_invariants(&mut tabs);
}

/// `select` reaches across windows: it activates the tab in its group *and*
/// brings that window's keyboard focus with it, which is what `terra select`
/// promises.
#[test]
fn select_crosses_windows() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 3);
    let win = tabs.move_tab_to_new_window(ids[0]).expect("three tabs");
    assert_eq!(tabs.focused_window(), win);

    assert!(tabs.select(ids[2]));
    assert_eq!(tabs.focused_window(), 0);
    assert_eq!(tabs.active_id(), Some(ids[2]));
    assert!(tabs.infos().iter().any(|i| i.id == ids[2] && i.active));
    assert_eq!(tabs.infos().iter().filter(|i| i.active).count(), 1);

    assert!(tabs.select(ids[0]));
    assert_eq!(tabs.focused_window(), win);
    assert_eq!(tabs.active_id(), Some(ids[0]));
    assert_invariants(&mut tabs);

    tabs.clear();
}

/// Sending and capturing still address a tab by its global id after it has
/// moved to another window — the wire protocol never learns windows exist.
#[test]
fn send_and_capture_reach_a_tab_in_another_window() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 2);
    let win = tabs.move_tab_to_new_window(ids[0]).expect("two tabs");
    assert_eq!(tabs.focused_window(), win);

    // The unfocused window's tab, by id.
    assert!(tabs.send(ids[1], "other-window", false));
    wait_for_capture(&mut tabs, ids[1], "other-window");
    assert!(
        !tabs.capture(ids[0], 0).unwrap().contains("other-window"),
        "bytes leaked into the focused window's tab"
    );

    // ...and after it moves again.
    assert!(tabs.move_tab_to_window(ids[1], win));
    assert!(tabs.send(ids[1], "after-the-move", false));
    wait_for_capture(&mut tabs, ids[1], "after-the-move");
    assert_invariants(&mut tabs);

    tabs.clear();
}

/// Poll `capture` until `text` shows up — the PTY echoes asynchronously.
fn wait_for_capture(tabs: &mut TabManager, id: u64, text: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let screen = tabs.capture(id, 0).expect("tab is open");
        if screen.contains(text) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{text:?} never appeared on tab {id}; the grid holds {screen:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// A storm across three windows — tearing out, moving back and forth,
/// splitting, closing, cycling — with the invariants checked after every
/// single step.
#[test]
fn a_multi_window_storm_never_breaks_the_invariants() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 6);
    assert_invariants(&mut tabs);

    // Two windows out of the root, one of them split in two.
    let a = tabs.move_tab_to_new_window(ids[0]).expect("six tabs");
    assert_invariants(&mut tabs);
    let b = tabs.move_tab_to_new_window(ids[1]).expect("five tabs left");
    assert_invariants(&mut tabs);
    assert!(tabs.move_tab_to_window(ids[2], a));
    assert_invariants(&mut tabs);
    assert!(tabs.focus_window(a));
    assert!(tabs.split_down(ids[2]));
    assert_invariants(&mut tabs);

    // Move a tab out of a split window, and one into it.
    assert!(tabs.move_tab_to_window(ids[2], b));
    assert_invariants(&mut tabs);
    assert!(tabs.move_tab_to_window(ids[3], a));
    assert_invariants(&mut tabs);

    // Group-scoped operations in each window in turn.
    for win in tabs.window_ids() {
        assert!(tabs.focus_window(win));
        tabs.next_group();
        assert_invariants(&mut tabs);
        tabs.prev_group();
        assert_invariants(&mut tabs);
        tabs.select_next();
        assert_invariants(&mut tabs);
        tabs.select_nth(0);
        assert_invariants(&mut tabs);
    }

    // Close a whole window, then everything else, one tab at a time.
    let taken = tabs.close_window(b);
    assert_invariants(&mut tabs);
    for id in taken {
        assert_eq!(tabs.window_of_tab(id), None, "tab {id} outlived its window");
    }
    let remaining = tabs.ids();
    for id in remaining {
        assert!(tabs.close(id));
        assert_invariants(&mut tabs);
    }
    assert!(tabs.is_empty(), "the quit condition");
    assert_eq!(tabs.window_ids().len(), 1);
    assert_eq!(tabs.active_id(), None);
}
