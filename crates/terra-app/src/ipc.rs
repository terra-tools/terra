//! Unix-socket control server.
//!
//! A background thread accepts connections on [`terra_protocol::socket_path`]
//! and reads newline-delimited JSON [`Request`]s. Each connection thread runs
//! the request itself, against the [`TabManager`] shared with the UI thread
//! behind a `Mutex`, and writes the [`Response`] back as a single JSON line.
//!
//! Executing here rather than on the UI thread is deliberate: eframe skips
//! running the app entirely while the window is occluded (another Space,
//! minimised, fully covered), so anything that waits for a frame to happen
//! would simply never be answered. Requests only ever wait for the lock, which
//! the UI thread holds for a few short stretches per frame.
//!
//! # Portability
//!
//! Only the *listener* is Unix-specific. [`start`] is `cfg`-gated the way
//! `macos.rs` gates AppKit: a real implementation on `cfg(unix)`, and a
//! fallback elsewhere that returns an error the existing call site already
//! logs, so the app runs with no control socket rather than not compiling.
//! [`execute`] — the actual protocol — is portable and still type-checks on
//! every platform, so only the ~40 lines that own a socket are Unix-shaped.
//! See `terra_protocol`'s module docs for why there is no Windows transport yet.

#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use terra_protocol::{Request, Response};

use crate::config::BidiMode;
use crate::tabs::TabManager;

/// Owns the listening socket path; unlinks it on drop (best effort).
pub struct IpcServer {
    socket_path: PathBuf,
}

impl IpcServer {
    pub fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Take the tab lock, ignoring poisoning: a panic on one thread must not brick
/// the other half of the app (the UI locks the same way, see `main.rs`).
#[cfg_attr(not(unix), allow(dead_code))]
fn lock(tabs: &Mutex<TabManager>) -> MutexGuard<'_, TabManager> {
    tabs.lock().unwrap_or_else(|err| err.into_inner())
}

/// Bind the socket and spawn the accept loop.
#[cfg(unix)]
pub fn start(ctx: egui::Context, tabs: Arc<Mutex<TabManager>>) -> anyhow::Result<IpcServer> {
    let socket_path = terra_protocol::socket_path();

    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent)?;
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
    }
    // A socket left behind by a crashed instance would make bind() fail.
    let _ = fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path)?;

    std::thread::Builder::new()
        .name("terra-ipc".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let tabs = Arc::clone(&tabs);
                        let ctx = ctx.clone();
                        let spawned = std::thread::Builder::new()
                            .name("terra-ipc-conn".into())
                            .spawn(move || serve(stream, &tabs, &ctx));
                        if let Err(err) = spawned {
                            log::warn!("terra ipc: cannot spawn connection thread: {err}");
                        }
                    }
                    Err(err) => {
                        log::warn!("terra ipc: accept failed: {err}");
                        break;
                    }
                }
            }
        })?;

    Ok(IpcServer { socket_path })
}

/// No listener on this platform: the app still starts, `terra ls` just has
/// nothing to talk to. The call site in `main.rs` logs this and carries on.
#[cfg(not(unix))]
pub fn start(_ctx: egui::Context, _tabs: Arc<Mutex<TabManager>>) -> anyhow::Result<IpcServer> {
    anyhow::bail!(terra_protocol::UNSUPPORTED_TRANSPORT)
}

#[cfg(unix)]
fn serve(stream: UnixStream, tabs: &Mutex<TabManager>, ctx: &egui::Context) {
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    let reader = BufReader::new(stream);

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                log::debug!("terra ipc: read failed: {err}");
                return;
            }
        };
        if line.trim().is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<Request>(line.trim()) {
            Ok(request) => dispatch(request, tabs, ctx),
            Err(err) => Response::err(format!("bad request: {err}")),
        };

        let Ok(mut payload) = serde_json::to_string(&response) else {
            return;
        };
        payload.push('\n');
        if out.write_all(payload.as_bytes()).is_err() || out.flush().is_err() {
            return;
        }
    }
}

