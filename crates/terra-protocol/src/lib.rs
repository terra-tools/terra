//! Wire protocol shared by the terra GUI app (server) and the `terra` CLI (client).
//!
//! Transport: Unix domain socket, newline-delimited JSON. One request per
//! connection is allowed but the server also handles multiple sequential
//! requests on the same connection. Every request line gets exactly one
//! response line.
//!
//! # Portability
//!
//! The message types and [`keys`] are pure data and build everywhere. The
//! *transport* is Unix-only: [`request`] is a real client on `cfg(unix)` and a
//! compile-clean stub elsewhere that fails with [`UNSUPPORTED_TRANSPORT`].
//!
//! Windows 10+ does support `AF_UNIX`, and a crate like `uds_windows` would
//! expose it — but wiring it up here would be a lie by omission today: nothing
//! *serves* the socket on Windows (the GUI's terminal backend is not ported
//! either, see `terra-app`), and the default socket path is `$HOME`-shaped.
//! So the honest v1 is "the CLI builds and runs on Windows, every
//! socket-backed subcommand tells you the transport is not implemented yet",
//! which is what this does. Swapping the stub for `uds_windows` later is a
//! change to exactly one function.

pub mod keys;

#[cfg(unix)]
use anyhow::Context;
use anyhow::Result;
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

/// What every socket-backed operation reports on a platform with no transport.
pub const UNSUPPORTED_TRANSPORT: &str =
    "the terra control socket is not implemented on this platform yet \
     (it needs a Unix domain socket); `terra doctor` and `terra record --decode` \
     still work without it";

/// Resolve the socket path. Honors `TERRA_SOCKET`, else `~/.terra/terra.sock`.
///
/// `HOME` is the Unix home; `USERPROFILE` is consulted so the path is at least
/// well-formed on Windows, where nothing binds it today (see the module docs).
pub fn socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("TERRA_SOCKET") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    home.join(".terra").join("terra.sock")
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
    },
    Err { error: String },
}

impl Response {
    pub fn ok() -> Self {
        Response::Ok { tabs: None, text: None, tab: None }
    }
    pub fn ok_tab(id: u64) -> Self {
        Response::Ok { tabs: None, text: None, tab: Some(id) }
    }
    pub fn ok_tabs(tabs: Vec<TabInfo>) -> Self {
        Response::Ok { tabs: Some(tabs), text: None, tab: None }
    }
    pub fn ok_text(text: String) -> Self {
        Response::Ok { tabs: None, text: Some(text), tab: None }
    }
    pub fn err(e: impl std::fmt::Display) -> Self {
        Response::Err { error: e.to_string() }
    }
}

/// Blocking client used by the CLI: connect, send one request, read one response.
#[cfg(unix)]
pub fn request(req: &Request) -> Result<Response> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path).with_context(|| {
        format!(
            "cannot connect to terra at {} — is the terra app running?",
            path.display()
        )
    })?;
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let mut buf = String::new();
    reader.read_line(&mut buf).context("reading response")?;
    let resp: Response = serde_json::from_str(buf.trim())
        .with_context(|| format!("bad response: {buf}"))?;
    Ok(resp)
}

/// Same signature, no transport: every socket-backed subcommand fails with one
/// clear sentence instead of the crate failing to compile. See the module docs
/// for why this is a stub rather than `uds_windows` or a named pipe.
#[cfg(not(unix))]
pub fn request(_req: &Request) -> Result<Response> {
    anyhow::bail!(UNSUPPORTED_TRANSPORT)
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
}
