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
mod confirm_close;
mod edit_tools;
mod fonts;
mod ghostty_theme;
mod ipc;
mod macos;
mod procinfo;
mod screenshot;
mod scrollbar;
mod tab_icon;
mod tabs;
mod transcript;
mod ui;

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};

use egui_term::{PtyEvent, TerminalView};
use terra_palette::{Palette, PaletteAction, PaletteEvent, PaletteIcon};

use crate::edit_tools::EditTool;
use crate::ipc::IpcServer;
use crate::screenshot::Screenshots;
use crate::scrollbar::ScrollbarState;
use crate::tabs::TabManager;
use crate::ui::AppAction;

const RENAME_PROMPT_ID: &str = "rename";

/// Width of the hairline between two sibling nodes of the split tree, on
/// either axis. Its drag hit-area
/// (`Id::new(("terra_group_separator", split path, boundary))`) is what a
/// resize drag hangs off — see [`GROUP_SEPARATOR_GRIP`].
const GROUP_SEPARATOR_WIDTH: f32 = 1.0;
/// How far either side of the hairline still grabs it: a 1px line is no drag
/// target, so the hit-area is widened invisibly, VS Code style.
const GROUP_SEPARATOR_GRIP: f32 = 3.0;
/// Same tone as the tab bar's underline, so the seams read as one system.
const GROUP_SEPARATOR_COLOR: egui::Color32 = egui::Color32::from_rgb(0x2a, 0x2a, 0x2e);
/// No group can be resized below this fraction of the window — a column
/// narrower than this is unusable, and collapsing-by-drag would be too easy.
const MIN_GROUP_FRACTION: f32 = 0.15;

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
/// Open the config file in whatever the OS considers its editor — the
/// Windows Terminal "Settings" gesture, where settings are a file you edit
/// rather than a UI. A missing file is seeded first with the documented
/// example (`docs/config.example.toml`, every key commented and guaranteed
/// warning-free by the config tests), so a first-timer lands in working
/// docs instead of an empty buffer.
fn open_config_in_editor(path: &std::path::Path) {
    if !ensure_config_file(path) {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        // `open` honours the user's `.toml` association (VS Code, Zed, …).
        // It exits non-zero when nothing claims the extension, so wait for
        // the status — it returns in milliseconds — and fall back to the
        // default text editor rather than silently doing nothing.
        let opened = std::process::Command::new("open")
            .arg(path)
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !opened {
            if let Err(err) = std::process::Command::new("open")
                .arg("-t")
                .arg(path)
                .spawn()
            {
                log::warn!("terra: cannot open {}: {err}", path.display());
            }
        }
    }
    #[cfg(target_os = "windows")]
    if let Err(err) = std::process::Command::new("cmd")
        .args(["/C", "start", ""])
        .arg(path)
        .spawn()
    {
        log::warn!("terra: cannot open {}: {err}", path.display());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Err(err) = std::process::Command::new("xdg-open").arg(path).spawn() {
        log::warn!("terra: cannot open {}: {err}", path.display());
    }
}

/// Make sure there is a file at `path` before handing it to anything, seeding
/// a missing one with the documented example. Returns whether there is now a
/// file to open — the one failure (an unwritable `~/.terra`) is logged here.
///
/// Split out of [`open_config_in_editor`] because every "edit the settings"
/// route needs it: an agent asked to open a file that does not exist starts by
/// arguing with the user about it.
fn ensure_config_file(path: &std::path::Path) -> bool {
    if path.exists() {
        return true;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let example = include_str!("../../../docs/config.example.toml");
    if let Err(err) = std::fs::write(path, example) {
        log::warn!("terra: cannot create {}: {err}", path.display());
        return false;
    }
    log::info!(
        "terra: created {} from the documented example",
        path.display()
    );
    true
}

fn lock(tabs: &Mutex<TabManager>) -> MutexGuard<'_, TabManager> {
    tabs.lock().unwrap_or_else(|err| err.into_inner())
}

/// Whether closing the window right now would take a running program down
/// with it — the world half of the "Close Window?" decision, with the
/// judgement itself left to [`confirm_close::should_confirm`].
///
/// One process-table snapshot answers for every tab at once (see
/// [`procinfo::foreground_commands`]), and the switch is checked before the
/// snapshot so `confirm_close = false` reads nothing at all.
fn close_would_kill_work(enabled: bool, tabs: Option<&Arc<Mutex<TabManager>>>) -> bool {
    if !enabled {
        return false;
    }
    let Some(arc) = tabs else {
        return false;
    };
    let pids: Vec<u32> = {
        let tabs = lock(arc);
        tabs.ids()
            .iter()
            .filter_map(|id| tabs.shell_pid(*id))
            .collect()
    };
    if pids.is_empty() {
        return false;
    }
    let foreground = procinfo::foreground_commands(&pids);
    let names: Vec<Option<&str>> = foreground
        .iter()
        .map(|fg| fg.as_ref().map(|fg| fg.name.as_str()))
        .collect();
    confirm_close::should_confirm(enabled, &names)
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

/// Whether `TERRA_NO_ACTIVATE` asks this launch to start *behind* whatever the
/// user is doing.
///
/// Same dev-instance story as [`dev_mark`]: a second terra is opened
/// constantly while working on terra — by `just restart`, by an agent
/// verifying a change — and macOS activates a launching app, which yanks focus
/// out of the editor or terminal the user was typing in. Setting this makes a
/// launch quiet: the window opens, takes no focus, and is still fully
/// drivable over its socket (`terra ls`, `terra screenshot`).
///
/// Only the *activation* is suppressed. The activation policy stays `Regular`,
/// so terra keeps its Dock tile, its ⌘-Tab entry and — the reason this is not
/// `Accessory` — its menu bar: an accessory app owns no menu bar at all, which
/// would silently delete the application menu (see `macos::install_app_menu`).
///
/// No-op off macOS, where nothing steals focus on launch.
fn no_activate() -> bool {
    matches!(
        std::env::var("TERRA_NO_ACTIVATE").as_deref(),
        Ok("1" | "true" | "yes" | "on")
    )
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
    let mut native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([480.0, 320.0])
            .with_title(format!("Terra{}", dev_mark()))
            .with_icon(app_icon()),
        ..Default::default()
    };
    if no_activate() {
        native_options.event_loop_builder = Some(Box::new(|builder| {
            #[cfg(target_os = "macos")]
            {
                use winit::platform::macos::EventLoopBuilderExtMacOS;
                builder.with_activate_ignoring_other_apps(false);
            }
            #[cfg(not(target_os = "macos"))]
            let _ = builder;
        }));
    }
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
    /// One scrollbar per group column, keyed by group index — each column
    /// scrolls (and fades its thumb) independently.
    scrollbars: HashMap<usize, ScrollbarState>,
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
    /// One icon per tab for the tab bar, on its own slower clock — see
    /// [`tab_icon`].
    tab_icons: tab_icon::IconCache,
    quitting: bool,
    /// Picks the frame the window fades in on; runs exactly once. (A `terra
    /// select` summon is not an opening and never touches it.)
    opening: macos::OpenAnimation,
    /// Where the *closing* transition is: a close request is canceled, the
    /// window fades, and only then is the close let through. See
    /// `macos::CloseAnimation`.
    closing: macos::CloseAnimation,
    /// The "Close Window?" question, and whether one is outstanding. Sits
    /// *in front of* `closing`: a close is confirmed first and animated
    /// second, so a canceled close never fades anything.
    confirm_close: confirm_close::ConfirmClose,
    last_window_title: String,
    /// Directory currently behind the titlebar proxy icon, so we only bother
    /// AppKit when it actually moves.
    last_represented_path: Option<std::path::PathBuf>,
    /// Whether the macOS application menu has been built yet. It cannot be
    /// built at launch: half of it is the list of installed agents/editors,
    /// which `edit_tools` is still probing for on a background thread.
    app_menu_installed: bool,
}

/// Tag of the plain "Settings…" row in the application menu. The
/// "Edit Settings With ▸" rows are tagged by their index in
/// [`EditTool::ALL`], so this sits clear of them.
const MENU_TAG_SETTINGS: isize = 1000;

/// The menu tag naming `tool`: its position in [`EditTool::ALL`], which is a
/// compile-time constant and so cannot drift between the two ends.
fn tool_tag(tool: EditTool) -> isize {
    EditTool::ALL
        .iter()
        .position(|t| *t == tool)
        .expect("every tool is in ALL") as isize
}

/// The "Edit Settings With ▸" rows for a probe result.
///
/// Split out of [`App::sync_app_menu`] so the menu's *contents* are testable
/// without AppKit. They have to be: a native menu bar cannot be screenshotted
/// — the OS only renders the frontmost app's, and terra must never steal
/// focus to be looked at (see `TERRA_NO_ACTIVATE`).
fn edit_with_specs(found: &[edit_tools::Found]) -> Vec<macos::MenuSpec> {
    found
        .iter()
        .map(|f| macos::MenuSpec {
            tag: tool_tag(f.tool),
            title: f.tool.label().to_owned(),
            key: "",
        })
        .collect()
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Before anything is drawn: hide the window so the first frame fades
        // in rather than snapping on. This is the earliest AppKit is reachable
        // — later than this and there is a flash to see.
        macos::prime_open(cc);
        fonts::install(&cc.egui_ctx);
        // A shell start and a couple of LaunchServices lookups; off the main
        // thread so the first frame does not wait on them.
        edit_tools::prime();
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
            scrollbars: HashMap::new(),
            cached_config_generation: config.generation(),
            cached_font,
            foreground: None,
            foreground_checked: f64::NEG_INFINITY,
            tab_icons: tab_icon::IconCache::default(),
            config,
            quitting: false,
            opening: macos::OpenAnimation::default(),
            closing: macos::CloseAnimation::default(),
            confirm_close: confirm_close::ConfirmClose::default(),
            last_window_title: String::new(),
            last_represented_path: None,
            app_menu_installed: false,
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
        lock(&tabs).set_profiles(self.config.get().profiles.clone());
        lock(&tabs).set_transcript_bytes(self.config.get().tabs.transcript_bytes());
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
            .unwrap_or_else(|| "Terra".to_string());
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

    fn palette_actions(&self, ctx: &egui::Context) -> Vec<PaletteAction> {
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
            PaletteAction::new("split.right", "Split Tab Right", Some("⌘\\"))
                .in_section(TABS)
                .with_icon(PaletteIcon::ArrowRight),
            PaletteAction::new("split.left", "Split Tab Left", None)
                .in_section(TABS)
                .with_icon(PaletteIcon::ArrowLeft),
            PaletteAction::new("split.down", "Split Tab Down", None)
                .in_section(TABS)
                .with_icon(PaletteIcon::Dot),
            PaletteAction::new("split.up", "Split Tab Up", None)
                .in_section(TABS)
                .with_icon(PaletteIcon::Dot),
            PaletteAction::new("tab.next", "Next Tab", Some("⇧⌘]"))
                .in_section(NAVIGATE)
                .with_icon(PaletteIcon::ArrowRight),
            PaletteAction::new("tab.prev", "Previous Tab", Some("⇧⌘["))
                .in_section(NAVIGATE)
                .with_icon(PaletteIcon::ArrowLeft),
            PaletteAction::new("group.next", "Focus Next Group", Some("⌥⌘→"))
                .in_section(NAVIGATE)
                .with_icon(PaletteIcon::ArrowRight),
            PaletteAction::new("group.prev", "Focus Previous Group", Some("⌥⌘←"))
                .in_section(NAVIGATE)
                .with_icon(PaletteIcon::ArrowLeft),
        ];
        if let Some(tabs) = self.tabs.as_ref().map(|t| lock(t)) {
            // One entry per profile, alphabetical (the table is a BTreeMap) —
            // the same list every group's ⌄ menu offers. Opening one lands in
            // the focused group, like every other way of opening a tab.
            for name in tabs.profiles().keys() {
                actions.push(
                    PaletteAction::new(format!("tab.new.{name}"), format!("New Tab: {name}"), None)
                        .in_section(TABS)
                        .with_icon(PaletteIcon::Plus),
                );
            }
            // Every tab across every group, in visual order. With a single
            // group the label is just the title; with more, the group ordinal
            // prefixes it ("2: htop") so twins in different columns tell apart.
            let many = tabs.group_count() > 1;
            for group in 0..tabs.group_count() {
                for id in tabs.group_tabs(group) {
                    let title = tabs.title(id).unwrap_or("shell");
                    let label = if many {
                        format!("Go to Tab: {}: {title}", group + 1)
                    } else {
                        format!("Go to Tab: {title}")
                    };
                    actions.push(
                        PaletteAction::new(format!("tab.select.{id}"), label, None)
                            .in_section(NAVIGATE)
                            .with_icon(PaletteIcon::Terminal),
                    );
                }
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
            PaletteAction::new("config.open", "Open Config File", Some("⌘,"))
                .in_section(SETTINGS)
                // The gear that used to head the chevron menu's Settings row,
                // now heading the palette's — see `edit_tools`. It is chrome,
                // not a brand, so it takes the section's accent.
                .with_icon(
                    tab_icon::texture_id(ctx, tab_icon::TabIcon::Gear)
                        .map_or(PaletteIcon::Pencil, PaletteIcon::Mask),
                ),
        );
        actions.push(
            PaletteAction::new("config.reload", "Reload Config File", None)
                .in_section(SETTINGS)
                .with_icon(PaletteIcon::ArrowRight),
        );
        // One row per tool actually installed (see `edit_tools`), each wearing
        // its own brand mark rather than a stroked glyph — the same mark the
        // tab an agent row opens will wear.
        for found in edit_tools::detected() {
            let tool = found.tool;
            let icon = tab_icon::texture_id(ctx, tool.icon())
                .map_or(PaletteIcon::Pencil, PaletteIcon::Image);
            actions.push(
                PaletteAction::new(
                    format!("config.edit.{}", tool.slug()),
                    format!("Config: Edit with {}", tool.label()),
                    None,
                )
                .in_section(SETTINGS)
                .with_icon(icon),
            );
        }
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
                    "split.right" => actions.push(AppAction::SplitRight),
                    "split.left" => actions.push(AppAction::SplitLeft),
                    "split.down" => actions.push(AppAction::SplitDown),
                    "split.up" => actions.push(AppAction::SplitUp),
                    "group.next" => actions.push(AppAction::NextGroup),
                    "group.prev" => actions.push(AppAction::PrevGroup),
                    "config.toggle_bidi" => actions.push(AppAction::ToggleBidi),
                    "config.cycle_bidi_base" => actions.push(AppAction::CycleBidiBase),
                    "config.font_bigger" => actions.push(AppAction::NudgeFontSize(1)),
                    "config.font_smaller" => actions.push(AppAction::NudgeFontSize(-1)),
                    "config.reset_session" => actions.push(AppAction::ResetSession),
                    "config.reload" => actions.push(AppAction::ReloadConfig),
                    "config.open" => actions.push(AppAction::OpenConfig),
                    "config.warnings" => actions.push(AppAction::ShowConfigWarnings),
                    edit if edit.starts_with("config.edit.") => {
                        match EditTool::from_slug(&edit["config.edit.".len()..]) {
                            Some(tool) => actions.push(AppAction::EditConfigWith(tool)),
                            None => log::warn!("terra: unknown edit tool {edit}"),
                        }
                    }
                    "app.quit" => actions.push(AppAction::Quit),
                    // `tab.new` (exact) is handled above; `tab.new.<name>` is
                    // one profile, as `tab.select.<id>` is one tab.
                    other => match other.strip_prefix("tab.new.") {
                        Some(name) => {
                            actions.push(AppAction::NewTabProfile(name.to_owned()));
                        }
                        None => match other.strip_prefix("tab.select.") {
                            Some(id) => match id.parse::<u64>() {
                                Ok(id) => actions.push(AppAction::SelectTab(id)),
                                Err(_) => log::warn!("terra: bad palette action {other}"),
                            },
                            None => log::warn!("terra: unknown palette action {other}"),
                        },
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
                // The tab manager keeps its own copy (see `tabs.rs`), so the
                // chevron menu and `terra new --profile` must be handed the
                // reloaded one or they would answer from the old file forever.
                if let Some(arc) = self.tabs.clone() {
                    lock(&arc).set_profiles(self.config.get().profiles.clone());
                    // Sizes the *next* tab's ring; open tabs keep the one they
                    // were created with rather than losing what they recorded.
                    lock(&arc).set_transcript_bytes(self.config.get().tabs.transcript_bytes());
                }
                log::info!("terra: reloaded {}", self.config.path().display());
            }
            AppAction::ShowConfigWarnings => {
                for warning in self.config.warnings() {
                    log::warn!("terra: config: {warning}");
                }
            }
            AppAction::OpenConfig => open_config_in_editor(self.config.path()),
            AppAction::EditConfigWith(tool) => self.edit_config_with(tool),
            other => log::warn!("terra: {other:?} is not a config action"),
        }
    }

    /// Build the macOS application menu, once, on the first frame after the
    /// tool probe lands. Before that there is nothing to put in the submenu,
    /// and a menu built empty would stay empty for the session.
    fn sync_app_menu(&mut self) {
        if self.app_menu_installed {
            return;
        }
        let Some(found) = edit_tools::ready() else {
            return;
        };
        let settings = macos::MenuSpec {
            tag: MENU_TAG_SETTINGS,
            title: "Settings…".to_owned(),
            key: ",",
        };
        let edit_with = edit_with_specs(found);
        macos::install_app_menu(&format!("Terra{}", dev_mark()), &[settings], &edit_with);
        self.app_menu_installed = true;
    }

    /// Turn menu choices made since the last frame into actions.
    fn drain_menu_actions(&self, actions: &mut Vec<AppAction>) {
        for tag in macos::take_menu_actions() {
            match tag {
                MENU_TAG_SETTINGS => actions.push(AppAction::OpenConfig),
                macos::QUIT_TAG => actions.push(AppAction::Quit),
                tag => match EditTool::ALL.get(tag.unsigned_abs()) {
                    Some(tool) => actions.push(AppAction::EditConfigWith(*tool)),
                    None => log::warn!("terra: unknown menu tag {tag}"),
                },
            }
        }
    }

    /// Hand the config file to one detected tool.
    ///
    /// An *agent* gets a terra tab of its own, opened the ordinary way
    /// (`TabManager::open` types the command into a login shell), so it
    /// inherits exactly the environment a user-opened tab would — which is the
    /// only reason `claude` is runnable at all from a Finder-launched app. It
    /// is handed one positional argument, [`edit_tools::EDIT_PROMPT`], and
    /// starts its first turn on it. An *editor* is simply given the file and
    /// no tab.
    fn edit_config_with(&mut self, tool: EditTool) {
        let path = self.config.path().to_path_buf();
        if !ensure_config_file(&path) {
            return;
        }
        if !tool.is_agent() {
            return edit_tools::open_file_with(tool, &path);
        }
        let Some(arc) = self.tabs.clone() else { return };
        let command = vec![tool.cli().to_owned(), edit_tools::edit_prompt(&path)];
        // The title says what the tab is for; the icon comes from the process
        // table a moment later and agrees with it (see `tab_icon`).
        let title = format!("config \u{b7} {}", tool.cli());
        let opened = lock(&arc).open(&command, None, Some(title));
        if let Err(err) = opened {
            log::error!("terra: cannot open a {} tab: {err}", tool.label());
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

    /// Refresh the tab bar's per-tab icons.
    ///
    /// Done here rather than inside `ui::tab_bar` because it is a syscall on a
    /// clock, and paint routines should not be the thing deciding when to talk
    /// to the kernel. One call resolves every tab from a single process-table
    /// snapshot (see [`procinfo::foreground_commands`]), so the cost does not
    /// grow with the number of tabs.
    ///
    /// With `[tabs] icons = false` the cache is emptied and nothing is polled
    /// at all — the switch buys back the syscall, not just the pixels.
    fn sync_tab_icons(&mut self, ctx: &egui::Context, tabs: &TabManager) {
        if !self.config.get().tabs.icons {
            self.tab_icons.clear();
            return;
        }
        // The fallback text is the title and the spawn command together, so a
        // tab opened as `terra new -- htop` is recognisable before its shell
        // has even echoed the command.
        let rows: Vec<(u64, Option<u32>, String)> = tabs
            .ids()
            .iter()
            .map(|id| {
                let title = tabs.title(*id).unwrap_or_default();
                let spawn = tabs.spawn(*id).unwrap_or_default();
                (*id, tabs.shell_pid(*id), format!("{title} {spawn}"))
            })
            .collect();
        let facts: Vec<tab_icon::TabFacts<'_>> = rows
            .iter()
            .map(|(id, shell_pid, text)| tab_icon::TabFacts {
                id: *id,
                shell_pid: *shell_pid,
                text,
            })
            .collect();
        let now = ctx.input(|i| i.time);
        self.tab_icons
            .poll(now, &facts, procinfo::foreground_commands);
    }

    /// A close request arrived: hold it back behind the "Close Window?"
    /// dialog, or let it through?
    ///
    /// The process table is walked here rather than read off `tab_icons`,
    /// which is a *cache* on a one-second clock and is empty outright when
    /// `[tabs] icons = false`. A close happens once; one `sysctl` at the
    /// moment it does is both cheap and the only way to be current.
    fn ask_before_closing(&mut self) -> bool {
        // `requested` calls this at most once per question — never while a
        // dialog is already up, and never after the user has approved.
        let enabled = self.config.get().window.confirm_close;
        let tabs = self.tabs.clone();
        self.confirm_close
            .requested(|| close_would_kill_work(enabled, tabs.as_ref()))
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
    fn apply(&mut self, ctx: &egui::Context, action: AppAction) {
        // Config actions touch no tab, so they must not be gated on one
        // existing — otherwise they would silently no-op before the first
        // shell has spawned.
        match action {
            AppAction::ToggleBidi
            | AppAction::CycleBidiBase
            | AppAction::NudgeFontSize(_)
            | AppAction::ResetSession
            | AppAction::ReloadConfig
            | AppAction::OpenConfig
            | AppAction::EditConfigWith(_)
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
            AppAction::NewTabProfile(name) => {
                if let Err(err) = lock(&arc).open_profile(&name) {
                    // Covers both "the profile went away under a reload" and a
                    // shell that would not spawn; either way the log names it.
                    log::error!("terra: cannot open profile {name:?}: {err}");
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
            AppAction::FocusGroup(idx) => {
                lock(&arc).focus_group(idx);
            }
            AppAction::SplitRight
            | AppAction::SplitLeft
            | AppAction::SplitDown
            | AppAction::SplitUp => {
                // Split the *globally* active tab — the focused group's. A
                // lone tab in its group has nothing to split from and the
                // model refuses; silently, as VS Code does.
                let mut tabs = lock(&arc);
                if let Some(id) = tabs.active_id() {
                    match action {
                        AppAction::SplitRight => tabs.split_right(id),
                        AppAction::SplitLeft => tabs.split_left(id),
                        AppAction::SplitDown => tabs.split_down(id),
                        _ => tabs.split_up(id),
                    };
                }
            }
            AppAction::NextGroup => lock(&arc).next_group(),
            AppAction::PrevGroup => lock(&arc).prev_group(),
            AppAction::MoveTab { id, group, index } => {
                // A drop on another group's bar. Focus follows the tab, as it
                // does in VS Code — `select` also makes it the global active.
                let mut tabs = lock(&arc);
                if tabs.move_tab(id, group, index) {
                    tabs.select(id);
                }
            }
            AppAction::SplitTab { id, group, dir } => {
                // A drop on a terminal half. On the tab's own group this is a
                // plain split (the model refuses it for a lone tab); on a
                // foreign group the tab first moves in, which guarantees the
                // group has the two tabs a split needs. After the move the
                // tab is addressed by id, so the DFS indices shifting under a
                // collapsed source group cannot misroute the split.
                let mut tabs = lock(&arc);
                if tabs.group_of(id) != Some(group) && !tabs.move_tab(id, group, usize::MAX) {
                    return;
                }
                match dir {
                    ui::SplitDir::Right => tabs.split_right(id),
                    ui::SplitDir::Left => tabs.split_left(id),
                    ui::SplitDir::Down => tabs.split_down(id),
                    ui::SplitDir::Up => tabs.split_up(id),
                };
            }
            AppAction::OpenPalette => {
                let actions = self.palette_actions(ctx);
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
            | AppAction::OpenConfig
            | AppAction::EditConfigWith(_)
            | AppAction::ShowConfigWarnings => unreachable!("handled above"),
            // The tabs are *not* torn down here: the window fades out first,
            // and an empty window is not what should be fading. Closing them
            // is the last thing the close path does, once the fade is over.
            AppAction::Quit => self.quitting = true,
        }
    }
}

/// Everything one frame's split-tree walk reads but does not mutate.
struct RenderEnv {
    /// The command palette or the close-confirmation dialog is up, so the
    /// terminal must not hold focus (see `TerminalView::set_focus`).
    modal_open: bool,
    bidi: bool,
    bidi_base: egui_term::BidiBase,
    font: egui_term::TerminalFont,
    /// DFS index of the focused group, read once — the walk itself never
    /// changes focus.
    focused_group: usize,
    /// `[tabs] bar_with_one_tab`, read once a frame like the rest of this
    /// struct, so a config reload lands on the very next frame.
    bar_with_one_tab: bool,
}

/// The recursive renderer for one frame: the split tree becomes nested rects
/// (rows within columns within rows…), each leaf its tab bar plus the active
/// tab's `TerminalView`, with a draggable separator between siblings on both
/// axes. Leaves are visited in DFS order, so `geoms[i]` is group `i`'s
/// geometry — what the cross-group drag overlay routes drops with.
struct TreeFrame<'a> {
    env: RenderEnv,
    tabs: &'a mut TabManager,
    /// App-level, filled once a frame by [`App::sync_tab_icons`]: one process
    /// snapshot answers every group's bar.
    icons: &'a tab_icon::IconCache,
    scrollbars: &'a mut HashMap<usize, ScrollbarState>,
    geoms: Vec<ui::GroupGeometry>,
    actions: &'a mut Vec<AppAction>,
}

impl TreeFrame<'_> {
    fn node(
        &mut self,
        ui: &mut egui::Ui,
        node: &tabs::LayoutNode,
        path: &mut Vec<usize>,
        rect: egui::Rect,
    ) {
        let (axis, weights, children) = match node {
            tabs::LayoutNode::Leaf(group) => return self.leaf(ui, *group, rect),
            tabs::LayoutNode::Split {
                axis,
                weights,
                children,
            } => (*axis, weights, children),
        };
        let count = children.len();
        let horizontal = axis == tabs::Axis::Horizontal;
        let extent = if horizontal {
            rect.width()
        } else {
            rect.height()
        };
        let usable = extent - GROUP_SEPARATOR_WIDTH * (count as f32 - 1.0);
        let mut cursor = if horizontal { rect.left() } else { rect.top() };
        for (i, child) in children.iter().enumerate() {
            // The last child takes exactly what is left, so rounding never
            // opens a gap at the far edge.
            let end = if i + 1 == count {
                if horizontal {
                    rect.right()
                } else {
                    rect.bottom()
                }
            } else {
                cursor + (usable * weights[i]).max(0.0)
            };
            let child_rect = if horizontal {
                egui::Rect::from_min_max(
                    egui::pos2(cursor, rect.top()),
                    egui::pos2(end, rect.bottom()),
                )
            } else {
                egui::Rect::from_min_max(
                    egui::pos2(rect.left(), cursor),
                    egui::pos2(rect.right(), end),
                )
            };
            path.push(i);
            self.node(ui, child, path, child_rect);
            path.pop();
            cursor = end;

            // Thin separator between two siblings, draggable to resize them.
            // Registered after the subtree's terminals, so its (invisibly
            // widened) grip wins the hit test.
            if i + 1 < count {
                let sep = if horizontal {
                    egui::Rect::from_min_max(
                        egui::pos2(cursor, rect.top()),
                        egui::pos2(cursor + GROUP_SEPARATOR_WIDTH, rect.bottom()),
                    )
                } else {
                    egui::Rect::from_min_max(
                        egui::pos2(rect.left(), cursor),
                        egui::pos2(rect.right(), cursor + GROUP_SEPARATOR_WIDTH),
                    )
                };
                let grip = if horizontal {
                    sep.expand2(egui::vec2(GROUP_SEPARATOR_GRIP, 0.0))
                } else {
                    sep.expand2(egui::vec2(0.0, GROUP_SEPARATOR_GRIP))
                };
                let icon = if horizontal {
                    egui::CursorIcon::ResizeHorizontal
                } else {
                    egui::CursorIcon::ResizeVertical
                };
                let response = ui
                    .interact(
                        grip,
                        egui::Id::new(("terra_group_separator", path.clone(), i)),
                        egui::Sense::drag(),
                    )
                    .on_hover_cursor(icon);
                if response.dragged() {
                    ui.ctx().set_cursor_icon(icon);
                    let raw = if horizontal {
                        response.drag_delta().x
                    } else {
                        response.drag_delta().y
                    };
                    let delta = raw / usable.max(1.0);
                    let mut next = self.tabs.split_weights(path);
                    if next.len() == count {
                        // The drag trades extent between the two neighbours
                        // only, and neither may go below the floor. A child
                        // already under it (splits can make one) can only
                        // grow, never shrink further.
                        let lo = (MIN_GROUP_FRACTION - next[i]).min(0.0);
                        let hi = (next[i + 1] - MIN_GROUP_FRACTION).max(0.0);
                        let delta = delta.clamp(lo, hi);
                        if delta != 0.0 {
                            next[i] += delta;
                            next[i + 1] -= delta;
                            self.tabs.set_split_weights(path, &next);
                        }
                    }
                }
                ui.painter().rect_filled(sep, 0.0, GROUP_SEPARATOR_COLOR);
                cursor = if horizontal {
                    sep.right()
                } else {
                    sep.bottom()
                };
            }
        }
    }

    /// One group: its tab bar across the top of `column`, the active tab's
    /// terminal below, and this leaf's geometry pushed for the drag overlay.
    fn leaf(&mut self, ui: &mut egui::Ui, group: usize, column: egui::Rect) {
        debug_assert_eq!(self.geoms.len(), group, "leaves arrive in DFS order");
        let focused = group == self.env.focused_group;

        // Clicking anywhere in the leaf — bar or grid — focuses its group.
        // Read, not consumed: the click still reaches whatever it landed on.
        let pressed_here = !self.env.modal_open
            && ui.input(|i| {
                i.pointer.primary_pressed()
                    && i.pointer.interact_pos().is_some_and(|p| column.contains(p))
            });
        if pressed_here && !focused {
            self.actions.push(AppAction::FocusGroup(group));
        }

        // Salted by the leaf's stable id, not its DFS index: a split
        // renumbers every group after it, and any egui state hanging off
        // this ui (scroll fades, terminal view state) would jump to a
        // neighbour's column.
        let leaf = self.tabs.group_leaf_id(group).unwrap_or(u64::MAX);
        let mut col_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(column)
                .id_salt(("terra_group_column", leaf)),
        );
        col_ui.set_clip_rect(column);
        ui::tab_bar(
            &mut col_ui,
            self.tabs,
            group,
            focused,
            self.icons,
            self.env.bar_with_one_tab,
            self.actions,
        );

        // The group's active terminal fills the leaf below its bar, inset by
        // the familiar margins.
        let area = col_ui.available_rect_before_wrap();
        self.geoms.push(ui::GroupGeometry {
            bar: if ui::bar_visible(
                self.tabs.group_tabs(group).len(),
                self.tabs.group_count(),
                self.env.bar_with_one_tab,
            ) {
                egui::Rect::from_min_size(
                    column.min,
                    egui::vec2(column.width(), ui::TAB_BAR_HEIGHT),
                )
            } else {
                egui::Rect::NOTHING
            },
            terminal: area,
        });
        let grid = egui::Rect::from_min_max(
            egui::pos2(area.left() + 10.0, area.top() + 8.0),
            egui::pos2(area.right() - 4.0, area.bottom() - 4.0),
        );
        let active = self.tabs.group_active(group);
        if let Some(tab) = active.and_then(|id| self.tabs.get_mut(id)) {
            if grid.width() > 1.0 && grid.height() > 1.0 {
                let mut term_ui = col_ui.new_child(egui::UiBuilder::new().max_rect(grid));
                term_ui.set_clip_rect(column);
                // Only the focused group's view takes the keyboard; the
                // palette beats them all.
                let view = TerminalView::new(&mut term_ui, &mut tab.backend)
                    .set_focus(!self.env.modal_open && focused)
                    .set_theme(terminal_theme())
                    .set_font(self.env.font.clone())
                    .set_bidi(self.env.bidi)
                    .set_bidi_base(self.env.bidi_base)
                    .set_size(grid.size());
                let rect = term_ui.add(view).rect;
                // After the terminal, so the thumb wins the hit test.
                scrollbar::show(
                    &mut term_ui,
                    rect,
                    &mut tab.backend,
                    self.scrollbars.entry(group).or_default(),
                );
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.ensure_started(&ctx);

        let now = ctx.input(|i| i.time);
        // There is content behind the alpha now, so fade it up — on the frame
        // `OpenAnimation` picks, which is never this first one.
        match self.opening.step(now, macos::window_visible(frame)) {
            macos::OpenStep::Wait => ctx.request_repaint(),
            macos::OpenStep::Animate => macos::animate_open(frame),
            macos::OpenStep::GiveUp => macos::show_now(frame),
            macos::OpenStep::Done => {}
        }

        // Both ways out of terra land on `close_requested`: the red traffic
        // light raises it directly, and ⌘Q — the app menu's Quit row, the
        // palette's `app.quit` — sets `self.quitting`, which sends
        // `ViewportCommand::Close` at the end of the frame and raises it on
        // the next one. So one interception here covers both.
        //
        // It sits *in front of* the fade: a close the user then cancels must
        // not have animated anything, and the fade's own re-issued request
        // must not be mistaken for a fresh one (`ConfirmClose` remembers the
        // answer instead of re-asking).
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        let held = close_requested && self.ask_before_closing();
        if held {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            // A pending Quit would otherwise re-issue the close every frame,
            // leaving the dialog answering a question that keeps coming back.
            self.quitting = false;
            ctx.request_repaint();
        }
        let step = if close_requested && !held {
            self.closing.requested(now, || macos::animate_close(frame))
        } else {
            self.closing.tick(now)
        };
        match step {
            macos::CloseStep::Close => {
                if let Some(arc) = self.tabs.clone() {
                    lock(&arc).clear();
                }
                self.ipc = None;
                return;
            }
            // Hold the window open and keep painting it: what fades has to be
            // the terminal, not an empty rectangle.
            macos::CloseStep::Fade => {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.request_repaint();
            }
            // The fade is spent — ask again, and this time nothing cancels it.
            macos::CloseStep::Confirm => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            macos::CloseStep::Idle => {}
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

        self.sync_app_menu();

        // Before the shortcut table reads the keyboard and before the terminal
        // is composed: while the dialog is up, Return and Escape are answers
        // to it and must reach nothing else.
        if self.confirm_close.is_open() {
            if let Some(choice) = confirm_close::show(&ctx) {
                self.confirm_close.answer(choice);
                if choice == confirm_close::Choice::Close {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }

        let mut actions: Vec<AppAction> = Vec::new();
        // The application menu dispatches between frames, so its choices are
        // collected first and applied with everything else this frame.
        self.drain_menu_actions(&mut actions);
        // Global shortcuts are consumed before the terminal widget reads events.
        if !self.palette.is_open() && !self.confirm_close.is_open() {
            actions.extend(ui::consume_shortcuts(ui));
        }
        self.handle_palette(&ctx, &mut actions);
        for action in std::mem::take(&mut actions) {
            self.apply(&ctx, action);
        }

        // One process-table snapshot for the whole window: every group's bar
        // reads the same cache, so the cost is per frame, not per group.
        if let Some(arc) = self.tabs.clone() {
            let tabs = lock(&arc);
            self.sync_tab_icons(&ctx, &tabs);
        }

        // Re-read after the actions above, so a toggle applied this frame is
        // the one this frame paints with.
        self.sync_config_cache();
        // Either modal takes the keyboard away from the terminal.
        let modal_open = self.palette.is_open() || self.confirm_close.is_open();
        let bidi = self.active_bidi(&ctx);
        let bidi_base = self.config.get().text.bidi_base;
        let bar_with_one_tab = self.config.get().tabs.bar_with_one_tab;
        let font = self.cached_font.clone();
        let tabs_arc = self.tabs.clone();
        let icons = &self.tab_icons;
        let scrollbars = &mut self.scrollbars;
        let panel_actions = &mut actions;
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::from_rgb(0x1e, 0x1e, 0x1e)))
            .show(ui, |ui| {
                let Some(arc) = tabs_arc else {
                    return;
                };
                // One guard for the whole window render: `TerminalView` wants
                // a `&mut Tab` that lives inside the manager, and nothing in
                // here reaches for the lock a second time.
                let mut tabs = lock(&arc);
                let Some(root) = tabs.layout() else {
                    return;
                };
                let full = ui.available_rect_before_wrap();
                let mut frame = TreeFrame {
                    env: RenderEnv {
                        modal_open,
                        bidi,
                        bidi_base,
                        font: font.clone(),
                        focused_group: tabs.focused_group(),
                        bar_with_one_tab,
                    },
                    tabs: &mut tabs,
                    icons,
                    scrollbars: &mut *scrollbars,
                    geoms: Vec::new(),
                    actions: &mut *panel_actions,
                };
                frame.node(ui, &root, &mut Vec::new(), full);
                let geoms = frame.geoms;

                // The cross-group half of a tab drag: floating ghost, drop
                // zones, and the drop itself (as actions applied below).
                ui::tab_drag_overlay(ui, &tabs, icons, &geoms, panel_actions);
            });
        for action in std::mem::take(&mut actions) {
            self.apply(&ctx, action);
        }

        // Last tab gone (or Quit chosen) -> the app is done. Not while the
        // window is already fading out, though: re-asking every frame would
        // count as the user insisting, and cut the animation short.
        let empty = self.tabs.as_ref().is_some_and(|tabs| lock(tabs).is_empty());
        // The last tab exiting while the dialog is up answers the question:
        // there is nothing left to protect, so the window must not be stuck
        // behind a modal about sessions that no longer exist.
        if empty && self.confirm_close.is_open() {
            self.confirm_close.answer(confirm_close::Choice::Close);
        }
        if (self.quitting || empty) && !self.closing.is_fading() {
            self.ipc = None;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::dev_suffix;
    use super::*;

    /// Every menu tag the application menu can hand back maps to exactly one
    /// action, and the three families never collide: Settings is 1000, Quit is
    /// negative, and the tools are small indices.
    #[test]
    fn every_menu_tag_names_one_thing() {
        let mut seen = vec![MENU_TAG_SETTINGS, macos::QUIT_TAG];
        for tool in EditTool::ALL {
            let tag = tool_tag(*tool);
            assert!(!seen.contains(&tag), "tag {tag} is used twice");
            assert!(EditTool::ALL.get(tag.unsigned_abs()) == Some(tool));
            seen.push(tag);
        }
    }

    /// The submenu is the probe's answer, one row per installed tool, in
    /// declaration order and labelled the way the palette labels it. Stands in
    /// for a screenshot: a native menu bar only renders for the frontmost app.
    #[test]
    fn the_edit_settings_with_submenu_is_one_row_per_detected_tool() {
        let found: Vec<edit_tools::Found> = [EditTool::ClaudeCode, EditTool::Cursor]
            .into_iter()
            .map(|tool| edit_tools::Found { tool, cli: None })
            .collect();
        let specs = edit_with_specs(&found);
        assert_eq!(
            specs.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(),
            ["Claude Code", "Cursor"]
        );
        // No key equivalents: these rows are discovery, not muscle memory, and
        // every ⌘-something in a menu is a key the terminal stops receiving.
        assert!(specs.iter().all(|s| s.key.is_empty()));
        // Each row's tag round-trips to the tool it names.
        for (spec, f) in specs.iter().zip(&found) {
            assert_eq!(EditTool::ALL.get(spec.tag.unsigned_abs()), Some(&f.tool));
        }
        // Nothing detected, nothing offered — and `install_app_menu` then
        // leaves the submenu out entirely.
        assert!(edit_with_specs(&[]).is_empty());
    }

    /// `TERRA_NO_ACTIVATE` is opt-in and only for the spellings a human or a
    /// recipe would actually write; anything else keeps today's behaviour.
    #[test]
    fn only_a_truthy_no_activate_suppresses_activation() {
        let saved = std::env::var("TERRA_NO_ACTIVATE").ok();
        for (value, expected) in [
            ("1", true),
            ("true", true),
            ("yes", true),
            ("on", true),
            ("0", false),
            ("false", false),
            ("", false),
        ] {
            // SAFETY: single-threaded test, and the variable is restored below.
            unsafe { std::env::set_var("TERRA_NO_ACTIVATE", value) };
            assert_eq!(no_activate(), expected, "TERRA_NO_ACTIVATE={value:?}");
        }
        unsafe { std::env::remove_var("TERRA_NO_ACTIVATE") };
        assert!(!no_activate());
        if let Some(saved) = saved {
            unsafe { std::env::set_var("TERRA_NO_ACTIVATE", saved) };
        }
    }

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
