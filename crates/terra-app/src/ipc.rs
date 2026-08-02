//! Local-socket control server.
//!
//! A background thread accepts connections on [`terra_protocol::socket_address`]
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
//! One code path everywhere. `interprocess`'s local sockets are Unix domain
//! sockets on Unix and named pipes on Windows; which of the two a
//! [`terra_protocol::Address`] denotes is decided once, in `terra-protocol`.
//! Nothing here is `cfg`-gated except two things that genuinely have no
//! cross-platform spelling: [`harden`], which is `chmod 0700` on Unix and a
//! comment about pipe DACLs on Windows, and [`InstanceLock`], which is a named
//! kernel mutex on Windows and a no-op elsewhere.
//!
//! # Access control
//!
//! *Unix* — the socket lives in `~/.terra`, which is created `0700`. Connect
//! permission on a Unix domain socket is the filesystem's, so no other user can
//! reach it.
//!
//! *Windows* — a named pipe has no filesystem presence, so there is no
//! directory mode to set; the equivalent is the pipe's DACL. terra creates the
//! pipe with the default one, which (per `CreateNamedPipe`) grants full control
//! to the creator, `LocalSystem` and administrators, and **read** access to
//! `Everyone` and anonymous. Driving a terminal requires *writing* a request
//! line, and write access is exactly what other users do not have, so the
//! "any local user can drive the terminal" hazard is already closed. What the
//! default does leave open is a same-machine nuisance: another user can open
//! the pipe read-only and occupy an instance. Closing that too means handing
//! `interprocess`'s `ListenerOptionsExt::security_descriptor` a DACL naming
//! this user's SID — a change confined to [`bind`], and one that should be made
//! by someone who can actually run it on Windows, since an SD that is wrong in
//! either direction breaks the transport outright.

use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use interprocess::local_socket::{traits::ListenerExt as _, ListenerOptions, Stream};
use terra_protocol::{Address, Request, Response};

use crate::config::BidiMode;
use crate::screenshot::Screenshots;
use crate::tabs::TabManager;

/// Owns the listening address; unlinks the socket file on drop (best effort,
/// and a no-op for a named pipe, which the kernel reaps with the process).
pub struct IpcServer {
    address: Address,
    /// Held for the process's lifetime so a second terra keeps seeing us.
    #[allow(dead_code)]
    lock: InstanceLock,
}

impl IpcServer {
    /// The address as a printable path — `main.rs` logs it at startup.
    pub fn socket_path(&self) -> std::path::PathBuf {
        self.address.display()
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        if let Some(file) = self.address.file() {
            let _ = std::fs::remove_file(file);
        }
    }
}

/// Take the tab lock, ignoring poisoning: a panic on one thread must not brick
/// the other half of the app (the UI locks the same way, see `main.rs`).
fn lock(tabs: &Mutex<TabManager>) -> MutexGuard<'_, TabManager> {
    tabs.lock().unwrap_or_else(|err| err.into_inner())
}

/// Bind the socket and spawn the accept loop.
///
/// Also the single-instance gate: if another terra of a wire-compatible version
/// already owns the address, this focuses *that* one and exits the process
/// rather than opening a rival app. See [`hand_over`].
pub fn start(
    ctx: egui::Context,
    tabs: Arc<Mutex<TabManager>>,
    shots: Arc<Screenshots>,
) -> anyhow::Result<IpcServer> {
    let address = terra_protocol::socket_address();

    // Windows: an atomic, kernel-owned claim, taken before we touch the pipe.
    // "Does the address already exist?" is not atomic and, for a socket file,
    // not even truthful — see the stale-socket dance in `bind`.
    let Some(lock) = InstanceLock::acquire() else {
        hand_over(&address, "another terra is already running");
    };

    if let Some(parent) = address.file().and_then(Path::parent) {
        std::fs::create_dir_all(parent)?;
        harden(parent);
    }

    let listener = bind(&address)?;

    std::thread::Builder::new()
        .name("terra-ipc".into())
        .spawn(move || {
            // A named pipe hands back an error for a client that connected and
            // left before `accept` reached it, and that must not take the whole
            // server down with it — but a listener that errors every time would
            // spin, so give up after a run of failures with nothing in between.
            let mut failures = 0u32;
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        failures = 0;
                        let tabs = Arc::clone(&tabs);
                        let shots = Arc::clone(&shots);
                        let ctx = ctx.clone();
                        let spawned = std::thread::Builder::new()
                            .name("terra-ipc-conn".into())
                            .spawn(move || serve(stream, &tabs, &shots, &ctx));
                        if let Err(err) = spawned {
                            log::warn!("terra ipc: cannot spawn connection thread: {err}");
                        }
                    }
                    Err(err) => {
                        log::warn!("terra ipc: accept failed: {err}");
                        failures += 1;
                        if failures >= 16 {
                            log::error!("terra ipc: giving up on the listener");
                            break;
                        }
                    }
                }
            }
        })?;

    Ok(IpcServer { address, lock })
}

