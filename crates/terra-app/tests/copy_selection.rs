//! What a selection actually copies.
//!
//! Selecting is only half the gesture: the other half is the string that
//! lands on the pasteboard when ⌘C follows. That string used to be built by
//! walking the *visible* grid and pushing every cell the selection range
//! contained, which got four separate things wrong in one real user copy —
//! rows ran together with no newline between them, every row carried the
//! blank padding out to the pane's right edge, the second cell of a
//! double-width character came along as a stray space, and anything the
//! selection reached in scrollback was silently dropped. The last one
//! stopped being theoretical the moment a drag past the edge of the view
//! started autoscrolling (`drag_autoscroll.rs`): a selection now routinely
//! ends up mostly off screen.
//!
//! These tests drive the same PTY-backed headless harness as
//! `shift_selection.rs` and `hover_scroll.rs`: a real `/bin/sh`, egui events
//! injected by hand, assertions on `TerminalBackend::selectable_content()`.
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

/// Eight numbered rows of a recognisable word, then a program that just sits
/// there so nothing scrolls out from under the drag.
const ROWS: &str = "i=0; while [ $i -lt 8 ]; do echo \"COPY$i-COPY$i\"; i=$((i+1)); done; cat";
/// 300 numbered lines: more than one screen, so there is history to select
/// from.
const FILL_SCROLLBACK: &str = "i=0; while [ $i -lt 300 ]; do echo line$i; i=$((i+1)); done; cat";
/// One logical line far wider than any pane, printed without a newline until
/// the end — the terminal soft-wraps it across several grid rows and marks
/// every continued row `WRAPLINE`.
const LONG_LINE: &str =
    "i=0; while [ $i -lt 300 ]; do printf X; i=$((i+1)); done; printf '\\n'; cat";
/// A row of double-width characters, whose second cell the grid fills with a
/// `WIDE_CHAR_SPACER`.
const WIDE_CHARS: &str = "printf '\\346\\227\\245\\346\\234\\254\\350\\252\\236ABC\\n'; cat";

