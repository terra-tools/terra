//! Wire protocol shared by the terra GUI app (server) and the `terra` CLI (client).
//!
//! Transport: one *local socket* — a Unix domain socket on Unix, a named pipe
//! on Windows — carrying newline-delimited JSON. One request per connection is
//! allowed but the server also handles multiple sequential requests on the same
//! connection. Every request line gets exactly one response line.
//!
//! # Portability
//!
//! There is a single code path. [`interprocess`]'s `local_socket` gives both
//! platforms the same stream-oriented, connection-per-client primitive behind
//! one type, so [`request`] and the server in `terra-app`'s `ipc.rs` are
//! written once and are not `cfg`-split at all. The synchronous API is used
//! deliberately: `interprocess`'s Tokio support is an optional feature and
//! terra has no async runtime (see `docs/ARCHITECTURE.md` on dependency
//! weight).
//!
//! What *is* platform-shaped is the **address**, and that is confined to
//! [`Address`] and [`resolve`]:
//!
//! - **Unix** — a filesystem path ([`GenericFilePath`]), `~/.terra/terra.sock`
//!   by default, exactly as before. Access control is the containing
//!   directory's `0700` mode, applied by the server.
//! - **Windows** — a name in the `\\.\pipe\` namespace ([`GenericNamespaced`]),
//!   `terra-<user>-<version>` by default. The pipe namespace is machine-wide
//!   and flat, so the name carries the user (two sessions on one machine must
//!   not fight over one pipe) and the semver-compatible version (two installed
//!   builds must never talk to each other, see [`instance_tag`]).
//!
//! `GenericNamespaced` is *not* used on Unix on purpose: off Linux it resolves
//! to `/tmp/<name>`, a world-writable directory, which would be a downgrade
//! from `~/.terra` at `0700`.

pub mod keys;

use anyhow::{Context, Result};
use interprocess::local_socket::{
    traits::Stream as _, GenericFilePath, GenericNamespaced, Name, Stream, ToFsName, ToNsName,
};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Where the control socket lives, in the form the OS actually wants.
///
/// Kept private-ish (constructed only by [`resolve`]) so that the two ways of
/// naming a local socket cannot drift apart between client and server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    /// A filesystem path: a Unix socket file, or a literal `\\.\pipe\…` path.
    Path(PathBuf),
    /// A name inside the platform's socket namespace (`\\.\pipe\` on Windows).
    Namespaced(String),
}

impl Address {
    /// The address as a human would write it — for logs, `terra doctor`, and
    /// the "is the app running?" error.
    pub fn display(&self) -> PathBuf {
        match self {
            Address::Path(path) => path.clone(),
            Address::Namespaced(name) => {
                if cfg!(windows) {
                    PathBuf::from(format!(r"\\.\pipe\{name}"))
                } else {
                    PathBuf::from(name)
                }
            }
        }
    }

    /// The filesystem path backing this address, if it has one. `None` for a
    /// named pipe, which has no filesystem presence to create, chmod or unlink.
    pub fn file(&self) -> Option<&Path> {
        match self {
            Address::Path(path) => Some(path),
            Address::Namespaced(_) => None,
        }
    }

    /// Convert to the `interprocess` address type. Fails only for addresses the
    /// platform cannot express (e.g. a non-`\\.\pipe\` path on Windows).
    pub fn to_name(&self) -> Result<Name<'static>> {
        let name = match self {
            Address::Path(path) => path.clone().to_fs_name::<GenericFilePath>(),
            Address::Namespaced(name) => name.clone().to_ns_name::<GenericNamespaced>(),
        };
        name.with_context(|| {
            format!(
                "{} is not a usable socket address",
                self.display().display()
            )
        })
    }
}

