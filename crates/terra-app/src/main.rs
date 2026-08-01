//! terra — a terminal for watching (and driving) your agents.
//!
//! - `tabs.rs`  — `TabManager`: create/kill/rename/select/capture/send
//! - `ui.rs`    — pill-style tab bar + keybindings
//! - `ipc.rs`   — unix-socket server; its threads drive the tabs directly
//! - palette integration (terra-palette)

mod fonts;
mod ghostty_theme;
mod ipc;
mod macos;
mod scrollbar;
mod tabs;
mod ui;

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};

use egui_term::{PtyEvent, TerminalView};
use terra_palette::{Palette, PaletteAction, PaletteEvent};

use crate::ipc::IpcServer;
use crate::scrollbar::ScrollbarState;
use crate::tabs::TabManager;
use crate::ui::AppAction;

const RENAME_PROMPT_ID: &str = "rename";

/// Take the tab lock, ignoring poisoning: a panic on an IPC thread must not
/// take the window down with it (`ipc.rs` locks the same way).
///
/// Every caller keeps its guard to the smallest possible scope, and never
/// acquires a second one while holding the first — the UI thread is one thread,
/// so a nested lock would simply deadlock against itself.
fn lock(tabs: &Mutex<TabManager>) -> MutexGuard<'_, TabManager> {
    tabs.lock().unwrap_or_else(|err| err.into_inner())
}

/// Ghostty-like readability: bright foreground on a soft dark background
/// (egui_term's defaults are dimmer and smaller than a real terminal).
fn terminal_theme() -> egui_term::TerminalTheme {
    egui_term::TerminalTheme::new(Box::new(ghostty_theme::palette()))
}

fn terminal_font() -> egui_term::TerminalFont {
    egui_term::TerminalFont::new(egui_term::FontSettings {
        // Ghostty macOS: 13pt CoreText ≈ 15px egui em, plus the user's
        // `adjust-cell-height = 30%`.
        font_type: egui::FontId::monospace(15.0),
        line_height: 1.3,
    })
}

/// The window/taskbar icon. (The Dock icon on macOS comes from the .app
/// bundle's `terra.icns` instead — see `just bundle`.)
fn app_icon() -> egui::IconData {
    const PNG: &[u8] = include_bytes!("../assets/icon/terra-256.png");
    match eframe::icon_data::from_png_bytes(PNG) {
        Ok(icon) => icon,
        Err(err) => {
            log::warn!("terra: cannot decode the app icon: {err}");
            egui::IconData::default()
        }
    }
}

