//! Hover owns the pane: the wheel scrolls whatever is under the cursor, and
//! moving the pointer into a pane hands it the keyboard too.
//!
//! Two layers meet here, and the tests keep them apart:
//!
//! * **egui_term** (`view.rs::accepts`) routes the *wheel* by hover alone, so a
//!   pane that does not hold the keyboard still scrolls when the pointer is
//!   over it. This is what runs whenever focus-follows-mouse is suppressed —
//!   a modal is up, a drag is in progress, or `[input] focus_follows_mouse` is
//!   switched off.
//! * **terra-app** (`main.rs::hover_focus`) moves the *focus* when the pointer
//!   moves into a pane's terminal. Its rules are unit-tested there; what needs
//!   a PTY is the end of the chain — that the keystroke really does come out
//!   of the newly hovered pane's shell, and really does not come out of the
//!   other one.
//!
//! Two real PTYs side by side in one frame, the way `TreeFrame::leaf` renders
//! them. Same headless harness as `tab_focus.rs` / `mouse_reporting.rs`.
//! Unix-only.
#![cfg(unix)]

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use egui::{Event, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect, Vec2};
use egui_term::{BackendSettings, PtyEvent, TerminalBackend, TerminalView};

const SCREEN: Rect = Rect {
    min: Pos2::ZERO,
    max: Pos2::new(800.0, 600.0),
};
/// Middle of the left pane, and of the right one.
const IN_THE_LEFT_PANE: Pos2 = Pos2::new(200.0, 300.0);
const IN_THE_RIGHT_PANE: Pos2 = Pos2::new(600.0, 300.0);

/// Which of the two panes holds the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focused {
    Left,
    Right,
}

/// Whether terra-app's focus-follows-mouse rule runs this frame.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Policy {
    /// The ordinary case: a pointer move into a pane focuses it.
    FollowsMouse,
    /// Focus is frozen — what terra does while a modal is up, while a drag is
    /// in progress, and permanently under `[input] focus_follows_mouse =
    /// false`. Isolates the wheel's hover routing, which is the layer
    /// underneath and keeps working in all three.
    Pinned,
}

/// The two panes and the keyboard between them: one frame's worth of the state
/// `TreeFrame` walks.
struct Panes {
    left: TerminalBackend,
    right: TerminalBackend,
    focused: Focused,
    policy: Policy,
}

impl Panes {
    /// One frame of a terra-shaped window split into two columns, rendered the
    /// way `TreeFrame::leaf` renders them: a child `Ui` per pane, clipped to
    /// its own rect, holding one `TerminalView` whose `set_focus` says whether
    /// that pane is the focused group's.
    ///
    /// Focus-follows-mouse is applied **after** the panes are drawn, because
    /// that is what terra does: `leaf` pushes an `AppAction::FocusGroup` and
    /// the frame that pushed it has already rendered. So the move focuses the
    /// pane, and the *next* frame's keystrokes go there.
    ///
    /// The rule reproduced here is `main.rs::hover_focus` — a `PointerMoved`
    /// landing in the pane's terminal, with no button down. (This harness
    /// draws no tab bar, so a pane's rect *is* its terminal rect; that the
    /// real rule ignores moves over the bar is pinned by main.rs's own unit
    /// tests.)
    fn frame(&mut self, ctx: &egui::Context, events: Vec<Event>) {
        let input = egui::RawInput {
            screen_rect: Some(SCREEN),
            events,
            ..Default::default()
        };
        let focused = self.focused;
        let policy = self.policy;
        let left = &mut self.left;
        let right = &mut self.right;
        let mut next = focused;
        let _ = ctx.run_ui(input, |ui: &mut egui::Ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let full = ui.max_rect();
                let mid = full.center().x;
                let columns = [
                    Rect::from_min_max(full.min, Pos2::new(mid, full.max.y)),
                    Rect::from_min_max(Pos2::new(mid, full.min.y), full.max),
                ];
                let panes: [(&mut TerminalBackend, Focused); 2] =
                    [(left, Focused::Left), (right, Focused::Right)];
                let button_down = ui.input(|i| i.pointer.any_down());
                for ((backend, side), rect) in panes.into_iter().zip(columns) {
                    let mut pane = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(rect)
                            .id_salt(("terra_group_column", backend.id())),
                    );
                    pane.set_clip_rect(rect);
                    let view = TerminalView::new(&mut pane, backend)
                        .set_focus(side == focused)
                        .set_size(rect.size());
                    pane.add(view);

                    let moved_in = policy == Policy::FollowsMouse
                        && !button_down
                        && pane.input(|i| {
                            i.events
                                .iter()
                                .any(|e| matches!(e, Event::PointerMoved(p) if rect.contains(*p)))
                        });
                    if moved_in {
                        next = side;
                    }
                }
            });
        });
        self.focused = next;
    }
}

