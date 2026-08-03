//! Paste: does `egui::Event::Paste` become bytes on the PTY, and are they
//! wrapped when the program asked for bracketed paste (DECSET 2004)?
//!
//! The user's report is "I can't paste into the terminal", from a tab running
//! ssh → tmux (mouse on) → codex, where bracketed paste is active. Three
//! questions, all answerable headlessly:
//!
//!   a) a plain shell: does an injected `Event::Paste` reach the program at all?
//!   b) with `\033[?2004h` set: is the text wrapped in `ESC[200~ … ESC[201~`?
//!   c) through tmux (the reported chain): does the paste survive the pipe?
//!
//! plus one that documents a boundary the harness cannot cross: the OS turns
//! ⌘V into `Event::Paste` inside egui-winit, *not* inside terra, so injecting
//! ⌘V as `Event::Key` here is expected to do nothing and proves nothing about
//! the real app.
//!
//! `/bin/cat` echoes everything back through the tty, so the grid is the
//! assertion: control bytes come back caret-style (`^[[200~`).
#![cfg(unix)]

use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use alacritty_terminal::term::cell::Flags;
use egui::{Event, Modifiers, Pos2, Rect};
use egui_term::{BackendSettings, PtyEvent, TerminalBackend, TerminalMode, TerminalView};

const SCREEN: Rect = Rect {
    min: Pos2::ZERO,
    max: Pos2::new(800.0, 600.0),
};

/// What gets pasted. No spaces would hide a word-splitting bug, and the marker
/// is unmistakable on a grid full of shell noise.
const PAYLOAD: &str = "PASTE-PAYLOAD-123";

/// The modifiers a real paste carries. ⌘ on macOS, where Ctrl never reaches
/// the clipboard path at all; Ctrl+Shift everywhere else, where a *bare*
/// Ctrl+V deliberately stays the terminal's literal ^V (egui_term's
/// `clipboard_key_is_passthrough`) — which is exactly what these tests were
/// accidentally exercising on the Linux runner when they held plain COMMAND.
const PASTE_MODS: Modifiers = if cfg!(target_os = "macos") {
    Modifiers::COMMAND
} else {
    Modifiers {
        alt: false,
        ctrl: true,
        shift: true,
        mac_cmd: false,
        command: true,
    }
};

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

fn screen_text(backend: &mut TerminalBackend) -> String {
    backend
        .sync()
        .grid
        .display_iter()
        .filter(|c| !c.cell.flags.contains(Flags::WIDE_CHAR_SPACER))
        .map(|c| c.cell.c)
        .collect()
}

/// A PTY running `sh -c <script>`, terra-side.
fn spawn(
    ctx: &egui::Context,
    shell: &str,
    args: Vec<String>,
) -> (TerminalBackend, Receiver<(u64, PtyEvent)>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let backend = TerminalBackend::new(
        0,
        ctx.clone(),
        tx,
        BackendSettings {
            shell: shell.to_string(),
            args,
            ..Default::default()
        },
    )
    .expect("spawn pty");
    (backend, rx)
}

fn sh(ctx: &egui::Context, script: &str) -> (TerminalBackend, Receiver<(u64, PtyEvent)>) {
    spawn(ctx, "/bin/sh", vec!["-c".to_string(), script.to_string()])
}

