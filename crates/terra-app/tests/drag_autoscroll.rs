//! Dragging past the edge of the terminal scrolls, and keeps selecting.
//!
//! Selecting more than one screen of output is the whole point of scrollback:
//! press somewhere, drag up past the top of the view, and the viewport has to
//! follow the pointer into history while the selection keeps growing behind
//! it. Every terminal does this; without it a selection can never be larger
//! than what happens to be on screen.
//!
//! The mechanism is a **per-frame poll**, not an event handler, and that is
//! what these tests are shaped around. Once the pointer leaves the widget's
//! rect the view stops accepting pointer events at all (`view.rs::accepts`
//! wants `hovered && focused`), so there is no `PointerMoved` to react to —
//! the view has to look at `latest_pos()` / `primary_down()` itself, every
//! frame, for as long as the button is held. The tests therefore park the
//! pointer outside the rect, send *no* events, and pump frames: anything that
//! only fires on a move event would sit still and fail.
//!
//! Two things the poll must not do: scroll while a program owns the mouse
//! (a reported drag belongs to the program), and scroll on the alternate
//! screen (where `scroll()` types arrow keys at the program instead —
//! `backend/mod.rs`'s `ALTERNATE_SCROLL | ALT_SCREEN` path — so a drag near
//! the edge of vim would silently move the cursor).
//!
//! Same headless harness as `hover_scroll.rs` / `shift_selection.rs`: a real
//! PTY, egui events injected by hand, assertions on the grid. Unix-only.
#![cfg(unix)]

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect};
use egui_term::{BackendSettings, PtyEvent, TerminalBackend, TerminalView};

/// A window with room above and below the terminal, so the pointer has
/// somewhere to overshoot to. The rect the view is drawn in is the middle
/// band; 200px of slack on each side is enough for a "just past the edge"
/// and a "far past the edge" position that differ by a lot.
const SCREEN: Rect = Rect {
    min: Pos2::ZERO,
    max: Pos2::new(800.0, 800.0),
};
const VIEW: Rect = Rect {
    min: Pos2::new(0.0, 200.0),
    max: Pos2::new(800.0, 600.0),
};

/// Simulated seconds added to `RawInput::time` per frame. The autoscroll is
/// paced on the clock egui reports, so the tests advance it deliberately
/// rather than depending on how fast the machine runs the frame loop.
const FRAME_DT: f64 = 0.05;
/// Frames pumped in each "hold still and watch it scroll" stretch.
const PUMPED_FRAMES: usize = 12;

/// A pane with plenty of scrollback and nothing else going on.
const FILL_SCROLLBACK: &str = "i=0; while [ $i -lt 300 ]; do echo line$i; i=$((i+1)); done; cat";
/// The same, but with SGR mouse tracking on at the end — a program that owns
/// the mouse, on the primary screen so there is still scrollback to scroll.
const MOUSE_REPORTING: &str = "i=0; while [ $i -lt 300 ]; do echo line$i; i=$((i+1)); done; \
     printf '\\033[?1000h\\033[?1006h'; cat";
/// A program on the alternate screen, echoing whatever it is sent — so an
/// arrow key typed at it by the scroll path shows up on the grid as `^[OA`.
const ALT_SCREEN: &str = "printf '\\033[?1049h'; \
     i=0; while [ $i -lt 40 ]; do echo altline$i; i=$((i+1)); done; cat";

