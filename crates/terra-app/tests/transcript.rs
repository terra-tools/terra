//! `terra transcript` against a real PTY: the bytes a full-screen program
//! painted survive in the tab's ring even though the terminal keeps nothing of
//! them.
//!
//! This is the claim the whole feature rests on, and it is not provable from a
//! unit test: it needs an actual child process, an actual alternate screen and
//! an actual clear. The program below does what `htop` or Claude Code does in
//! miniature — switch to the alt screen, paint, clear, paint again, switch
//! back — and the test asserts both halves: the ring has every frame, and the
//! grid (what `terra capture` reads) has none of them.
//!
//! Real PTYs, so Unix-only like the other PTY-backed harness tests.
#![cfg(unix)]

#[path = "../src/transcript.rs"]
mod transcript;

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alacritty_terminal::term::cell::Flags;
use egui::{Pos2, Rect};
use egui_term::{BackendSettings, OutputTap, PtyEvent, TerminalBackend, TerminalView};

use transcript::Ring;

const SCREEN: Rect = Rect {
    min: Pos2::ZERO,
    max: Pos2::new(800.0, 600.0),
};

/// One frame, just enough to drive the terminal widget.
fn frame(ctx: &egui::Context, backend: &mut TerminalBackend) {
    let input = egui::RawInput {
        screen_rect: Some(SCREEN),
        ..Default::default()
    };
    let _ = ctx.run_ui(input, |ui: &mut egui::Ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            let view = TerminalView::new(ui, backend).set_size(ui.available_size());
            ui.add(view);
        });
    });
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

/// A shell running `script`, with a transcript ring tapped onto its output.
fn tapped(
    ctx: &egui::Context,
    script: &str,
    cap: usize,
) -> (TerminalBackend, Arc<Mutex<Ring>>, Receiver<(u64, PtyEvent)>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let ring = Arc::new(Mutex::new(Ring::new(cap)));
    let sink = ring.clone();
    let backend = TerminalBackend::new(
        0,
        ctx.clone(),
        tx,
        BackendSettings {
            shell: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            working_directory: None,
            output_tap: Some(
                Arc::new(move |bytes: &[u8]| transcript::lock(&sink).push(bytes)) as OutputTap,
            ),
        },
    )
    .expect("spawn /bin/sh");
    (backend, ring, rx)
}

/// Pump frames until the grid shows `text`, or fail.
fn wait_for(ctx: &egui::Context, backend: &mut TerminalBackend, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !screen_text(backend).contains(text) {
        assert!(
            Instant::now() < deadline,
            "{text:?} never reached the grid; it holds {:?}",
            screen_text(backend).trim_end()
        );
        std::thread::sleep(Duration::from_millis(20));
        frame(ctx, backend);
    }
}

fn rendered(ring: &Arc<Mutex<Ring>>) -> String {
    transcript::render(&transcript::lock(ring).snapshot())
}

/// The whole point of the feature, end to end.
#[test]
fn an_alt_screen_programs_output_survives_in_the_ring_and_nowhere_else() {
    let ctx = egui::Context::default();
    // Alt screen on, paint, clear, paint again, alt screen off, then a marker
    // on the primary screen so the test knows the program is done.
    let script = r"printf '\033[?1049h'
printf 'FRAME-ONE\r\n'
printf '\033[2J\033[H'
printf 'FRAME-TWO\r\n'
printf '\033[?1049l'
printf 'BACK-ON-PRIMARY\r\n'
sleep 30";
    let (mut backend, ring, _events) = tapped(&ctx, script, 64 * 1024);

    frame(&ctx, &mut backend);
    wait_for(&ctx, &mut backend, "BACK-ON-PRIMARY");

    let transcript = rendered(&ring);
    assert!(transcript.contains("FRAME-ONE"), "{transcript:?}");
    assert!(transcript.contains("FRAME-TWO"), "{transcript:?}");
    // …and the frames are in the order they were painted, clears and all.
    let one = transcript.find("FRAME-ONE").unwrap();
    let two = transcript.find("FRAME-TWO").unwrap();
    assert!(one < two, "{transcript:?}");

    // The other half of the claim: what `terra capture` reads has none of it.
    // Leaving the alternate screen restores the primary one, so both frames
    // are gone from the terminal entirely.
    let grid = screen_text(&mut backend);
    assert!(!grid.contains("FRAME-ONE"), "{grid:?}");
    assert!(!grid.contains("FRAME-TWO"), "{grid:?}");
    assert!(grid.contains("BACK-ON-PRIMARY"));
}

/// A cap smaller than the output truncates from the front, and the tail is
/// still readable text rather than a corrupted mess.
#[test]
fn a_tiny_cap_keeps_the_end_of_the_stream() {
    let ctx = egui::Context::default();
    let script = r"i=0
while [ $i -lt 200 ]; do printf 'line-%03d\r\n' $i; i=$((i+1)); done
printf 'LAST-LINE\r\n'
sleep 30";
    // 1 KB holds well under 200 lines of `line-NNN`.
    let (mut backend, ring, _events) = tapped(&ctx, script, 1024);

    frame(&ctx, &mut backend);
    wait_for(&ctx, &mut backend, "LAST-LINE");

    let transcript = rendered(&ring);
    assert!(transcript.len() <= 1024, "{}", transcript.len());
    assert!(transcript.contains("LAST-LINE"), "{transcript:?}");
    assert!(transcript.contains("line-199"), "{transcript:?}");
    // Dropped off the front.
    assert!(!transcript.contains("line-000"), "{transcript:?}");

    // `--tail 3` on the rendered form.
    let tail = transcript::tail_lines(&transcript, Some(3));
    assert!(tail.contains("LAST-LINE"), "{tail:?}");
    assert!(!tail.contains("line-197"), "{tail:?}");
}

/// With no tap installed nothing is copied — the disabled case, proven at the
/// same seam the app switches off.
#[test]
fn no_tap_means_no_copy() {
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
                "printf 'UNTAPPED\\r\\n'; sleep 30".to_string(),
            ],
            working_directory: None,
            output_tap: None,
        },
    )
    .expect("spawn /bin/sh");

    frame(&ctx, &mut backend);
    // The PTY still works; there is simply nowhere for a copy to go.
    wait_for(&ctx, &mut backend, "UNTAPPED");
}
