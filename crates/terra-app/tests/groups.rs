//! Editor groups ("splits"): the model invariants, and that a two-group frame
//! routes the keyboard to the focused group's PTY only.
//!
//! `terra-app` is a binary crate, so the manager is pulled in by path — the
//! same `tabs.rs` the app compiles, together with the two things it reaches
//! for at the crate root (`config` and `terminal_theme`). The app's real frame
//! composition (`App::ui` in `main.rs`) is not reachable from here; the
//! rendering test mirrors its column layout instead: one `new_child` column
//! per group, the group's active terminal below, and only the focused group's
//! `TerminalView` taking `set_focus(true)`.
//!
//! Real PTYs are spawned (`/bin/cat` echoes through the tty, so the screen is
//! the assertion), which makes the whole file Unix-only like `tab_focus.rs`.
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

use std::time::{Duration, Instant};

use egui::{Event, Pos2, Rect};
use egui_term::TerminalView;
use tabs::TabManager;

/// `tabs.rs` calls `crate::terminal_theme()`; in this test crate, this is it —
/// the same construction `main.rs` uses.
fn terminal_theme() -> egui_term::TerminalTheme {
    egui_term::TerminalTheme::new(Box::new(ghostty_theme::palette()))
}

const SCREEN: Rect = Rect {
    min: Pos2::ZERO,
    max: Pos2::new(800.0, 600.0),
};

/// A manager with `n` `/bin/cat` tabs, all in one group.
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

/// The invariants every mutation must restore, checked in one place:
/// - every open tab appears in exactly one group, in `ids()` order;
/// - no group is empty, and each group's active tab is one of its own;
/// - `focused` names a real group, and weights are positive and sum to 1;
/// - `infos()` lists the same tabs in the same order and marks exactly one
///   active — the focused group's active tab (the IPC "active" flag).
fn assert_invariants(tabs: &TabManager) {
    let ids = tabs.ids();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len(), "a tab id appears in two groups");

    let count = tabs.group_count();
    let mut union: Vec<u64> = Vec::new();
    for group in 0..count {
        let members = tabs.group_tabs(group);
        assert!(!members.is_empty(), "empty group {group} survived");
        let active = tabs
            .group_active(group)
            .unwrap_or_else(|| panic!("group {group} has no active tab"));
        assert!(
            members.contains(&active),
            "group {group}'s active tab {active} is not one of its own"
        );
        union.extend(members);
    }
    assert_eq!(union, ids, "ids() disagrees with the groups' union");

    if count > 0 {
        assert!(tabs.focused_group() < count, "focused group out of range");
        let weights = tabs.group_weights();
        assert_eq!(weights.len(), count);
        assert!(weights.iter().all(|w| *w > 0.0), "non-positive weight");
        assert!((weights.iter().sum::<f32>() - 1.0).abs() < 1e-4);
    }

    let infos = tabs.infos();
    assert_eq!(infos.iter().map(|i| i.id).collect::<Vec<_>>(), ids);
    let actives: Vec<u64> = infos.iter().filter(|i| i.active).map(|i| i.id).collect();
    match tabs.active_id() {
        Some(active) => assert_eq!(actives, vec![active], "exactly one IPC-active tab"),
        None => assert!(actives.is_empty()),
    }
}

