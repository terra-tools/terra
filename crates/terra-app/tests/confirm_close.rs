//! The "Close Window?" / "Close Tab?" dialog, driven headlessly over real PTYs.
//!
//! `App::ui` in `main.rs` is not reachable from a test crate (terra-app is a
//! binary), and neither is a window-close gesture: nothing here can raise
//! `ViewportInfo::close_requested`. So the split is deliberate —
//! `confirm_close`'s own unit tests cover the ask/don't-ask decision and the
//! Idle → Asking → Approved state machine, and this file covers the two halves
//! that only exist against the world:
//!
//! - the dialog on screen: that it answers Esc, Return and both buttons, and
//!   that while it is up **nothing** typed reaches the terminal underneath;
//! - the *close path*: a real [`TabManager`] with real shells in it, gated by
//!   the shipped `ConfirmClose` and the real process table, so "a tab running
//!   a command asks, a tab at a prompt does not" is asserted against actual
//!   processes rather than a table of strings.
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
// The included modules carry plenty the tests here never call — `fonts` alone
// is pulled in whole just to register the UI families the dialog draws with.
#![allow(dead_code)]

#[path = "../src/config.rs"]
mod config;
#[path = "../src/confirm_close.rs"]
mod confirm_close;
#[path = "../src/fonts.rs"]
mod fonts;
#[path = "../src/ghostty_theme.rs"]
mod ghostty_theme;
#[path = "../src/procinfo.rs"]
mod procinfo;
#[path = "../src/tabs.rs"]
mod tabs;
#[path = "../src/transcript.rs"]
mod transcript;

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use alacritty_terminal::term::cell::Flags;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use egui_term::{BackendSettings, PtyEvent, TerminalBackend, TerminalView};

use confirm_close::{Choice, ConfirmClose, Subject, CLOSE_LABEL};
use tabs::TabManager;

/// `tabs.rs` calls `crate::terminal_theme()`; in this test crate, this is it —
/// the same construction `main.rs` uses.
fn terminal_theme() -> egui_term::TerminalTheme {
    egui_term::TerminalTheme::new(Box::new(ghostty_theme::palette()))
}

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
    dialog: Option<Subject>,
) -> Option<Choice> {
    let input = egui::RawInput {
        screen_rect: Some(SCREEN),
        events,
        ..Default::default()
    };
    let mut choice = None;
    let _ = ctx.run_ui(input, |ui: &mut egui::Ui| {
        let ctx = ui.ctx().clone();
        if let Some(subject) = dialog {
            choice = confirm_close::show(&ctx, subject);
        }
        egui::CentralPanel::default().show(ui, |ui| {
            let view = TerminalView::new(ui, backend)
                .set_focus(dialog.is_none())
                .set_size(ui.available_size());
            ui.add(view);
        });
    });
    choice
}

/// The window's own question, which is what most of the on-screen tests are
/// about — the wording is the only thing a tab close changes.
const WINDOW: Option<Subject> = Some(Subject::Window);

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
        frame(ctx, backend, Vec::new(), None);
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
    frame(&ctx, &mut backend, Vec::new(), WINDOW);

    let choice = frame(&ctx, &mut backend, vec![key(Key::Escape)], WINDOW);
    assert_eq!(choice, Some(Choice::Cancel));

    // The key was consumed, so a frame that still had the dialog up would see
    // nothing left of it.
    let again = frame(&ctx, &mut backend, Vec::new(), WINDOW);
    assert_eq!(again, None);
}

/// Return is the primary button — the dialog's default, as in the reference.
#[test]
fn return_confirms_the_close() {
    let ctx = context();
    let (mut backend, _events) = cat(&ctx);
    frame(&ctx, &mut backend, Vec::new(), WINDOW);

    let choice = frame(&ctx, &mut backend, vec![key(Key::Enter)], WINDOW);
    assert_eq!(choice, Some(Choice::Close));
}