/// Create the listener, resolving the one thing a Unix socket gets wrong on its
/// own: a socket file left behind by a crashed instance makes `bind` fail with
/// `AddrInUse` forever.
///
/// The old code unlinked it unconditionally, which also let a second terra
/// silently steal the socket from a *running* first one. Instead the address is
/// probed first: if something answers, there is a live terra and we hand over to
/// it; only a socket nobody is listening on is removed. (`interprocess` offers
/// `ListenerOptions::try_overwrite` for this, but it is the unconditional
/// version — it displaces a live listener too.)
///
/// Windows never reaches the retry: a named pipe cannot go stale, and a name
/// that is already taken fails with `PermissionDenied`, not `AddrInUse`. There
/// the `InstanceLock` is what catches the second instance.
fn bind(address: &Address) -> anyhow::Result<interprocess::local_socket::Listener> {
    let create = || {
        ListenerOptions::new()
            .name(address.to_name()?)
            .create_sync()
            .map_err(anyhow::Error::from)
    };

    match create() {
        Ok(listener) => Ok(listener),
        Err(err) if is_addr_in_use(&err) => {
            if terra_protocol::request_at(address, &Request::List).is_ok() {
                hand_over(address, "another terra is already listening on this socket");
            }
            if let Some(file) = address.file() {
                log::info!("terra ipc: removing a stale socket at {}", file.display());
                let _ = std::fs::remove_file(file);
            }
            create()
        }
        Err(err) => Err(err),
    }
}

fn is_addr_in_use(err: &anyhow::Error) -> bool {
    err.downcast_ref::<std::io::Error>()
        .is_some_and(|err| err.kind() == std::io::ErrorKind::AddrInUse)
}

/// Second instance: bring the running one forward and get out of the way.
///
/// The handover rides the transport that already exists rather than a
/// platform-specific side channel (the Tauri plugin's `WM_COPYDATA` is one-way
/// and window-bound, which is the wrong shape for a request/response CLI). A
/// `Select` on the running instance's active tab is precisely "focus yourself":
/// `dispatch` already answers it with `activate_app()` plus a viewport `Focus`.
///
/// Handing over rather than refusing is the behaviour a GUI expects — clicking
/// the dock icon twice should not open a second terminal, and should not print
/// an error either.
///
/// What actually raises the window is platform-dependent, and only macOS is
/// covered end to end: `crate::macos::activate_app` is a real
/// `NSRunningApplication` activation there and an empty function elsewhere. On
/// Windows and X11 the `ViewportCommand::Focus` that follows it is winit's
/// `SetForegroundWindow`/`_NET_ACTIVE_WINDOW`, which usually works but is
/// subject to the foreground lock; on Wayland, compositors refuse
/// self-activation outright and the window will not come forward. That is a gap
/// to fix in `macos.rs`'s counterparts, not something to fake here.
fn hand_over(address: &Address, why: &str) -> ! {
    log::info!("terra: {why}; focusing it instead of starting a second app");
    match terra_protocol::request_at(address, &Request::List) {
        Ok(Response::Ok {
            tabs: Some(tabs), ..
        }) => {
            if let Some(active) = tabs.iter().find(|tab| tab.active).or_else(|| tabs.first()) {
                let _ = terra_protocol::request_at(address, &Request::Select { tab: active.id });
            }
        }
        Ok(_) => {}
        Err(err) => log::warn!("terra: could not reach the running instance: {err:#}"),
    }
    std::process::exit(0)
}

