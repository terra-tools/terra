//! terra — a terminal for watching (and driving) your agents.
//!
//! - `tabs.rs`  — `TabManager`: create/kill/rename/select/capture/send
//! - `ui.rs`    — pill-style tab bar + keybindings
//! - `ipc.rs`   — unix-socket server; its threads drive the tabs directly
//! - palette integration (terra-palette)

// Windows gives a process either a console or a window, decided at link time by
// the subsystem in the PE header. The default is `console`, so a released
// terra would open a stray black console box behind its own window, which the
// user cannot close without killing the app.
//
// Gated on `debug_assertions` rather than applied outright, because the
// subsystem is also what makes stdout exist: under `windows` there is no
// console attached, so `println!` and everything `env_logger` writes to stderr
// go nowhere at all — including the `RUST_LOG` output that is the only way to
// see `terra: ipc server unavailable` or `cannot spawn the initial shell`. A
// debug build keeps its console and stays debuggable; a release build is a
// GUI. This is the same split eframe's own template and the Tauri/egui
// ecosystem use.
//
// The attribute is ignored on every non-Windows target, so it needs no
// `cfg(windows)` of its own.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod fonts;
mod ghostty_theme;
mod ipc;
mod macos;
mod procinfo;
mod screenshot;
mod scrollbar;
mod tabs;
mod ui;

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};

use egui_term::{PtyEvent, TerminalView};
use terra_palette::{Palette, PaletteAction, PaletteEvent, PaletteIcon};

use crate::ipc::IpcServer;
use crate::screenshot::Screenshots;
use crate::scrollbar::ScrollbarState;
use crate::tabs::TabManager;
use crate::ui::AppAction;

const RENAME_PROMPT_ID: &str = "rename";

/// How often to re-check which program is running in the active tab. Fast
/// enough that launching an agent takes effect before you can read a line of
/// its output, slow enough to be free.
const FOREGROUND_POLL_SECS: f64 = 0.5;

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

/// Ghostty macOS: 13pt CoreText ≈ 15px egui em, plus the user's
/// `adjust-cell-height = 30%`. Both are `[font]` keys now — see `config.rs`,
/// whose defaults are pinned to exactly these numbers.
fn terminal_font(cfg: &config::FontConfig) -> egui_term::TerminalFont {
    egui_term::TerminalFont::new(egui_term::FontSettings {
        font_type: egui::FontId::monospace(cfg.size),
        line_height: cfg.line_height,
    })
}

/// Suffix appended to the window title so a development build is not mistaken
/// for the installed one. Pure, so the rules can be tested without touching the
/// process environment.
///
/// `just run`/`just restart` put the debug build on its own socket
/// (`TERRA_SOCKET=~/.terra/terra-dev.sock`) so it can live next to the release
/// the user works in all day — the socket *is* the single-instance claim. A
/// custom socket is therefore the signal that this window is not the daily
/// driver. `TERRA_DEV` overrides that guess in both directions: set it to mark
/// a window that uses the default socket, or to `0`/`false`/empty to suppress
/// the mark on a relocated one (e.g. a real second install).
fn dev_suffix(dev: Option<&str>, socket: Option<&str>) -> &'static str {
    const MARK: &str = " (dev)";
    match dev.map(str::trim) {
        Some("0" | "false" | "no" | "off" | "") => "",
        Some(_) => MARK,
        None if socket.is_some_and(|s| !s.trim().is_empty()) => MARK,
        None => "",
    }
}

