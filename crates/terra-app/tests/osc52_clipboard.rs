//! OSC 52 — the program's route to the system clipboard, and the reason a
//! plain mouse drag inside tmux behaves in terra the way it does in Ghostty.
//!
//! Inside `ssh` → `tmux` (`mouse on`) a drag belongs to tmux: tmux paints the
//! selection itself and, on release, hands the text back *out* to the terminal
//! as `ESC ] 52 ; c ; <base64> BEL`. A terminal that ignores that sequence
//! leaves the user with a selection they can see and cannot paste — which is
//! exactly what terra did before this test existed.
//!
//! Two levels, both headless:
//!
//!   a) the sequence itself, straight down the PTY;
//!   b) the real chain — a private tmux server, a selection made with
//!      `copy-mode` commands (no mouse needed to make tmux emit the sequence).
//!
//! The assertion is at the egui_term boundary: `PtyEvent::ClipboardStore`, with
//! the payload already base64-decoded by alacritty. Nothing here touches the
//! real system pasteboard — a test suite that rewrote the developer's clipboard
//! would be a bug of its own.
#![cfg(unix)]

use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use alacritty_terminal::term::cell::Flags;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect};
use egui_term::{
    BackendSettings, ClipboardType, PtyEvent, TerminalBackend, TerminalMode, TerminalView,
};

const SCREEN: Rect = Rect {
    min: Pos2::ZERO,
    max: Pos2::new(800.0, 600.0),
};

