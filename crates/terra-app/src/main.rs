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

/// The window terra opens with, and the one eframe owns
/// ([`egui::ViewportId::ROOT`]). Every other window is a deferred viewport
/// created by tearing a tab out — see [`App::show_extra_windows`].
const ROOT_WINDOW: u64 = 0;

/// Size a torn-out window opens at. How far its top-left is nudged from the
/// pointer (so the pointer lands on the tab's pill rather than beside it) is
/// the drag's business: [`ui::tear_anchor`].
const NEW_WINDOW_SIZE: [f32; 2] = [900.0, 600.0];
/// Fallback cascade when there is no pointer to open next to (a torn-out
/// window from the palette, from IPC): each window steps down-right from the
/// root's corner, macOS style.
const NEW_WINDOW_CASCADE: egui::Vec2 = egui::vec2(36.0, 36.0);

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

/// The same question for a *tab* close: would closing tab `id` take a running
/// program down with it?
///
/// The window's other doors (the red traffic light, ⌘Q, the Apple event) all
/// land on `close_requested` and share [`close_would_kill_work`]. A tab close
/// does not: it goes through `TabManager::close`, and an emptied window quits
/// on its own, so every tab close is a separate door onto the same decision.
/// One pid is read here rather than the whole table — only that tab is going
/// away, whether or not the window goes with it.
fn tab_close_would_kill_work(
    enabled: bool,
    tabs: Option<&Arc<Mutex<TabManager>>>,
    id: u64,
) -> bool {
    if !enabled {
        return false;
    }
    let Some(arc) = tabs else {
        return false;
    };
    let Some(pid) = lock(arc).shell_pid(id) else {
        return false;
    };
    let foreground = procinfo::foreground_commands(&[pid]);
    let name = foreground
        .first()
        .and_then(|fg| fg.as_ref())
        .map(|fg| fg.name.as_str());
    confirm_close::should_confirm_tab_close(enabled, name)
}

/// `window.confirm_close`, mirrored for the AppKit termination hook.
///
/// `applicationShouldTerminate:` runs on the main thread *between* frames,
/// where `App` — and so the `ConfigStore` inside it — is not reachable. The
/// switch is one `bool` and it moves only when the config file does, so a
/// mirror is enough; `App::sync_config_cache` keeps it current.
static CONFIRM_CLOSE_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(config::DEFAULT_WINDOW_CONFIRM_CLOSE);

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

/// The same icon, decoded once and shared. A torn-out window rebuilds its
/// [`egui::ViewportBuilder`] every frame (that is how a deferred viewport
/// stays alive), and decoding a PNG per frame per window is not free.
fn shared_app_icon() -> Arc<egui::IconData> {
    static ICON: std::sync::OnceLock<Arc<egui::IconData>> = std::sync::OnceLock::new();
    Arc::clone(ICON.get_or_init(|| Arc::new(app_icon())))
}

/// The viewport a terra window is drawn in. The root window is eframe's own
/// ([`egui::ViewportId::ROOT`]); every other one is derived from its window id,
/// so the same window keeps the same viewport across frames — which is what
/// tells egui to keep the OS window rather than open a second one.
fn viewport_id(win: u64) -> egui::ViewportId {
    if win == ROOT_WINDOW {
        return egui::ViewportId::ROOT;
    }
    egui::ViewportId::from_hash_of(("terra_window", win))
}

/// Everything the window renderer needs that is *not* the tabs, shared with
/// the deferred viewport callbacks.
///
/// Those callbacks are `Fn(&mut Ui, ViewportClass) + Send + Sync + 'static`, so
/// they cannot borrow `App` — they capture `Arc`s instead: one for the tabs
/// (already shared with the IPC threads) and this one for the rest. It is
/// republished from the root frame, and read by every window's render.
///
/// Lock order is **shared, then tabs**, everywhere. The IPC threads only ever
/// take the tab lock, so they cannot be part of a cycle.
struct WindowShared {
    /// This frame's read-only render inputs, refreshed by the root frame.
    env: RenderEnv,
    /// One process-table snapshot for every window's bars (see
    /// [`App::sync_tab_icons`]).
    icons: tab_icon::IconCache,
    /// One scrollbar per pane, keyed by `(window, group)` — group indices are
    /// per window, so the window has to be part of the key or two windows'
    /// second columns would share one thumb.
    scrollbars: HashMap<(u64, usize), ScrollbarState>,
    /// What the windows raised this frame, each tagged with the window it was
    /// raised in: a tab bar's ＋, a click on a pill, a middle-click close. The
    /// root frame drains and applies them (with that window focused, so the
    /// group indices inside them mean what they meant when they were raised).
    actions: Vec<(u64, AppAction)>,
    /// Extra windows whose OS close box was hit, drained by the root frame.
    closed: Vec<u64>,
    /// Where every window's tab bars are on the desktop: one screen rect per
    /// group, recorded by that window's own render (only it is told its own
    /// geometry). A tab drag that has left its window needs all of them at
    /// once — it attaches the moment the pointer enters one, and no viewport
    /// can see another's geometry by itself.
    bars: HashMap<u64, Vec<(egui::Rect, usize)>>,
    /// The same for every window's terminal areas, in that window's group (DFS)
    /// order: a torn drag released over one of them splits that group, and the
    /// window it belongs to has to be able to say which group the pointer is
    /// in without seeing the pointer.
    terms: HashMap<u64, Vec<egui::Rect>>,
    /// The tab currently being carried between windows, as the window driving
    /// the drag published it on its last frame. Every *other* window reads it
    /// to paint the half of itself a drop would split into — the pointer is
    /// reported to the dragging viewport alone, so this is how the news
    /// travels.
    carry: Option<ui::CarryState>,
}