/// [`dev_suffix`] for this process, read once — the environment cannot change
/// under us, and `sync_window_title` would otherwise ask on every rename.
fn dev_mark() -> &'static str {
    static MARK: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    MARK.get_or_init(|| {
        let dev = std::env::var("TERRA_DEV").ok();
        let socket = std::env::var("TERRA_SOCKET").ok();
        dev_suffix(dev.as_deref(), socket.as_deref())
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
            .with_title(format!("terra{}", dev_mark()))
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
    /// The `terra screenshot` rendezvous, shared with the IPC threads. It is
    /// the one request they cannot answer alone: the pixels exist only because
    /// this thread drew them (see `screenshot.rs`).
    screenshots: Arc<Screenshots>,
    scrollbar: ScrollbarState,
    config: config::ConfigStore,
    /// `config.generation()` that `cached_font` was built from, so the font
    /// is rebuilt when a setting moves rather than on every frame.
    cached_config_generation: u64,
    cached_font: egui_term::TerminalFont,
    /// The active tab's foreground command, and when it was last looked up.
    ///
    /// Resolving it is a `sysctl` over the whole process table — cheap, but
    /// not per-frame cheap, and what is running in a tab changes on human
    /// timescales. Polled instead of watched.
    foreground: Option<String>,
    foreground_checked: f64,
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
        let config = config::ConfigStore::load();
        let cached_font = terminal_font(&config.get().font);
        Self {
            pty_events,
            pty_sender,
            tabs: None,
            palette: Palette::default(),
            ipc: None,
            screenshots: Arc::default(),
            scrollbar: ScrollbarState::default(),
            cached_config_generation: config.generation(),
            cached_font,
            foreground: None,
            foreground_checked: f64::NEG_INFINITY,
            config,
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
        // IPC first, then the shell. `TabManager::new` spawns nothing — only
        // `open` does — and `ipc::start` is where the single-instance claim is
        // made and where a second launch hands over and exits. Opening the tab
        // first would spawn a PTY that is thrown away moments later.
        let tabs = Arc::new(Mutex::new(TabManager::new(
            ctx.clone(),
            self.pty_sender.clone(),
        )));
        self.tabs = Some(Arc::clone(&tabs));

        match ipc::start(
            ctx.clone(),
            Arc::clone(&tabs),
            Arc::clone(&self.screenshots),
        ) {
            Ok(server) => {
                log::info!("terra: listening on {}", server.socket_path().display());
                self.ipc = Some(server);
            }
            Err(err) => log::error!("terra: ipc server unavailable: {err}"),
        }

        // Scoped so the guard is dropped before `ensure_started` returns —
        // every other caller takes this lock too.
        let spawned = lock(&tabs).open(&[], None, None);
        if let Err(err) = spawned {
            log::error!("terra: cannot spawn the initial shell: {err}");
            self.quitting = true;
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
        // The mark decorates only what the titlebar shows: `last_window_title`
        // stays the tab's own title, which `title_path` below parses as a cwd.
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
            "{title}{}",
            dev_mark()
        )));
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
        // Section *declaration* order fixes both the group order in the list
        // and each group's accent colour, so neither moves while filtering.
        const TABS: &str = "Tabs";
        const NAVIGATE: &str = "Navigate";
        const SETTINGS: &str = "Settings";
        const APPLICATION: &str = "Application";

        let mut actions = vec![
            PaletteAction::new("tab.new", "New Tab", Some("⌘T"))
                .in_section(TABS)
                .with_icon(PaletteIcon::Plus),
            PaletteAction::new("tab.close", "Close Tab", Some("⌘W"))
                .in_section(TABS)
                .with_icon(PaletteIcon::Cross),
            PaletteAction::new("tab.rename", "Rename Tab…", None)
                .in_section(TABS)
                .with_icon(PaletteIcon::Pencil),
            PaletteAction::new("tab.next", "Next Tab", Some("⇧⌘]"))
                .in_section(NAVIGATE)
                .with_icon(PaletteIcon::ArrowRight),
            PaletteAction::new("tab.prev", "Previous Tab", Some("⇧⌘["))
                .in_section(NAVIGATE)
                .with_icon(PaletteIcon::ArrowLeft),
        ];
        if let Some(tabs) = self.tabs.as_ref().map(|t| lock(t)) {
            for id in tabs.ids() {
                let title = tabs.title(id).unwrap_or("shell");
                actions.push(
                    PaletteAction::new(
                        format!("tab.select.{id}"),
                        format!("Go to Tab: {title}"),
                        None,
                    )
                    .in_section(NAVIGATE)
                    .with_icon(PaletteIcon::Terminal),
                );
            }
        }
        // The palette has no checkbox, so the label carries the state — the
        // list is rebuilt every time it opens, so it can never go stale.
        // One guard, not two: `lock(t).active_id().and_then(|id| lock(t)…)`
        // deadlocks, because the first temporary guard lives to the end of
        // the enclosing expression and this is all one thread.
        let bidi = self
            .tabs
            .as_ref()
            .and_then(|t| {
                let tabs = lock(t);
                tabs.active_id().and_then(|id| tabs.bidi(id))
            })
            .flatten()
            .unwrap_or(self.config.get().text.bidi);
        actions.push(
            PaletteAction::new(
                "config.toggle_bidi",
                format!("RTL Reordering (this tab): {} — cycle", bidi.name()),
                Some("⇧⌘B"),
            )
            .in_section(SETTINGS)
            .with_icon(PaletteIcon::Dot),
        );
        actions.push(
            PaletteAction::new(
                "config.cycle_bidi_base",
                format!(
                    "RTL Paragraph Direction: {} — cycle",
                    match self.config.get().text.bidi_base {
                        egui_term::BidiBase::Ltr => "left-to-right",
                        egui_term::BidiBase::Auto => "auto",
                        egui_term::BidiBase::Rtl => "right-to-left",
                    }
                ),
                None,
            )
            .in_section(SETTINGS)
            .with_icon(PaletteIcon::ArrowLeft),
        );
        actions.push(
            PaletteAction::new("config.font_bigger", "Increase Font Size", Some("⌘+"))
                .in_section(SETTINGS)
                .with_icon(PaletteIcon::Plus),
        );
        actions.push(
            PaletteAction::new("config.font_smaller", "Decrease Font Size", Some("⌘-"))
                .in_section(SETTINGS)
                .with_icon(PaletteIcon::ArrowLeft),
        );
        actions.push(
            PaletteAction::new("config.reset_session", "Reset Settings", Some("⌘0"))
                .in_section(SETTINGS)
                .with_icon(PaletteIcon::Cross),
        );
        actions.push(
            PaletteAction::new("config.reload", "Reload Config File", None)
                .in_section(SETTINGS)
                .with_icon(PaletteIcon::ArrowRight),
        );
        if !self.config.warnings().is_empty() {
            actions.push(
                PaletteAction::new(
                    "config.warnings",
                    format!(
                        "Config: {} problem(s) — show in log",
                        self.config.warnings().len()
                    ),
                    None,
                )
                .in_section(SETTINGS)
                .with_icon(PaletteIcon::Cross),
            );
        }
        actions.push(
            PaletteAction::new("app.quit", "Quit terra", Some("⌘Q"))
                .in_section(APPLICATION)
                .with_icon(PaletteIcon::Power),
        );
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
                    "config.toggle_bidi" => actions.push(AppAction::ToggleBidi),
                    "config.cycle_bidi_base" => actions.push(AppAction::CycleBidiBase),
                    "config.font_bigger" => actions.push(AppAction::NudgeFontSize(1)),
                    "config.font_smaller" => actions.push(AppAction::NudgeFontSize(-1)),
                    "config.reset_session" => actions.push(AppAction::ResetSession),
                    "config.reload" => actions.push(AppAction::ReloadConfig),
                    "config.warnings" => actions.push(AppAction::ShowConfigWarnings),
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

    /// Settings actions. Separate from [`Self::apply`] because they touch the
    /// config rather than the tabs, and so must work before the first shell
    /// has spawned.
    fn apply_config(&mut self, action: AppAction) {
        match action {
            AppAction::ToggleBidi => {
                // Cycles the *tab*, not the app: one window routinely has a
                // shell in one tab and an agent that does its own BiDi in
                // another, and they need opposite settings.
                use config::BidiMode::{Auto, Off, On};
                let Some(arc) = self.tabs.clone() else { return };
                let mut tabs = lock(&arc);
                let Some(id) = tabs.active_id() else { return };
                let current = tabs
                    .bidi(id)
                    .flatten()
                    .unwrap_or(self.config.get().text.bidi);
                let next = match current {
                    Off => On,
                    On => Auto,
                    Auto => Off,
                };
                tabs.set_bidi(id, Some(next));
                log::info!("terra: tab {id} RTL reordering {}", next.name());
            }
            AppAction::CycleBidiBase => {
                use egui_term::BidiBase::{Auto, Ltr, Rtl};
                let next = match self.config.get().text.bidi_base {
                    Auto => Ltr,
                    Ltr => Rtl,
                    Rtl => Auto,
                };
                self.config.apply(config::SessionEdit::BidiBase(Some(next)));
                log::info!("terra: RTL paragraph direction {next:?}");
            }
            AppAction::NudgeFontSize(delta) => {
                // Clamping lives in `config::resolve`, so repeatedly hitting
                // the key at either end parks rather than drifting.
                let next = self.config.get().font.size + f32::from(delta);
                self.config.apply(config::SessionEdit::FontSize(Some(next)));
            }
            AppAction::ResetSession => {
                self.config.clear_session();
                log::info!("terra: settings reset to {}", self.config.path().display());
            }
            AppAction::ReloadConfig => {
                self.config.reload();
                log::info!("terra: reloaded {}", self.config.path().display());
            }
            AppAction::ShowConfigWarnings => {
                for warning in self.config.warnings() {
                    log::warn!("terra: config: {warning}");
                }
            }
            other => log::warn!("terra: {other:?} is not a config action"),
        }
    }

    /// Whether the active tab should reorder right-to-left text this frame.
    ///
    /// Precedence: the tab's own override (palette, `terra bidi`) beats the
    /// config, and `auto` consults the quirks table for whatever program is
    /// running in the tab. Nothing inspects the *text* — logical and visual
    /// order are the same bytes, so the choice has to be declared.
    fn active_bidi(&mut self, ctx: &egui::Context) -> bool {
        let Some(arc) = self.tabs.clone() else {
            return false;
        };
        let (active, shell_pid) = {
            let tabs = lock(&arc);
            let Some(active) = tabs.active_id() else {
                return false;
            };
            (tabs.bidi(active).flatten(), tabs.shell_pid(active))
        };

        let Some(mode) = active else {
            // No per-tab override: the config decides, and only `auto` needs
            // to know what is running.
            if self.config.get().text.bidi != config::BidiMode::Auto {
                return config::should_reorder(self.config.get(), None);
            }
            let now = ctx.input(|i| i.time);
            if now - self.foreground_checked >= FOREGROUND_POLL_SECS {
                self.foreground_checked = now;
                self.foreground = shell_pid.and_then(procinfo::foreground_command);
            }
            let command = self.foreground.clone();
            return config::should_reorder(self.config.get(), command.as_deref());
        };
        // Only `auto` needs to know what is running, so the syscall is
        // skipped entirely in the default configuration.
        if mode != config::BidiMode::Auto {
            return config::should_reorder_mode(mode, &self.config.get().text.quirks, None);
        }

        let now = ctx.input(|i| i.time);
        if now - self.foreground_checked >= FOREGROUND_POLL_SECS {
            self.foreground_checked = now;
            self.foreground = shell_pid.and_then(procinfo::foreground_command);
        }
        config::should_reorder_mode(
            mode,
            &self.config.get().text.quirks,
            self.foreground.as_deref(),
        )
    }

    /// Rebuild anything derived from the config, but only when it moved.
    fn sync_config_cache(&mut self) {
        if self.cached_config_generation == self.config.generation() {
            return;
        }
        self.cached_font = terminal_font(&self.config.get().font);
        self.cached_config_generation = self.config.generation();
    }

    /// Every arm takes the lock for exactly as long as it needs it — never
    /// across a call that would want it again (`palette_actions`).
    fn apply(&mut self, action: AppAction) {
        // Config actions touch no tab, so they must not be gated on one
        // existing — otherwise they would silently no-op before the first
        // shell has spawned.
        match action {
            AppAction::ToggleBidi
            | AppAction::CycleBidiBase
            | AppAction::NudgeFontSize(_)
            | AppAction::ResetSession
            | AppAction::ReloadConfig
            | AppAction::ShowConfigWarnings => return self.apply_config(action),
            _ => {}
        }
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
            AppAction::ToggleBidi
            | AppAction::CycleBidiBase
            | AppAction::NudgeFontSize(_)
            | AppAction::ResetSession
            | AppAction::ReloadConfig
            | AppAction::ShowConfigWarnings => unreachable!("handled above"),
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

        // Before anything else this frame: the framebuffer readback for a
        // `terra screenshot` arrives as an input event, and a client thread is
        // blocked on it. While one is outstanding, keep painting — the capture
        // lands a frame or two after the one it was asked for, and an idle
        // terra would park in between and never produce it.
        self.screenshots.deliver(&ctx);
        if self.screenshots.pending() {
            ctx.request_repaint();
        }

        self.drain_pty_events();
        self.sync_window_title(&ctx, frame);
        self.sync_config_cache();

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

        // Re-read after the actions above, so a toggle applied this frame is
        // the one this frame paints with.
        self.sync_config_cache();
        let palette_open = self.palette.is_open();
        let bidi = self.active_bidi(&ctx);
        let bidi_base = self.config.get().text.bidi_base;
        let font = self.cached_font.clone();
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
                        .set_font(font)
                        .set_bidi(bidi)
                        .set_bidi_base(bidi_base)
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

#[cfg(test)]
mod tests {
    use super::dev_suffix;

    /// The daily driver — default socket, no override — is unmarked.
    #[test]
    fn the_installed_build_keeps_a_plain_title() {
        assert_eq!(dev_suffix(None, None), "");
        assert_eq!(dev_suffix(None, Some("  ")), "");
    }

    /// `just run` sets only TERRA_SOCKET, and that alone must mark the window.
    #[test]
    fn a_relocated_socket_marks_the_window() {
        assert_eq!(
            dev_suffix(None, Some("/home/ada/.terra/terra-dev.sock")),
            " (dev)"
        );
    }

    #[test]
    fn terra_dev_marks_the_window_on_the_default_socket() {
        assert_eq!(dev_suffix(Some("1"), None), " (dev)");
    }

    /// …and switches the mark off for a second *installed* build that merely
    /// happens to use its own socket.
    #[test]
    fn terra_dev_can_suppress_the_mark() {
        for off in ["0", "false", "no", "off", "", " "] {
            assert_eq!(
                dev_suffix(Some(off), Some("/tmp/other.sock")),
                "",
                "{off:?}"
            );
        }
    }
}
