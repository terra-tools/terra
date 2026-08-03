//! Holding Shift (or Option) must bypass mouse reporting so text can still be
//! selected inside a program that grabbed the mouse (claude code, htop, vim).
//!
//! Without an escape hatch every click, drag and release is encoded for the
//! program and terra never builds a selection, so nothing on screen can be
//! copied. Shift is xterm's convention, Option the macOS one; both are
//! honoured, and neither may change what a *bare* drag sends to the program.
//!
//! Same headless harness as `mouse_reporting.rs`: a real PTY, `/bin/sh`
//! printing a known page and then echoing every byte that arrives, egui events
//! injected by hand. Unix-only.
#![cfg(unix)]

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use alacritty_terminal::term::cell::Flags;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, Vec2};
use egui_term::{BackendSettings, PtyEvent, TerminalBackend, TerminalView};

const SCREEN: Rect = Rect {
    min: Pos2::ZERO,
    max: Pos2::new(800.0, 600.0),
};
/// The drag: both ends sit inside the printed block, several cells apart on
/// the same short row, so the selection can only be printed text.
const DRAG_FROM: Pos2 = Pos2::new(60.0, 40.0);
const DRAG_TO: Pos2 = Pos2::new(260.0, 40.0);

/// One frame of a terra-shaped window, with `modifiers` held down for the
/// whole frame — `PointerMoved` carries no modifiers of its own, the widget
/// reads them off the context, so a drag has to hold them at the frame level.
fn frame(
    ctx: &egui::Context,
    backend: &mut TerminalBackend,
    events: Vec<Event>,
    modifiers: Modifiers,
) {
    let input = egui::RawInput {
        screen_rect: Some(SCREEN),
        events,
        modifiers,
        ..Default::default()
    };
    let _ = ctx.run_ui(input, |ui: &mut egui::Ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            let view = TerminalView::new(ui, backend)
                .set_focus(true)
                .set_size(ui.available_size());
            ui.add(view);
        });
    });
}

/// Everything on the grid, as one string. Bytes the tty echoes back show up
/// caret-style, so an SGR mouse report reads as `^[[<0;12;3M`.
fn screen_text(backend: &mut TerminalBackend) -> String {
    backend
        .sync()
        .grid
        .display_iter()
        .filter(|c| !c.cell.flags.contains(Flags::WIDE_CHAR_SPACER))
        .map(|c| c.cell.c)
        .collect()
}

/// A PTY that fills the top of the screen with a recognisable word, turns on
/// SGR + button mouse tracking the way claude code does, and then echoes
/// every byte it receives.
///
/// Deliberately *not* on the alternate screen: the printed page has to stay
/// visible for the drag to have something to select.
fn mouse_reporting_page(ctx: &egui::Context) -> (TerminalBackend, Receiver<(u64, PtyEvent)>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let backend = TerminalBackend::new(
        0,
        ctx.clone(),
        tx,
        BackendSettings {
            shell: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                r"i=0; while [ $i -lt 8 ]; do \
                  printf 'SELECTME-SELECTME-SELECTME-SELECTME-SELECTME\n'; \
                  i=$((i+1)); done; \
                  printf '\033[?1000h\033[?1006h'; cat"
                    .to_string(),
            ],
            ..Default::default()
        },
    )
    .expect("spawn /bin/sh");
    (backend, rx)
}

fn click(pos: Pos2, pressed: bool, modifiers: Modifiers) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers,
    }
}

/// Pump frames until the program has enabled SGR mouse reporting and its
/// output has landed on the grid.
fn ready(ctx: &egui::Context, backend: &mut TerminalBackend) {
    let deadline = Instant::now() + Duration::from_secs(10);
    frame(ctx, backend, Vec::new(), Modifiers::NONE);
    loop {
        let moded = backend
            .sync()
            .terminal_mode
            .contains(egui_term::TerminalMode::SGR_MOUSE);
        if moded && screen_text(backend).contains("SELECTME") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the PTY program never came up; the grid holds {:?}",
            screen_text(backend).trim_end()
        );
        std::thread::sleep(Duration::from_millis(20));
        frame(ctx, backend, Vec::new(), Modifiers::NONE);
    }
}

