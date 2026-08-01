//! Wire protocol shared by the terra GUI app (server) and the `terra` CLI (client).
//!
//! Transport: Unix domain socket, newline-delimited JSON. One request per
//! connection is allowed but the server also handles multiple sequential
//! requests on the same connection. Every request line gets exactly one
//! response line.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

/// Resolve the socket path. Honors `TERRA_SOCKET`, else `~/.terra/terra.sock`.
pub fn socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("TERRA_SOCKET") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".terra").join("terra.sock")
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
    },
    /// Capture the visible screen (and up to `scrollback` lines above it) as plain text.
    Capture {
        tab: u64,
        #[serde(default)]
        scrollback: usize,
    },
    /// Rename a tab (sets a user title that overrides the shell-reported one).
    Rename { tab: u64, title: String },
    /// Focus/activate a tab.
    Select { tab: u64 },
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