/// Pump frames until `pred` holds, or fail with what the grid shows.
fn wait_for(
    ctx: &egui::Context,
    backend: &mut TerminalBackend,
    what: &str,
    secs: u64,
    mut pred: impl FnMut(&mut TerminalBackend) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        frame(ctx, backend, Vec::new(), Modifiers::NONE);
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

/// Inject one paste, then pump frames so the bytes can go down the PTY and be
/// echoed back onto the grid.
fn paste(ctx: &egui::Context, backend: &mut TerminalBackend, text: &str) {
    frame(
        ctx,
        backend,
        vec![Event::Paste(text.to_string())],
        PASTE_MODS,
    );
    for _ in 0..60 {
        std::thread::sleep(Duration::from_millis(20));
        frame(ctx, backend, Vec::new(), Modifiers::NONE);
        if screen_text(backend).contains(PAYLOAD) {
            // Give the trailing bytes (a bracket terminator) a few more frames.
            for _ in 0..10 {
                std::thread::sleep(Duration::from_millis(20));
                frame(ctx, backend, Vec::new(), Modifiers::NONE);
            }
            return;
        }
    }
}

/// The baseline: a focused terminal, a plain program, one paste. If this
/// fails, paste is broken everywhere and the chain is irrelevant.
#[test]
fn paste_reaches_a_plain_program() {
    let ctx = egui::Context::default();
    let (mut backend, _events) = sh(&ctx, "printf 'READY\\n'; cat");
    wait_for(&ctx, &mut backend, "the program to start", 30, |b| {
        screen_text(b).contains("READY")
    });

    paste(&ctx, &mut backend, PAYLOAD);

    let text = screen_text(&mut backend);
    println!(
        "grid after a paste into a plain program: {:?}",
        text.trim_end()
    );
    assert!(
        text.contains(PAYLOAD),
        "the pasted text never reached the program: {:?}",
        text.trim_end()
    );
}

/// An unfocused view must not take the paste — the paste belongs to whichever
/// terminal holds the keyboard, and `main.rs` clears focus while a modal or
/// the palette is open.
#[test]
fn paste_into_an_unfocused_view_is_ignored() {
    let ctx = egui::Context::default();
    let (mut backend, _events) = sh(&ctx, "printf 'READY\\n'; cat");
    wait_for(&ctx, &mut backend, "the program to start", 30, |b| {
        screen_text(b).contains("READY")
    });

    // Same injection, but the view is built with `set_focus(false)`.
    let input = egui::RawInput {
        screen_rect: Some(SCREEN),
        events: vec![Event::Paste(PAYLOAD.to_string())],
        modifiers: PASTE_MODS,
        ..Default::default()
    };
    let _ = ctx.run_ui(input, |ui: &mut egui::Ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            let view = TerminalView::new(ui, &mut backend)
                .set_focus(false)
                .set_size(ui.available_size());
            ui.add(view);
        });
    });
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(20));
        frame(&ctx, &mut backend, Vec::new(), Modifiers::NONE);
    }

    let text = screen_text(&mut backend);
    assert!(
        !text.contains(PAYLOAD),
        "an unfocused terminal swallowed the paste: {:?}",
        text.trim_end()
    );
}

/// The mode the user's chain is in. A program that set DECSET 2004 asked to be
/// told where a paste starts and ends; every other terminal wraps the text in
/// `ESC[200~` / `ESC[201~`. Without that, a multi-line paste is executed line
/// by line and TUIs that gate on the marker (shells with syntax highlighting,
/// codex, claude) see the paste as ordinary typing.
#[test]
fn paste_is_bracketed_when_the_program_asked_for_it() {
    let ctx = egui::Context::default();
    let (mut backend, _events) = sh(&ctx, "printf '\\033[?2004h'; printf 'READY\\n'; cat");
    wait_for(&ctx, &mut backend, "bracketed paste mode", 30, |b| {
        b.sync()
            .terminal_mode
            .contains(TerminalMode::BRACKETED_PASTE)
            && screen_text(b).contains("READY")
    });

    paste(&ctx, &mut backend, PAYLOAD);

    let text = screen_text(&mut backend);
    println!(
        "grid after a paste in bracketed mode: {:?}",
        text.trim_end()
    );
    assert!(
        text.contains(PAYLOAD),
        "the pasted text never reached the program: {:?}",
        text.trim_end()
    );
    assert!(
        text.contains("^[[200~") && text.contains("^[[201~"),
        "the paste was not bracketed; the program received {:?}",
        text.trim_end()
    );
}

/// A multi-line paste is where the missing brackets bite hardest: without
/// them, every newline is an Enter, and a shell runs each line.
#[test]
fn a_multiline_paste_is_bracketed() {
    let ctx = egui::Context::default();
    let (mut backend, _events) = sh(&ctx, "printf '\\033[?2004h'; printf 'READY\\n'; cat");
    wait_for(&ctx, &mut backend, "bracketed paste mode", 30, |b| {
        b.sync()
            .terminal_mode
            .contains(TerminalMode::BRACKETED_PASTE)
            && screen_text(b).contains("READY")
    });

    paste(&ctx, &mut backend, &format!("{PAYLOAD}\nsecond line"));

    let text = screen_text(&mut backend);
    println!("grid after a multi-line paste: {:?}", text.trim_end());
    assert!(
        text.contains("^[[200~"),
        "a multi-line paste arrived as bare keystrokes: {:?}",
        text.trim_end()
    );
}