/// Press at one end, move, release at the other — the whole gesture held
/// under `modifiers`.
fn drag(ctx: &egui::Context, backend: &mut TerminalBackend, modifiers: Modifiers) {
    frame(
        ctx,
        backend,
        vec![Event::PointerMoved(DRAG_FROM)],
        modifiers,
    );
    frame(
        ctx,
        backend,
        vec![click(DRAG_FROM, true, modifiers)],
        modifiers,
    );
    frame(ctx, backend, vec![Event::PointerMoved(DRAG_TO)], modifiers);
    frame(
        ctx,
        backend,
        vec![click(DRAG_TO, false, modifiers)],
        modifiers,
    );
    // Let anything the drag wrote come back through the tty.
    for _ in 0..25 {
        std::thread::sleep(Duration::from_millis(20));
        frame(ctx, backend, Vec::new(), Modifiers::NONE);
    }
}

/// Baseline: without a modifier the program still owns the mouse. The drag
/// must reach it as SGR reports and must leave terra with no selection.
#[test]
fn a_bare_drag_still_goes_to_the_program() {
    let ctx = egui::Context::default();
    let (mut backend, _events) = mouse_reporting_page(&ctx);
    ready(&ctx, &mut backend);

    drag(&ctx, &mut backend, Modifiers::NONE);

    let text = screen_text(&mut backend);
    assert!(
        text.contains("^[[<0;"),
        "no SGR button report reached the program: {:?}",
        text.trim_end()
    );
    assert!(
        text.contains('m'),
        "the release was never reported (SGR release ends in 'm'): {:?}",
        text.trim_end()
    );
    assert_eq!(
        backend.selectable_content(),
        "",
        "a bare drag must not select anything while the program owns the mouse"
    );
}

/// The fix: Shift hands the mouse back to terra. Nothing reaches the program,
/// and the dragged text is selected — the same `SelectStart`/`SelectUpdate`
/// machinery a drag uses with mouse reporting off, so ⌘C copies it.
#[test]
fn shift_drag_selects_instead_of_reporting() {
    let ctx = egui::Context::default();
    let (mut backend, _events) = mouse_reporting_page(&ctx);
    ready(&ctx, &mut backend);

    drag(&ctx, &mut backend, Modifiers::SHIFT);

    let text = screen_text(&mut backend);
    assert!(
        !text.contains("^[[<"),
        "a mouse report reached the program despite Shift: {:?}",
        text.trim_end()
    );
    let selected = backend.selectable_content();
    assert!(
        selected.contains("SELECT"),
        "Shift-drag selected {selected:?}, expected part of the printed page"
    );
}

/// Option is the macOS convention for the same escape hatch.
#[test]
fn option_drag_selects_instead_of_reporting() {
    let ctx = egui::Context::default();
    let (mut backend, _events) = mouse_reporting_page(&ctx);
    ready(&ctx, &mut backend);

    drag(&ctx, &mut backend, Modifiers::ALT);

    let text = screen_text(&mut backend);
    assert!(
        !text.contains("^[[<"),
        "a mouse report reached the program despite Option: {:?}",
        text.trim_end()
    );
    assert!(backend.selectable_content().contains("SELECT"));
}