/// Everything on one pane's grid, as a string. Control bytes echoed by the tty
/// show up caret-style (`^[[<64;…M`), which is what the assertions read.
fn screen_text(backend: &mut TerminalBackend) -> String {
    backend
        .sync()
        .grid
        .display_iter()
        .filter(|c| !c.cell.flags.contains(Flags::WIDE_CHAR_SPACER))
        .map(|c| c.cell.c)
        .collect()
}

fn display_offset(backend: &mut TerminalBackend) -> usize {
    backend.sync().grid.display_offset()
}

fn spawn(
    ctx: &egui::Context,
    id: u64,
    script: &str,
) -> (TerminalBackend, Receiver<(u64, PtyEvent)>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let backend = TerminalBackend::new(
        id,
        ctx.clone(),
        tx,
        BackendSettings {
            shell: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            working_directory: None,
            output_tap: None,
        },
    )
    .expect("spawn /bin/sh");
    (backend, rx)
}

/// Two panes, left one focused, each running `script`.
///
/// The PTY event receivers come back attached to the struct: nothing here
/// reads them, but the senders must stay connected for as long as the backends
/// live.
fn panes(
    ctx: &egui::Context,
    script: &str,
    policy: Policy,
) -> (Panes, [Receiver<(u64, PtyEvent)>; 2]) {
    let (left, l) = spawn(ctx, 0, script);
    let (right, r) = spawn(ctx, 1, script);
    (
        Panes {
            left,
            right,
            focused: Focused::Left,
            policy,
        },
        [l, r],
    )
}

/// A pane with plenty of scrollback and nothing else going on.
const FILL_SCROLLBACK: &str = "i=0; while [ $i -lt 300 ]; do echo line$i; i=$((i+1)); done; cat";
/// A pane doing what claude code does: alt screen + SGR mouse tracking, then
/// echoing every byte it is sent.
const MOUSE_REPORTING: &str = r"printf '\033[?1049h\033[?1000h\033[?1006h'; cat";

fn wheel(delta: f32) -> Event {
    Event::MouseWheel {
        unit: MouseWheelUnit::Line,
        delta: Vec2::new(0.0, delta),
        modifiers: Modifiers::NONE,
        phase: egui::TouchPhase::Move,
    }
}

fn click(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

/// Pump frames until `ready` holds, or fail with `what`.
fn pump(
    ctx: &egui::Context,
    panes: &mut Panes,
    what: &str,
    mut ready: impl FnMut(&mut Panes) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if ready(panes) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
        panes.frame(ctx, Vec::new());
    }
}

// ---------------------------------------------------------------------------
// Focus follows the mouse (terra-app policy)
// ---------------------------------------------------------------------------

/// The headline: no click anywhere, just a pointer move into the other pane,
/// and what you type comes out of *that* shell.
#[test]
fn a_pointer_move_into_a_pane_hands_it_the_keyboard() {
    let ctx = egui::Context::default();
    let (mut p, _pty) = panes(&ctx, "cat", Policy::FollowsMouse);

    p.frame(&ctx, Vec::new());
    assert_eq!(p.focused, Focused::Left);

    p.frame(&ctx, vec![Event::PointerMoved(IN_THE_RIGHT_PANE)]);
    assert_eq!(
        p.focused,
        Focused::Right,
        "the pointer moved into the right pane and it did not take focus"
    );

    p.frame(&ctx, vec![Event::Text("terra".to_string())]);
    pump(
        &ctx,
        &mut p,
        "the hovered pane to echo what was typed",
        |p| screen_text(&mut p.right).contains("terra"),
    );
    assert!(
        !screen_text(&mut p.left).contains("terra"),
        "the keystroke also reached the pane the pointer left: {:?}",
        screen_text(&mut p.left).trim_end()
    );

    // And back: moving into the left pane returns the keyboard there.
    p.frame(&ctx, vec![Event::PointerMoved(IN_THE_LEFT_PANE)]);
    assert_eq!(p.focused, Focused::Left);
    p.frame(&ctx, vec![Event::Text("home".to_string())]);
    pump(&ctx, &mut p, "the left pane to echo what was typed", |p| {
        screen_text(&mut p.left).contains("home")
    });
    assert!(!screen_text(&mut p.right).contains("home"));
}

