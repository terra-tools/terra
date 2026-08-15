//! The overlay scrollbar and the text selection underneath it.
//!
//! The thumb is painted *into the terminal's own layer* — no gutter, no
//! separate window — and egui's hit test only occludes across layers. So both
//! widgets sit under the pointer at once: pressing the thumb used to start a
//! text selection in the terminal as well, and once drag-autoscroll existed
//! (`drag_autoscroll.rs`), hauling the thumb up past the top edge had the
//! autoscroll poll issuing `Scroll` deltas behind the scrollbar's back. The
//! scrollbar measures each frame's delta against the offset *it* last asked
//! for, so the viewport ran away from the thumb while a bogus selection was
//! painted under the whole gesture.
//!
//! The fix is `TerminalView::set_pointer_exclusion`: the strip the overlay
//! owns is handed to the view, which then drops `PointerButton` events landing
//! in it. Only buttons — motion still goes through, so a selection begun in the
//! middle of the pane keeps extending when the pointer crosses the strip.
//!
//! The exclusion must also *stop* at the right moment: the thumb is invisible
//! and non-interactive at the live edge, and there is none at all without
//! scrollback, so the rightmost ~11 pixels of every pane have to stay ordinary
//! selectable text the rest of the time. The wiring can only consult the
//! previous frame's `ScrollbarState` (`show` runs after the view is added, so
//! the thumb wins the hit test), which is why these tests hover the strip for a
//! frame before pressing — exactly what a real pointer does.
//!
//! Same headless harness as `drag_autoscroll.rs` / `shift_selection.rs`: a real
//! PTY, egui events injected by hand, assertions on the grid. Unix-only.
#![cfg(unix)]
// `scrollbar.rs` is pulled in whole; these tests do not touch all of it.
#![allow(dead_code)]

#[path = "../src/scrollbar.rs"]
mod scrollbar;

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect};
use egui_term::{BackendSettings, PtyEvent, TerminalBackend, TerminalView};

/// A window with room above the terminal, so the thumb can be dragged out
/// through the top edge — the position that used to wake the autoscroll.
const SCREEN: Rect = Rect {
    min: Pos2::ZERO,
    max: Pos2::new(800.0, 800.0),
};
const VIEW: Rect = Rect {
    min: Pos2::new(0.0, 200.0),
    max: Pos2::new(800.0, 600.0),
};

/// Simulated seconds per frame. The thumb's fade is paced on the clock egui
/// reports, so the tests advance it deliberately rather than depending on how
/// fast the machine runs the frame loop.
const FRAME_DT: f64 = 0.05;

// Both fixtures open by building `$s`, a 200-character run of `x`, so every
// row they print wraps full width and there is real text under the rightmost
// pixels of the pane. A selection covering only blank cells reads back as an
// empty string, which would make "nothing was selected" true for the wrong
// reason.

/// Full-width rows *and* plenty of scrollback: a thumb to grab, and somewhere
/// for a runaway viewport to run to.
const WIDE_SCROLLBACK: &str = concat!(
    "s=; i=0; while [ $i -lt 200 ]; do s=\"${s}x\"; i=$((i+1)); done; ",
    "j=0; while [ $j -lt 300 ]; do echo \"line$j$s\"; j=$((j+1)); done; cat"
);

/// The same wide rows, but few enough to fit on screen: history stays empty,
/// so there is no scrollbar and the strip belongs to the terminal.
const WIDE_NO_SCROLLBACK: &str = concat!(
    "s=; i=0; while [ $i -lt 200 ]; do s=\"${s}x\"; i=$((i+1)); done; ",
    "j=0; while [ $j -lt 3 ]; do echo \"line$j$s\"; j=$((j+1)); done; cat"
);

/// One terminal plus its overlay scrollbar, wired the way `main.rs` wires
/// them: the view first (with last frame's exclusion), the scrollbar after.
struct Harness {
    ctx: egui::Context,
    backend: TerminalBackend,
    state: scrollbar::ScrollbarState,
    time: f64,
    /// The rect the view was drawn in last frame, read back rather than
    /// assumed — it is what the scrollbar's geometry is derived from.
    rect: Rect,
    _pty: Receiver<(u64, PtyEvent)>,
}

