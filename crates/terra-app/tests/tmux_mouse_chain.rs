//! Issue #36: inside `ssh host` → `tmux attach` (mouse on) → codex, a drag
//! selects nothing, so nothing can be copied.
//!
//! ssh is a transparent byte pipe, so a *local* tmux reproduces the whole
//! chain: tmux with `mouse on` turns mouse reporting on toward terra, and the
//! full-screen TUI inside the pane turns it on toward tmux. What terra sees is
//! one program that owns the mouse — the same situation `shift_selection.rs`
//! covers with a bare program, only with a real multiplexer in between.
//!
//! These tests answer two questions with the headless PTY harness:
//!   a) does a plain drag over that chain leave terra without a selection?
//!   b) does Shift-drag (the v1.2.0 `selection_override` escape hatch) still
//!      reach terra's own selection through tmux?
//!
//! The inner program is the skill's `cat` trick: it enters the alt screen,
//! paints a page of `SELECTME`, grabs the mouse, then echoes every byte it
//! receives — so the grid itself shows which mouse reports got through.
//!
//! Every test runs tmux on its own private server socket with `-f` pointing at
//! a generated config, so the user's tmux server, config and sessions are
//! never touched; the server is killed on drop, panic or not. Unix-only.
#![cfg(unix)]

use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use alacritty_terminal::term::cell::Flags;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect};
use egui_term::{BackendSettings, PtyEvent, TerminalBackend, TerminalMode, TerminalView};

const SCREEN: Rect = Rect {
    min: Pos2::ZERO,
    max: Pos2::new(800.0, 600.0),
};
/// Both ends of the drag sit inside the painted block, on the same row, well
/// clear of tmux's status line at the bottom of the window.
const DRAG_FROM: Pos2 = Pos2::new(60.0, 40.0);
const DRAG_TO: Pos2 = Pos2::new(260.0, 40.0);

/// What the inner "codex" runs: alt screen, a page of selectable text, mouse
/// tracking (1000/1002/1006 — button, drag, SGR), then echo everything.
const INNER_TUI: &str = r"printf '\033[?1049h'; \
     i=0; while [ $i -lt 8 ]; do \
       printf 'SELECTME-SELECTME-SELECTME-SELECTME-SELECTME\n'; \
       i=$((i+1)); done; \
     printf '\033[?1000h\033[?1002h\033[?1006h'; cat";

/// A private tmux server: its own socket name, a config that only turns the
/// mouse on, and a `kill-server` on drop so a panicking test cannot leave one
/// behind.
struct PrivateTmux {
    socket: String,
    conf: PathBuf,
}