/// A big paste, byte for byte. A truncating write would look exactly like
/// "paste doesn't work" for the snippet-sized text a user actually pastes into
/// an agent: `dd` only prints once all 5000 bytes have arrived.
#[test]
fn a_large_paste_arrives_whole() {
    let ctx = egui::Context::default();
    let (mut backend, _events) = sh(
        &ctx,
        // Raw mode: the line discipline must not buffer (canonical mode holds
        // everything back until a newline, and drops past MAX_CANON), so what
        // the program reads is exactly what terra wrote.
        "stty raw -echo; printf 'READY\\r\\n'; dd bs=1 count=5000 of=/dev/null 2>/dev/null; \
         printf 'GOT-ALL\\r\\n'; cat",
    );
    wait_for(&ctx, &mut backend, "the program to start", 30, |b| {
        screen_text(b).contains("READY")
    });

    let big = "x".repeat(5000);
    frame(&ctx, &mut backend, vec![Event::Paste(big)], PASTE_MODS);
    wait_for(&ctx, &mut backend, "all 5000 bytes to arrive", 30, |b| {
        screen_text(b).contains("GOT-ALL")
    });
}

/// ⌘V injected as key events does nothing here, and that is *expected*: on
/// macOS `egui_winit` intercepts the Cmd+V key press and turns it into
/// `Event::Paste` before egui ever sees a `Key::V`, so no `Event::Key` for V
/// reaches a terra frame in the real app either. This test pins the boundary
/// so a future reader does not mistake it for the bug — but it does show what
/// the terminal would send if the translation ever stopped happening: nothing
/// (`bindings.rs` maps ⌘V to `BindingAction::Paste`, which `process_keyboard_key`
/// turns into `InputAction::Ignore`).
#[test]
fn cmd_v_as_a_bare_key_event_writes_nothing() {
    let ctx = egui::Context::default();
    let (mut backend, _events) = sh(&ctx, "printf 'READY\\n'; cat");
    wait_for(&ctx, &mut backend, "the program to start", 30, |b| {
        screen_text(b).contains("READY")
    });
    let before = screen_text(&mut backend);

    frame(
        &ctx,
        &mut backend,
        vec![Event::Key {
            key: egui::Key::V,
            physical_key: Some(egui::Key::V),
            pressed: true,
            repeat: false,
            modifiers: Modifiers::COMMAND,
        }],
        Modifiers::COMMAND,
    );
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(20));
        frame(&ctx, &mut backend, Vec::new(), Modifiers::NONE);
    }

    let after = screen_text(&mut backend);
    println!("grid after a bare ⌘V key event: {:?}", after.trim_end());
    assert_eq!(
        before.trim_end(),
        after.trim_end(),
        "⌘V as a key event wrote something to the PTY"
    );
}

// ---------------------------------------------------------------------------
// The reported chain: ssh → tmux (mouse on) → a full-screen program.
// ---------------------------------------------------------------------------

/// The inner "codex": alt screen, mouse tracking, bracketed paste, then echo.
const INNER_TUI: &str = r"printf '\033[?1049h'; \
     printf 'READY\n'; \
     printf '\033[?1000h\033[?1002h\033[?1006h\033[?2004h'; cat";

struct PrivateTmux {
    socket: String,
    conf: PathBuf,
}

impl PrivateTmux {
    fn new(tag: &str) -> Self {
        let socket = format!("terra-paste-{tag}");
        let conf = std::env::temp_dir().join(format!("{socket}.conf"));
        let mut f = std::fs::File::create(&conf).expect("write tmux config");
        writeln!(f, "set -g mouse on").unwrap();
        writeln!(f, "set -g default-terminal \"xterm-256color\"").unwrap();
        writeln!(f, "set -g status off").unwrap();
        writeln!(f, "set -g escape-time 0").unwrap();
        drop(f);
        let me = Self { socket, conf };
        me.kill();
        me
    }

