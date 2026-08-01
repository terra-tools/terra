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

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use terra_protocol::{Request, Response};

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
        let _ = fs::remove_file(&self.socket_path);
    }
}

/// Take the tab lock, ignoring poisoning: a panic on one thread must not brick
/// the other half of the app (the UI locks the same way, see `main.rs`).
fn lock(tabs: &Mutex<TabManager>) -> MutexGuard<'_, TabManager> {
    tabs.lock().unwrap_or_else(|err| err.into_inner())
}

/// Bind the socket and spawn the accept loop.
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
fn dispatch(request: Request, tabs: &Mutex<TabManager>, ctx: &egui::Context) -> Response {
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

/// The whole protocol, in terms of [`TabManager`].
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
        Request::Send { tab, text, enter } => {
            if tabs.send(tab, &text, enter) {
                Response::ok()
            } else {
                no_tab(tab)
            }
        }
        Request::Capture { tab, scrollback } => match tabs.capture(tab, scrollback) {
            Some(text) => Response::ok_text(text),
            None => no_tab(tab),
        },
        Request::Rename { tab, title } => {
            if tabs.set_custom_title(tab, title) {
                Response::ok()
            } else {
                no_tab(tab)
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
