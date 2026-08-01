//! Unix-socket control server.
//!
//! A background thread accepts connections on [`terra_protocol::socket_path`]
//! and reads newline-delimited JSON [`Request`]s. Each request is forwarded to
//! the UI thread together with a one-shot reply channel; the connection thread
//! then blocks (bounded by [`REPLY_TIMEOUT`]) for the [`Response`] and writes it
//! back as a single JSON line.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use terra_protocol::{Request, Response};

/// How long a connection waits for the UI thread to answer.
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// A request from a client plus the channel its response must go back on.
pub struct IpcRequest {
    pub request: Request,
    pub reply: Sender<Response>,
}

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

/// Bind the socket and spawn the accept loop.
pub fn start(ctx: egui::Context) -> anyhow::Result<(IpcServer, Receiver<IpcRequest>)> {
    let socket_path = terra_protocol::socket_path();

    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent)?;
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
    }
    // A socket left behind by a crashed instance would make bind() fail.
    let _ = fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path)?;
    let (tx, rx) = mpsc::channel();

    std::thread::Builder::new()
        .name("terra-ipc".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let tx = tx.clone();
                        let ctx = ctx.clone();
                        let spawned = std::thread::Builder::new()
                            .name("terra-ipc-conn".into())
                            .spawn(move || serve(stream, &tx, &ctx));
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

    Ok((IpcServer { socket_path }, rx))
}

fn serve(stream: UnixStream, tx: &Sender<IpcRequest>, ctx: &egui::Context) {
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
            Ok(request) => dispatch(request, tx, ctx),
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

fn dispatch(request: Request, tx: &Sender<IpcRequest>, ctx: &egui::Context) -> Response {
    let summon = matches!(request, Request::Select { .. });
    let (reply_tx, reply_rx) = mpsc::channel();
    if tx
        .send(IpcRequest {
            request,
            reply: reply_tx,
        })
        .is_err()
    {
        return Response::err("terra ui is gone");
    }
    // Wake the UI so it drains the queue even when idle.
    ctx.request_repaint();

    match reply_rx.recv_timeout(REPLY_TIMEOUT) {
        Ok(response) => {
            // `terra select` also summons the window. Done here on the IPC
            // thread via the thread-safe NSRunningApplication — activating
            // from inside the frame callback wedges winit's waker.
            if summon && matches!(response, Response::Ok { .. }) {
                crate::macos::activate_app();
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                ctx.request_repaint();
            }
            response
        }
        Err(_) => Response::err("timed out waiting for the terra ui"),
    }
}