/// One terminal in one window.
struct Harness {
    ctx: egui::Context,
    backend: TerminalBackend,
    /// The rect the view was drawn in last frame, read back from the widget
    /// rather than assumed — the panel's margins move it.
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
            rect: SCREEN,
            _pty: rx,
        }
    }

    /// One frame of a terra-shaped window. The rect the view lands in is read
    /// back out, since every position these tests aim at is derived from it.
    fn frame(&mut self, events: Vec<Event>) {
        let input = egui::RawInput {
            screen_rect: Some(SCREEN),
            events,
            ..Default::default()
        };
        let backend = &mut self.backend;
        let mut drawn = None;
        let _ = self.ctx.run_ui(input, |ui: &mut egui::Ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let view = TerminalView::new(ui, backend)
                    .set_focus(true)
                    .set_size(ui.available_size());
                drawn = Some(ui.add(view).rect);
            });
        });
        if let Some(rect) = drawn {
            self.rect = rect;
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

    /// Frames plus real time, for output that has to come back off the PTY.
    fn settle(&mut self, frames: usize) {
        for _ in 0..frames {
            std::thread::sleep(Duration::from_millis(20));
            self.frame(Vec::new());
        }
    }

    /// Wait for `needle` to appear on the grid.
    fn ready_when_printed(&mut self, needle: &str) {
        self.frame(Vec::new());
        let needle = needle.to_string();
        self.pump("the fixture to print", |h| {
            h.screen_text().contains(&needle)
        });
    }

    /// Everything on the grid, as one string, spacers dropped.
    fn screen_text(&mut self) -> String {
        self.backend
            .sync()
            .grid
            .display_iter()
            .filter(|c| !c.cell.flags.contains(Flags::WIDE_CHAR_SPACER))
            .map(|c| c.cell.c)
            .collect()
    }

    fn columns(&mut self) -> usize {
        self.backend.sync().grid.columns()
    }

    fn screen_lines(&mut self) -> usize {
        self.backend.sync().grid.screen_lines()
    }

    fn display_offset(&mut self) -> usize {
        self.backend.sync().grid.display_offset()
    }

    fn selection(&self) -> String {
        self.backend.selectable_content()
    }

    /// The middle of the cell at (`row`, `col`) of the *visible* grid.
    ///
    /// Derived from the rect the view reported and the grid it decided to
    /// use, so the tests never guess at a font metric.
    fn cell_center(&mut self, row: usize, col: usize) -> Pos2 {
        let cw = self.rect.width() / self.columns() as f32;
        let ch = self.rect.height() / self.screen_lines() as f32;
        Pos2::new(
            self.rect.min.x + (col as f32 + 0.5) * cw,
            self.rect.min.y + (row as f32 + 0.5) * ch,
        )
    }

    /// Press at one cell, drag to another, release — the gesture a user makes
    /// to select. Each step is its own frame, exactly as a real pointer
    /// delivers it (the view only takes a click it is hovering).
    fn drag(&mut self, from: (usize, usize), to: (usize, usize)) {
        let from = self.cell_center(from.0, from.1);
        let to = self.cell_center(to.0, to.1);
        self.frame(vec![Event::PointerMoved(from)]);
        self.frame(vec![click(from, true)]);
        self.frame(vec![Event::PointerMoved(to)]);
        self.frame(vec![click(to, false)]);
        self.frame(Vec::new());
    }

    /// Scroll terra's own viewport by `lines` (positive scrolls up, into
    /// history), with the pointer parked over the terminal.
    fn scroll(&mut self, lines: f32) {
        let over = self.rect.center();
        self.frame(vec![Event::PointerMoved(over)]);
        self.frame(vec![Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            delta: Vec2::new(0.0, lines),
            modifiers: Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        }]);
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

/// The headline: rows come back as rows. A selection spanning three printed
/// lines pastes as three lines — before the fix it was one run-on string that
/// only *looked* wrapped wherever the receiving app happened to break it.
#[test]
fn a_multi_row_selection_copies_with_newlines() {
    let mut h = Harness::new(ROWS);
    h.ready_when_printed("COPY7");

    let cols = h.columns();
    h.drag((0, 0), (2, cols - 1));

    let selected = h.selection();
    println!("three rows copied as {selected:?}");
    let lines: Vec<&str> = selected.lines().collect();
    assert_eq!(
        lines,
        vec!["COPY0-COPY0", "COPY1-COPY1", "COPY2-COPY2"],
        "a three-row selection must copy as three lines, got {selected:?}"
    );
}

/// Every row of the grid is padded out to the pane's width with blanks. A
/// selection that reaches the right edge must not bring them along: pasting
/// used to leave a trail of spaces behind each line, which reflows into
/// nonsense in anything that wraps.
#[test]
fn trailing_blanks_are_trimmed_from_every_row() {
    let mut h = Harness::new(ROWS);
    h.ready_when_printed("COPY7");

    let cols = h.columns();
    h.drag((0, 0), (3, cols - 1));

    let selected = h.selection();
    println!("a full-width band copied as {selected:?}");
    assert!(
        !selected.contains("  "),
        "the copy kept the row padding: {selected:?}"
    );
    for line in selected.lines() {
        assert!(
            !line.ends_with(' '),
            "row {line:?} kept its trailing blanks (full copy {selected:?})"
        );
    }
}

/// A selection is anchored to the terminal's history, not to what is on
/// screen. Scroll into scrollback, select there, scroll back to the bottom:
/// the copy must still hold the rows that have gone off screen. This is the
/// defect drag autoscroll made unavoidable — the old code walked
/// `display_iter()`, so everything outside the viewport vanished from the
/// copy.
#[test]
fn a_selection_in_scrollback_copies_the_offscreen_rows() {
    let mut h = Harness::new(FILL_SCROLLBACK);
    h.frame(Vec::new());
    h.pump("the fixture to fill the scrollback", |h| {
        h.backend.sync().grid.history_size() > 100 && h.screen_text().contains("line299")
    });

    // Into history, far enough that the rows selected below can never be on
    // screen again once the viewport comes back to the bottom.
    for _ in 0..12 {
        h.scroll(10.0);
    }
    assert!(
        h.display_offset() > 60,
        "the wheel never took the viewport into history (offset {})",
        h.display_offset()
    );

    let cols = h.columns();
    h.drag((0, 0), (2, cols - 1));
    let while_visible = h.selection();
    println!("selected in history: {while_visible:?}");
    let rows: Vec<String> = while_visible.lines().map(str::to_string).collect();
    assert_eq!(
        rows.len(),
        3,
        "expected three history rows: {while_visible:?}"
    );
    assert!(
        rows.iter().all(|r| r.starts_with("line")),
        "the selection did not land on the numbered fixture rows: {while_visible:?}"
    );

    // Back to the live end. The selected rows are now nowhere near the
    // viewport; the copy must be byte-for-byte what it was.
    for _ in 0..30 {
        h.scroll(-10.0);
    }
    assert_eq!(
        h.display_offset(),
        0,
        "the viewport never came back to the bottom"
    );
    let after_scroll = h.selection();
    println!("the same selection, now off screen: {after_scroll:?}");
    assert_eq!(
        after_scroll, while_visible,
        "scrolling the selection off screen changed what it copies"
    );
    assert!(
        !h.screen_text().contains(rows[0].as_str()),
        "the fixture rows are still on screen, so this proved nothing"
    );
}

/// A line too long for the pane is one line the user typed, drawn across
/// several grid rows. The grid marks each continued row `WRAPLINE`, and a
/// copy must honour it: pasting a wrapped command back into a shell has to
/// give the shell the same single command, not three fragments separated by
/// newlines that each run on their own.
#[test]
fn a_soft_wrapped_line_copies_back_as_one_line() {
    let mut h = Harness::new(LONG_LINE);
    h.ready_when_printed("XXXX");
    h.settle(10);

    let cols = h.columns();
    let wrapped_rows = 300_usize.div_ceil(cols);
    assert!(
        wrapped_rows >= 2,
        "the fixture line did not wrap at {cols} columns"
    );
    h.drag((0, 0), (wrapped_rows - 1, cols - 1));

    let selected = h.selection();
    println!("a wrapped line copied as {} chars", selected.len());
    assert!(
        !selected.contains('\n'),
        "the copy broke the wrapped line at the pane edge: {selected:?}"
    );
    assert_eq!(
        selected,
        "X".repeat(300),
        "the wrapped line came back changed"
    );
}

/// The second cell of a double-width character is a spacer, not a character.
/// Copying it as one used to inject a blank after every CJK glyph.
#[test]
fn double_width_characters_copy_without_their_spacers() {
    let mut h = Harness::new(WIDE_CHARS);
    h.ready_when_printed("ABC");

    let cols = h.columns();
    h.drag((0, 0), (0, cols - 1));

    let selected = h.selection();
    println!("a row of wide characters copied as {selected:?}");
    assert_eq!(selected, "日本語ABC");
}

/// The regression guard: the case that always worked must keep working. A
/// selection inside a single row is the plain substring — no newline, no
/// padding, nothing added by the rewrite.
#[test]
fn a_single_row_selection_is_unchanged() {
    let mut h = Harness::new(ROWS);
    h.ready_when_printed("COPY7");

    // Released on the left half of column 5, which is where a user lets go
    // to take the five characters before it.
    h.drag((1, 0), (1, 5));

    let selected = h.selection();
    println!("one row copied as {selected:?}");
    assert_eq!(selected, "COPY1");
}