/// A storm of split/move/close/select operations, with the invariants checked
/// after every single step — the model equivalent of dragging tabs around two
/// columns for a while.
#[test]
fn a_split_move_close_storm_never_breaks_the_invariants() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 4);
    assert_invariants(&tabs);

    assert!(tabs.split_right(ids[1]));
    assert_invariants(&tabs);
    assert!(tabs.split_left(ids[3]));
    assert_invariants(&tabs);

    // Shuffle across all three groups, including clamped indices.
    assert!(tabs.move_tab(ids[0], 2, 99));
    assert_invariants(&tabs);
    assert!(tabs.move_tab(ids[2], 0, 0));
    assert_invariants(&tabs);

    // Selecting in another group, cycling, ⌘n.
    assert!(tabs.select(ids[1]));
    assert_invariants(&tabs);
    tabs.select_next();
    assert_invariants(&tabs);
    tabs.select_prev();
    assert_invariants(&tabs);
    tabs.select_nth(0);
    assert_invariants(&tabs);

    // Refused operations must leave the state untouched too.
    assert!(!tabs.move_tab(u64::MAX, 0, 0));
    assert!(!tabs.move_tab(ids[0], 9, 0));
    assert!(!tabs.split_right(u64::MAX));
    assert!(!tabs.focus_group(9));
    assert_invariants(&tabs);

    // Close everything in an arbitrary order; every intermediate state holds.
    for id in [ids[1], ids[3], ids[0], ids[2]] {
        assert!(tabs.close(id));
        assert_invariants(&tabs);
    }
    assert!(tabs.is_empty(), "the quit condition");
    assert_eq!(tabs.group_count(), 0);
    assert_eq!(tabs.active_id(), None);
}

/// The same storm in two dimensions: vertical splits nested inside the
/// horizontal ones (and vice versa), moves between leaves of different
/// subtrees, closes that collapse whole splits — the invariants hold after
/// every step, and the tree invariants (DFS group order, weights as window
/// shares) with them.
#[test]
fn a_2d_split_storm_never_breaks_the_invariants() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 6);
    assert_invariants(&tabs);

    assert!(tabs.split_right(ids[1]));
    assert_invariants(&tabs);
    assert!(tabs.split_down(ids[3]));
    assert_invariants(&tabs);
    assert!(tabs.split_up(ids[5]));
    assert_invariants(&tabs);
    // Split a stacked leaf sideways: a horizontal split nested inside the
    // vertical one.
    assert!(tabs.move_tab(ids[0], tabs.group_of(ids[3]).unwrap(), 99));
    assert_invariants(&tabs);
    assert!(tabs.split_right(ids[0]));
    assert_invariants(&tabs);

    // Cross-subtree moves, including ones that collapse their source leaf.
    assert!(tabs.move_tab(ids[3], tabs.group_of(ids[1]).unwrap(), 0));
    assert_invariants(&tabs);
    assert!(tabs.move_tab(ids[5], tabs.group_of(ids[2]).unwrap(), 1));
    assert_invariants(&tabs);

    // Selecting and cycling still walk the DFS order.
    assert!(tabs.select(ids[4]));
    assert_invariants(&tabs);
    tabs.next_group();
    assert_invariants(&tabs);
    tabs.prev_group();
    assert_invariants(&tabs);

    // Refused operations leave the state untouched.
    assert!(!tabs.split_down(u64::MAX));
    assert!(!tabs.split_up(u64::MAX));
    assert_invariants(&tabs);

    // Tear it all down in an arbitrary order; every intermediate holds.
    for id in [ids[0], ids[4], ids[2], ids[5], ids[1], ids[3]] {
        assert!(tabs.close(id));
        assert_invariants(&tabs);
    }
    assert!(tabs.is_empty());
}

/// A 2x2 grid — split right, then split down on both columns — keeps every
/// leaf addressable: four groups in DFS order, and `send`/`capture` still
/// reach each tab by its unchanged global id.
#[test]
fn a_2x2_grid_keeps_every_leaf_addressable() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 4);

    assert!(tabs.split_right(ids[1]));
    // Left column: [0, 2, 3]; right column: [1]. Stack the columns.
    assert!(tabs.split_down(ids[2]));
    assert!(tabs.move_tab(ids[3], tabs.group_of(ids[1]).unwrap(), 99));
    assert!(tabs.split_down(ids[3]));
    assert_invariants(&tabs);

    assert_eq!(tabs.group_count(), 4);
    // DFS: left-top, left-bottom, right-top, right-bottom.
    assert_eq!(tabs.group_tabs(0), vec![ids[0]]);
    assert_eq!(tabs.group_tabs(1), vec![ids[2]]);
    assert_eq!(tabs.group_tabs(2), vec![ids[1]]);
    assert_eq!(tabs.group_tabs(3), vec![ids[3]]);
    // Four quarters: every leaf holds a quarter of the window.
    for weight in tabs.group_weights() {
        assert!((weight - 0.25).abs() < 1e-4);
    }

    // The wire protocol still addresses every corner by global id.
    for (n, id) in ids.iter().enumerate() {
        let marker = format!("corner-{n}");
        assert!(tabs.send(*id, &marker, false));
        wait_for_capture(&mut tabs, *id, &marker);
    }

    tabs.clear();
}