/// A resting cursor is not a gesture. Once the pointer stops moving, panes may
/// re-layout and output may scroll underneath it without the keyboard moving —
/// and a pane the cursor merely happens to be sitting over never steals it.
#[test]
fn a_stationary_pointer_keeps_the_keyboard_where_it_is() {
    let ctx = egui::Context::default();
    let (mut p, _pty) = panes(&ctx, FILL_SCROLLBACK, Policy::FollowsMouse);

    // Park the pointer over the right pane while focus is pinned to the left —
    // the state a drag or a dismissed modal leaves behind.
    p.policy = Policy::Pinned;
    p.frame(&ctx, Vec::new());
    p.frame(&ctx, vec![Event::PointerMoved(IN_THE_RIGHT_PANE)]);
    assert_eq!(p.focused, Focused::Left);

    // Now the rule is live again, and the pointer never moves: both shells
    // pour out hundreds of lines, scrolling the grid under the cursor, and
    // frames keep being drawn.
    p.policy = Policy::FollowsMouse;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(10));
        p.frame(&ctx, Vec::new());
    }
    assert!(
        p.left.sync().grid.history_size() > 20,
        "the panes never produced the output that scrolls under the cursor"
    );
    assert_eq!(
        p.focused,
        Focused::Left,
        "focus drifted to the pane the cursor was resting over, with no pointer movement"
    );

    // The keyboard is where it was, too.
    p.frame(&ctx, vec![Event::Text("still-left".to_string())]);
    pump(&ctx, &mut p, "the left pane to echo what was typed", |p| {
        screen_text(&mut p.left).contains("still-left")
    });
    assert!(!screen_text(&mut p.right).contains("still-left"));
}

/// A drag is one gesture and it belongs to the pane it started in. Crossing
/// into the neighbour mid-selection must not move the keyboard — the selection
/// would be cut in half and the next keystroke would go somewhere new.
#[test]
fn a_drag_crossing_into_the_neighbour_keeps_focus_until_it_ends() {
    let ctx = egui::Context::default();
    let (mut p, _pty) = panes(&ctx, "cat", Policy::FollowsMouse);

    p.frame(&ctx, Vec::new());
    // Press in the left pane, then drag across the boundary.
    p.frame(
        &ctx,
        vec![
            Event::PointerMoved(IN_THE_LEFT_PANE),
            click(IN_THE_LEFT_PANE, true),
        ],
    );
    assert_eq!(p.focused, Focused::Left);
    p.frame(&ctx, vec![Event::PointerMoved(IN_THE_RIGHT_PANE)]);
    assert_eq!(
        p.focused,
        Focused::Left,
        "a drag that crossed the pane boundary moved the keyboard mid-gesture"
    );
    p.frame(&ctx, vec![Event::Text("dragging".to_string())]);
    pump(
        &ctx,
        &mut p,
        "the dragging pane to echo what was typed",
        |p| screen_text(&mut p.left).contains("dragging"),
    );
    assert!(!screen_text(&mut p.right).contains("dragging"));

    // Released over the right pane: the button is up, but focus only moves on
    // the next real *move*, so the release itself is not a hover event.
    p.frame(&ctx, vec![click(IN_THE_RIGHT_PANE, false)]);
    assert_eq!(p.focused, Focused::Left);
    p.frame(&ctx, vec![Event::PointerMoved(Pos2::new(601.0, 301.0))]);
    assert_eq!(
        p.focused,
        Focused::Right,
        "the drag ended and the pointer moved, so the pane under it should have focus"
    );
}

// ---------------------------------------------------------------------------
// The wheel follows hover, focus or no focus (egui_term routing)
// ---------------------------------------------------------------------------