/// The whole point of a modal: while it is up the terminal is deaf, and the
/// moment it goes away the very next keystroke lands.
#[test]
fn the_dialog_keeps_the_keyboard_and_gives_it_back() {
    let ctx = context();
    let (mut backend, _events) = cat(&ctx);
    frame(&ctx, &mut backend, Vec::new(), None);
    // Pointer parked over the grid — the case where the terminal would
    // otherwise win the input.
    frame(
        &ctx,
        &mut backend,
        vec![Event::PointerMoved(Pos2::new(400.0, 300.0))],
        None,
    );

    for _ in 0..5 {
        frame(
            &ctx,
            &mut backend,
            vec![Event::Text("stolen".to_string())],
            WINDOW,
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    frame(&ctx, &mut backend, Vec::new(), WINDOW);
    assert!(
        !screen_text(&mut backend).contains("stolen"),
        "the dialog lost keystrokes to the terminal below"
    );

    frame(
        &ctx,
        &mut backend,
        vec![Event::Text("back".to_string())],
        None,
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
        frame(&ctx, &mut backend, Vec::new(), WINDOW);
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
        WINDOW,
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
        frame(&ctx, &mut backend, Vec::new(), WINDOW);
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
        WINDOW,
    );
    assert_eq!(choice, Some(Choice::Cancel));
}

// ---------------------------------------------------------------------------
// The close path — real tabs, real processes
// ---------------------------------------------------------------------------

/// A manager with no tabs yet.
///
/// `$SHELL` is pinned: [`TabManager::open`] spawns the *user's* shell and types
/// the command into it (tmux send-keys style), so "an idle tab reports a name
/// the shell list knows" is only deterministic if the shell is.
fn manager(ctx: &egui::Context) -> TabManager {
    static PIN: std::sync::Once = std::sync::Once::new();
    PIN.call_once(|| std::env::set_var("SHELL", "/bin/sh"));
    let (tx, rx) = std::sync::mpsc::channel();
    // The sender must stay connected for the life of the backends; nothing
    // reads the events, so the receiver is parked in a leaked box.
    Box::leak(Box::new(rx));
    TabManager::new(ctx.clone(), tx)
}

fn foreground(tabs: &TabManager, id: u64) -> Option<String> {
    tabs.shell_pid(id).and_then(procinfo::foreground_command)
}

/// Open a tab and wait until the process table reports a foreground command
/// `want` accepts — a shell takes a moment to start, and longer to launch what
/// was typed into it.
fn tab_running(tabs: &mut TabManager, command: &[&str], want: impl Fn(&str) -> bool) -> u64 {
    let argv: Vec<String> = command.iter().map(|c| (*c).to_string()).collect();
    let id = tabs.open(&argv, None, None).expect("spawn a shell");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let seen = foreground(tabs, id);
        if seen.as_deref().is_some_and(&want) {
            return id;
        }
        assert!(
            Instant::now() < deadline,
            "tab {id} never settled; the process table says {seen:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A tab sitting at a prompt.
///
/// Which shell that is depends on the machine — `/bin/sh` *is* bash on macOS —
/// so the wait is for a name the shell list recognises rather than for one
/// spelling of it.
fn idle_tab(tabs: &mut TabManager) -> u64 {
    tab_running(tabs, &[], |name| {
        !confirm_close::should_confirm_tab_close(true, Some(name))
    })
}

/// A tab with a program in the foreground. `/bin/cat` blocks on the tty, which
/// is exactly the shape of the sessions this feature protects.
fn busy_tab(tabs: &mut TabManager) -> u64 {
    tab_running(tabs, &["/bin/cat"], |name| name == "cat")
}

/// `App::close_tab` in `main.rs` with egui taken out: the one door every
/// in-window close goes through — the tab's ✕, a middle-click on the pill, ⌘W,
/// the palette's `tab.close`. Returns whether the close was *held*.
fn close_tab(gate: &mut ConfirmClose, tabs: &mut TabManager, id: u64, enabled: bool) -> bool {
    let last = tabs.ids().as_slice() == [id];
    let held = gate.tab_requested(id, last, || {
        confirm_close::should_confirm_tab_close(enabled, foreground(tabs, id).as_deref())
    });
    if !held {
        tabs.close(id);
    }
    held
}

/// `App::ui`'s half: the answer comes in, and an approved close runs.
fn answer(gate: &mut ConfirmClose, tabs: &mut TabManager, choice: Choice) {
    let subject = gate.subject();
    gate.answer(choice);
    if choice != Choice::Close {
        return;
    }
    if let Some(id) = subject.tab_id() {
        assert!(
            !close_tab(gate, tabs, id, true),
            "an approved close must not raise the question again"
        );
        if !tabs.is_empty() {
            gate.reset();
        }
    }
}

/// The new behaviour: a tab that is *not* the last still asks, and Cancel
/// keeps it — that the window survives the close changes nothing.
#[test]
fn closing_a_busy_tab_asks_and_cancelling_keeps_it() {
    let ctx = context();
    let mut tabs = manager(&ctx);
    let idle = idle_tab(&mut tabs);
    let busy = busy_tab(&mut tabs);
    let mut gate = ConfirmClose::default();

    assert!(close_tab(&mut gate, &mut tabs, busy, true), "held");
    assert!(gate.is_open(), "the dialog is up");
    assert_eq!(
        gate.subject(),
        Subject::Tab {
            id: busy,
            last: false
        }
    );
    assert_eq!(
        gate.subject().title(),
        confirm_close::TITLE_TAB,
        "a close the window survives is worded as the tab's"
    );
    assert!(tabs.ids().contains(&busy), "the tab is still open");

    answer(&mut gate, &mut tabs, Choice::Cancel);
    assert!(!gate.is_open());
    assert_eq!(tabs.ids(), vec![idle, busy], "cancel closed nothing");
}

/// Approving runs the held close — and only it: the tab goes, the window and
/// its other tabs stay.
#[test]
fn approving_closes_that_one_tab() {
    let ctx = context();
    let mut tabs = manager(&ctx);
    let idle = idle_tab(&mut tabs);
    let busy = busy_tab(&mut tabs);
    let mut gate = ConfirmClose::default();

    assert!(close_tab(&mut gate, &mut tabs, busy, true));
    answer(&mut gate, &mut tabs, Choice::Close);
    assert_eq!(tabs.ids(), vec![idle], "the approved tab, and nothing else");
    assert!(!gate.is_open());
}

/// One approval is not a standing one: the *next* busy tab asks again. (The
/// Approved state exists for the window close's own retry, and must not leak
/// past a window that survived.)
#[test]
fn an_approved_tab_close_does_not_silence_the_next_one() {
    let ctx = context();
    let mut tabs = manager(&ctx);
    idle_tab(&mut tabs);
    let first = busy_tab(&mut tabs);
    let second = busy_tab(&mut tabs);
    let mut gate = ConfirmClose::default();

    assert!(close_tab(&mut gate, &mut tabs, first, true));
    answer(&mut gate, &mut tabs, Choice::Close);

    assert!(
        close_tab(&mut gate, &mut tabs, second, true),
        "the second busy tab gets its own question"
    );
    assert_eq!(gate.subject().tab_id(), Some(second));
}

/// A bare prompt protects nothing: it closes on the spot, dialog-free.
#[test]
fn a_tab_at_a_shell_prompt_closes_without_asking() {
    let ctx = context();
    let mut tabs = manager(&ctx);
    let idle = idle_tab(&mut tabs);
    let other = idle_tab(&mut tabs);
    let mut gate = ConfirmClose::default();

    assert!(!close_tab(&mut gate, &mut tabs, idle, true), "not held");
    assert!(!gate.is_open());
    assert_eq!(tabs.ids(), vec![other]);

    // …including when it is the last one, where the window goes with it.
    assert!(!close_tab(&mut gate, &mut tabs, other, true));
    assert!(!gate.is_open());
    assert!(tabs.is_empty());
}

/// The last tab is the same path with the window's wording, and it must not
/// have regressed: still held, still worded "Close Window?", and approving it
/// empties the window (which is what quits terra, fade included).
#[test]
fn the_last_busy_tab_still_asks_as_the_window() {
    let ctx = context();
    let mut tabs = manager(&ctx);
    let busy = busy_tab(&mut tabs);
    let mut gate = ConfirmClose::default();

    assert!(close_tab(&mut gate, &mut tabs, busy, true));
    assert_eq!(
        gate.subject(),
        Subject::Tab {
            id: busy,
            last: true
        }
    );
    assert_eq!(gate.subject().title(), confirm_close::TITLE_WINDOW);

    answer(&mut gate, &mut tabs, Choice::Close);
    assert!(tabs.is_empty(), "the window is empty, so terra quits");
}

/// `[window] confirm_close = false` is the whole switch: no door asks.
#[test]
fn the_config_switch_off_never_asks_for_a_tab() {
    let ctx = context();
    let mut tabs = manager(&ctx);
    let busy = busy_tab(&mut tabs);
    let other = busy_tab(&mut tabs);
    let mut gate = ConfirmClose::default();

    assert!(!close_tab(&mut gate, &mut tabs, busy, false));
    assert!(!gate.is_open());
    assert_eq!(tabs.ids(), vec![other]);
}

/// IPC `Kill` (`terra kill`) stays exempt: it goes straight to
/// `TabManager::close`, never through the gate. A remote controller means it,
/// and a modal on a blocked client would deadlock an agent.
#[test]
fn killing_a_busy_tab_over_ipc_never_asks() {
    let ctx = context();
    let mut tabs = manager(&ctx);
    let idle = idle_tab(&mut tabs);
    let busy = busy_tab(&mut tabs);
    let gate = ConfirmClose::default();

    // `ipc::handle`'s Kill arm, verbatim in spirit: close, no question.
    assert!(tabs.close(busy));
    assert_eq!(tabs.ids(), vec![idle]);
    assert!(!gate.is_open(), "no dialog was ever raised");
}