/// Decide the address from the environment. Pure, so it can be checked for
/// *both* platforms from a test on either one.
///
/// `env` is `$TERRA_SOCKET`. On Unix it is a socket path, as it always was. On
/// Windows a value that already looks like `\\.\pipe\…` (or `\\host\pipe\…`) is
/// taken literally; anything else is treated as a *pipe name* and ends up at
/// `\\.\pipe\<value>`, because Windows has nowhere else to put a local socket.
/// A Unix-shaped path such as `C:\tmp\terra.sock` therefore becomes a pipe
/// called `C:\tmp\terra.sock`, which is legal and works, rather than an error.
fn resolve(env: Option<&str>, home: &Path, user: &str, windows: bool) -> Address {
    if let Some(raw) = env.map(str::trim).filter(|raw| !raw.is_empty()) {
        if windows && !is_pipe_path(raw) {
            return Address::Namespaced(pipe_component(raw));
        }
        return Address::Path(PathBuf::from(raw));
    }
    if windows {
        Address::Namespaced(format!("terra-{}-{}", pipe_component(user), instance_tag()))
    } else {
        Address::Path(home.join(".terra").join("terra.sock"))
    }
}

/// `\\host\pipe\name` — the only path shape Windows accepts as a named pipe.
fn is_pipe_path(raw: &str) -> bool {
    let Some(rest) = raw.strip_prefix(r"\\") else {
        return false;
    };
    let Some((host, tail)) = rest.split_once('\\') else {
        return false;
    };
    !host.is_empty()
        && tail
            .strip_prefix(r"pipe\")
            .is_some_and(|name| !name.is_empty())
}

/// Make a string safe to embed in a pipe name: `\` is the namespace separator
/// and would silently re-point the address, and NUL terminates it.
fn pipe_component(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c == '\\' || c == '/' || c == '\0' {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// Reduce a version to the range it is wire-compatible with, so two installed
/// builds never end up on the same socket or the same single-instance mutex:
/// `0.1.0` and `0.1.7` share `0_1_x`, `0.2.0` does not.
///
/// Cargo's semver rules, minus the `semver` crate — this only has to classify a
/// version string that Cargo has already validated, and a dependency for four
/// `split`s would not pay for itself. Pre-releases are kept whole: they are
/// compatible with nothing but themselves.
pub fn semver_compat_tag(version: &str) -> String {
    let core = version.split('+').next().unwrap_or(version);
    if core.contains('-') {
        return version.replace(['.', '-', '+'], "_");
    }
    let mut parts = core.split('.');
    let major = parts.next().unwrap_or("0");
    let minor = parts.next().unwrap_or("0");
    let patch = parts.next().unwrap_or("0");
    match (major, minor) {
        ("0", "0") => format!("0_0_{patch}"),
        ("0", _) => format!("0_{minor}_x"),
        _ => format!("{major}_x_x"),
    }
}

/// This build's compatibility tag (`0_1_x`). Used in the default pipe name and
/// in the Windows single-instance mutex name.
pub fn instance_tag() -> String {
    semver_compat_tag(env!("CARGO_PKG_VERSION"))
}

/// The control socket's address, resolved from the environment.
pub fn socket_address() -> Address {
    let env = std::env::var("TERRA_SOCKET").ok();
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "default".to_string());
    resolve(env.as_deref(), &home, &user, cfg!(windows))
}

/// The socket as a printable path. Honors `TERRA_SOCKET`, else
/// `~/.terra/terra.sock` (Unix) or `\\.\pipe\terra-<user>-<version>` (Windows).
///
/// Kept for `terra doctor` and the app's startup log; anything that needs to
/// *bind* or *connect* wants [`socket_address`].
pub fn socket_path() -> PathBuf {
    socket_address().display()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// List all tabs.
    List,
    /// Create a new tab. `command` empty -> user's $SHELL.
    New {
        #[serde(default)]
        title: Option<String>,
        /// Program + args to run instead of the default shell.
        #[serde(default)]
        command: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
        /// Name of a `[profile.<name>]` in the app's config, supplying the
        /// command/cwd/title. The app resolves it, because the config is the
        /// app's — the CLI may be on the far end of an ssh forward and have no
        /// config file at all.
        ///
        /// Additive and defaulted, and skipped when absent, so a request
        /// written by any older client still parses and a request that names
        /// no profile is byte-identical to what it always was.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
    },
    /// Close a tab (kills its PTY).
    Kill { tab: u64 },
    /// Write text to a tab's PTY. If `enter` is true a carriage return is appended.
    Send {
        tab: u64,
        text: String,
        #[serde(default)]
        enter: bool,
        /// Interpret `{Enter}`, `{C-c}`, `{Delay 300}` … in `text` (see
        /// [`keys`]). Additive and defaulted, so a request written by any
        /// older client still means "write these bytes literally".
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        keys: bool,
    },
    /// Capture the visible screen (and up to `scrollback` lines above it) as plain text.
    Capture {
        tab: u64,
        #[serde(default)]
        scrollback: usize,
        /// Return the full cell grid with styling as JSON instead of plain
        /// text. Additive and defaulted, so `{"cmd":"capture","tab":1}` keeps
        /// meaning exactly what it always did; the JSON still travels in the
        /// existing `text` field of [`Response::Ok`].
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        cells: bool,
    },
    /// Rename a tab (sets a user title that overrides the shell-reported one).
    Rename { tab: u64, title: String },
    /// Focus/activate a tab.
    Select { tab: u64 },
    /// Capture the app window's framebuffer as a PNG.
    ///
    /// Window-wide rather than per-tab: this is the *rendered* pixels, so what
    /// it can show is whatever the window is showing — the active tab, the tab
    /// bar and the palette included. Use [`Request::Select`] first to choose
    /// the tab. The reply is [`Response::Ok`]'s `png` field, base64.
    Screenshot,
    /// Get or set a tab's right-to-left reordering mode.
    Bidi {
        tab: u64,
        /// `None` queries; `Some` sets. One of "off", "on", "auto".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabInfo {
    pub id: u64,
    pub title: String,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tabs: Option<Vec<TabInfo>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tab: Option<u64>,
        /// A PNG image, base64 (standard alphabet, padded) — the reply to
        /// [`Request::Screenshot`].
        ///
        /// Additive and defaulted like every field above it: it is absent from
        /// every other reply's JSON, so an older client parsing a newer
        /// server's answer sees exactly the bytes it saw before. Binary rides
        /// in base64 because the transport is one JSON object per line and a
        /// PNG contains both newlines and invalid UTF-8; see [`encode_png`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        png: Option<String>,
    },
    Err {
        error: String,
    },
}

impl Response {
    pub fn ok() -> Self {
        Response::Ok {
            tabs: None,
            text: None,
            tab: None,
            png: None,
        }
    }
    pub fn ok_tab(id: u64) -> Self {
        Response::Ok {
            tabs: None,
            text: None,
            tab: Some(id),
            png: None,
        }
    }
    pub fn ok_tabs(tabs: Vec<TabInfo>) -> Self {
        Response::Ok {
            tabs: Some(tabs),
            text: None,
            tab: None,
            png: None,
        }
    }
    pub fn ok_text(text: String) -> Self {
        Response::Ok {
            tabs: None,
            text: Some(text),
            tab: None,
            png: None,
        }
    }
    /// A PNG reply. `png` is the encoded file, *not* base64 — the encoding is
    /// applied here so the two sides cannot disagree about the alphabet.
    pub fn ok_png(png: &[u8]) -> Self {
        Response::Ok {
            tabs: None,
            text: None,
            tab: None,
            png: Some(encode_png(png)),
        }
    }
    pub fn err(e: impl std::fmt::Display) -> Self {
        Response::Err {
            error: e.to_string(),
        }
    }
}

/// Base64-encode a PNG for [`Response::Ok`]'s `png` field.
///
/// The wire format is one JSON object per line, so an image cannot travel
/// as-is: a PNG is not UTF-8 and contains `\n` bytes in its own right. Standard
/// alphabet, padded — the default `serde_json` and `base64 --decode` both
/// expect, so `terra screenshot --json | jq -r .png | base64 -d > shot.png`
/// works from a shell with no terra code involved.
pub fn encode_png(png: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(png)
}

/// Inverse of [`encode_png`]. Fails on anything that is not valid base64;
/// whether the bytes are a *PNG* is the caller's problem.
pub fn decode_png(encoded: &str) -> Result<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .context("the screenshot payload is not valid base64")
}

/// Blocking client used by the CLI: connect, send one request, read one
/// response. Identical on every platform.
pub fn request(req: &Request) -> Result<Response> {
    request_at(&socket_address(), req)
}

/// [`request`], against an address the caller names rather than the one in the
/// environment. The server uses it to ask "is anybody home?" about the specific
/// address it is trying to bind.
pub fn request_at(address: &Address, req: &Request) -> Result<Response> {
    let mut stream = Stream::connect(address.to_name()?).with_context(|| {
        format!(
            "cannot connect to terra at {} — is the terra app running?",
            address.display().display()
        )
    })?;
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()?;
    // The stream is consumed here rather than duplicated: one request, one
    // response, nothing left to write. (Windows named pipes have no `dup`
    // equivalent that `interprocess` exposes portably, and none is needed.)
    let mut reader = BufReader::new(stream);
    let mut buf = String::new();
    reader.read_line(&mut buf).context("reading response")?;
    let resp: Response =
        serde_json::from_str(buf.trim()).with_context(|| format!("bad response: {buf}"))?;
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bidi_request_with_a_mode_carries_it_on_the_wire() {
        let json = serde_json::to_string(&Request::Bidi {
            tab: 1,
            mode: Some("off".into()),
        })
        .unwrap();
        assert_eq!(json, r#"{"cmd":"bidi","tab":1,"mode":"off"}"#);
    }

    #[test]
    fn a_bidi_query_omits_the_mode_field_entirely() {
        let json = serde_json::to_string(&Request::Bidi { tab: 1, mode: None }).unwrap();
        assert_eq!(json, r#"{"cmd":"bidi","tab":1}"#);
    }

    #[test]
    fn an_absent_mode_deserialises_to_none() {
        let req: Request = serde_json::from_str(r#"{"cmd":"bidi","tab":7}"#).unwrap();
        match req {
            Request::Bidi { tab, mode } => {
                assert_eq!(tab, 7);
                assert!(mode.is_none());
            }
            other => panic!("expected Bidi, got {other:?}"),
        }
        let req: Request = serde_json::from_str(r#"{"cmd":"bidi","tab":7,"mode":"auto"}"#).unwrap();
        match req {
            Request::Bidi { mode, .. } => assert_eq!(mode.as_deref(), Some("auto")),
            other => panic!("expected Bidi, got {other:?}"),
        }
    }

    /// The `cells` flag is additive: a capture request written by any older
    /// client still parses, and still means "plain text".
    #[test]
    fn an_old_capture_request_still_deserialises_without_cells() {
        let req: Request = serde_json::from_str(r#"{"cmd":"capture","tab":1}"#).unwrap();
        match req {
            Request::Capture {
                tab,
                scrollback,
                cells,
            } => {
                assert_eq!(tab, 1);
                assert_eq!(scrollback, 0);
                assert!(!cells);
            }
            other => panic!("expected Capture, got {other:?}"),
        }

        let req: Request =
            serde_json::from_str(r#"{"cmd":"capture","tab":2,"scrollback":50}"#).unwrap();
        match req {
            Request::Capture {
                scrollback, cells, ..
            } => {
                assert_eq!(scrollback, 50);
                assert!(!cells);
            }
            other => panic!("expected Capture, got {other:?}"),
        }
    }

    /// A plain-text capture serialises byte-identically to before; only a
    /// `cells: true` request puts the new field on the wire.
    #[test]
    fn the_cells_flag_is_only_serialised_when_set() {
        let json = serde_json::to_string(&Request::Capture {
            tab: 1,
            scrollback: 0,
            cells: false,
        })
        .unwrap();
        assert_eq!(json, r#"{"cmd":"capture","tab":1,"scrollback":0}"#);

        let json = serde_json::to_string(&Request::Capture {
            tab: 1,
            scrollback: 0,
            cells: true,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"cmd":"capture","tab":1,"scrollback":0,"cells":true}"#
        );

        let req: Request =
            serde_json::from_str(r#"{"cmd":"capture","tab":1,"cells":true}"#).unwrap();
        match req {
            Request::Capture { cells, .. } => assert!(cells),
            other => panic!("expected Capture, got {other:?}"),
        }
    }

    // --- screenshot -------------------------------------------------------

    #[test]
    fn a_screenshot_request_is_just_its_verb() {
        let json = serde_json::to_string(&Request::Screenshot).unwrap();
        assert_eq!(json, r#"{"cmd":"screenshot"}"#);
        assert!(matches!(
            serde_json::from_str::<Request>(r#"{"cmd":"screenshot"}"#).unwrap(),
            Request::Screenshot
        ));
    }

    /// The `png` field is additive: it appears only in a screenshot reply, so
    /// every other response is byte-identical to what shipped before it.
    #[test]
    fn the_png_field_is_only_serialised_when_present() {
        assert_eq!(
            serde_json::to_string(&Response::ok()).unwrap(),
            r#"{"status":"ok"}"#
        );
        assert_eq!(
            serde_json::to_string(&Response::ok_png(b"\x89PNG")).unwrap(),
            r#"{"status":"ok","png":"iVBORw=="}"#
        );
    }

    /// …and an older server's reply, which has no `png` at all, still parses.
    #[test]
    fn a_response_without_a_png_still_deserialises() {
        let resp: Response = serde_json::from_str(r#"{"status":"ok","text":"hi"}"#).unwrap();
        match resp {
            Response::Ok { text, png, .. } => {
                assert_eq!(text.as_deref(), Some("hi"));
                assert!(png.is_none());
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn png_payloads_round_trip_through_base64() {
        let bytes: Vec<u8> = (0u8..=255).chain(b"\n\r\0\xff".iter().copied()).collect();
        let encoded = encode_png(&bytes);
        assert!(!encoded.contains('\n'), "the payload must fit on one line");
        assert_eq!(decode_png(&encoded).unwrap(), bytes);
        assert!(decode_png("not base64!!").is_err());
    }

    /// A `screenshot` sent to a terra that predates it must come back as a
    /// readable error, not a panic or a hang, on either side. This is the
    /// server half: an unknown `cmd` is a deserialisation failure, and the
    /// server answers it (see `ipc::serve`) with `bad request: <that>`.
    #[test]
    fn an_unknown_verb_is_an_error_that_names_itself() {
        let err = serde_json::from_str::<Request>(r#"{"cmd":"teleport"}"#).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("teleport"), "{msg}");
        assert!(msg.contains("unknown variant"), "{msg}");
    }

    /// `profile` is additive: a `new` request written by any older client
    /// still parses, and still means "no profile".
    #[test]
    fn an_old_new_request_still_deserialises_without_a_profile() {
        let req: Request = serde_json::from_str(r#"{"cmd":"new"}"#).unwrap();
        match req {
            Request::New {
                title,
                command,
                cwd,
                profile,
            } => {
                assert!(title.is_none());
                assert!(command.is_empty());
                assert!(cwd.is_none());
                assert!(profile.is_none());
            }
            other => panic!("expected New, got {other:?}"),
        }

        let req: Request = serde_json::from_str(
            r#"{"cmd":"new","title":"build","command":["cargo","test"],"cwd":"/tmp"}"#,
        )
        .unwrap();
        match req {
            Request::New {
                title,
                command,
                profile,
                ..
            } => {
                assert_eq!(title.as_deref(), Some("build"));
                assert_eq!(command, vec!["cargo", "test"]);
                assert!(profile.is_none());
            }
            other => panic!("expected New, got {other:?}"),
        }
    }

    /// The compatibility proof in the other direction: a request that names no
    /// profile serialises byte-identically to what an older terra emitted, so
    /// an older *server* still understands this client.
    #[test]
    fn the_profile_field_is_only_serialised_when_set() {
        let json = serde_json::to_string(&Request::New {
            title: None,
            command: Vec::new(),
            cwd: None,
            profile: None,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"cmd":"new","title":null,"command":[],"cwd":null}"#
        );

        let json = serde_json::to_string(&Request::New {
            title: None,
            command: Vec::new(),
            cwd: None,
            profile: Some("htop".into()),
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"cmd":"new","title":null,"command":[],"cwd":null,"profile":"htop"}"#
        );
    }

    /// Round trip: what the CLI writes is what the app reads.
    #[test]
    fn a_new_request_with_a_profile_round_trips() {
        let req = Request::New {
            title: Some("t".into()),
            command: Vec::new(),
            cwd: Some("/tmp".into()),
            profile: Some("htop".into()),
        };
        let back: Request = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        match back {
            Request::New {
                title,
                cwd,
                profile,
                command,
            } => {
                assert_eq!(title.as_deref(), Some("t"));
                assert_eq!(cwd.as_deref(), Some("/tmp"));
                assert_eq!(profile.as_deref(), Some("htop"));
                assert!(command.is_empty());
            }
            other => panic!("expected New, got {other:?}"),
        }
    }

    // --- addressing -------------------------------------------------------
    //
    // `resolve` takes the platform as an argument, so both halves are checked
    // from the macOS host and neither can rot unnoticed until someone boots
    // Windows.

    fn home() -> PathBuf {
        PathBuf::from("/home/ada")
    }

    #[test]
    fn unix_defaults_to_the_dot_terra_socket_it_always_used() {
        let addr = resolve(None, &home(), "ada", false);
        assert_eq!(
            addr,
            Address::Path(PathBuf::from("/home/ada/.terra/terra.sock"))
        );
        assert_eq!(addr.file(), Some(Path::new("/home/ada/.terra/terra.sock")));
    }

    #[test]
    fn windows_defaults_to_a_per_user_versioned_pipe_name() {
        let addr = resolve(None, &home(), "ada", true);
        let tag = semver_compat_tag(env!("CARGO_PKG_VERSION"));
        assert_eq!(addr, Address::Namespaced(format!("terra-ada-{tag}")));
        // No filesystem presence: nothing to create, chmod or unlink.
        assert_eq!(addr.file(), None);
    }

    /// A domain login (`CONTOSO\ada`) must not smuggle a namespace separator
    /// into the pipe name.
    #[test]
    fn a_backslash_in_the_user_name_cannot_repoint_the_pipe() {
        let addr = resolve(None, &home(), r"CONTOSO\ada", true);
        let tag = semver_compat_tag(env!("CARGO_PKG_VERSION"));
        assert_eq!(addr, Address::Namespaced(format!("terra-CONTOSO_ada-{tag}")));
        // The point of the test: no separator survives into the name.
        let Address::Namespaced(name) = &addr else { panic!("expected a pipe") };
        assert!(!name.contains('\\'), "{name} can repoint the pipe");
    }

    #[test]
    fn terra_socket_overrides_the_path_on_unix() {
        let addr = resolve(Some("/run/user/1000/t.sock"), &home(), "ada", false);
        assert_eq!(addr, Address::Path(PathBuf::from("/run/user/1000/t.sock")));
    }

    /// On Windows an override that is already a pipe path is taken literally…
    #[test]
    fn terra_socket_that_is_a_pipe_path_is_taken_literally_on_windows() {
        let addr = resolve(Some(r"\\.\pipe\custom"), &home(), "ada", true);
        assert_eq!(addr, Address::Path(PathBuf::from(r"\\.\pipe\custom")));
        let addr = resolve(Some(r"\\SERVER\pipe\custom"), &home(), "ada", true);
        assert_eq!(addr, Address::Path(PathBuf::from(r"\\SERVER\pipe\custom")));
    }

    /// …and anything else becomes a pipe *name*, since Windows has nowhere
    /// else to put a local socket.
    #[test]
    fn terra_socket_that_is_not_a_pipe_path_becomes_a_pipe_name_on_windows() {
        let addr = resolve(Some("mine"), &home(), "ada", true);
        assert_eq!(addr, Address::Namespaced("mine".into()));
        assert_eq!(
            addr.display(),
            PathBuf::from(if cfg!(windows) {
                r"\\.\pipe\mine"
            } else {
                "mine"
            })
        );

        let addr = resolve(Some(r"C:\tmp\terra.sock"), &home(), "ada", true);
        assert_eq!(addr, Address::Namespaced("C:_tmp_terra.sock".into()));
    }

    /// An empty or whitespace-only override is treated as unset rather than as
    /// a request to bind the empty path.
    #[test]
    fn a_blank_terra_socket_falls_back_to_the_default() {
        assert_eq!(
            resolve(Some("  "), &home(), "ada", false),
            resolve(None, &home(), "ada", false)
        );
    }

    #[test]
    fn only_a_double_backslash_host_pipe_prefix_counts_as_a_pipe_path() {
        assert!(is_pipe_path(r"\\.\pipe\x"));
        assert!(is_pipe_path(r"\\host\pipe\x"));
        assert!(!is_pipe_path(r"\\.\pipe\"));
        assert!(!is_pipe_path(r"\\\pipe\x"));
        assert!(!is_pipe_path(r"\\.\mailslot\x"));
        assert!(!is_pipe_path(r"C:\pipe\x"));
        assert!(!is_pipe_path("/home/ada/.terra/terra.sock"));
    }

    /// The default address must round-trip through `interprocess` on whatever
    /// platform the test is running on — this is the check that would have
    /// caught `GenericNamespaced` silently landing in `/tmp` on macOS.
    #[test]
    fn the_default_address_is_expressible_on_this_platform() {
        let addr = socket_address();
        addr.to_name().expect("default address must be usable");
        assert_eq!(addr.file().is_none(), cfg!(windows));
    }

    #[test]
    fn a_version_maps_to_the_range_it_is_compatible_with() {
        assert_eq!(semver_compat_tag("0.1.0"), "0_1_x");
        assert_eq!(semver_compat_tag("0.1.7"), "0_1_x");
        assert_eq!(semver_compat_tag("0.2.0"), "0_2_x");
        assert_eq!(semver_compat_tag("0.0.3"), "0_0_3");
        assert_eq!(semver_compat_tag("1.4.2"), "1_x_x");
        assert_eq!(semver_compat_tag("12.0.0"), "12_x_x");
        // Build metadata does not affect compatibility.
        assert_eq!(semver_compat_tag("1.4.2+build.9"), "1_x_x");
        // Pre-releases are compatible with nothing but themselves.
        assert_eq!(semver_compat_tag("0.1.0-rc.1"), "0_1_0_rc_1");
        assert_eq!(semver_compat_tag("2.0.0-alpha"), "2_0_0_alpha");
    }

    /// The tag is what keeps two installed builds off each other's socket, so
    /// it has to track the crate version rather than a hard-coded string.
    #[test]
    fn the_instance_tag_comes_from_this_crates_version() {
        assert_eq!(instance_tag(), semver_compat_tag(env!("CARGO_PKG_VERSION")));
    }
}