/// One terminal in one window, plus the simulated clock.
struct Harness {
    ctx: egui::Context,
    backend: TerminalBackend,
    time: f64,
    /// The rect the view was drawn in last frame — the edges the pointer has
    /// to get past. Read back from the widget rather than assumed, since the
    /// panel's own margins move it.
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
            time: 0.0,
            rect: VIEW,
            _pty: rx,
        }
    }

    /// One frame, with the simulated clock advanced by `FRAME_DT`.
    ///
    /// The view is drawn into a child `Ui` clipped to `VIEW`, the way
    /// `TreeFrame::leaf` draws a pane, so the window has area that is *not*
    /// the terminal — which is the only way to put the pointer past an edge.
    fn frame(&mut self, events: Vec<Event>) {
        self.time += FRAME_DT;
        let input = egui::RawInput {
            screen_rect: Some(SCREEN),
            events,
            time: Some(self.time),
            ..Default::default()
        };
        let backend = &mut self.backend;
        let mut drawn = None;
        let _ = self.ctx.run_ui(input, |ui: &mut egui::Ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let mut pane = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(VIEW)
                        .id_salt("terra_autoscroll_pane"),
                );
                pane.set_clip_rect(VIEW);
                let view = TerminalView::new(&mut pane, backend)
                    .set_focus(true)
                    .set_size(VIEW.size());
                drawn = Some(pane.add(view).rect);
            });
        });
        if let Some(rect) = drawn {
            self.rect = rect;
        }
    }

    /// Frames with no input at all: the pointer is wherever it was left and
    /// the button is however it was left. This is where a poll keeps working
    /// and an event handler goes quiet.
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
        let deadline = Instant::now() + Duration::from_secs(10);
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

    /// Everything on the grid, as one string. Bytes the tty echoes back show
    /// up caret-style, so an SGR mouse report reads as `^[[<0;12;3M` and an
    /// arrow key as `^[OA`.
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

    /// The middle of the view — where a drag starts.
    fn middle(&self) -> Pos2 {
        self.rect.center()
    }

    /// `overshoot` pixels above the top edge, and below the bottom one.
    fn above(&self, overshoot: f32) -> Pos2 {
        Pos2::new(self.rect.center().x, self.rect.top() - overshoot)
    }

    fn below(&self, overshoot: f32) -> Pos2 {
        Pos2::new(self.rect.center().x, self.rect.bottom() + overshoot)
    }

    /// Wait for the fixture to have printed its 300 lines and filled the
    /// scrollback, so there is somewhere to scroll *to*.
    fn ready_with_scrollback(&mut self) {
        self.frame(Vec::new());
        self.pump("the fixture to fill the scrollback", |h| {
            h.backend.sync().grid.history_size() > 100 && h.screen_text().contains("line299")
        });
    }

    /// Press the primary button at `pos`, hovering it first — the view only
    /// takes a click it is under, so the move and the press are separate
    /// frames exactly as a real pointer delivers them.
    fn press(&mut self, pos: Pos2) {
        self.frame(vec![Event::PointerMoved(pos)]);
        self.frame(vec![click(pos, true)]);
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

/// Every `lineNNN` label in `text`, in the order they appear. The fixture
/// numbers its output, so a label is a stable name for a row wherever it has
/// scrolled to — which is what lets a test say "the selection reached
/// something that was not on screen when the drag began".
fn labels(text: &str) -> Vec<u32> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("line") {
        rest = &rest[at + "line".len()..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse() {
            found.push(n);
        }
    }
    found
}

fn lowest(text: &str) -> u32 {
    labels(text).into_iter().min().unwrap_or(u32::MAX)
}

fn highest(text: &str) -> u32 {
    labels(text).into_iter().max().unwrap_or(0)
}

/// The headline: a drag that leaves the top of the view pulls the viewport
/// into scrollback, and the selection keeps growing into rows that were not
/// on screen when the button went down.
#[test]
fn dragging_past_the_top_scrolls_into_scrollback_and_keeps_selecting() {
    let mut h = Harness::new(FILL_SCROLLBACK);
    h.ready_with_scrollback();

    let on_screen_at_press = h.screen_text();
    let oldest_visible = lowest(&on_screen_at_press);
    assert!(
        oldest_visible < u32::MAX,
        "the fixture printed no numbered lines: {:?}",
        on_screen_at_press.trim_end()
    );
    assert_eq!(
        h.display_offset(),
        0,
        "the view should start at the live edge"
    );

    // Press in the middle, then leave the view through the top and hold
    // still. No further events: the poll is the only thing running.
    let start = h.middle();
    h.press(start);
    let out = h.above(40.0);
    h.frame(vec![Event::PointerMoved(out)]);
    h.park(PUMPED_FRAMES);

    assert!(
        h.display_offset() > 0,
        "the pointer sat above the view with the button held and the viewport \
         never scrolled into scrollback"
    );
    let selected = h.selection();
    assert!(
        !selected.is_empty(),
        "nothing was selected at all by the drag"
    );
    assert!(
        lowest(&selected) < oldest_visible,
        "the selection reached back only to line{}, but line{} was already the \
         oldest row on screen when the drag started — it never grew into the \
         newly revealed scrollback",
        lowest(&selected),
        oldest_visible
    );
}

/// Symmetric, and in one gesture: having scrolled up into history, dragging
/// out through the *bottom* edge winds the viewport back toward the live edge
/// and extends the selection the other way.
#[test]
fn dragging_past_the_bottom_scrolls_back_down_and_extends_the_other_way() {
    let mut h = Harness::new(FILL_SCROLLBACK);
    h.ready_with_scrollback();

    let start = h.middle();
    h.press(start);

    // Up first, so there is somewhere to come back from.
    let up = h.above(40.0);
    h.frame(vec![Event::PointerMoved(up)]);
    h.park(PUMPED_FRAMES);
    let scrolled_up = h.display_offset();
    assert!(scrolled_up > 0, "the drag never scrolled up to begin with");
    let reached_up = highest(&h.selection());

    // Now out through the bottom, same held button.
    let down = h.below(40.0);
    h.frame(vec![Event::PointerMoved(down)]);
    h.park(PUMPED_FRAMES);

    assert!(
        h.display_offset() < scrolled_up,
        "the pointer sat below the view and the viewport stayed at display \
         offset {scrolled_up} instead of winding back toward the live edge"
    );
    let selected = h.selection();
    assert!(
        highest(&selected) > reached_up,
        "the selection still ends at line{}, the same row the upward drag left \
         it at — dragging past the bottom did not extend it forward",
        highest(&selected)
    );
}

/// The poll, isolated. The pointer stops moving entirely and no event is sent
/// for the rest of the test: successive frames must keep scrolling, and a
/// pointer held far past the edge must cover more ground than one held just
/// past it over the same simulated time.
#[test]
fn holding_still_outside_the_view_keeps_scrolling_and_farther_out_scrolls_faster() {
    // Just past the edge.
    let mut near = Harness::new(FILL_SCROLLBACK);
    near.ready_with_scrollback();
    let start = near.middle();
    near.press(start);
    // Past the dead zone, but only just: the rate ramps up with distance, so
    // this is the slow end of it.
    let just_out = near.above(20.0);
    near.frame(vec![Event::PointerMoved(just_out)]);

    near.park(PUMPED_FRAMES);
    let after_first_stretch = near.display_offset();
    assert!(
        after_first_stretch > 0,
        "holding the pointer just past the top edge never scrolled"
    );
    near.park(PUMPED_FRAMES);
    let after_second_stretch = near.display_offset();
    assert!(
        after_second_stretch > after_first_stretch,
        "the scroll stopped after the first stretch of frames (offset stuck at \
         {after_first_stretch}) — it fired once on the move event instead of \
         polling every frame"
    );

    // Far past the edge, from a fresh terminal so the two runs are comparable.
    let mut far = Harness::new(FILL_SCROLLBACK);
    far.ready_with_scrollback();
    let start = far.middle();
    far.press(start);
    let way_out = far.above(180.0);
    far.frame(vec![Event::PointerMoved(way_out)]);
    far.park(PUMPED_FRAMES);
    let far_offset = far.display_offset();

    assert!(
        far_offset > after_first_stretch,
        "a pointer 180px past the edge scrolled {far_offset} lines, no more than \
         the {after_first_stretch} of one 20px past it — the speed does not \
         follow the overshoot"
    );
}

/// A hair past the edge is a dead zone, not a scroll. The view is inset from
/// the pane it sits in, so a pointer resting in that gutter — or a hand that
/// overshoots the last row by a pixel while aiming at it — must not send the
/// viewport flying into the scrollback.
#[test]
fn a_pointer_a_hair_past_the_edge_does_not_scroll() {
    let mut h = Harness::new(FILL_SCROLLBACK);
    h.ready_with_scrollback();
    let start = h.middle();
    h.press(start);
    let barely_out = h.above(2.0);
    h.frame(vec![Event::PointerMoved(barely_out)]);
    h.park(PUMPED_FRAMES * 3);

    assert_eq!(
        h.display_offset(),
        0,
        "a pointer 2px past the top edge scrolled into the scrollback — the \
         dead zone is gone and the pane's own inset now reads as a scroll"
    );
}

/// Letting go outside the view ends the drag. The release lands where the
/// terminal takes no pointer events at all, so the poll is the only thing
/// that can notice it; before this, the drag stayed latched and the terminal
/// went on scrolling under a button nobody was holding.
#[test]
fn releasing_outside_the_view_stops_the_autoscroll() {
    let mut h = Harness::new(FILL_SCROLLBACK);
    h.ready_with_scrollback();

    let start = h.middle();
    h.press(start);
    let out = h.above(60.0);
    h.frame(vec![Event::PointerMoved(out)]);
    h.park(PUMPED_FRAMES);
    assert!(h.display_offset() > 0, "the drag never scrolled");

    h.frame(vec![click(out, false)]);
    let offset_at_release = h.display_offset();
    let selection_at_release = h.selection();

    // Plenty of frames with the pointer still parked outside.
    h.park(PUMPED_FRAMES * 3);

    assert_eq!(
        h.display_offset(),
        offset_at_release,
        "the viewport kept scrolling after the button was released outside the view"
    );
    assert_eq!(
        h.selection(),
        selection_at_release,
        "the selection kept growing after the button was released outside the view"
    );
}

/// A program that grabbed the mouse owns the whole drag: press, motion and
/// release are its business, and terra must not scroll its viewport out from
/// under it — nor invent extra reports while the pointer sits outside.
#[test]
fn a_reported_drag_never_autoscrolls() {
    let mut h = Harness::new(MOUSE_REPORTING);
    h.ready_with_scrollback();
    h.pump("the program to enable SGR mouse reporting", |h| {
        h.backend
            .sync()
            .terminal_mode
            .contains(egui_term::TerminalMode::SGR_MOUSE)
    });

    let start = h.middle();
    h.press(start);
    h.settle(15);
    let reports_after_press = h.screen_text().matches("^[[<").count();
    assert!(
        reports_after_press > 0,
        "the press was not reported to the program, so this is not the case \
         under test: {:?}",
        h.screen_text().trim_end()
    );

    let out = h.above(120.0);
    h.frame(vec![Event::PointerMoved(out)]);
    h.park(PUMPED_FRAMES);
    h.settle(15);

    assert_eq!(
        h.display_offset(),
        0,
        "terra scrolled its own viewport during a drag the program owns"
    );
    assert_eq!(
        h.screen_text().matches("^[[<").count(),
        reports_after_press,
        "the autoscroll poll sent the program extra mouse reports while the \
         pointer sat outside the view: {:?}",
        h.screen_text().trim_end()
    );
}

/// On the alternate screen there is no scrollback, and the scroll path types
/// arrow keys at the program instead (`ALTERNATE_SCROLL | ALT_SCREEN`). A
/// drag that strays past the edge of a full-screen program must therefore not
/// scroll at all — otherwise selecting text in vim moves the cursor.
#[test]
fn a_drag_past_the_edge_types_nothing_on_the_alternate_screen() {
    let mut h = Harness::new(ALT_SCREEN);
    h.frame(Vec::new());
    h.pump("the program to switch to the alternate screen", |h| {
        h.backend
            .sync()
            .terminal_mode
            .contains(egui_term::TerminalMode::ALT_SCREEN)
            && h.screen_text().contains("altline")
    });

    let start = h.middle();
    h.press(start);
    let out = h.above(120.0);
    h.frame(vec![Event::PointerMoved(out)]);
    h.park(PUMPED_FRAMES);
    h.settle(20);

    let text = h.screen_text();
    assert!(
        !text.contains("^[OA") && !text.contains("^[[A"),
        "the autoscroll typed up-arrows at the full-screen program: {:?}",
        text.trim_end()
    );
    assert!(
        !text.contains("^[OB") && !text.contains("^[[B"),
        "the autoscroll typed down-arrows at the full-screen program: {:?}",
        text.trim_end()
    );
    assert_eq!(
        h.display_offset(),
        0,
        "the alternate screen has no scrollback, but the viewport moved anyway"
    );
}

/// Sanity: the harness really does leave room outside the terminal, so
/// "past the edge" means something. If the view ever filled the window these
/// tests would be pressing and holding inside it and passing for the wrong
/// reason.
#[test]
fn the_harness_leaves_room_above_and_below_the_view() {
    let mut h = Harness::new("cat");
    h.frame(Vec::new());
    assert!(
        h.rect.top() > 100.0 && h.rect.bottom() < SCREEN.max.y - 100.0,
        "the terminal was drawn at {:?}, with no slack for the pointer to \
         overshoot into",
        h.rect
    );
}