/// Run one request against the shared tabs and apply its side effects on the
/// window.
#[cfg_attr(not(unix), allow(dead_code))]
fn dispatch(request: Request, tabs: &Mutex<TabManager>, ctx: &egui::Context) -> Response {
    // Key notation is driven here rather than in `execute` because `{Delay N}`
    // has to wait *without* the tabs mutex held — `execute` runs under it, and
    // sleeping there would stall the UI thread and every other client for the
    // length of the delay.
    if let Request::Send {
        tab,
        text,
        enter,
        keys: true,
    } = &request
    {
        return send_keys(tabs, ctx, *tab, text, *enter);
    }

    let summon = matches!(request, Request::Select { .. });
    // `List` and `Capture` only read; everything else changes what the window
    // should be showing.
    let mutating = !matches!(request, Request::List | Request::Capture { .. });

    let response = {
        let mut tabs = lock(tabs);
        execute(&mut tabs, request)
    };

    if mutating {
        // A visible window must show the new/renamed/selected tab right away;
        // an occluded one simply picks it up whenever it is drawn again.
        ctx.request_repaint();
    }

    // `terra select` also summons the window. Done here on the IPC thread via
    // the thread-safe NSRunningApplication — activating from inside the frame
    // callback wedges winit's waker.
    if summon && matches!(response, Response::Ok { .. }) {
        crate::macos::activate_app();
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    response
}

/// Replay a key string (`"y{Enter}{Delay 300}n{Enter}"`) against one tab.
///
/// The lock is taken per chunk and dropped again before any wait, so a long
/// `{Delay}` never blocks the UI or another `terra` invocation. The tab is
/// re-looked-up each time as a consequence — if it exits mid-sequence the
/// remaining chunks are dropped and the client is told, rather than the write
/// silently going nowhere.
#[cfg_attr(not(unix), allow(dead_code))]
fn send_keys(
    tabs: &Mutex<TabManager>,
    ctx: &egui::Context,
    tab: u64,
    text: &str,
    enter: bool,
) -> Response {
    let mut chunks = terra_protocol::keys::parse(text);
    if enter {
        // `--enter` predates key notation and still means what it always did.
        chunks.push(terra_protocol::keys::Chunk::Bytes(b"\r".to_vec()));
    }

    for chunk in chunks {
        match chunk {
            terra_protocol::keys::Chunk::Bytes(bytes) => {
                // Every byte we produce is either the caller's own UTF-8 text
                // or an ASCII control sequence, so this never actually falls
                // back — the lossy path is here so a future key table entry
                // cannot turn into a panic.
                let text = String::from_utf8_lossy(&bytes);
                let mut tabs = lock(tabs);
                if !tabs.send(tab, &text, false) {
                    return Response::err(format!("no such tab: {tab}"));
                }
            }
            terra_protocol::keys::Chunk::Delay(ms) => {
                ctx.request_repaint();
                std::thread::sleep(std::time::Duration::from_millis(ms));
            }
        }
    }

    ctx.request_repaint();
    Response::ok()
}

/// The whole protocol, in terms of [`TabManager`].
#[cfg_attr(not(unix), allow(dead_code))]
fn execute(tabs: &mut TabManager, request: Request) -> Response {
    let no_tab = |id: u64| Response::err(format!("no such tab: {id}"));

    match request {
        Request::List => Response::ok_tabs(tabs.infos()),
        Request::New {
            title,
            command,
            cwd,
        } => match tabs.open(&command, cwd.as_deref(), title) {
            Ok(id) => Response::ok_tab(id),
            Err(err) => Response::err(err),
        },
        Request::Kill { tab } => {
            if tabs.close(tab) {
                Response::ok()
            } else {
                no_tab(tab)
            }
        }
        // `keys: true` is intercepted in `dispatch`; this is the literal path.
        Request::Send { tab, text, enter, .. } => {
            if tabs.send(tab, &text, enter) {
                Response::ok()
            } else {
                no_tab(tab)
            }
        }
        // `cells` swaps the plain-text dump for the styled grid, so a
        // rendering question ("is that row's background actually grey?",
        // "where exactly is the cursor?") is a `jq` query rather than a
        // screenshot someone has to squint at.
        Request::Capture {
            tab,
            scrollback,
            cells,
        } => {
            let dump = if cells {
                tabs.capture_cells(tab, scrollback)
            } else {
                tabs.capture(tab, scrollback)
            };
            match dump {
                Some(text) => Response::ok_text(text),
                None => no_tab(tab),
            }
        }
        Request::Rename { tab, title } => {
            if tabs.set_custom_title(tab, title) {
                Response::ok()
            } else {
                no_tab(tab)
            }
        }
        Request::Bidi { tab, mode } => {
            let parsed = match mode.as_deref().map(BidiMode::parse) {
                // A mode we do not recognise must not silently do nothing.
                Some(None) => {
                    return Response::err(format!(
                        "bad bidi mode: {}; expected off, on or auto",
                        mode.unwrap_or_default()
                    ))
                }
                Some(Some(m)) => Some(m),
                None => None,
            };
            match parsed {
                // Setting.
                Some(m) if tabs.set_bidi(tab, Some(m)) => {
                    Response::ok_text(m.name().to_string())
                }
                Some(_) => no_tab(tab),
                // Querying: report the tab's own override, or the fact that
                // it is following the config.
                None => match tabs.bidi(tab) {
                    Some(Some(m)) => Response::ok_text(m.name().to_string()),
                    Some(None) => Response::ok_text("config".to_string()),
                    None => no_tab(tab),
                },
            }
        }
        Request::Select { tab } => {
            if tabs.select(tab) {
                Response::ok()
            } else {
                no_tab(tab)
            }
        }
    }
}