/// Focus follows `select` — it makes the tab globally active wherever it
/// lives — but does *not* follow `move_tab`, which is the drag path.
#[test]
fn focus_follows_select_but_not_move() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 3);
    assert!(tabs.split_right(ids[2]));
    // Groups: [0, 1] | [2], focused = 1.
    assert_eq!(tabs.focused_group(), 1);

    // Selecting a tab of group 0 focuses group 0 and flips the global active.
    assert!(tabs.select(ids[1]));
    assert_eq!(tabs.focused_group(), 0);
    assert_eq!(tabs.active_id(), Some(ids[1]));
    assert!(tabs.infos().iter().any(|i| i.id == ids[1] && i.active));

    // Dragging a tab into the other group changes membership, not focus:
    // the globally active tab is still group 0's active one.
    assert!(tabs.move_tab(ids[0], 1, 0));
    assert_eq!(tabs.focused_group(), 0);
    assert_eq!(tabs.active_id(), Some(tabs.group_active(0).unwrap()));
    // ...but the moved tab did become *its* group's active tab.
    assert_eq!(tabs.group_active(1), Some(ids[0]));
    assert_invariants(&tabs);

    tabs.clear();
}

/// Closing a focused group's last tab collapses it and lands focus on a
/// surviving group whose active tab becomes the global one — never a dangling
/// index, never a window with zero active tabs while tabs remain.
#[test]
fn collapsing_the_focused_group_hands_focus_to_a_survivor() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 3);
    assert!(tabs.split_right(ids[1]));
    assert!(tabs.split_right(ids[2]));
    // Groups: [0] | [1] | [2]? — split_right(ids[2]) split from group 0's
    // remainder; whatever the exact shape, the invariants say it is legal.
    assert_invariants(&tabs);
    let focused = tabs.focused_group();
    let doomed = tabs.group_active(focused).unwrap();

    assert!(tabs.close(doomed));
    assert_invariants(&tabs);
    assert!(tabs.group_count() >= 1);
    let active = tabs.active_id().expect("tabs remain, one must be active");
    assert!(tabs.ids().contains(&active));

    tabs.clear();
}

/// The wire protocol is unchanged: tabs keep their global ids through splits
/// and moves, and `send`/`capture` address them by that id no matter which
/// group holds them or which group is focused.
#[test]
fn send_and_capture_reach_a_tab_in_an_unfocused_group() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 2);
    assert!(tabs.split_right(ids[1]));
    // Focused group is the new one (ids[1]); ids[0] sits unfocused.
    assert_eq!(tabs.active_id(), Some(ids[1]));

    // IPC send by global id, to the *unfocused* group's tab.
    assert!(tabs.send(ids[0], "background-bytes", false));
    wait_for_capture(&mut tabs, ids[0], "background-bytes");
    // Nothing leaked into the focused tab.
    assert!(
        !tabs
            .capture(ids[1], 0)
            .unwrap()
            .contains("background-bytes"),
        "bytes sent to tab {} surfaced in tab {}",
        ids[0],
        ids[1]
    );

    // A move to another group does not renumber: the same id still answers.
    assert!(tabs.move_tab(ids[0], 1, 0));
    assert!(tabs.send(ids[0], "after-move", false));
    wait_for_capture(&mut tabs, ids[0], "after-move");

    tabs.clear();
}