fn main() -> eframe::Result {
    env_logger::init();
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([480.0, 320.0])
            .with_title("terra")
            .with_icon(app_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "terra",
        native_options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

struct App {
    pty_events: Receiver<(u64, PtyEvent)>,
    pty_sender: Sender<(u64, PtyEvent)>,
    /// Shared with the IPC threads, which run `terra` CLI requests against it
    /// themselves — the UI thread does not get a frame at all while the window
    /// is occluded, so it cannot be the one executing them.
    tabs: Option<Arc<Mutex<TabManager>>>,
    palette: Palette,
    ipc: Option<IpcServer>,
    scrollbar: ScrollbarState,
    quitting: bool,
    last_window_title: String,
    /// Directory currently behind the titlebar proxy icon, so we only bother
    /// AppKit when it actually moves.
    last_represented_path: Option<std::path::PathBuf>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        fonts::install(&cc.egui_ctx);
        let (pty_sender, pty_events) = mpsc::channel();
        Self {
            pty_events,
            pty_sender,
            tabs: None,
            palette: Palette::default(),
            ipc: None,
            scrollbar: ScrollbarState::default(),
            quitting: false,
            last_window_title: String::new(),
            last_represented_path: None,
        }
    }

    /// One-time setup that needs a live `egui::Context`: the first tab and the
    /// IPC listener.
    fn ensure_started(&mut self, ctx: &egui::Context) {
        if self.tabs.is_some() {
            return;
        }
        let mut tabs = TabManager::new(ctx.clone(), self.pty_sender.clone());
        if let Err(err) = tabs.open(&[], None, None) {
            log::error!("terra: cannot spawn the initial shell: {err}");
            self.quitting = true;
        }
        let tabs = Arc::new(Mutex::new(tabs));
        self.tabs = Some(Arc::clone(&tabs));

        match ipc::start(ctx.clone(), tabs) {
            Ok(server) => {
                log::info!("terra: listening on {}", server.socket_path().display());
                self.ipc = Some(server);
            }
            Err(err) => log::error!("terra: ipc server unavailable: {err}"),
        }
    }

    /// Keep the macOS window title — and the titlebar proxy icon that goes with
    /// it — in sync with the active tab (like Ghostty).
    fn sync_window_title(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let title = self
            .tabs
            .as_ref()
            .and_then(|tabs| lock(tabs).infos().into_iter().find(|i| i.active))
            .map(|i| i.title)
            .unwrap_or_else(|| "terra".to_string());
        if title == self.last_window_title {
            return; // nothing moved — don't stat the disk on every frame
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
        self.last_window_title = title;

        // The title doubles as the cwd (`~/src/terra`), which is exactly what
        // the titlebar proxy icon wants to point at.
        let path = macos::title_path(&self.last_window_title);
        if path != self.last_represented_path {
            macos::set_represented_path(frame, path.as_deref());
            self.last_represented_path = path;
        }
    }

    /// PTY titles and exits are handled here, on the UI thread, not on the IPC
    /// threads: nothing a client can ask for depends on them. The cost is that
    /// while the window is occluded titles go stale and a tab whose shell has
    /// exited stays in `terra ls` until the window is drawn again — both catch
    /// up on the next frame, and neither can strand a request.
    fn drain_pty_events(&mut self) {
        let Some(arc) = self.tabs.clone() else {
            return;
        };
        let mut tabs = lock(&arc);
        while let Ok((id, event)) = self.pty_events.try_recv() {
            match event {
                PtyEvent::Title(title) => tabs.set_shell_title(id, title),
                PtyEvent::Exit => {
                    tabs.close(id);
                }
                _ => {}
            }
        }
    }

    fn palette_actions(&self) -> Vec<PaletteAction> {
        let mut actions = vec![
            PaletteAction::new("tab.new", "New Tab", Some("⌘T")),
            PaletteAction::new("tab.close", "Close Tab", Some("⌘W")),
            PaletteAction::new("tab.rename", "Rename Tab…", None),
            PaletteAction::new("tab.next", "Next Tab", Some("⇧⌘]")),
            PaletteAction::new("tab.prev", "Previous Tab", Some("⇧⌘[")),
        ];
        if let Some(tabs) = self.tabs.as_ref().map(|t| lock(t)) {
            for id in tabs.ids() {
                let title = tabs.title(id).unwrap_or("shell");
                actions.push(PaletteAction::new(
                    format!("tab.select.{id}"),
                    format!("Go to Tab: {title}"),
                    None,
                ));
            }
        }
        actions.push(PaletteAction::new("app.quit", "Quit terra", Some("⌘Q")));
        actions
    }

    fn handle_palette(&mut self, ctx: &egui::Context, actions: &mut Vec<AppAction>) {
        let Some(event) = self.palette.show(ctx) else {
            return;
        };
        match event {
            PaletteEvent::ActionChosen { action_id } => {
                self.palette.close();
                match action_id.as_str() {
                    "tab.new" => actions.push(AppAction::NewTab),
                    "tab.close" => actions.push(AppAction::CloseActive),
                    "tab.rename" => actions.push(AppAction::RenameActive),
                    "tab.next" => actions.push(AppAction::NextTab),
                    "tab.prev" => actions.push(AppAction::PrevTab),
                    "app.quit" => actions.push(AppAction::Quit),
                    other => match other.strip_prefix("tab.select.") {
                        Some(id) => match id.parse::<u64>() {
                            Ok(id) => actions.push(AppAction::SelectTab(id)),
                            Err(_) => log::warn!("terra: bad palette action {other}"),
                        },
                        None => log::warn!("terra: unknown palette action {other}"),
                    },
                }
            }
            PaletteEvent::PromptSubmitted { id, text } => {
                self.palette.close();
                if id == RENAME_PROMPT_ID {
                    if let Some(arc) = self.tabs.clone() {
                        let mut tabs = lock(&arc);
                        if let Some(active) = tabs.active_id() {
                            tabs.set_custom_title(active, text);
                        }
                    }
                }
            }
            PaletteEvent::Dismissed => self.palette.close(),
        }
    }

    /// Every arm takes the lock for exactly as long as it needs it — never
    /// across a call that would want it again (`palette_actions`).
    fn apply(&mut self, action: AppAction) {
        let Some(arc) = self.tabs.clone() else {
            return;
        };
        match action {
            AppAction::NewTab => {
                if let Err(err) = lock(&arc).open(&[], None, None) {
                    log::error!("terra: cannot spawn a shell: {err}");
                }
            }
            AppAction::CloseActive => lock(&arc).close_active(),
            AppAction::CloseTab(id) => {
                lock(&arc).close(id);
            }
            AppAction::SelectTab(id) => {
                lock(&arc).select(id);
            }
            AppAction::SelectNth(n) => lock(&arc).select_nth(n),
            AppAction::NextTab => lock(&arc).select_next(),
            AppAction::PrevTab => lock(&arc).select_prev(),
            AppAction::OpenPalette => {
                let actions = self.palette_actions();
                self.palette.open(actions);
            }
            AppAction::RenameActive => {
                let prefill = {
                    let tabs = lock(&arc);
                    tabs.active_id()
                        .and_then(|id| tabs.title(id))
                        .unwrap_or("")
                        .to_string()
                };
                self.palette
                    .open_prompt("Rename tab", prefill, RENAME_PROMPT_ID);
            }
            AppAction::Quit => {
                lock(&arc).clear();
                self.quitting = true;
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.ensure_started(&ctx);

        if ctx.input(|i| i.viewport().close_requested()) {
            if let Some(arc) = self.tabs.clone() {
                lock(&arc).clear();
            }
            self.ipc = None;
            return;
        }

        self.drain_pty_events();
        self.sync_window_title(&ctx, frame);

        let mut actions: Vec<AppAction> = Vec::new();
        // Global shortcuts are consumed before the terminal widget reads events.
        if !self.palette.is_open() {
            actions.extend(ui::consume_shortcuts(ui));
        }
        self.handle_palette(&ctx, &mut actions);
        for action in std::mem::take(&mut actions) {
            self.apply(action);
        }

        if let Some(arc) = self.tabs.clone() {
            let tabs = lock(&arc);
            ui::tab_bar(ui, &tabs, &mut actions);
        }
        for action in std::mem::take(&mut actions) {
            self.apply(action);
        }

        let palette_open = self.palette.is_open();
        let tabs_arc = self.tabs.clone();
        let scrollbar = &mut self.scrollbar;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(egui::Color32::from_rgb(0x1e, 0x1e, 0x1e))
                    .inner_margin(egui::Margin {
                        left: 10,
                        right: 4,
                        top: 8,
                        bottom: 4,
                    }),
            )
            .show(ui, |ui| {
                let Some(arc) = tabs_arc else {
                    return;
                };
                // One guard for the whole active-tab render: `TerminalView`
                // wants a `&mut Tab` that lives inside the manager, and nothing
                // in here reaches for the lock a second time.
                let mut tabs = lock(&arc);
                if let Some(tab) = tabs.active_mut() {
                    let view = TerminalView::new(ui, &mut tab.backend)
                        .set_focus(!palette_open)
                        .set_theme(terminal_theme())
                        .set_font(terminal_font())
                        .set_size(ui.available_size());
                    let rect = ui.add(view).rect;
                    // After the terminal, so the thumb wins the hit test.
                    scrollbar::show(ui, rect, &mut tab.backend, scrollbar);
                }
            });

        // Last tab gone (or Quit chosen) -> the app is done.
        let empty = self.tabs.as_ref().is_some_and(|tabs| lock(tabs).is_empty());
        if self.quitting || empty {
            self.ipc = None;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}
