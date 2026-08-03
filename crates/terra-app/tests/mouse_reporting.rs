//! When a program turns on mouse reporting, the wheel must reach it as mouse
//! wheel events, not as arrow keys (issue #21).
//!
//! Programs like claude code enter the alt screen *and* enable SGR mouse
//! tracking. Alacritty's alternate-scroll mode (wheel → arrows on the alt
//! screen) is on by default, and terra's wheel path consults only that — so
//! the program asks for `\e[<64;…M` and gets `\e[A`, which TUIs answer with a
//! "scrolling is not supported" style warning.
//!
//! Same headless harness as `tab_focus.rs`: a real PTY, `/bin/cat` echoing
//! whatever bytes arrive, egui events injected by hand. Unix-only.
#![cfg(unix)]

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use alacritty_terminal::term::cell::Flags;
use egui::{Event, Modifiers, MouseWheelUnit, Pos2, Rect, Vec2};
use egui_term::{BackendSettings, PtyEvent, TerminalBackend, TerminalView};

const SCREEN: Rect = Rect {
    min: Pos2::ZERO,
    max: Pos2::new(800.0, 600.0),
};
const IN_THE_GRID: Pos2 = Pos2::new(400.0, 300.0);

fn frame(ctx: &egui::Context, backend: &mut TerminalBackend, events: Vec<Event>) {
    let input = egui::RawInput {
        screen_rect: Some(SCREEN),
        events,
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

/// Everything on the grid, as one string. Control bytes echoed by the tty show
/// up caret-style (`^[[A`), which is what the assertions read.
fn screen_text(backend: &mut TerminalBackend) -> String {
    backend
        .sync()
        .grid
        .display_iter()
        .filter(|c| !c.cell.flags.contains(Flags::WIDE_CHAR_SPACER))
        .map(|c| c.cell.c)
        .collect()
}

/// A PTY whose program does what claude code does on startup: alt screen on,
/// SGR + button mouse tracking on — then echoes every byte it receives.
fn mouse_reporting_cat(ctx: &egui::Context) -> (TerminalBackend, Receiver<(u64, PtyEvent)>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let backend = TerminalBackend::new(
        0,
        ctx.clone(),
        tx,
        BackendSettings {
            shell: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                r"printf '\033[?1049h\033[?1000h\033[?1006h'; cat".to_string(),
            ],
            working_directory: None,
            output_tap: None,
        },
    )
    .expect("spawn /bin/sh");
    (backend, rx)
}

fn wheel(delta: f32) -> Event {
    Event::MouseWheel {
        unit: MouseWheelUnit::Line,
        delta: Vec2::new(0.0, delta),
        modifiers: Modifiers::NONE,
        phase: egui::TouchPhase::Move,
    }
}

/// Pump frames until the PTY program has demonstrably received *something*
/// from the wheel, then return the grid text.
fn wait_for_any_echo(ctx: &egui::Context, backend: &mut TerminalBackend) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let text = screen_text(backend);
        if text.contains('^') || text.contains('M') {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "the wheel produced no bytes at all; the grid holds {:?}",
            text.trim_end()
        );
        std::thread::sleep(Duration::from_millis(20));
        frame(ctx, backend, Vec::new());
    }
}

/// The acceptance test for issue #21: before the fix the grid showed `^[OA`
/// (alternate-scroll arrows); honouring mouse reporting shows SGR wheel
/// reports (`^[[<64;…M`).
#[test]
fn wheel_reaches_a_mouse_reporting_program_as_mouse_events() {
    let ctx = egui::Context::default();
    let (mut backend, _events) = mouse_reporting_cat(&ctx);

    // Let the PTY program start and its mode changes land in the emulator.
    let deadline = Instant::now() + Duration::from_secs(10);
    frame(&ctx, &mut backend, Vec::new());
    while !backend
        .sync()
        .terminal_mode
        .contains(egui_term::TerminalMode::SGR_MOUSE)
    {
        assert!(
            Instant::now() < deadline,
            "the PTY program never enabled mouse reporting"
        );
        std::thread::sleep(Duration::from_millis(20));
        frame(&ctx, &mut backend, Vec::new());
    }

    // Pointer over the grid, then scroll.
    frame(&ctx, &mut backend, vec![Event::PointerMoved(IN_THE_GRID)]);
    frame(&ctx, &mut backend, vec![wheel(1.0)]);

    let text = wait_for_any_echo(&ctx, &mut backend);
    assert!(
        !["^[[A", "^[[B", "^[OA", "^[OB"]
            .iter()
            .any(|s| text.contains(s)),
        "the wheel arrived as arrow keys, not mouse reports: {:?}",
        text.trim_end()
    );
    assert!(
        text.contains("^[[<64;") || text.contains("^[[<65;"),
        "no SGR wheel report reached the program: {:?}",
        text.trim_end()
    );
}