/// Poll `capture` until `text` shows up — the PTY echoes asynchronously.
fn wait_for_capture(tabs: &mut TabManager, id: u64, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let screen = tabs.capture(id, 0).expect("tab is open");
        if screen.contains(text) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{text:?} never appeared on tab {id}; the grid holds {screen:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

// -- rendering: keyboard routing across two group columns --------------------

/// One frame shaped the way `App::ui` composes a multi-group window: a column
/// per group (weights → widths), each column showing its group's active
/// terminal, and only the focused group's `TerminalView` getting
/// `set_focus(true)`.
fn group_frame(ctx: &egui::Context, tabs: &mut TabManager, events: Vec<Event>) {
    let input = egui::RawInput {
        screen_rect: Some(SCREEN),
        events,
        ..Default::default()
    };
    let _ = ctx.run_ui(input, |ui: &mut egui::Ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            let weights = tabs.group_weights();
            let focused = tabs.focused_group();
            let count = weights.len();
            let full = ui.available_rect_before_wrap();
            let mut x = full.left();
            for (group, weight) in weights.iter().enumerate() {
                let right = if group + 1 == count {
                    full.right()
                } else {
                    x + full.width() * weight
                };
                let column =
                    Rect::from_min_max(egui::pos2(x, full.top()), egui::pos2(right, full.bottom()));
                let mut col_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(column)
                        .id_salt(("terra_group_column", group)),
                );
                col_ui.set_clip_rect(column);
                let active = tabs.group_active(group);
                if let Some(tab) = active.and_then(|id| tabs.get_mut(id)) {
                    let view = TerminalView::new(&mut col_ui, &mut tab.backend)
                        .set_focus(group == focused)
                        .set_size(column.size());
                    col_ui.add(view);
                }
                x = column.right();
            }
        });
    });
}

/// Pump frames until `text` lands on tab `id`'s grid, or fail.
fn wait_for_echo(ctx: &egui::Context, tabs: &mut TabManager, id: u64, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !tabs.capture(id, 0).expect("tab is open").contains(text) {
        assert!(
            Instant::now() < deadline,
            "{text:?} never reached tab {id}'s PTY; the grid holds {:?}",
            tabs.capture(id, 0).unwrap()
        );
        std::thread::sleep(Duration::from_millis(20));
        group_frame(ctx, tabs, Vec::new());
    }
}

/// Typing into a two-group frame reaches the focused group's PTY and no
/// other; focusing the other group reroutes the very next keystroke.
#[test]
fn typing_goes_to_the_focused_group_and_follows_refocus() {
    let ctx = egui::Context::default();
    let (mut tabs, ids) = manager_with(&ctx, 2);
    assert!(tabs.split_right(ids[1]));
    // Two columns: group 0 shows ids[0], group 1 (focused) shows ids[1].
    assert_eq!(tabs.focused_group(), 1);

    // Warm-up frames: size both grids and let the focused view take the
    // keyboard, exactly as the app does before the first keystroke.
    group_frame(&ctx, &mut tabs, Vec::new());
    group_frame(&ctx, &mut tabs, Vec::new());

    group_frame(&ctx, &mut tabs, vec![Event::Text("alpha".to_string())]);
    wait_for_echo(&ctx, &mut tabs, ids[1], "alpha");
    assert!(
        !tabs.capture(ids[0], 0).unwrap().contains("alpha"),
        "keystrokes leaked into the unfocused group's PTY"
    );

    // Focus the other group; the next keystroke must land there instead.
    assert!(tabs.focus_group(0));
    group_frame(&ctx, &mut tabs, Vec::new());
    group_frame(&ctx, &mut tabs, vec![Event::Text("bravo".to_string())]);
    wait_for_echo(&ctx, &mut tabs, ids[0], "bravo");
    assert!(
        !tabs.capture(ids[1], 0).unwrap().contains("bravo"),
        "keystrokes kept flowing to the previously focused group"
    );
    // And the first group's screen still has no trace of the first burst.
    assert!(!tabs.capture(ids[0], 0).unwrap().contains("alpha"));

    tabs.clear();
}
