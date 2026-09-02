//! Files dragged out of the file manager onto the window, over a real PTY.
//!
//! The mapping from a drop to text is a pure function with its own unit tests
//! (`src/drop.rs`); what this file pins is the half that only exists against
//! the world — that the text actually reaches the program in the focused tab,
//! and that it goes through egui_term's paste path rather than being written
//! raw. So the assertions are made on the *grid*: `/bin/cat` echoes everything
//! it is given back through the tty, which makes the screen the record of what
//! the child received, control bytes and all (caret notation: `ESC` shows as
//! `^[`).
//!
//! The frame is composed the way `App::ui` composes it — `dropped_files` read
//! off `RawInput` before the panel is drawn, `TerminalView` rendered with the
//! focus below — so the wiring under test is the shipped one.
//!
//! Unix-only, like the other PTY-backed tests.
#![cfg(unix)]
#![allow(dead_code)]

#[path = "../src/drop.rs"]
mod drop;

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use alacritty_terminal::term::cell::Flags;
use egui::{DroppedFile, Modifiers, Pos2, Rect};
use egui_term::{
    BackendCommand, BackendSettings, PtyEvent, TerminalBackend, TerminalMode, TerminalView,
};

const SCREEN: Rect = Rect {
    min: Pos2::ZERO,
    max: Pos2::new(800.0, 600.0),
};

/// One frame, optionally carrying a drop — `App::ui`'s order: the drop is
/// handled first, against the tab the keyboard is talking to, and the terminal
/// is drawn after.
fn frame_with_drop(
    ctx: &egui::Context,
    backend: &mut TerminalBackend,
    dropped_files: Vec<DroppedFile>,
) {
    let input = egui::RawInput {
        screen_rect: Some(SCREEN),
        modifiers: Modifiers::NONE,
        dropped_files,
        ..Default::default()
    };
    let _ = ctx.run_ui(input, |ui: &mut egui::Ui| {
        // The shipped handler, transcribed: read the frame's drops, map them,
        // and put them on the PTY as a paste.
        let files = ui.ctx().input(|i| i.raw.dropped_files.clone());
        if let Some(text) = drop::dropped_text(&files, drop::Style::Posix) {
            let bracketed = backend
                .last_content()
                .terminal_mode
                .contains(TerminalMode::BRACKETED_PASTE);
            backend.process_command(BackendCommand::Write(egui_term::paste_bytes(
                &text, bracketed,
            )));
        }
        egui::CentralPanel::default().show(ui, |ui| {
            let view = TerminalView::new(ui, backend)
                .set_focus(true)
                .set_size(ui.available_size());
            ui.add(view);
        });
    });
}

fn frame(ctx: &egui::Context, backend: &mut TerminalBackend) {
    frame_with_drop(ctx, backend, Vec::new());
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

fn sh(ctx: &egui::Context, script: &str) -> (TerminalBackend, Receiver<(u64, PtyEvent)>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let backend = TerminalBackend::new(
        0,
        ctx.clone(),
        tx,
        BackendSettings {
            shell: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            ..Default::default()
        },
    )
    .expect("spawn pty");
    (backend, rx)
}

fn wait_for(
    ctx: &egui::Context,
    backend: &mut TerminalBackend,
    what: &str,
    secs: u64,
    mut pred: impl FnMut(&mut TerminalBackend) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        frame(ctx, backend);
        if pred(backend) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; the grid holds {:?}",
            screen_text(backend).trim_end()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn dropped(paths: &[&str]) -> Vec<DroppedFile> {
    paths
        .iter()
        .map(|p| DroppedFile {
            path: Some(PathBuf::from(p)),
            ..Default::default()
        })
        .collect()
}

/// The feature itself: a path with a space in it arrives as **one** shell
/// word, and a plain one arrives as the bare text a user would have typed.
#[test]
fn dropped_paths_reach_the_program_shell_quoted() {
    let ctx = egui::Context::default();
    let (mut backend, _events) = sh(&ctx, "printf 'READY\\n'; cat");
    wait_for(&ctx, &mut backend, "cat to start", 30, |b| {
        screen_text(b).contains("READY")
    });

    frame_with_drop(
        &ctx,
        &mut backend,
        dropped(&["/tmp/two words/a.txt", "/tmp/plain.txt"]),
    );

    wait_for(&ctx, &mut backend, "the paths to echo back", 30, |b| {
        screen_text(b).contains("/tmp/plain.txt")
    });
    let screen = screen_text(&mut backend);
    println!("grid after the drop: {:?}", screen.trim_end());
    assert!(
        screen.contains("'/tmp/two words/a.txt' /tmp/plain.txt "),
        "the drop reached the program as {screen:?}"
    );
    // Nothing ran: the trailing space leaves the line ready for a Return the
    // user has not pressed yet.
    assert!(
        !screen.contains("not found"),
        "the paste must not have carried a newline: {screen:?}"
    );
}

/// A program that asked for bracketed paste (DECSET 2004) gets the markers,
/// because the drop goes down the same `paste_bytes` a ⌘V goes down. `cat`
/// echoes them caret-style, so `^[[200~` on the grid *is* the ESC on the wire.
#[test]
fn a_drop_into_a_bracketed_paste_program_arrives_bracketed() {
    let ctx = egui::Context::default();
    let (mut backend, _events) = sh(&ctx, "printf 'READY\\n\\033[?2004h'; cat");
    wait_for(&ctx, &mut backend, "DECSET 2004 to land", 30, |b| {
        b.sync()
            .terminal_mode
            .contains(TerminalMode::BRACKETED_PASTE)
            && screen_text(b).contains("READY")
    });

    frame_with_drop(&ctx, &mut backend, dropped(&["/tmp/x"]));

    wait_for(&ctx, &mut backend, "the bracketed paste to echo", 30, |b| {
        screen_text(b).contains("^[[201~")
    });
    let screen = screen_text(&mut backend);
    println!("grid after the bracketed drop: {:?}", screen.trim_end());
    assert!(
        screen.contains("^[[200~/tmp/x ^[[201~"),
        "the paste was not bracketed: {screen:?}"
    );
}

/// A drop carrying nothing terra can name writes nothing at all — no stray
/// space typed into whatever the user was in the middle of.
#[test]
fn a_pathless_drop_types_nothing() {
    let ctx = egui::Context::default();
    let (mut backend, _events) = sh(&ctx, "printf 'READY\\n'; cat");
    wait_for(&ctx, &mut backend, "cat to start", 30, |b| {
        screen_text(b).contains("READY")
    });
    let before = screen_text(&mut backend);

    frame_with_drop(&ctx, &mut backend, vec![DroppedFile::default()]);
    for _ in 0..10 {
        frame(&ctx, &mut backend);
        std::thread::sleep(Duration::from_millis(20));
    }

    assert_eq!(
        screen_text(&mut backend).trim_end(),
        before.trim_end(),
        "a drop with no path changed the screen"
    );
}
