//! Selecting a tab must mean you are typing into it (issue #20).
//!
//! The regression this guards is subtle: egui focus was never the problem —
//! `TerminalView::set_focus` requests it every frame the palette is closed, and
//! the tab bar's buttons never take it. What dropped the keystrokes was the
//! widget also demanding that the *pointer* hover the grid, which it does not
//! after a click that landed in the tab bar. So the test parks the pointer in
//! the bar, clicks there, and then types.
//!
//! A real PTY is spawned (`/bin/cat`, which echoes through the tty), so this is
//! Unix-only, like the PTY-backed tests in `tabs.rs`.
#![cfg(unix)]

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use alacritty_terminal::term::cell::Flags;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, Sense};
use egui_term::{BackendSettings, PtyEvent, TerminalBackend, TerminalView};

const SCREEN: Rect = Rect {
    min: Pos2::ZERO,
    max: Pos2::new(800.0, 600.0),
};
/// Inside the tab bar strip, and so outside the terminal grid.
const IN_THE_TAB_BAR: Pos2 = Pos2::new(20.0, 5.0);

/// One frame of a terra-shaped window: a tab bar on top, the terminal below,
/// and — when `palette_open` — a text input over both, focused the way
/// `terra-palette` focuses its query field.
///
/// `palette_open` also drives `set_focus`, exactly as `main.rs` does.
fn frame(
    ctx: &egui::Context,
    backend: &mut TerminalBackend,
    events: Vec<Event>,
    palette_open: bool,
) {
    let input = egui::RawInput {
        screen_rect: Some(SCREEN),
        events,
        ..Default::default()
    };
    let _ = ctx.run_ui(input, |ui: &mut egui::Ui| {
        let ctx = ui.ctx().clone();
        if palette_open {
            let edit_id = egui::Id::new("palette_query");
            egui::Area::new(egui::Id::new("palette"))
                .order(egui::Order::Foreground)
                .show(&ctx, |ui| {
                    ui.memory_mut(|m| m.request_focus(edit_id));
                    let mut query = String::new();
                    ui.add(egui::TextEdit::singleline(&mut query).id(edit_id));
                });
        }
        egui::Panel::top("bar").exact_size(32.0).show(ui, |ui| {
            // Stands in for `ui::tab_bar`: a clickable, focusable widget.
            let _ = ui.interact(ui.max_rect(), egui::Id::new("tab"), Sense::click_and_drag());
        });
        egui::CentralPanel::default().show(ui, |ui| {
            let view = TerminalView::new(ui, backend)
                .set_focus(!palette_open)
                .set_size(ui.available_size());
            ui.add(view);
        });
    });
}

fn click(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

/// Everything on the grid, as one string.
fn screen_text(backend: &mut TerminalBackend) -> String {
    backend
        .sync()
        .grid
        .display_iter()
        .filter(|c| !c.cell.flags.contains(Flags::WIDE_CHAR_SPACER))
        .map(|c| c.cell.c)
        .collect()
}

/// `/bin/cat` under a PTY: the tty echoes whatever reaches it, so anything
/// typed shows up on the grid and anything dropped never does.
///
/// The event receiver comes back with it, and has to outlive the backend —
/// nothing here reads PTY events, but the sender must stay connected.
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

/// Pump frames until `text` is echoed back, or fail.
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

#[test]
fn typing_reaches_the_terminal_with_the_pointer_left_in_the_tab_bar() {
    let ctx = egui::Context::default();
    let (mut backend, _events) = cat(&ctx);

    // A first frame to size the grid, then a click on the tab bar — which is
    // where the pointer stays afterwards, as it does after picking a tab.
    frame(&ctx, &mut backend, Vec::new(), false);
    frame(
        &ctx,
        &mut backend,
        vec![
            Event::PointerMoved(IN_THE_TAB_BAR),
            click(IN_THE_TAB_BAR, true),
            click(IN_THE_TAB_BAR, false),
        ],
        false,
    );

    // No second click, no pointer move: just type.
    frame(
        &ctx,
        &mut backend,
        vec![Event::Text("terra".to_string())],
        false,
    );
    wait_for_echo(&ctx, &mut backend, "terra");
}

/// The palette owns the keyboard while it is open — including when the pointer
/// is sitting over the grid, which is the case the terminal must not win — and
/// hands it straight back when it closes.
#[test]
fn the_palette_keeps_the_keyboard_and_gives_it_back() {
    let ctx = egui::Context::default();
    let (mut backend, _events) = cat(&ctx);

    frame(&ctx, &mut backend, Vec::new(), false);
    // Pointer parked in the middle of the grid.
    let in_the_grid = Pos2::new(400.0, 300.0);
    frame(
        &ctx,
        &mut backend,
        vec![Event::PointerMoved(in_the_grid)],
        false,
    );

    // Palette open: nothing typed may reach the PTY.
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
        "the palette's input lost keystrokes to the terminal"
    );

    // Closed again: the very next keystroke goes to the terminal.
    frame(
        &ctx,
        &mut backend,
        vec![Event::Text("back".to_string())],
        false,
    );
    wait_for_echo(&ctx, &mut backend, "back");
}