/// Letting go of Shift mid-drag must not hand the drag over: the press
/// decided who owns it. Before the latch, the release was re-decided against
/// the modifiers as they were *then*, so the program got a release it had
/// never seen a press for and terra's drag never ended.
#[test]
fn releasing_shift_mid_drag_keeps_the_selection() {
    let ctx = egui::Context::default();
    let (mut backend, _events) = mouse_reporting_page(&ctx);
    ready(&ctx, &mut backend);

    frame(
        &ctx,
        &mut backend,
        vec![Event::PointerMoved(DRAG_FROM)],
        Modifiers::SHIFT,
    );
    frame(
        &ctx,
        &mut backend,
        vec![click(DRAG_FROM, true, Modifiers::SHIFT)],
        Modifiers::SHIFT,
    );
    // Shift comes up before the mouse button.
    frame(
        &ctx,
        &mut backend,
        vec![Event::PointerMoved(DRAG_TO)],
        Modifiers::NONE,
    );
    frame(
        &ctx,
        &mut backend,
        vec![click(DRAG_TO, false, Modifiers::NONE)],
        Modifiers::NONE,
    );
    for _ in 0..25 {
        std::thread::sleep(Duration::from_millis(20));
        frame(&ctx, &mut backend, Vec::new(), Modifiers::NONE);
    }

    let text = screen_text(&mut backend);
    assert!(
        !text.contains("^[[<"),
        "an orphan release reached the program: {:?}",
        text.trim_end()
    );
    assert!(backend.selectable_content().contains("SELECT"));

    // And the drag really ended: moving the pointer afterwards must not keep
    // growing the selection.
    let selected = backend.selectable_content();
    frame(
        &ctx,
        &mut backend,
        vec![Event::PointerMoved(Pos2::new(700.0, 300.0))],
        Modifiers::NONE,
    );
    assert_eq!(
        backend.selectable_content(),
        selected,
        "the pointer kept extending the selection after the button came up"
    );
}

/// With mouse reporting off, Shift changes nothing: a drag selects, exactly as
/// a bare drag does.
#[test]
fn shift_drag_without_mouse_reporting_selects_as_before() {
    let ctx = egui::Context::default();
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut backend = TerminalBackend::new(
        0,
        ctx.clone(),
        tx,
        BackendSettings {
            shell: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                r"i=0; while [ $i -lt 8 ]; do \
                  printf 'SELECTME-SELECTME-SELECTME-SELECTME-SELECTME\n'; \
                  i=$((i+1)); done; cat"
                    .to_string(),
            ],
            ..Default::default()
        },
    )
    .expect("spawn /bin/sh");

    let deadline = Instant::now() + Duration::from_secs(10);
    frame(&ctx, &mut backend, Vec::new(), Modifiers::NONE);
    while !screen_text(&mut backend).contains("SELECTME") {
        assert!(Instant::now() < deadline, "the PTY program never printed");
        std::thread::sleep(Duration::from_millis(20));
        frame(&ctx, &mut backend, Vec::new(), Modifiers::NONE);
    }

    drag(&ctx, &mut backend, Modifiers::SHIFT);
    let with_shift = backend.selectable_content();
    assert!(
        with_shift.contains("SELECT"),
        "Shift-drag selected {with_shift:?} with mouse reporting off"
    );

    drag(&ctx, &mut backend, Modifiers::NONE);
    assert_eq!(
        backend.selectable_content(),
        with_shift,
        "Shift-drag and bare drag must select the same cells when no program \
         is reading the mouse"
    );
}

/// Shift-scroll is untouched by all of this: it still bypasses reporting and
/// scrolls terra's own viewport (issue #21's rule), while a bare scroll still
/// reaches the program.
#[test]
fn shift_scroll_still_bypasses_reporting() {
    let ctx = egui::Context::default();
    let (mut backend, _events) = mouse_reporting_page(&ctx);
    ready(&ctx, &mut backend);

    frame(
        &ctx,
        &mut backend,
        vec![Event::PointerMoved(DRAG_FROM)],
        Modifiers::SHIFT,
    );
    frame(
        &ctx,
        &mut backend,
        vec![Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: Vec2::new(0.0, 1.0),
            modifiers: Modifiers::SHIFT,
            phase: egui::TouchPhase::Move,
        }],
        Modifiers::SHIFT,
    );
    for _ in 0..15 {
        std::thread::sleep(Duration::from_millis(20));
        frame(&ctx, &mut backend, Vec::new(), Modifiers::NONE);
    }

    let text = screen_text(&mut backend);
    assert!(
        !text.contains("^[[<6"),
        "Shift-scroll was reported to the program: {:?}",
        text.trim_end()
    );
}