/// Restrict the socket's directory to this user.
///
/// Unix only in the literal sense: a named pipe has no containing directory, so
/// on Windows the equivalent protection is the pipe's DACL and lives in [`bind`]
/// (see the module docs).
#[cfg(unix)]
fn harden(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn harden(_dir: &Path) {}

/// A process-wide claim on "the running terra of this version".
struct InstanceLock {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
}

impl InstanceLock {
    /// `Some` if this process is the first instance, `None` if another one
    /// already holds the claim.
    ///
    /// Windows: a named kernel mutex, following the Tauri single-instance
    /// plugin. `CreateMutexW` either creates the object or reports
    /// `ERROR_ALREADY_EXISTS` in one atomic step, and the kernel destroys it
    /// when the last handle closes — including when the process is killed — so
    /// unlike a socket file it cannot go stale. The name is scoped to the login
    /// session (`Local\`) and to the wire-compatible version range, so two
    /// sessions and two installed builds each get their own.
    ///
    /// Elsewhere the claim is the socket itself: binding a Unix domain socket
    /// is atomic, and [`bind`] distinguishes a live listener from a socket file
    /// left behind by a crash. A second mechanism would only add a second thing
    /// that can disagree.
    #[cfg(windows)]
    fn acquire() -> Option<Self> {
        use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
        use windows_sys::Win32::System::Threading::CreateMutexW;

        let name: Vec<u16> = format!(
            r"Local\terra-single-instance-{}",
            terra_protocol::instance_tag()
        )
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

        // Not taking ownership (`bInitialOwner = FALSE`): only the *existence*
        // of the named object is the signal, and an unowned mutex cannot be
        // abandoned.
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            // We cannot tell whether anyone else is running; starting is the
            // less destructive of the two guesses.
            log::warn!("terra: single-instance mutex unavailable; not checking for a second app");
            return Some(Self { handle });
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe { CloseHandle(handle) };
            return None;
        }
        Some(Self { handle })
    }

    #[cfg(not(windows))]
    fn acquire() -> Option<Self> {
        Some(Self {})
    }
}

#[cfg(windows)]
impl Drop for InstanceLock {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
        }
    }
}