impl Harness {
    fn new(script: &str) -> Self {
        let ctx = egui::Context::default();
        let (tx, rx) = std::sync::mpsc::channel();
        let backend = TerminalBackend::new(
            0,
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
        Self {
            ctx,
            backend,
            state: scrollbar::ScrollbarState::default(),
            time: 0.0,
            rect: VIEW,
            _pty: rx,
        }
    }

    /// One frame, with the simulated clock advanced by `FRAME_DT`.
    ///
    /// This is the wiring under test, in miniature. The exclusion comes from
    /// the scrollbar state as the *previous* frame left it, because `show`
    /// cannot run until the view has been added.
    fn frame(&mut self, events: Vec<Event>) {
        self.time += FRAME_DT;
        let input = egui::RawInput {
            screen_rect: Some(SCREEN),
            events,
            time: Some(self.time),
            ..Default::default()
        };
        let backend = &mut self.backend;
        let state = &mut self.state;
        let mut drawn = None;
        let _ = self.ctx.run_ui(input, |ui: &mut egui::Ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let mut pane = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(VIEW)
                        .id_salt("terra_scrollbar_selection_pane"),
                );
                pane.set_clip_rect(VIEW);
                let exclusion = state.interactive().then(|| scrollbar::hit_area(VIEW));
                let view = TerminalView::new(&mut pane, backend)
                    .set_pointer_exclusion(exclusion)
                    .set_focus(true)
                    .set_size(VIEW.size());
                let rect = pane.add(view).rect;
                drawn = Some(rect);
                // After the terminal, so the thumb wins the hit test.
                scrollbar::show(&mut pane, rect, backend, state);
            });
        });
        if let Some(rect) = drawn {
            self.rect = rect;
        }
    }

    /// Frames with no input at all: the pointer and the button stay as they
    /// were left. A held drag has to keep working across these.
    fn park(&mut self, frames: usize) {
        for _ in 0..frames {
            self.frame(Vec::new());
        }
    }

    /// Frames plus real time, for output that has to come back off the PTY.
    fn settle(&mut self, frames: usize) {
        for _ in 0..frames {
            std::thread::sleep(Duration::from_millis(20));
            self.frame(Vec::new());
        }
    }

    /// Pump frames until `ready` holds, or fail with `what`.
    fn pump(&mut self, what: &str, mut ready: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if ready(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; the grid holds {:?}",
                self.screen_text().trim_end()
            );
            std::thread::sleep(Duration::from_millis(20));
            self.frame(Vec::new());
        }
    }

    fn screen_text(&mut self) -> String {
        self.backend
            .sync()
            .grid
            .display_iter()
            .filter(|c| !c.cell.flags.contains(Flags::WIDE_CHAR_SPACER))
            .map(|c| c.cell.c)
            .collect()
    }

    fn display_offset(&mut self) -> usize {
        self.backend.sync().grid.display_offset()
    }

    fn selection(&self) -> String {
        self.backend.selectable_content()
    }

    fn metrics(&mut self) -> scrollbar::Metrics {
        let grid = &self.backend.sync().grid;
        scrollbar::Metrics {
            history: grid.history_size(),
            screen: grid.screen_lines(),
            offset: grid.display_offset(),
        }
    }

    fn bar(&self) -> Rect {
        scrollbar::bar_rect(self.rect)
    }

    /// Where the thumb is right now.
    fn thumb(&mut self) -> Rect {
        let bar = self.bar();
        self.metrics().thumb_rect(bar).expect("a scrollable grid")
    }

    /// A point on the strip the overlay owns, at height `y`.
    fn on_strip(&self, y: f32) -> Pos2 {
        Pos2::new(self.bar().center().x, y)
    }

    /// Wait for the fixture to fill the scrollback, then reveal the thumb by
    /// resting the pointer on the strip — which is what a hand does on its way
    /// to grabbing it, and what flips `interactive` on for the next frame.
    fn ready_with_thumb(&mut self) {
        self.frame(Vec::new());
        self.pump("the fixture to fill the scrollback", |h| {
            h.backend.sync().grid.history_size() > 100 && h.screen_text().contains("line299")
        });
        let thumb = self.thumb();
        let hover = self.on_strip(thumb.center().y);
        self.frame(vec![Event::PointerMoved(hover)]);
        assert!(
            self.state.interactive(),
            "the pointer resting on the strip did not reveal the thumb, so \
             nothing below is testing the exclusion"
        );
    }

    /// Press the primary button at `pos`, hovering it first — the move and the
    /// press arrive on separate frames exactly as a real pointer delivers them.
    fn press(&mut self, pos: Pos2) {
        self.frame(vec![Event::PointerMoved(pos)]);
        self.frame(vec![click(pos, true)]);
    }

    fn drag_to(&mut self, pos: Pos2) {
        self.frame(vec![Event::PointerMoved(pos)]);
        self.park(2);
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

/// The headline: a press that lands on the thumb belongs to the scrollbar
/// alone. No selection is started underneath it, and the viewport tracks the
/// thumb the whole way — including out through the top of the view, where the
/// autoscroll used to join in and drag the offset off to the end of history.
#[test]
fn dragging_the_thumb_scrolls_without_selecting_anything() {
    let mut h = Harness::new(WIDE_SCROLLBACK);
    h.ready_with_thumb();
    assert_eq!(
        h.display_offset(),
        0,
        "the view should start at the live edge"
    );

    // Grab the thumb near its bottom, so the first (small) move that trips
    // egui's drag threshold is still inside it — outside would page instead.
    let thumb = h.thumb();
    let grab = h.on_strip(thumb.bottom() - 4.0);
    h.press(grab);
    assert!(
        h.selection().is_empty(),
        "pressing the scrollbar thumb started a text selection in the terminal \
         underneath it: {:?}",
        h.selection()
    );

    // A short first move: it has to trip egui's drag threshold while the
    // pointer is still within the thumb, since a press-and-move that lands
    // above it is a page-up, not a grab.
    let bar = h.bar();
    h.drag_to(h.on_strip(thumb.bottom() - 20.0));

    // Then up the bar in steps, watching the viewport follow.
    let mut offsets = Vec::new();
    for step in 1..=6 {
        let y = thumb.bottom() - 20.0 - step as f32 * (bar.height() / 16.0);
        let to = h.on_strip(y);
        h.drag_to(to);
        offsets.push(h.display_offset());
    }
    assert!(
        offsets.windows(2).all(|w| w[1] >= w[0]),
        "the viewport did not follow the thumb monotonically up the bar: {offsets:?}"
    );
    assert!(
        offsets[0] > 0,
        "dragging the thumb up the bar never scrolled at all"
    );

    // Out through the top edge of the view — the autoscroll's territory. It
    // must add nothing of its own: the thumb is pinned to the top of the bar,
    // so the offset is the top of history and no more.
    let out = h.on_strip(h.rect.top() - 120.0);
    h.drag_to(out);
    h.park(8);
    let history = h.metrics().history;
    let at_top = h.display_offset();
    assert_eq!(
        at_top, history,
        "the thumb was hauled to the top of the bar but the viewport sits at \
         offset {at_top} of {history}"
    );
    assert!(
        h.selection().is_empty(),
        "the drag out through the top of the view painted a selection: {:?}",
        h.selection()
    );

    // And back down. This is what a runaway cannot do: the scrollbar tracks
    // the offset *it* asked for, so anything scrolling behind its back leaves
    // the viewport stranded somewhere short of (or past) the live edge.
    let home = h.on_strip(bar.bottom() + 40.0);
    h.drag_to(home);
    h.park(8);
    assert_eq!(
        h.display_offset(),
        0,
        "dragging the thumb back to the bottom of the bar left the viewport at \
         offset {} instead of the live edge — something else was scrolling",
        h.display_offset()
    );
    assert!(
        h.selection().is_empty(),
        "the whole thumb drag selected {:?}",
        h.selection()
    );

    h.frame(vec![click(home, false)]);
    h.park(2);
    assert!(h.selection().is_empty(), "the release started a selection");
}

/// The other half of the contract: with no scrollback there is no thumb, and
/// the rightmost pixels of the pane are ordinary text again. A permanent
/// exclusion would quietly cost every pane its last column.
#[test]
fn without_a_scrollbar_the_right_edge_still_selects() {
    let mut h = Harness::new(WIDE_NO_SCROLLBACK);
    h.frame(Vec::new());
    h.pump("the fixture to print its wide rows", |h| {
        h.screen_text().contains("line2")
    });

    // Rest on the strip first: even that must not turn the exclusion on when
    // there is nothing to scroll.
    let strip = h.on_strip(h.rect.center().y);
    h.frame(vec![Event::PointerMoved(strip)]);
    assert_eq!(h.metrics().history, 0, "the fixture left scrollback behind");
    assert!(
        !h.state.interactive(),
        "the scrollbar claimed the strip with no scrollback to scroll"
    );

    // Press in the last few pixels of the view and drag left and down.
    let start = Pos2::new(h.rect.right() - 3.0, h.rect.top() + 10.0);
    h.press(start);
    let end = Pos2::new(h.rect.center().x, h.rect.top() + 60.0);
    h.drag_to(end);
    h.frame(vec![click(end, false)]);
    h.park(2);

    let selected = h.selection();
    assert!(
        selected.contains('x'),
        "a drag starting 3px from the right edge of a pane with no scrollbar \
         selected {selected:?} — the exclusion is eating the strip full time"
    );
}

/// Motion is not excluded, only buttons. A selection begun in the middle of
/// the pane has to keep growing when the pointer wanders over the strip on its
/// way to the right edge — otherwise selecting the end of a long line would
/// stall an inch short of it.
#[test]
fn a_selection_keeps_extending_across_the_strip() {
    let mut h = Harness::new(WIDE_SCROLLBACK);
    h.ready_with_thumb();

    // Press well clear of the strip, a few rows up from the bottom.
    let start = Pos2::new(h.rect.center().x, h.rect.center().y);
    h.press(start);
    let mid = Pos2::new(h.rect.center().x + 80.0, h.rect.center().y - 20.0);
    h.drag_to(mid);
    let before = h.selection();
    assert!(
        !before.is_empty(),
        "the drag selected nothing before it ever reached the strip"
    );

    // Now out over the strip and up a few more rows.
    let over = h.on_strip(h.rect.center().y - 60.0);
    h.drag_to(over);
    h.park(2);
    assert!(
        h.state.interactive(),
        "the pointer is sitting on a live strip — otherwise this test proves \
         nothing about motion crossing an exclusion"
    );
    let after = h.selection();
    assert!(
        after.len() > before.len(),
        "the selection stopped growing once the pointer crossed the scrollbar \
         strip: {} chars before, {} after",
        before.len(),
        after.len()
    );

    h.frame(vec![click(over, false)]);
    h.park(2);
    assert_eq!(
        h.selection(),
        after,
        "releasing over the strip changed the selection"
    );
}
