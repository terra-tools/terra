//! The "Close Window?" dialog, driven headlessly over a real PTY.
//!
//! `App::ui` in `main.rs` is not reachable from a test crate (terra-app is a
//! binary), and neither is a window-close gesture: nothing here can raise
//! `ViewportInfo::close_requested`. So the split is deliberate —
//! `confirm_close`'s own unit tests cover the ask/don't-ask decision and the
//! Idle → Asking → Approved state machine, and this file covers the half that
//! only exists on screen: that the dialog answers Esc, Return and both
//! buttons, and that while it is up **nothing** typed reaches the terminal
//! underneath.
//!
//! The frame is composed the way `App::ui` composes it — dialog first (it
//! consumes keys before anything else looks at them), `TerminalView` with
//! `set_focus(!modal_open)` below — so what is asserted here is the same
//! wiring the app runs.
//!
//! A real `/bin/cat` PTY echoes whatever reaches it, so a keystroke that
//! leaked shows up on the grid and one that did not never does. Unix-only,
//! like the other PTY-backed tests.
#![cfg(unix)]
// `fonts` is pulled in whole to register the UI families the dialog draws
// with; nothing here calls the rest of it.
#![allow(dead_code)]

#[path = "../src/confirm_close.rs"]
mod confirm_close;
#[path = "../src/fonts.rs"]
mod fonts;

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use alacritty_terminal::term::cell::Flags;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use egui_term::{BackendSettings, PtyEvent, TerminalBackend, TerminalView};

use confirm_close::{Choice, CLOSE_LABEL};

const SCREEN: Rect = Rect {
    min: Pos2::ZERO,
    max: Pos2::new(800.0, 600.0),
};

/// One frame of a terra window with the dialog optionally up. Returns the
/// choice the dialog made this frame, if any.
fn frame(
    ctx: &egui::Context,
    backend: &mut TerminalBackend,
    events: Vec<Event>,
    dialog: bool,
) -> Option<Choice> {
    let input = egui::RawInput {
        screen_rect: Some(SCREEN),
        events,
        ..Default::default()
    };
    let mut choice = None;
    let _ = ctx.run_ui(input, |ui: &mut egui::Ui| {
        let ctx = ui.ctx().clone();
        if dialog {
            choice = confirm_close::show(&ctx);
        }
        egui::CentralPanel::default().show(ui, |ui| {
            let view = TerminalView::new(ui, backend)
                .set_focus(!dialog)
                .set_size(ui.available_size());
            ui.add(view);
        });
    });
    choice
}

fn click(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

fn key(key: Key) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }
}

fn screen_text(backend: &mut TerminalBackend) -> String {
    backend
        .sync()
        .grid
        .display_iter()
        .filter(|c| !c.cell.flags.contains(Flags::WIDE_CHAR_SPACER))
        .map(|c| c.cell.c)
        .collect()
}

/// `/bin/cat` under a PTY. The receiver has to outlive the backend.
fn cat(ctx: &egui::Context) -> (TerminalBackend, Receiver<(u64, PtyEvent)>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let backend = TerminalBackend::new(
        0,
        ctx.clone(),
        tx,
        BackendSettings {
            shell: "/bin/cat".to_string(),
            args: Vec::new(),
            working_directory: None,
            output_tap: None,
        },
    )
    .expect("spawn /bin/cat");
    (backend, rx)
}

fn wait_for_echo(ctx: &egui::Context, backend: &mut TerminalBackend, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !screen_text(backend).contains(text) {
        assert!(
            Instant::now() < deadline,
            "{text:?} never reached the PTY; the grid holds {:?}",
            screen_text(backend).trim_end()
        );
        std::thread::sleep(Duration::from_millis(20));
        frame(ctx, backend, Vec::new(), false);
    }
}

/// A context with terra's fonts installed — the dialog draws in the UI
/// families, and an unregistered family would panic rather than fall back.
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    fonts::install(&ctx);
    ctx
}