fn serve(stream: Stream, tabs: &Mutex<TabManager>, shots: &Screenshots, ctx: &egui::Context) {
    // `&Stream` implements both `Read` and `Write`, so the two halves come from
    // one borrow — no `try_clone`, which named pipes have no equivalent of.
    let mut out = &stream;
    let reader = std::io::BufReader::new(&stream);

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
            Ok(request) => dispatch(request, tabs, shots, ctx),
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
fn dispatch(
    request: Request,
    tabs: &Mutex<TabManager>,
    shots: &Screenshots,
    ctx: &egui::Context,
) -> Response {
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

    // Likewise driven here rather than in `execute`: a screenshot touches no
    // tab at all, and it *waits* — holding the tabs mutex across a wait for the
    // UI thread would deadlock against the UI thread taking it to draw the very
    // frame being waited for.
    if matches!(request, Request::Screenshot) {
        return screenshot(shots, ctx);
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

/// Capture the window and hand back a PNG.
///
/// The window is summoned first, for the same reason `terra select` does it and
/// in the same way (the thread-safe `NSRunningApplication`, never from inside
/// the frame callback — that wedges winit's waker): the pixels asked for are
/// the ones the GPU draws, and eframe does not run the app at all while the
/// window is occluded. Bringing it forward turns "no frame will ever happen"
/// into "a frame happens now" for the ordinary cases — another Space, buried
/// behind a browser. What it cannot fix is a *minimised* window, or a
/// compositor that refuses self-activation (Wayland); those end at
/// `Screenshots::capture`'s timeout with a message that says so.
fn screenshot(shots: &Screenshots, ctx: &egui::Context) -> Response {
    // Quiet first: a visible window — focused or not — renders on a repaint
    // request alone, so most screenshots need no focus change at all. Only
    // when no frame arrives (minimised, fully covered, other Space — the
    // states where eframe parks the UI thread) is the window summoned, which
    // steals focus but is the one way to get a frame at all.
    if let Ok(png) = shots.capture_within(ctx, std::time::Duration::from_millis(600)) {
        return Response::ok_png(&png);
    }
    crate::macos::activate_app();
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    ctx.request_repaint();
    match shots.capture(ctx) {
        Ok(png) => Response::ok_png(&png),
        Err(err) => Response::err(err),
    }
}

/// The whole protocol, in terms of [`TabManager`].
fn execute(tabs: &mut TabManager, request: Request) -> Response {
    let no_tab = |id: u64| Response::err(format!("no such tab: {id}"));

    match request {
        Request::List => Response::ok_tabs(tabs.infos()),
        // A profile supplies the command, cwd and title; anything the client
        // stated explicitly still wins over it, so `--profile p --title x` is
        // "p, but called x" rather than an error or a silent ignore. The CLI
        // refuses `--profile` together with a `-- cmd`, but the wire allows
        // both and has to mean something, so an explicit command wins too.
        Request::New {
            title,
            command,
            cwd,
            profile,
        } => {
            let (command, cwd, title) = match profile.as_deref() {
                None => (command, cwd, title),
                Some(name) => {
                    // Cloned out so the immutable borrow of `tabs` ends before
                    // `open` takes a mutable one.
                    let profile = match crate::config::resolve_profile(tabs.profiles(), name) {
                        Ok(profile) => profile.clone(),
                        Err(err) => return Response::err(err),
                    };
                    let command = if command.is_empty() {
                        profile.command
                    } else {
                        command
                    };
                    (command, cwd.or(profile.cwd), title.or(profile.title))
                }
            };
            match tabs.open(&command, cwd.as_deref(), title) {
                Ok(id) => Response::ok_tab(id),
                Err(err) => Response::err(err),
            }
        }
        Request::Kill { tab } => {
            if tabs.close(tab) {
                Response::ok()
            } else {
                no_tab(tab)
            }
        }
        // `keys: true` is intercepted in `dispatch`; this is the literal path.
        Request::Send {
            tab, text, enter, ..
        } => {
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
                Some(m) if tabs.set_bidi(tab, Some(m)) => Response::ok_text(m.name().to_string()),
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
        // Answered in `dispatch`, which has the UI context and does not hold
        // this lock: a screenshot is a frame, not a tab operation.
        Request::Screenshot => Response::err("internal error: screenshot reached the tab executor"),
        Request::Select { tab } => {
            if tabs.select(tab) {
                Response::ok()
            } else {
                no_tab(tab)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the Unix-gated stale-socket test calls a `Listener` method.
    #[cfg(unix)]
    use interprocess::local_socket::traits::Listener as _;

    /// The stale-socket path is the one piece of `bind` that is exercisable
    /// without a GUI: a socket file with nothing behind it must be removed and
    /// the bind must then succeed, and a *live* listener on the same address
    /// must not be displaced.
    // Unix only: the address is a filesystem path there. A Windows named
    // pipe has no file to go stale — the kernel reclaims the name when the
    // last handle closes — so this branch of `bind` is unreachable on
    // Windows by construction, not merely untested.
    #[cfg(unix)]
    #[test]
    fn a_socket_file_with_nothing_behind_it_is_reclaimed() {
        let dir = std::env::temp_dir().join(format!("terra-ipc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("terra.sock");

        // A regular file standing where the socket goes is exactly what a
        // crashed instance leaves behind, as far as `bind` can tell.
        let address = Address::Path(path.clone());
        let first = bind(&address).expect("binding a fresh address");
        drop(first);
        assert!(!path.exists(), "the listener reclaims its own name on drop");

        // Bind, then abandon the file without unbinding, the way SIGKILL does.
        {
            let listener = bind(&address).expect("binding again");
            let mut listener = listener;
            listener.do_not_reclaim_name_on_drop();
        }
        assert!(path.exists(), "the socket file outlives a crashed instance");
        let reclaimed = bind(&address);
        assert!(
            reclaimed.is_ok(),
            "a stale socket must not be fatal: {reclaimed:?}"
        );
        drop(reclaimed);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `harden` is a no-op off Unix; where it does something, it must leave the
    /// directory readable by nobody else.
    #[test]
    fn the_socket_directory_is_private_to_this_user() {
        let dir = std::env::temp_dir().join(format!("terra-perm-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        harden(&dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "got {mode:o}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