/// Scrollback, with focus frozen — the modal / mid-drag case. The wheel over
/// the right pane moves *its* viewport into history and leaves the focused
/// left pane at the bottom.
#[test]
fn the_wheel_scrolls_the_hovered_pane_not_the_focused_one() {
    let ctx = egui::Context::default();
    let (mut p, _pty) = panes(&ctx, FILL_SCROLLBACK, Policy::Pinned);

    // Size the grids, then wait for both panes to have real scrollback.
    p.frame(&ctx, Vec::new());
    pump(&ctx, &mut p, "both panes to fill their scrollback", |p| {
        p.left.sync().grid.history_size() > 20 && p.right.sync().grid.history_size() > 20
    });

    // Pointer into the *unfocused* right pane, on its own frame, then scroll.
    // No click anywhere: hover is the whole gesture under test.
    p.frame(&ctx, vec![Event::PointerMoved(IN_THE_RIGHT_PANE)]);
    p.frame(&ctx, vec![wheel(3.0)]);
    p.frame(&ctx, Vec::new());

    assert_eq!(p.focused, Focused::Left, "the harness froze focus");
    assert!(
        display_offset(&mut p.right) > 0,
        "the hovered pane did not scroll; its display offset is still 0"
    );
    assert_eq!(
        display_offset(&mut p.left),
        0,
        "the focused pane scrolled even though the pointer was over its neighbour"
    );

    // And back the other way: hover the focused pane and it is the one that
    // moves, which is the single-pane case unchanged.
    let before = display_offset(&mut p.right);
    p.frame(&ctx, vec![Event::PointerMoved(IN_THE_LEFT_PANE)]);
    p.frame(&ctx, vec![wheel(3.0)]);
    p.frame(&ctx, Vec::new());
    assert!(
        display_offset(&mut p.left) > 0,
        "the focused, hovered pane did not scroll"
    );
    assert_eq!(
        display_offset(&mut p.right),
        before,
        "the previously hovered pane kept scrolling after the pointer left it"
    );
}

/// Mouse reporting: the hovered pane's program gets the wheel reports, at a
/// cell inside its own grid, and the focused pane's program gets nothing.
#[test]
fn wheel_reports_go_to_the_hovered_panes_program_only() {
    let ctx = egui::Context::default();
    let (mut p, _pty) = panes(&ctx, MOUSE_REPORTING, Policy::Pinned);

    p.frame(&ctx, Vec::new());
    pump(
        &ctx,
        &mut p,
        "both programs to enable SGR mouse reporting",
        |p| {
            let sgr = egui_term::TerminalMode::SGR_MOUSE;
            p.left.sync().terminal_mode.contains(sgr) && p.right.sync().terminal_mode.contains(sgr)
        },
    );

    p.frame(&ctx, vec![Event::PointerMoved(IN_THE_RIGHT_PANE)]);
    p.frame(&ctx, vec![wheel(1.0)]);

    pump(
        &ctx,
        &mut p,
        "the hovered pane's program to echo a wheel report",
        |p| screen_text(&mut p.right).contains("^[[<6"),
    );
    let text = screen_text(&mut p.right);
    assert!(
        text.contains("^[[<64;") || text.contains("^[[<65;"),
        "no SGR wheel report reached the hovered pane: {:?}",
        text.trim_end()
    );
    // The report names a cell in the hovered pane's own grid, not the pointer's
    // window coordinates and not the stale (1,1) of a pane that never saw the
    // pointer move.
    let column: u32 = text
        .split("^[[<6")
        .nth(1)
        .and_then(|rest| rest.split(';').nth(1))
        .and_then(|col| col.parse().ok())
        .unwrap_or_default();
    assert!(
        column > 1,
        "the wheel report named cell column {column}, so the hovered pane never \
         learned where the pointer was: {:?}",
        text.trim_end()
    );

    // Give the focused pane every chance to echo something it should not have.
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(20));
        p.frame(&ctx, Vec::new());
    }
    let left_text = screen_text(&mut p.left);
    assert!(
        !left_text.contains('^'),
        "the focused pane also received the wheel: {:?}",
        left_text.trim_end()
    );
}

/// The no-steal case that survives focus-follows-mouse: with the pointer
/// merely *parked* over a pane that does not hold the keyboard — no motion, so
/// nothing focuses it — a keystroke still goes only to the focused pane. This
/// is the egui_term guarantee underneath the policy (`accepts`: keyboard
/// events need focus, whatever the pointer is doing).
#[test]
fn typing_never_leaks_into_a_hovered_but_unfocused_pane() {
    let ctx = egui::Context::default();
    let (mut p, _pty) = panes(&ctx, "cat", Policy::Pinned);

    p.frame(&ctx, Vec::new());
    p.frame(&ctx, vec![Event::PointerMoved(IN_THE_RIGHT_PANE)]);
    assert_eq!(p.focused, Focused::Left);
    p.frame(&ctx, vec![Event::Text("terra".to_string())]);

    pump(
        &ctx,
        &mut p,
        "the focused pane to echo what was typed",
        |p| screen_text(&mut p.left).contains("terra"),
    );
    assert!(
        !screen_text(&mut p.right).contains("terra"),
        "a keystroke leaked into the hovered but unfocused pane: {:?}",
        screen_text(&mut p.right).trim_end()
    );
}