/// Esc is Cancel, and it is *consumed*: neither the terminal below nor the
/// app's shortcut table may also see it.
#[test]
fn escape_cancels_and_never_reaches_the_terminal() {
    let ctx = context();
    let (mut backend, _events) = cat(&ctx);
    frame(&ctx, &mut backend, Vec::new(), true);

    let choice = frame(&ctx, &mut backend, vec![key(Key::Escape)], true);
    assert_eq!(choice, Some(Choice::Cancel));

    // The key was consumed, so a frame that still had the dialog up would see
    // nothing left of it.
    let again = frame(&ctx, &mut backend, Vec::new(), true);
    assert_eq!(again, None);
}

/// Return is the primary button — the dialog's default, as in the reference.
#[test]
fn return_confirms_the_close() {
    let ctx = context();
    let (mut backend, _events) = cat(&ctx);
    frame(&ctx, &mut backend, Vec::new(), true);

    let choice = frame(&ctx, &mut backend, vec![key(Key::Enter)], true);
    assert_eq!(choice, Some(Choice::Close));
}

/// The whole point of a modal: while it is up the terminal is deaf, and the
/// moment it goes away the very next keystroke lands.
#[test]
fn the_dialog_keeps_the_keyboard_and_gives_it_back() {
    let ctx = context();
    let (mut backend, _events) = cat(&ctx);
    frame(&ctx, &mut backend, Vec::new(), false);
    // Pointer parked over the grid — the case where the terminal would
    // otherwise win the input.
    frame(
        &ctx,
        &mut backend,
        vec![Event::PointerMoved(Pos2::new(400.0, 300.0))],
        false,
    );

    for _ in 0..5 {
        frame(
            &ctx,
            &mut backend,
            vec![Event::Text("stolen".to_string())],
            true,
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    frame(&ctx, &mut backend, Vec::new(), true);
    assert!(
        !screen_text(&mut backend).contains("stolen"),
        "the dialog lost keystrokes to the terminal below"
    );

    frame(
        &ctx,
        &mut backend,
        vec![Event::Text("back".to_string())],
        false,
    );
    wait_for_echo(&ctx, &mut backend, "back");
}

/// Clicking the accent-filled Close button confirms. The rect comes from egui
/// rather than from arithmetic, so the test tracks the layout.
#[test]
fn clicking_close_confirms() {
    let ctx = context();
    let (mut backend, _events) = cat(&ctx);
    // A few frames first: an anchored `Area` only knows its own size once it
    // has been laid out, so the panel settles on the second frame.
    for _ in 0..3 {
        frame(&ctx, &mut backend, Vec::new(), true);
    }

    let button = ctx
        .read_response(confirm_close::button_id(CLOSE_LABEL))
        .expect("the Close button was drawn")
        .rect;
    let at = button.center();
    let choice = frame(
        &ctx,
        &mut backend,
        vec![Event::PointerMoved(at), click(at, true), click(at, false)],
        true,
    );
    assert_eq!(choice, Some(Choice::Close));
}

/// A click on the scrim is a dismissal, and a dismissal is never consent.
#[test]
fn clicking_outside_the_panel_cancels() {
    let ctx = context();
    let (mut backend, _events) = cat(&ctx);
    // A few frames first: an anchored `Area` only knows its own size once it
    // has been laid out, so the panel settles on the second frame.
    for _ in 0..3 {
        frame(&ctx, &mut backend, Vec::new(), true);
    }

    let panel = ctx
        .read_response(confirm_close::button_id(CLOSE_LABEL))
        .expect("the dialog was drawn")
        .rect;
    let outside = Pos2::new(8.0, 8.0);
    assert!(!panel.contains(outside));

    let choice = frame(
        &ctx,
        &mut backend,
        vec![
            Event::PointerMoved(outside),
            click(outside, true),
            click(outside, false),
        ],
        true,
    );
    assert_eq!(choice, Some(Choice::Cancel));
}