impl PrivateTmux {
    fn new(tag: &str) -> Self {
        let socket = format!("terra-issue36-{tag}");
        let conf = std::env::temp_dir().join(format!("{socket}.conf"));
        let mut f = std::fs::File::create(&conf).expect("write tmux config");
        // `mouse on` is the whole point; the rest keeps the pane predictable.
        writeln!(f, "set -g mouse on").unwrap();
        writeln!(f, "set -g default-terminal \"xterm-256color\"").unwrap();
        writeln!(f, "set -g status off").unwrap();
        writeln!(f, "set -g escape-time 0").unwrap();
        drop(f);
        let me = Self { socket, conf };
        me.kill(); // a leftover from an interrupted run would be reattached to
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

/// Everything on the grid, as one string. Bytes the inner `cat` echoes back
/// show up caret-style, so an SGR mouse report reads as `^[[<0;12;3M`.
fn screen_text(backend: &mut TerminalBackend) -> String {
    backend
        .sync()
        .grid
        .display_iter()
        .filter(|c| !c.cell.flags.contains(Flags::WIDE_CHAR_SPACER))
        .map(|c| c.cell.c)
        .collect()
}

/// Spawn `tmux new-session <inner tui>` on the private server, straight into
/// terra's PTY — exactly what `ssh host` + `tmux attach` delivers, minus the
/// network.
fn tmux_chain(
    ctx: &egui::Context,
    tmux: &PrivateTmux,
) -> (TerminalBackend, Receiver<(u64, PtyEvent)>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let backend = TerminalBackend::new(
        0,
        ctx.clone(),
        tx,
        BackendSettings {
            shell: "tmux".to_string(),
            args: vec![
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
            ..Default::default()
        },
    )
    .expect("spawn tmux");
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

/// Pump frames until tmux has forwarded the inner program's mouse-mode request
/// upstream *and* the painted page is on the grid. Returns the modes tmux
/// negotiated toward terra.
fn ready(ctx: &egui::Context, backend: &mut TerminalBackend) -> TerminalMode {
    let deadline = Instant::now() + Duration::from_secs(30);
    frame(ctx, backend, Vec::new(), Modifiers::NONE);
    loop {
        let mode = backend.sync().terminal_mode;
        if mode.intersects(TerminalMode::MOUSE_MODE) && screen_text(backend).contains("SELECTME") {
            return mode;
        }
        assert!(
            Instant::now() < deadline,
            "the tmux chain never came up (mode {:?}); the grid holds {:?}",
            mode,
            screen_text(backend).trim_end()
        );
        std::thread::sleep(Duration::from_millis(20));
        frame(ctx, backend, Vec::new(), Modifiers::NONE);
    }
}

/// Press at one end, move, release at the other — the whole gesture held under
/// `modifiers`, then frames pumped so anything written to the PTY can travel
/// down through tmux, be echoed by the inner `cat` and come back to the grid.
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
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(20));
        frame(ctx, backend, Vec::new(), Modifiers::NONE);
    }
}

/// Part (a) of issue #36: the plain drag. tmux owns the mouse, the reports go
/// downstream, terra selects nothing — this is the user's complaint, and it is
/// correct behaviour on its own.
#[test]
fn plain_drag_through_tmux_selects_nothing() {
    let tmux = PrivateTmux::new("plain");
    let ctx = egui::Context::default();
    let (mut backend, _events) = tmux_chain(&ctx, &tmux);
    let mode = ready(&ctx, &mut backend);
    println!("modes tmux negotiated toward terra: {mode:?}");

    drag(&ctx, &mut backend, Modifiers::NONE);

    let text = screen_text(&mut backend);
    println!("grid after a plain drag: {:?}", text.trim_end());
    assert!(
        text.contains("^[[<0;"),
        "the drag never reached the program through tmux: {:?}",
        text.trim_end()
    );
    assert_eq!(
        backend.selectable_content(),
        "",
        "a plain drag must not select while the chain owns the mouse"
    );
}

/// Part (b): the v1.2.0 escape hatch, through the same chain. Shift must hand
/// the drag back to terra — no mouse report downstream, and the dragged text
/// selected so ⌘C can copy it.
#[test]
fn shift_drag_through_tmux_selects_in_terra() {
    let tmux = PrivateTmux::new("shift");
    let ctx = egui::Context::default();
    let (mut backend, _events) = tmux_chain(&ctx, &tmux);
    let mode = ready(&ctx, &mut backend);
    println!("modes tmux negotiated toward terra: {mode:?}");

    drag(&ctx, &mut backend, Modifiers::SHIFT);

    let text = screen_text(&mut backend);
    println!("grid after a Shift-drag: {:?}", text.trim_end());
    assert!(
        !text.contains("^[[<"),
        "a mouse report reached the chain despite Shift: {:?}",
        text.trim_end()
    );
    let selected = backend.selectable_content();
    println!("Shift-drag selected: {selected:?}");
    assert!(
        selected.contains("SELECT"),
        "Shift-drag through tmux selected {selected:?}, expected part of the \
         painted page"
    );
}

/// Option, the macOS spelling of the same escape hatch, through tmux.
#[test]
fn option_drag_through_tmux_selects_in_terra() {
    let tmux = PrivateTmux::new("option");
    let ctx = egui::Context::default();
    let (mut backend, _events) = tmux_chain(&ctx, &tmux);
    ready(&ctx, &mut backend);

    drag(&ctx, &mut backend, Modifiers::ALT);

    let text = screen_text(&mut backend);
    assert!(
        !text.contains("^[[<"),
        "a mouse report reached the chain despite Option: {:?}",
        text.trim_end()
    );
    let selected = backend.selectable_content();
    println!("Option-drag selected: {selected:?}");
    assert!(
        selected.contains("SELECT"),
        "Option-drag selected {selected:?}"
    );
}