    fn kill(&self) {
        let _ = std::process::Command::new("tmux")
            .args(["-L", &self.socket, "kill-server"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

impl Drop for PrivateTmux {
    fn drop(&mut self) {
        self.kill();
        let _ = std::fs::remove_file(&self.conf);
    }
}

fn tmux_chain(
    ctx: &egui::Context,
    tmux: &PrivateTmux,
) -> (TerminalBackend, Receiver<(u64, PtyEvent)>) {
    spawn(
        ctx,
        "tmux",
        vec![
            "-L".to_string(),
            tmux.socket.clone(),
            "-f".to_string(),
            tmux.conf.display().to_string(),
            "new-session".to_string(),
            "--".to_string(),
            "/bin/sh".to_string(),
            "-c".to_string(),
            INNER_TUI.to_string(),
        ],
    )
}

/// The user's exact situation: does the paste reach the inner program through
/// tmux, and does it arrive bracketed the way the inner program asked?
#[test]
fn paste_through_the_tmux_chain() {
    let tmux = PrivateTmux::new("chain");
    let ctx = egui::Context::default();
    let (mut backend, _events) = tmux_chain(&ctx, &tmux);
    wait_for(&ctx, &mut backend, "the tmux chain to come up", 30, |b| {
        b.sync().terminal_mode.intersects(TerminalMode::MOUSE_MODE)
            && screen_text(b).contains("READY")
    });
    let mode = backend.sync().terminal_mode;
    println!("modes tmux negotiated toward terra: {mode:?}");

    paste(&ctx, &mut backend, PAYLOAD);

    let text = screen_text(&mut backend);
    println!("grid after a paste through tmux: {:?}", text.trim_end());
    assert!(
        text.contains(PAYLOAD),
        "the paste never reached the inner program: {:?}",
        text.trim_end()
    );
}

/// The sanitiser, through a real PTY. A payload carrying `ESC[201~` must not be
/// able to close the bracket early: the program has to see exactly one
/// terminator, at the end, with the "rest" of the paste still inside it.
#[test]
fn an_embedded_terminator_cannot_close_the_bracket_early() {
    let ctx = egui::Context::default();
    let (mut backend, _events) = sh(&ctx, "printf '\\033[?2004h'; printf 'READY\\n'; cat");
    wait_for(&ctx, &mut backend, "bracketed paste mode", 30, |b| {
        b.sync()
            .terminal_mode
            .contains(TerminalMode::BRACKETED_PASTE)
            && screen_text(b).contains("READY")
    });

    // The attack shape: end the bracket, then "type" a command.
    paste(&ctx, &mut backend, &format!("{PAYLOAD}\x1b[201~TAIL"));

    let text = screen_text(&mut backend);
    println!("grid after a booby-trapped paste: {:?}", text.trim_end());
    assert_eq!(
        text.matches("^[[201~").count(),
        1,
        "the payload's own terminator survived, so the bracket closed early: \
         {:?}",
        text.trim_end()
    );
    // The tail is still part of the paste, not something the program was told
    // to treat as typing.
    assert!(
        text.contains(&format!("{PAYLOAD}TAIL^[[201~")),
        "the tail escaped the bracket: {:?}",
        text.trim_end()
    );
}

/// A pasted line break is CR — the byte Enter produces — not LF. `cat` echoes
/// what it read, and a bare LF would show up as a line feed with no return,
/// staircasing the next line rightwards.
#[test]
fn a_pasted_newline_becomes_a_carriage_return() {
    let ctx = egui::Context::default();
    // Raw mode so the line discipline neither buffers nor rewrites what we
    // send: the echoed bytes are exactly terra's bytes.
    let (mut backend, _events) = sh(&ctx, "stty raw -echo; printf 'READY\\r\\n'; cat");
    wait_for(&ctx, &mut backend, "the program to start", 30, |b| {
        screen_text(b).contains("READY")
    });

    paste(&ctx, &mut backend, &format!("{PAYLOAD}\nSECOND"));

    // A CR with no LF returns the cursor to column 0 of the *same* row, so the
    // second half overwrites the first. That is the assertion: with a bare LF
    // the two would sit on different rows.
    let text = screen_text(&mut backend);
    println!(
        "grid after a two-line paste in raw mode: {:?}",
        text.trim_end()
    );
    assert!(
        text.contains("SECOND"),
        "the second line never arrived: {:?}",
        text.trim_end()
    );
    assert!(
        !text.contains(&format!("{PAYLOAD}\nSECOND")) && !text.contains(PAYLOAD),
        "the paste kept its LF, so the lines stacked instead of overwriting: \
         {:?}",
        text.trim_end()
    );
}