/// [`lock`] for the shared render state. Same poison policy, same reason.
fn lock_shared(shared: &Mutex<WindowShared>) -> MutexGuard<'_, WindowShared> {
    shared.lock().unwrap_or_else(|err| err.into_inner())
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
    /// Render state every window shares — scrollbars, tab icons, this frame's
    /// settings, and the actions the windows raise. Behind an `Arc<Mutex<…>>`
    /// because a torn-out window draws from a deferred viewport callback,
    /// which owns its captures (see [`WindowShared`]).
    shared: Arc<Mutex<WindowShared>>,
    /// Where a torn-out window opened, kept so its
    /// [`egui::ViewportBuilder`] — rebuilt every frame — does not yank the
    /// window back to the pointer on the next one.
    window_spawn: HashMap<u64, egui::Pos2>,
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
    /// Picks the frame the window fades in on; runs exactly once. (A `terra
    /// select` summon is not an opening and never touches it.)
    opening: macos::OpenAnimation,
    /// Where the *closing* transition is: a close request is canceled, the
    /// window fades, and only then is the close let through. See
    /// `macos::CloseAnimation`.
    closing: macos::CloseAnimation,
    /// The "Close Window?" / "Close Tab?" question, whether one is outstanding
    /// and what it is about (`confirm_close::Subject` — the held close's
    /// payload lives in there, so there is exactly one pending close). Sits
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
        // `sync_config_cache` only fires when the generation moves, so the
        // first value has to be published here.
        CONFIRM_CLOSE_ENABLED.store(
            config.get().window.confirm_close,
            std::sync::atomic::Ordering::Relaxed,
        );
        let shared = Arc::new(Mutex::new(WindowShared {
            env: RenderEnv {
                modal_open: false,
                bidi: false,
                bidi_base: config.get().text.bidi_base,
                font: cached_font.clone(),
                focused_group: 0,
                keyboard: true,
                bar_with_one_tab: config.get().tabs.bar_with_one_tab,
                focus_follows_mouse: config.get().input.focus_follows_mouse,
            },
            icons: tab_icon::IconCache::default(),
            scrollbars: HashMap::new(),
            actions: Vec::new(),
            closed: Vec::new(),
            bars: HashMap::new(),
            terms: HashMap::new(),
            carry: None,
        }));
        Self {
            pty_events,
            pty_sender,
            tabs: None,
            palette: Palette::default(),
            ipc: None,
            screenshots: Arc::default(),
            shared,
            window_spawn: HashMap::new(),
            cached_config_generation: config.generation(),
            cached_font,
            foreground: None,
            foreground_checked: f64::NEG_INFINITY,
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

        // Now that there are tabs to protect, let a `terminate:` be held back
        // too — see `macos::install_terminate_hook`. The guard runs between
        // frames, so it may take the tab lock but must not want a frame; the
        // same `close_would_kill_work` the in-window close path uses fits
        // exactly.
        {
            let guard_tabs = Arc::clone(&tabs);
            macos::install_terminate_hook(ctx, move || {
                close_would_kill_work(
                    CONFIRM_CLOSE_ENABLED.load(std::sync::atomic::Ordering::Relaxed),
                    Some(&guard_tabs),
                )
            });
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
    ///
    /// The *root* window's active tab, which is the globally active one only
    /// while the keyboard is in this window: a tab torn out into a window of
    /// its own must not go on retitling the window it left (each torn-out
    /// window titles itself — see [`App::show_extra_windows`]).
    fn sync_window_title(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let title = self
            .tabs
            .as_ref()
            .and_then(|arc| {
                let tabs = lock(arc);
                let mut here = tabs
                    .infos()
                    .into_iter()
                    .filter(|i| tabs.window_of_tab(i.id) == Some(ROOT_WINDOW))
                    .peekable();
                let first = here.peek().map(|i| i.title.clone());
                here.find(|i| i.active).map(|i| i.title).or(first)
            })
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
    fn drain_pty_events(&mut self, ctx: &egui::Context) {
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
                // OSC 52: the program asked for text to go on the system
                // clipboard — how tmux (`set -g set-clipboard on`) and vim
                // hand a copy back out, over ssh included, where there is no
                // other route from the far end to this Mac's pasteboard.
                // Both pasteboards a program can name (`c` and `p`/`s`) are
                // the one macOS pasteboard, which is also what Ghostty does.
                // The *read* direction is refused inside egui_term.
                PtyEvent::ClipboardStore(_, text) => ctx.copy_text(text),
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
            PaletteAction::new("tab.move-to-new-window", "Move Tab to New Window", None)
                .in_section(TABS)
                .with_icon(PaletteIcon::ArrowRight),
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
                    // The palette route acts on the active tab; the drag route
                    // names the pill that was let go outside the window.
                    "tab.move-to-new-window" => {
                        if let Some(id) = self.tabs.as_ref().and_then(|t| lock(t).active_id()) {
                            actions.push(AppAction::MoveTabToNewWindow { id, pos: None });
                        }
                    }
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
        // A quit that arrived as an Apple event — Dock ▸ Quit, `osascript …
        // quit`, logout — held back by `macos::install_terminate_hook`. It
        // becomes an ordinary [`AppAction::Quit`] here, so the confirmation
        // dialog, the fade and the teardown are the same code every other quit
        // runs.
        if macos::take_quit_request() {
            actions.push(AppAction::Quit);
        }
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
    ///
    /// The cache lives in [`WindowShared`] rather than in `App`: a torn-out
    /// window's bar is drawn from a viewport callback that cannot reach `App`
    /// at all, and every bar in every window must read the same snapshot.
    fn sync_tab_icons(
        enabled: bool,
        ctx: &egui::Context,
        tabs: &TabManager,
        cache: &mut tab_icon::IconCache,
    ) {
        if !enabled {
            cache.clear();
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
        cache.poll(now, &facts, procinfo::foreground_commands);
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
        // The root viewport's close *is* the app's (eframe ends the run loop
        // with it), so this subject always quits — torn-out windows go down
        // with it, which is why the work check below walks every window's
        // tabs, not just the root's.
        self.confirm_close.requested(
            confirm_close::Subject::window(confirm_close::ROOT_WINDOW, true),
            || close_would_kill_work(enabled, tabs.as_ref()),
        )
    }

    /// Close a tab — every in-window door does, so the question is asked in
    /// exactly one place.
    ///
    /// The doors: the tab's ✕ and a middle-click on it (`AppAction::CloseTab`),
    /// ⌘W and the palette's "Close Tab" (`AppAction::CloseActive`). Not the
    /// IPC `Request::Kill` behind `terra kill` — see `ipc::handle`.
    fn close_tab(&mut self, ctx: &egui::Context, arc: &Arc<Mutex<TabManager>>, id: u64) {
        if self.hold_tab_close(id) {
            // The tab bar drew this frame already; the dialog goes up on the
            // next one, which a parked terra would otherwise never paint.
            ctx.request_repaint();
            return;
        }
        lock(arc).close(id);
    }

    /// Is there work in this tab? Then hold its close behind the dialog.
    ///
    /// The last tab's close is the same code path wearing a different title:
    /// it empties the window, so what it really shuts is the window, and
    /// `Subject::Tab { last: true }` says "Close Window?" for it.
    ///
    /// Returns `true` when the caller must *not* close the tab. The answer
    /// arrives in `App::ui`, which runs the close then.
    fn hold_tab_close(&mut self, id: u64) -> bool {
        let enabled = self.config.get().window.confirm_close;
        let tabs = self.tabs.clone();
        // Every group's tabs in the tab's *window*, not the focused group's:
        // that window is empty only when the very last one goes — and only
        // emptying the last window quits terra, which is what turns the
        // question from "Close Window?" into one whose approval ends the app.
        let (win, last_in_window, last_window) = tabs
            .as_ref()
            .map(|arc| {
                let t = lock(arc);
                let win = t.window_of_tab(id).unwrap_or(confirm_close::ROOT_WINDOW);
                let in_window: Vec<u64> = t
                    .ids()
                    .into_iter()
                    .filter(|i| t.window_of_tab(*i) == Some(win))
                    .collect();
                (win, in_window.as_slice() == [id], t.window_ids().len() == 1)
            })
            .unwrap_or((confirm_close::ROOT_WINDOW, false, true));
        // A dialog already up owns the pending payload: a tab close arriving
        // mid-question is held (like any second request) but must not rewrite
        // what "Close" will mean. `ConfirmClose` enforces that itself.
        self.confirm_close
            .tab_requested(id, win, last_in_window, last_window, || {
                tab_close_would_kill_work(enabled, tabs.as_ref(), id)
            })
    }

    /// Show every torn-out window as a deferred viewport.
    ///
    /// Deferred (not immediate) because the two windows must be able to
    /// repaint independently: a `top` in a torn-out window animating at 1Hz
    /// must not drag the root window's frame rate along with it, and a modal
    /// in the root must not freeze the other window's output. The price is the
    /// callback's `Send + Sync + 'static`, which is why the render state is
    /// shared through `Arc`s rather than borrowed off `App`.
    ///
    /// This runs every frame: a deferred viewport lives exactly as long as its
    /// parent keeps showing it, so a window that leaves `window_ids` — its
    /// last tab closed, its tabs dragged back — closes itself here.
    fn show_extra_windows(&mut self, ctx: &egui::Context) {
        let Some(arc) = self.tabs.clone() else {
            return;
        };
        let windows: Vec<u64> = {
            let tabs = lock(&arc);
            tabs.window_ids()
                .into_iter()
                .filter(|win| *win != ROOT_WINDOW)
                .collect()
        };
        // Positions are minted once, when the window is torn out; anything
        // else (a window restored by the model, one this frame is seeing for
        // the first time) cascades off the root window's corner.
        for win in &windows {
            if !self.window_spawn.contains_key(win) {
                let pos = self.cascade_position(ctx);
                self.window_spawn.insert(*win, pos);
            }
        }
        self.window_spawn.retain(|win, _| windows.contains(win));
        // Same for the geometry the drag hit-tests against: a dead window must
        // not keep catching drops. The root window is never in `windows` (it
        // is not a torn-out one) and always stays.
        {
            let mut shared = lock_shared(&self.shared);
            shared
                .bars
                .retain(|win, _| *win == ROOT_WINDOW || windows.contains(win));
            shared
                .terms
                .retain(|win, _| *win == ROOT_WINDOW || windows.contains(win));
        }

        for win in windows {
            let title = self.window_title(&arc, win);
            let builder = egui::ViewportBuilder::default()
                .with_title(format!("{title}{}", dev_mark()))
                .with_inner_size(NEW_WINDOW_SIZE)
                .with_min_inner_size([480.0, 320.0])
                .with_icon(shared_app_icon())
                .with_position(self.window_spawn[&win]);
            let tabs_cb = Arc::clone(&arc);
            let shared_cb = Arc::clone(&self.shared);
            ctx.show_viewport_deferred(viewport_id(win), builder, move |ui, _class| {
                let ctx = ui.ctx().clone();
                if ctx.input(|i| i.viewport().close_requested()) {
                    // TODO(confirm-close per window): the root window's red
                    // traffic light asks "Close Window?" first; a torn-out
                    // window's closes its tabs outright. The question is
                    // asked from `App::ui`, which is the root viewport's
                    // frame and cannot put a dialog in front of *this*
                    // window — another agent owns that half.
                    lock_shared(&shared_cb).closed.push(win);
                    // The root frame is the one that acts on it, and a parked
                    // terra would otherwise never paint again.
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
                render_window(ui, win, &tabs_cb, &shared_cb);
            });
            // PTY output wakes the *root* viewport (the backends call
            // `Context::request_repaint` from their reader threads, which off
            // the UI thread means the root). So every root frame pulls the
            // other windows along with it: without this a torn-out window
            // would only repaint on its own input.
            ctx.request_repaint_of(viewport_id(win));
        }
    }

    /// The title bar of window `win`: its first tab's, which for a torn-out
    /// tab is the tab that was torn out.
    fn window_title(&self, arc: &Arc<Mutex<TabManager>>, win: u64) -> String {
        let tabs = lock(arc);
        tabs.ids()
            .into_iter()
            .find(|id| tabs.window_of_tab(*id) == Some(win))
            .and_then(|id| tabs.title(id))
            .unwrap_or("Terra")
            .to_string()
    }

    /// Where a window with nothing better to go on opens: stepped down-right
    /// from the root window, one step per window already open.
    fn cascade_position(&self, ctx: &egui::Context) -> egui::Pos2 {
        let root = ctx
            .input(|i| i.viewport().outer_rect)
            .map(|rect| rect.min)
            .unwrap_or(egui::pos2(120.0, 120.0));
        root + NEW_WINDOW_CASCADE * (self.window_spawn.len() as f32 + 1.0)
    }

    /// Where a *torn-out* window opens for the routes that have no drag to ask
    /// (the palette, IPC): under the pointer, anchored exactly as a dragged
    /// tear is ([`ui::tear_anchor`]) — the pointer on the first pill of the new
    /// bar — so the two arrive at the same place.
    ///
    /// The pointer is in this viewport's points; the window's position is in
    /// the desktop's, so it is measured from the root window's own corner.
    fn tear_out_position(&self, ctx: &egui::Context) -> egui::Pos2 {
        let (pointer, outer) = ctx.input(|i| (i.pointer.latest_pos(), i.viewport().outer_rect));
        match (pointer, outer) {
            // No drag, so no grab offset inside the pill: its left edge.
            (Some(pointer), Some(outer)) => {
                outer.min + pointer.to_vec2() - ui::tear_anchor(0.0, ui::decor_height(ctx))
            }
            _ => self.cascade_position(ctx),
        }
    }

    /// Close the torn-out windows whose close box was hit since the last
    /// frame, tabs and all.
    fn close_extra_windows(&mut self, ctx: &egui::Context) {
        let closed: Vec<u64> = std::mem::take(&mut lock_shared(&self.shared).closed);
        if closed.is_empty() {
            return;
        }
        let Some(arc) = self.tabs.clone() else {
            return;
        };
        let mut tabs = lock(&arc);
        for win in closed {
            // TODO(confirm-close per window): closed outright, without the
            // "Close Window?" question — see `show_extra_windows`.
            let doomed: Vec<u64> = tabs
                .ids()
                .into_iter()
                .filter(|id| tabs.window_of_tab(*id) == Some(win))
                .collect();
            for id in doomed {
                tabs.close(id);
            }
            // …and drop the (now empty) window itself. Anything it still holds
            // comes back here and goes the same way; `close` is idempotent, so
            // the two lists overlapping is harmless.
            let left = tabs.close_window(win);
            for id in left {
                tabs.close(id);
            }
        }
        drop(tabs);
        ctx.request_repaint();
    }

    /// Apply what the windows raised this frame, each with its own window
    /// focused: `FocusGroup(2)` means *that* window's third group.
    ///
    /// Focusing is not a loan here, unlike the render: an action was raised by
    /// a click or a key in that window, which is the same gesture that gives
    /// it the keyboard.
    fn apply_window_actions(&mut self, ctx: &egui::Context) {
        let raised: Vec<(u64, AppAction)> = std::mem::take(&mut lock_shared(&self.shared).actions);
        for (win, action) in raised {
            if let Some(arc) = self.tabs.clone() {
                lock(&arc).focus_window(win);
            }
            self.apply(ctx, action);
        }
    }

    /// Rebuild anything derived from the config, but only when it moved.
    fn sync_config_cache(&mut self) {
        if self.cached_config_generation == self.config.generation() {
            return;
        }
        self.cached_font = terminal_font(&self.config.get().font);
        CONFIRM_CLOSE_ENABLED.store(
            self.config.get().window.confirm_close,
            std::sync::atomic::Ordering::Relaxed,
        );
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
            AppAction::CloseActive => {
                let active = lock(&arc).active_id();
                if let Some(id) = active {
                    self.close_tab(ctx, &arc, id);
                }
            }
            AppAction::CloseTab(id) => self.close_tab(ctx, &arc, id),
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
            AppAction::MoveTabToNewWindow { id, pos } => {
                let win = lock(&arc).move_tab_to_new_window(id);
                let Some(win) = win else {
                    return;
                };
                // The window opens under the pointer that tore the tab out,
                // and takes the keyboard with it — the tab the user is
                // carrying is the tab they mean to type in. A drag names the
                // spot itself (it is mid-gesture, in a window that may not be
                // the root); the palette and IPC have no pointer to ask about
                // and fall back to this viewport's.
                let pos = pos.unwrap_or_else(|| self.tear_out_position(ctx));
                self.window_spawn.insert(win, pos);
                lock(&arc).focus_window(win);
                // `show_extra_windows` runs on the root frame, so the new
                // viewport appears on the next one.
                ctx.request_repaint();
            }
            AppAction::DockTab {
                id,
                host,
                win,
                group,
                index,
            } => {
                // A tab carried into another window's bar, mid-gesture. The
                // hold comes first: the move is about to empty `host`, and an
                // emptied window normally closes — taking with it the viewport
                // the OS is delivering this very drag to.
                {
                    let mut tabs = lock(&arc);
                    tabs.hold_window(host);
                    if !tabs.move_tab_to_window(id, win) {
                        return;
                    }
                    // Group indices are the focused window's, so the target
                    // takes focus before `group` and `index` name anything.
                    tabs.focus_window(win);
                    tabs.move_tab(id, group, index);
                    tabs.select(id);
                }
                // The window the tab is now in is the one the user is working
                // in, so it takes the keyboard — the mouse stays with `host`,
                // which is what keeps the drag running.
                ctx.send_viewport_cmd_to(viewport_id(win), egui::ViewportCommand::Focus);
                ctx.request_repaint();
            }
            AppAction::ReorderDocked {
                id,
                win,
                group,
                index,
            } => {
                let mut tabs = lock(&arc);
                tabs.focus_window(win);
                tabs.move_tab(id, group, index);
                drop(tabs);
                ctx.request_repaint();
            }
            AppAction::UndockTab { id, host } => {
                // Pulled back out of the bar: the tab goes home to the window
                // that has been held empty waiting for it.
                {
                    let mut tabs = lock(&arc);
                    if tabs.move_tab_to_window(id, host) {
                        tabs.select(id);
                    }
                }
                ctx.request_repaint();
            }
            AppAction::ReleaseDragHold { host } => {
                // The gesture is over: if the tab ended up elsewhere, `host` is
                // empty and goes now; if it came home, this changes nothing.
                lock(&arc).release_window(host);
                ctx.request_repaint();
            }
            AppAction::SplitTabInWindow {
                id,
                win,
                group,
                dir,
            } => {
                // The cross-window twin of `SplitTab`, and the order matters:
                // group indices are the *focused* window's, so the tab moves
                // house and the window takes focus before `group` names
                // anything. The move lands it in whichever group had focus
                // there, so it may still have to walk to the one the pointer
                // was over.
                {
                    let mut tabs = lock(&arc);
                    tabs.move_tab_to_window(id, win);
                    tabs.focus_window(win);
                    if tabs.group_of(id) != Some(group) && !tabs.move_tab(id, group, usize::MAX) {
                        return;
                    }
                    match dir {
                        ui::SplitDir::Right => tabs.split_right(id),
                        ui::SplitDir::Left => tabs.split_left(id),
                        ui::SplitDir::Down => tabs.split_down(id),
                        ui::SplitDir::Up => tabs.split_up(id),
                    };
                    tabs.select(id);
                }
                // The window the user dropped into is the one they are now
                // working in, so it takes the keyboard.
                ctx.send_viewport_cmd_to(viewport_id(win), egui::ViewportCommand::Focus);
                ctx.request_repaint();
            }
            AppAction::DragWindowTo { win, pos } => {
                // One frame of a torn drag. `viewport_id(ROOT_WINDOW)` is the
                // root viewport, so carrying the original window off works
                // through this same arm.
                ctx.send_viewport_cmd_to(
                    viewport_id(win),
                    egui::ViewportCommand::OuterPosition(pos),
                );
                // `show_extra_windows` rebuilds every torn window's builder
                // each frame, `with_position` included; leaving the remembered
                // spawn position behind would fight the drag.
                self.window_spawn.insert(win, pos);
                ctx.request_repaint_of(viewport_id(win));
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
#[derive(Clone)]
struct RenderEnv {
    /// The command palette or the close-confirmation dialog is up, so the
    /// terminal must not hold focus (see `TerminalView::set_focus`).
    modal_open: bool,
    bidi: bool,
    bidi_base: egui_term::BidiBase,
    font: egui_term::TerminalFont,
    /// DFS index of the focused group *within the window being drawn*, read
    /// once — the walk itself never changes focus.
    focused_group: usize,
    /// Whether this window is the one the keyboard belongs to
    /// (`TabManager::focused_window`). Only its focused leaf takes keystrokes;
    /// the other windows draw their focused pill exactly the same, so the
    /// window you left looks the way you left it.
    keyboard: bool,
    /// `[tabs] bar_with_one_tab`, read once a frame like the rest of this
    /// struct, so a config reload lands on the very next frame.
    bar_with_one_tab: bool,
    /// `[input] focus_follows_mouse`, read the same way — flipping it in the
    /// config file changes the very next frame's answer.
    focus_follows_mouse: bool,
}

/// Focus follows the mouse: does this frame's input hand the keyboard to the
/// pane whose terminal occupies `terminal`?
///
/// Three rules, and the first is the one that makes this safe:
///
/// * the trigger is a **`PointerMoved` event landing inside the rect**, never
///   "the pointer is resting there". egui raises `PointerMoved` only when the
///   OS says the pointer actually moved, so a stationary cursor keeps its
///   focus while panes are split, resized, closed or scrolled out from under
///   it — and a window opening beneath wherever the cursor happens to sit
///   focuses nothing. Only a move *into* a pane switches.
/// * **no pointer button may be down**. A drag that starts in one pane and
///   crosses into its neighbour is one gesture belonging to the pane it began
///   in — a selection must not be cut in half by the pane boundary, and a tab
///   pill dragged across the window must not focus every group it passes over.
/// * the rect is the pane's **terminal**, not its whole column, so travelling
///   over a tab bar (or the strip of window chrome above one) changes nothing.
///   The caller decides the rect; a modal open anywhere suppresses the whole
///   question, since the keyboard belongs to the modal either way.
fn hover_focus(
    enabled: bool,
    events: &[egui::Event],
    button_down: bool,
    terminal: egui::Rect,
) -> bool {
    if !enabled || button_down {
        return false;
    }
    events
        .iter()
        .any(|event| matches!(event, egui::Event::PointerMoved(pos) if terminal.contains(*pos)))
}

/// The recursive renderer for one frame: the split tree becomes nested rects
/// (rows within columns within rows…), each leaf its tab bar plus the active
/// tab's `TerminalView`, with a draggable separator between siblings on both
/// axes. Leaves are visited in DFS order, so `geoms[i]` is group `i`'s
/// geometry — what the cross-group drag overlay routes drops with.
struct TreeFrame<'a> {
    env: RenderEnv,
    /// The window this walk is drawing. Every `group` index below is that
    /// window's, and every egui id is salted with it.
    window: u64,
    tabs: &'a mut TabManager,
    /// App-level, filled once a frame by [`App::sync_tab_icons`]: one process
    /// snapshot answers every group's bar.
    icons: &'a tab_icon::IconCache,
    scrollbars: &'a mut HashMap<(u64, usize), ScrollbarState>,
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
                        egui::Id::new(("terra_group_separator", self.window, path.clone(), i)),
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

        // Salted by the leaf's stable id, not its DFS index: a split
        // renumbers every group after it, and any egui state hanging off
        // this ui (scroll fades, terminal view state) would jump to a
        // neighbour's column.
        let leaf = self.tabs.group_leaf_id(group).unwrap_or(u64::MAX);
        let mut col_ui = ui.new_child(egui::UiBuilder::new().max_rect(column).id_salt((
            "terra_group_column",
            self.window,
            leaf,
        )));
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

        // Focus follows the mouse: the pointer moving into this pane's
        // terminal focuses its group, no click required. `[input]
        // focus_follows_mouse = false` turns just this off — the wheel still
        // goes to the pane under the pointer either way — and leaves
        // click-to-focus as the only way in. Decided against
        // `area` — the whole region below the tab bar, scrollbar strip
        // included — so the answer does not change under the terminal's inner
        // margins, and grazing the scrollbar of the pane you are aiming at
        // never bounces the keyboard back. See [`hover_focus`] for the rules.
        let moved_here = !self.env.modal_open
            && col_ui.input(|i| {
                hover_focus(
                    self.env.focus_follows_mouse,
                    &i.events,
                    i.pointer.any_down(),
                    area,
                )
            });
        // `env.keyboard` gates both: a window that does not have OS focus must
        // not take the model's focus off the window the user is typing in.
        // macOS hands a background window the pointer's moves — so
        // focus-follows-mouse over a window you are not in would otherwise
        // move the keyboard to a window that cannot receive it. A *click*
        // there is fine: the OS focuses the window before the press arrives,
        // so `render_window` has already made this window the keyboard's.
        if (pressed_here || moved_here) && !focused && self.env.keyboard {
            self.actions.push(AppAction::FocusGroup(group));
        }

        let grid = egui::Rect::from_min_max(
            egui::pos2(area.left() + 10.0, area.top() + 8.0),
            egui::pos2(area.right() - 4.0, area.bottom() - 4.0),
        );
        let active = self.tabs.group_active(group);
        if let Some(tab) = active.and_then(|id| self.tabs.get_mut(id)) {
            if grid.width() > 1.0 && grid.height() > 1.0 {
                let mut term_ui = col_ui.new_child(egui::UiBuilder::new().max_rect(grid));
                term_ui.set_clip_rect(column);
                // The overlay scrollbar shares this layer with the terminal,
                // and egui only occludes across layers — so the terminal is
                // told which strip is not its to take presses in. `hit_area`
                // is the same helper the scrollbar senses with, applied to
                // the same rect it is about to be given below.
                //
                // `scrollbar::show` runs after the view is added (the thumb
                // has to win the hit test), so this can only ask what the
                // scrollbar owned on the *previous* frame — see
                // `ScrollbarState::interactive`. `None` while the thumb is
                // hidden, or the rightmost ~11px of every pane would stop
                // selecting text.
                let exclusion = self
                    .scrollbars
                    .get(&(self.window, group))
                    .is_some_and(scrollbar::ScrollbarState::interactive)
                    .then(|| scrollbar::hit_area(grid));
                // Only the focused group's view takes the keyboard; the
                // palette beats them all.
                let view = TerminalView::new(&mut term_ui, &mut tab.backend)
                    .set_pointer_exclusion(exclusion)
                    .set_focus(!self.env.modal_open && focused && self.env.keyboard)
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
                    self.scrollbars.entry((self.window, group)).or_default(),
                );
            }
        }
    }
}

/// Draw one terra window — its split tree, every leaf's tab bar and terminal,
/// and the cross-group drag overlay on top.
///
/// The root window and every torn-out window run this same function; the only
/// difference between them is which `ui` it is handed (eframe's, or a deferred
/// viewport's) and the window id. It is a free function rather than a method
/// for exactly that reason: the viewport callback has no `App` to call a
/// method on, only the two `Arc`s it captured.
///
/// Two things happen around the walk itself:
///
/// * **OS focus becomes model focus.** The window whose OS window has the
///   keyboard is the window `TabManager` calls focused, so typing lands in the
///   leaf the user is looking at rather than in the last window they clicked.
/// * **The model is focused on the window being drawn.** Every `group` index
///   in the tab API is the *focused* window's, so a background window is drawn
///   with focus lent to it and the real focus put straight back — inside one
///   lock, so nothing (an IPC thread included) can observe the loan.
fn render_window(
    ui: &mut egui::Ui,
    win: u64,
    tabs_arc: &Arc<Mutex<TabManager>>,
    shared_arc: &Arc<Mutex<WindowShared>>,
) {
    let mut shared_guard = lock_shared(shared_arc);
    let shared = &mut *shared_guard;
    let mut tabs = lock(tabs_arc);

    let os_focused = ui.ctx().input(|i| i.viewport().focused).unwrap_or(false);
    if os_focused && tabs.focused_window() != win {
        tabs.focus_window(win);
    }
    let keyboard_window = tabs.focused_window();
    if keyboard_window != win {
        tabs.focus_window(win);
    }

    let mut env = shared.env.clone();
    env.focused_group = tabs.focused_group();
    env.keyboard = keyboard_window == win;

    // Actions are collected per window: a ＋ or a pill click means a group in
    // *this* window, and the root frame re-focuses the window before applying
    // them so the index still names what it named here.
    let mut raised: Vec<AppAction> = Vec::new();
    // What this pass learns about a tab in flight, published below for the
    // windows it is being carried over.
    let mut report = ui::CarryReport::NotMine;
    // Where the *other* windows' tab bars and terminals are on the desktop, as
    // they stood at the end of their own last frame: the OS tells each viewport
    // its own rect and no other's, so the shared tables are built one window at
    // a time and read whole by whoever needs the desktop (today: a torn tab
    // drag, which docks on entering a bar and splits on a drop into a
    // terminal). This window's own geometry is only known once the columns
    // below have been laid out, so it is added there.
    let inner = ui.ctx().input(|i| i.viewport().inner_rect);
    let mut bars: Vec<ui::BarStrip> = shared
        .bars
        .iter()
        .filter(|(other, _)| **other != win)
        .flat_map(|(other, strips)| {
            strips
                .iter()
                .enumerate()
                .map(|(group, (rect, tabs))| ui::BarStrip {
                    win: *other,
                    group,
                    rect: *rect,
                    tabs: *tabs,
                })
        })
        .collect();
    let mut terms: Vec<(u64, usize, egui::Rect)> = shared
        .terms
        .iter()
        .filter(|(other, _)| **other != win)
        .flat_map(|(other, rects)| {
            rects
                .iter()
                .enumerate()
                .map(|(group, rect)| (*other, group, *rect))
        })
        .collect();
    let carry = shared.carry;
    let mut own_bars: Vec<(egui::Rect, usize)> = Vec::new();
    let mut own_terms: Vec<egui::Rect> = Vec::new();
    let icons = &shared.icons;
    let scrollbars = &mut shared.scrollbars;
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(egui::Color32::from_rgb(0x1e, 0x1e, 0x1e)))
        .show(ui, |ui| {
            // One guard for the whole window render: `TerminalView` wants a
            // `&mut Tab` that lives inside the manager, and nothing in here
            // reaches for the lock a second time.
            //
            // A window with no layout has no tabs — the tabless root, or a
            // window held alive while its only tab is docked in another one
            // ([`TabManager::hold_window`]). It draws nothing, but the pass
            // still runs to the end: a held window is usually the one pumping
            // the drag, and skipping the overlay would strand the gesture.
            let full = ui.available_rect_before_wrap();
            let geoms = match tabs.window_layout(win) {
                Some(root) => {
                    let mut frame = TreeFrame {
                        env,
                        window: win,
                        tabs: &mut tabs,
                        icons,
                        scrollbars,
                        geoms: Vec::new(),
                        actions: &mut raised,
                    };
                    frame.node(ui, &root, &mut Vec::new(), full);
                    frame.geoms
                }
                None => Vec::new(),
            };

            // This window's own bar strips and terminals, in screen points,
            // joining the other windows' from the shared tables. A hidden bar
            // still offers the band it would occupy, so a bare window can be
            // dropped into; the terminals keep their group order, which is what
            // a split action names.
            if let Some(inner) = inner {
                let offset = inner.min.to_vec2();
                own_bars.extend(geoms.iter().enumerate().map(|(group, geom)| {
                    (
                        ui::attach_strip(geom).translate(offset),
                        tabs.group_tabs(group).len(),
                    )
                }));
                own_terms.extend(geoms.iter().map(|geom| geom.terminal.translate(offset)));
                bars.extend(own_bars.iter().enumerate().map(|(group, (rect, count))| {
                    ui::BarStrip {
                        win,
                        group,
                        rect: *rect,
                        tabs: *count,
                    }
                }));
                terms.extend(
                    own_terms
                        .iter()
                        .enumerate()
                        .map(|(group, rect)| (win, group, *rect)),
                );
            }

            // The cross-group half of a tab drag: floating ghost, drop zones,
            // and the drop itself (as actions applied below). Every window
            // runs this over the one global drag; the overlay works out whose
            // it is. `local` has to be this panel's rect rather than
            // `ctx.viewport_rect()`, which inside a deferred viewport is not
            // this window's at all.
            let windows = ui::DragWindows {
                win,
                local: full,
                origin: inner.map(|rect| rect.min),
                bars: std::mem::take(&mut bars),
                terms: std::mem::take(&mut terms),
                carry,
            };
            report = ui::tab_drag_overlay(ui, &tabs, icons, &geoms, &windows, &mut raised);
        });

    if keyboard_window != win {
        tabs.focus_window(keyboard_window);
    }
    drop(tabs);
    if inner.is_some() {
        // Published for the other windows' next frames; a window whose OS rect
        // is not known yet keeps whatever it last published rather than
        // dropping off the desktop for a frame.
        shared.bars.insert(win, own_bars);
        shared.terms.insert(win, own_terms);
    }
    match report {
        // Only the window driving the drag knows where the tab is; a bystander
        // saying nothing is what keeps it from clearing the news every frame.
        ui::CarryReport::NotMine => {}
        ui::CarryReport::Carrying(state) => shared.carry = Some(state),
        ui::CarryReport::Idle => shared.carry = None,
    }
    if !raised.is_empty() {
        // The root frame is the one that applies these; a parked root would
        // sit on them forever. It matters most for a torn drag, which feeds
        // it a fresh window position every frame.
        ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
    }
    shared
        .actions
        .extend(raised.into_iter().map(|action| (win, action)));
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
            // This close is going through. Tell the AppKit hook, so a
            // `terminate:` that lands while the window fades — macOS re-sending
            // the Apple event during logout, a second Dock ▸ Quit — is answered
            // "yes" rather than turned into another question the user has
            // already answered.
            macos::approve_termination();
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

        self.drain_pty_events(&ctx);
        // A torn-out window's close box was hit between frames; its tabs go
        // now, before anything is drawn from them.
        self.close_extra_windows(&ctx);
        self.sync_window_title(&ctx, frame);
        self.sync_config_cache();

        self.sync_app_menu();

        // Before the shortcut table reads the keyboard and before the terminal
        // is composed: while the dialog is up, Return and Escape are answers
        // to it and must reach nothing else.
        if self.confirm_close.is_open() {
            let subject = self.confirm_close.subject();
            if let Some(choice) = confirm_close::show(&ctx, subject) {
                // Cancel drops the payload with the question: nothing closes,
                // and the next attempt asks from scratch.
                self.confirm_close.answer(choice);
                if choice == confirm_close::Choice::Close {
                    match subject.tab_id() {
                        // Run the close that was held. `ConfirmClose` is
                        // Approved now, so `close_tab` lets it straight
                        // through rather than asking the same question again.
                        // If that was the last tab, emptying the window is
                        // what closes it, down today's path: `empty` below
                        // sends the close, unquestioned for the same reason.
                        Some(id) => {
                            self.apply(&ctx, AppAction::CloseTab(id));
                            // The window survived it, so this answer must not
                            // outlive the tab it was about: the next busy tab
                            // gets its own question.
                            let alive = self.tabs.as_ref().is_some_and(|t| !lock(t).is_empty());
                            if alive {
                                self.confirm_close.reset();
                            }
                        }
                        None => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                    }
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

        // One process-table snapshot for every window: every group's bar in
        // every window reads the same cache, so the cost is per frame, not per
        // group and not per window.
        if let Some(arc) = self.tabs.clone() {
            let enabled = self.config.get().tabs.icons;
            // Shared before tabs, the one lock order in this file.
            let mut shared = lock_shared(&self.shared);
            let tabs = lock(&arc);
            Self::sync_tab_icons(enabled, &ctx, &tabs, &mut shared.icons);
        }

        // Re-read after the actions above, so a toggle applied this frame is
        // the one this frame paints with.
        self.sync_config_cache();
        // Either modal takes the keyboard away from the terminal.
        let modal_open = self.palette.is_open() || self.confirm_close.is_open();
        let bidi = self.active_bidi(&ctx);
        let bidi_base = self.config.get().text.bidi_base;
        let bar_with_one_tab = self.config.get().tabs.bar_with_one_tab;
        let focus_follows_mouse = self.config.get().input.focus_follows_mouse;
        let font = self.cached_font.clone();
        // Publish this frame's render inputs before anything draws: the
        // torn-out windows read them from their own viewport callbacks, which
        // run outside this function entirely.
        {
            let mut shared = lock_shared(&self.shared);
            shared.env = RenderEnv {
                modal_open,
                bidi,
                bidi_base,
                font,
                // Per window, filled in by `render_window`.
                focused_group: 0,
                keyboard: false,
                bar_with_one_tab,
                focus_follows_mouse,
            };
        }
        if let Some(arc) = self.tabs.clone() {
            render_window(ui, ROOT_WINDOW, &arc, &self.shared);
        }
        // Every other window, each in an OS window of its own.
        self.show_extra_windows(&ctx);
        for action in std::mem::take(&mut actions) {
            self.apply(&ctx, action);
        }
        // …and what the windows themselves raised, this one included.
        self.apply_window_actions(&ctx);

        // Last tab gone (or Quit chosen) -> the app is done. Not while the
        // window is already fading out, though: re-asking every frame would
        // count as the user insisting, and cut the animation short.
        let empty = self.tabs.as_ref().is_some_and(|tabs| lock(tabs).is_empty());
        // The last tab exiting while the dialog is up answers the question:
        // there is nothing left to protect, so the window must not be stuck
        // behind a modal about sessions that no longer exist.
        // (Which also covers a held *last tab* close whose process exits on
        // its own: the tab is gone, so the payload is stale and goes with it —
        // `answer` drops it.)
        if empty && self.confirm_close.is_open() {
            self.confirm_close.answer(confirm_close::Choice::Close);
        } else if self.confirm_close.is_open() {
            // The same staleness one tab down: the tab the question is about
            // exited on its own while the dialog was up. Nothing is left to
            // protect and nothing left to close, so the question goes and the
            // window carries on.
            let gone = self.confirm_close.subject().tab_id().is_some_and(|id| {
                self.tabs
                    .as_ref()
                    .is_some_and(|tabs| !lock(tabs).ids().contains(&id))
            });
            if gone {
                self.confirm_close.reset();
            }
        }
        if (self.quitting || empty) && !self.closing.is_fading() {
            // The listener is *not* dropped here. A quit only becomes a close
            // one frame later, and it may still be held back by the "Close
            // Window?" dialog — a cancelled quit that had already unlinked the
            // socket would leave a perfectly alive terra that no `terra`
            // command can reach again. The close path drops it once the answer
            // is in and the fade is over (`CloseStep::Close`).
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::dev_suffix;
    use super::*;

    fn pane() -> egui::Rect {
        egui::Rect::from_min_max(egui::pos2(400.0, 32.0), egui::pos2(800.0, 600.0))
    }

    fn moved(x: f32, y: f32) -> egui::Event {
        egui::Event::PointerMoved(egui::pos2(x, y))
    }

    /// The gesture the feature exists for: the pointer travels into a pane and
    /// the pane takes the keyboard.
    #[test]
    fn a_move_into_the_terminal_focuses_it() {
        assert!(hover_focus(true, &[moved(600.0, 300.0)], false, pane()));
    }

    /// `[input] focus_follows_mouse = false` restores pure click-to-focus:
    /// the same move that would otherwise hand over the keyboard does nothing.
    #[test]
    fn the_config_switch_off_suppresses_the_whole_rule() {
        assert!(!hover_focus(false, &[moved(600.0, 300.0)], false, pane()));
    }

    /// …and nothing else does. A frame with no pointer motion cannot move
    /// focus, which is what keeps a resting cursor's pane focused while splits
    /// re-layout the window or output scrolls underneath it.
    #[test]
    fn a_stationary_pointer_never_moves_focus() {
        assert!(!hover_focus(true, &[], false, pane()));
        // Keystrokes, wheel events and clicks are not motion either.
        let wheel = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::Vec2::new(0.0, 1.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        };
        assert!(!hover_focus(true, &[wheel], false, pane()));
        assert!(!hover_focus(
            true,
            &[egui::Event::Text("a".into())],
            false,
            pane()
        ));
    }

    /// A move that lands somewhere else — the neighbouring pane, the tab bar
    /// above this one — is not this pane's business.
    #[test]
    fn a_move_outside_the_terminal_is_ignored() {
        // Left of the pane, and above it in the tab bar strip.
        assert!(!hover_focus(true, &[moved(200.0, 300.0)], false, pane()));
        assert!(!hover_focus(true, &[moved(600.0, 10.0)], false, pane()));
    }

    /// A held button means a gesture in progress — a selection drag, a tab
    /// pill being carried across the window — and it belongs to the pane it
    /// started in until the button comes back up.
    #[test]
    fn a_drag_crossing_the_pane_keeps_its_focus_where_it_started() {
        assert!(!hover_focus(true, &[moved(600.0, 300.0)], true, pane()));
        // Released: the very next move focuses normally again.
        assert!(hover_focus(true, &[moved(601.0, 300.0)], false, pane()));
    }

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