fn frame(ctx: &egui::Context, backend: &mut TerminalBackend) {
    let input = egui::RawInput {
        screen_rect: Some(SCREEN),
        modifiers: Modifiers::NONE,
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

/// Pump frames until a `ClipboardStore` shows up, and return what it carried.
fn wait_for_clipboard_store(
    ctx: &egui::Context,
    backend: &mut TerminalBackend,
    events: &Receiver<(u64, PtyEvent)>,
    secs: u64,
) -> (ClipboardType, String) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        frame(ctx, backend);
        while let Ok((_, event)) = events.try_recv() {
            if let PtyEvent::ClipboardStore(kind, text) = event {
                return (kind, text);
            }
        }
        assert!(
            Instant::now() < deadline,
            "no ClipboardStore ever surfaced; the grid holds {:?}",
            screen_text(backend).trim_end()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The sequence on its own. If this fails, nothing downstream can work — and
/// the failure is in the vendored backend, not in tmux's willingness to send.
#[test]
fn a_bare_osc52_reaches_the_embedder() {
    let ctx = egui::Context::default();
    let (mut backend, events) = sh(
        &ctx,
        // Exactly the line a user can paste into a terra tab to check this by
        // hand, and the one the live verification runs.
        r"printf 'READY\n'; printf '\033]52;c;%s\a' $(printf FROM-OSC52 | base64); cat",
    );
    wait_for(&ctx, &mut backend, "the program to start", 30, |b| {
        screen_text(b).contains("READY")
    });

    let (kind, text) = wait_for_clipboard_store(&ctx, &mut backend, &events, 30);
    println!("OSC 52 surfaced as {kind:?} {text:?}");
    assert_eq!(text, "FROM-OSC52");
    assert!(
        matches!(kind, ClipboardType::Clipboard),
        "`;c;` names the clipboard, not {kind:?}"
    );
}

/// `;p;` is the X11 primary selection. macOS has one pasteboard, so terra maps
/// both onto it — this pins that the variant still arrives distinctly, which is
/// what `main.rs` deliberately flattens.
#[test]
fn the_primary_selection_target_arrives_too() {
    let ctx = egui::Context::default();
    let (mut backend, events) = sh(
        &ctx,
        r"printf 'READY\n'; printf '\033]52;p;%s\a' $(printf PRIMARY-52 | base64); cat",
    );
    wait_for(&ctx, &mut backend, "the program to start", 30, |b| {
        screen_text(b).contains("READY")
    });

    let (kind, text) = wait_for_clipboard_store(&ctx, &mut backend, &events, 30);
    assert_eq!(text, "PRIMARY-52");
    assert!(matches!(kind, ClipboardType::Selection), "{kind:?}");
}

/// The *read* direction must stay refused. A program that asks
/// (`ESC]52;c;?`) is asking terra to hand this Mac's clipboard to whatever is
/// on the far end of an ssh connection; alacritty's default `Osc52::OnlyCopy`
/// drops it before it becomes an event, and the arm in `backend/mod.rs`
/// answers empty if that default ever moves. Either way: no event, and nothing
/// resembling the clipboard's contents echoed back.
#[test]
fn an_osc52_read_is_not_answered_with_the_clipboard() {
    let ctx = egui::Context::default();
    let (mut backend, events) = sh(
        &ctx,
        r"stty raw -echo; printf 'READY\r\n'; printf '\033]52;c;?\a'; cat",
    );
    wait_for(&ctx, &mut backend, "the program to start", 30, |b| {
        screen_text(b).contains("READY")
    });
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(20));
        frame(&ctx, &mut backend);
    }

    while let Ok((_, event)) = events.try_recv() {
        assert!(
            !matches!(event, PtyEvent::ClipboardStore(..)),
            "an OSC 52 query turned into a clipboard *write*"
        );
    }
    // `cat` echoes whatever terra wrote back onto the grid, so a reply would
    // be visible as a `^[]52;` there. Nothing is the correct answer.
    let text = screen_text(&mut backend);
    println!("grid after an OSC 52 query: {:?}", text.trim_end());
    assert!(
        !text.contains("]52;"),
        "terra answered the clipboard query: {:?}",
        text.trim_end()
    );
}

// ---------------------------------------------------------------------------
// The real chain: tmux copies its selection out through OSC 52.
// ---------------------------------------------------------------------------

/// A tmux server on a private socket, so nothing here can touch the user's own.
struct PrivateTmux {
    socket: String,
    conf: PathBuf,
}

impl PrivateTmux {
    fn new(tag: &str) -> Self {
        let socket = format!("terra-osc52-{tag}");
        let conf = std::env::temp_dir().join(format!("{socket}.conf"));
        let mut f = std::fs::File::create(&conf).expect("write tmux config");
        writeln!(f, "set -g mouse on").unwrap();
        writeln!(f, "set -g default-terminal \"xterm-256color\"").unwrap();
        writeln!(f, "set -g status off").unwrap();
        writeln!(f, "set -g escape-time 0").unwrap();
        // Deliberately *no* `set-clipboard` line. tmux only forwards a copy
        // when it believes the outer terminal can take it — the `Ms` capability
        // — and macOS's system `xterm-256color` terminfo has no `Ms` entry, so
        // this looks like it ought to need forcing. Measured on tmux 3.5a it
        // does not: `set-clipboard` already defaults to `external` (which does
        // attempt to set the terminal's clipboard) and tmux carries its own
        // `terminal-features[0] xterm*:clipboard`, which grants `Ms` to terra's
        // TERM without consulting terminfo at all. Leaving the line out is the
        // stronger pin: it proves a user with a stock tmux needs nothing but
        // `set -g mouse on`.
        //
        // `set -s set-clipboard on` governs the *other* direction — whether
        // tmux accepts OSC 52 from programs running inside it — and is what a
        // user wants if vim or an agent inside tmux should reach the Mac's
        // pasteboard.
        let me = Self { socket, conf };
        me.kill();
        me
    }

    fn tmux(&self, args: &[&str]) -> std::process::Output {
        std::process::Command::new("tmux")
            .arg("-L")
            .arg(&self.socket)
            .args(args)
            .output()
            .expect("run tmux")
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

/// The user's chain, minus ssh (which changes nothing about the escape
/// sequence — it is bytes on the same stream): a drag inside tmux makes *tmux*
/// select, and the copy comes back out to terra as OSC 52. The selection is
/// driven with `copy-mode` commands rather than synthetic mouse events, because
/// what is under test is the emission, not tmux's own mouse handling — and the
/// mouse path through this chain is already pinned by `tmux_mouse_chain.rs`.
#[test]
fn tmux_copies_its_selection_out_through_osc52() {
    let tmux = PrivateTmux::new("chain");
    let ctx = egui::Context::default();
    let (mut backend, events) = spawn(
        &ctx,
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
            // One unmistakable word on an otherwise empty screen, at the top,
            // so the copy-mode cursor starts on it.
            r"clear; printf 'COPYME\n'; cat".to_string(),
        ],
    );
    wait_for(&ctx, &mut backend, "the tmux session to paint", 30, |b| {
        screen_text(b).contains("COPYME")
    });

    // Select the first word of the top line and copy it. `copy-selection`
    // is what a mouse release runs too, so this is the same code path tmux
    // takes when the user drags.
    tmux.tmux(&["copy-mode"]);
    tmux.tmux(&["send-keys", "-X", "history-top"]);
    tmux.tmux(&["send-keys", "-X", "start-of-line"]);
    tmux.tmux(&["send-keys", "-X", "begin-selection"]);
    tmux.tmux(&["send-keys", "-X", "next-word-end"]);
    tmux.tmux(&["send-keys", "-X", "copy-selection-and-cancel"]);

    let (kind, text) = wait_for_clipboard_store(&ctx, &mut backend, &events, 30);
    println!("tmux copied out as {kind:?} {text:?}");
    assert!(
        text.contains("COPYME"),
        "tmux sent OSC 52 but with {text:?}, not the selected word"
    );
}

// ---------------------------------------------------------------------------
// The gesture the user actually makes: a plain drag, no copy-mode commands.
// ---------------------------------------------------------------------------

/// A frame that carries injected events. The file's other tests never inject
/// anything, so they use the argument-free `frame`.
fn frame_with(ctx: &egui::Context, backend: &mut TerminalBackend, events: Vec<Event>) {
    let input = egui::RawInput {
        screen_rect: Some(SCREEN),
        events,
        modifiers: Modifiers::NONE,
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

fn press(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

/// PATCHES entry 13, end to end: a plain drag inside tmux (`mouse on`, nothing
/// else) must land on the Mac's clipboard the way it does in Ghostty.
///
/// tmux asks for *button-event* tracking (DECSET 1002): report motion only
/// while a button is down. terra used to forward drag motion solely under 1003
/// (any-motion), so tmux received press…nothing…release — two clicks, never a
/// drag. `copy-mode -M` opened on the press and `copy-selection-and-cancel` ran
/// on the release with nothing selected, so no OSC 52 was ever emitted and the
/// user's drag "did nothing".
///
/// The pin is therefore the `ClipboardStore` at the far end: it can only exist
/// if the motion reports in the middle reached tmux. Unlike
/// `tmux_copies_its_selection_out_through_osc52` above, no `send-keys -X` is
/// used — every step of the selection is a synthetic egui pointer event, which
/// is what makes this a regression test for terra's side rather than tmux's.
#[test]
fn a_plain_drag_inside_tmux_copies_through_osc52() {
    let tmux = PrivateTmux::new("drag");
    let ctx = egui::Context::default();
    let (mut backend, events) = spawn(
        &ctx,
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
            // A page of it, so wherever the drag row lands it is over text.
            // No mouse tracking in here: the pane is a plain shell, so the
            // drag belongs to tmux itself — the user's real situation.
            r"clear; i=0; while [ $i -lt 8 ]; do \
                  printf 'DRAGME-DRAGME-DRAGME-DRAGME-DRAGME\n'; i=$((i+1)); \
              done; cat"
                .to_string(),
        ],
    );

    // Wait for both halves of readiness: the page painted, and tmux's mouse
    // tracking negotiated toward terra. MOUSE_DRAG is the mode the fix turned
    // into a motion-forwarding one.
    wait_for(
        &ctx,
        &mut backend,
        "tmux to paint and grab the mouse",
        30,
        |b| {
            let synced = b.sync();
            let mode = synced.terminal_mode;
            mode.contains(TerminalMode::MOUSE_DRAG) && screen_text(b).contains("DRAGME")
        },
    );
    println!(
        "modes tmux negotiated toward terra: {:?}",
        backend.sync().terminal_mode
    );

    // Press at one end of a text row and walk across it in several steps — a
    // single jump would still be one motion report, but a real drag is a
    // stream of them and tmux extends its selection on each.
    let row_y = 40.0;
    frame_with(
        &ctx,
        &mut backend,
        vec![Event::PointerMoved(Pos2::new(60.0, row_y))],
    );
    frame_with(
        &ctx,
        &mut backend,
        vec![press(Pos2::new(60.0, row_y), true)],
    );
    for x in [100.0, 140.0, 180.0, 220.0, 260.0] {
        frame_with(
            &ctx,
            &mut backend,
            vec![Event::PointerMoved(Pos2::new(x, row_y))],
        );
        std::thread::sleep(Duration::from_millis(20));
        frame(&ctx, &mut backend);
    }

    // Mid-drag, before the release: tmux opens copy-mode on the press, so this
    // is a diagnostic rather than the pin — it separates "the press never
    // arrived" from "the motion never arrived" when this test goes red.
    let in_mode = tmux.tmux(&["display-message", "-p", "#{pane_in_mode}"]);
    let in_mode = String::from_utf8_lossy(&in_mode.stdout).trim().to_string();
    println!("mid-drag pane_in_mode: {in_mode:?}");
    assert_eq!(
        in_mode, "1",
        "tmux never entered copy-mode, so the drag never reached it"
    );

    frame_with(
        &ctx,
        &mut backend,
        vec![press(Pos2::new(260.0, row_y), false)],
    );

    let (kind, text) = wait_for_clipboard_store(&ctx, &mut backend, &events, 30);
    println!("a plain tmux drag copied out as {kind:?} {text:?}");
    assert!(
        text.contains("DRAGME-DRAGME"),
        "the drag copied {text:?}; motion reports never reached tmux, so its \
         selection stayed empty (or one cell wide)"
    );
    assert_eq!(
        backend.selectable_content(),
        "",
        "the drag belonged to tmux; terra must not have selected as well"
    );
}
