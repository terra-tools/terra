//! Talking to the controlling terminal directly, through `/dev/tty`.
//!
//! `doctor` and `record` both need the real terminal rather than stdin/stdout:
//! stdout may be a pipe (`terra doctor > report.txt` must still probe), and a
//! probe must not be answered by a shell that happens to be on stdin.
//!
//! Raw mode is entered through [`RawMode`], whose `Drop` restores the previous
//! termios. Restoring is not optional: a CLI that returns to a shell with echo
//! off has broken the user's session, so the guard covers early returns, `?`,
//! and panics (which unwind by default).
//!
//! # Portability
//!
//! Everything here is termios/ioctl, i.e. Unix. Following `terra-app`'s
//! `macos.rs`: the real implementation lives in a `cfg(unix)` module, a
//! same-shaped stub lives in the other one, and callers name [`Tty`] /
//! [`RawMode`] unconditionally. The stub's `Tty::open` fails with a clear
//! message, which is exactly the path `doctor` already takes in CI — so on
//! Windows `terra doctor` prints its environment section and says the probes
//! need a Unix tty, instead of the command not existing.
//!
//! Windows *does* have an equivalent (`GetConsoleMode`/`SetConsoleMode` with
//! `ENABLE_VIRTUAL_TERMINAL_INPUT`, `GetConsoleScreenBufferInfo`), but it is a
//! different API rather than a port of this one, and doing it properly means
//! `windows-sys` plus a second probe path to test. Not in this change.

/// How long a single terminal query may take to answer. Terminals reply in
/// microseconds over a pty; 200ms is generous even over ssh, and it bounds the
/// worst case (a terminal that never answers) at "one blink per probe".
pub const QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

// `window_size`/`set_window_size` are only called by `record`, which is itself
// Unix-only; re-exported unconditionally so the two modules keep one import
// list rather than a `cfg` at every use site.
#[allow(unused_imports)]
pub use imp::{window_size, RawMode, Tty};

#[cfg(unix)]
pub use imp::set_window_size;

/// Does this look like a whole reply? Terminal replies end with the final byte
/// of a CSI sequence or with a string terminator; recognising that shaves the
/// timeout off every probe that *does* answer.
///
/// Pure, and therefore compiled and tested everywhere even though only the
/// Unix reader calls it.
#[cfg_attr(not(unix), allow(dead_code))]
fn response_is_complete(buf: &[u8]) -> bool {
    match buf.last() {
        Some(b'c') | Some(b'R') | Some(b'y') | Some(b'S') | Some(b'\\') | Some(&0x07) => {
            buf.first() == Some(&0x1b)
        }
        _ => false,
    }
}

#[cfg(unix)]
mod imp {
    use super::response_is_complete;
    use anyhow::{Context, Result};
    use std::fs::OpenOptions;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;

    pub use std::os::fd::RawFd;

    /// Restores the terminal's termios when dropped.
    pub struct RawMode {
        fd: RawFd,
        saved: libc::termios,
    }

    impl RawMode {
        /// Raw mode where `read(2)` gives up after `timeout` of silence.
        ///
        /// `VMIN=0` + `VTIME` turns the read itself into the deadline, which is
        /// why nothing here needs poll/select. VTIME counts tenths of a second,
        /// so the timeout rounds down to a minimum of one tenth.
        pub fn enable_timed(fd: RawFd, timeout: std::time::Duration) -> Result<Self> {
            let vtime = (timeout.as_millis() / 100).clamp(1, 255) as libc::cc_t;
            Self::enable_with(fd, 0, vtime)
        }

        /// Raw mode with blocking reads (`VMIN=1`), for pass-through forwarding
        /// where a read should wait for the user rather than spin.
        pub fn enable_blocking(fd: RawFd) -> Result<Self> {
            Self::enable_with(fd, 1, 0)
        }

        fn enable_with(fd: RawFd, vmin: libc::cc_t, vtime: libc::cc_t) -> Result<Self> {
            // SAFETY: `fd` is a live descriptor for the lifetime of the call and
            // termios is a plain POD struct we own.
            unsafe {
                let mut saved: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(fd, &mut saved) != 0 {
                    return Err(std::io::Error::last_os_error()).context("tcgetattr");
                }
                let mut raw = saved;
                libc::cfmakeraw(&mut raw);
                raw.c_cc[libc::VMIN] = vmin;
                raw.c_cc[libc::VTIME] = vtime;
                if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
                    return Err(std::io::Error::last_os_error()).context("tcsetattr");
                }
                Ok(Self { fd, saved })
            }
        }
    }

    impl Drop for RawMode {
        fn drop(&mut self) {
            // SAFETY: restoring the exact struct we read at construction.
            unsafe {
                libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved);
            }
        }
    }

    /// The controlling terminal, opened read/write.
    pub struct Tty {
        file: std::fs::File,
    }

    impl Tty {
        /// Open `/dev/tty`. Fails when there is no controlling terminal (cron,
        /// CI, `terra doctor < /dev/null` in some setups) — callers degrade
        /// instead of dying.
        pub fn open() -> Result<Self> {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")
                .context("open /dev/tty")?;
            Ok(Self { file })
        }

        pub fn fd(&self) -> RawFd {
            self.file.as_raw_fd()
        }

        /// Window size as `(rows, cols)` via `TIOCGWINSZ`.
        pub fn size(&self) -> Result<(u16, u16)> {
            window_size(self.fd())
        }

        /// Write a query and collect the answer until the terminal goes quiet.
        ///
        /// The caller must already hold a [`RawMode`] on this fd, otherwise the
        /// line discipline eats the reply and echoes it into the user's
        /// scrollback. An empty result means "no response" — never an error,
        /// because "this terminal does not implement this query" is a finding,
        /// not a failure.
        pub fn query(&mut self, request: &[u8]) -> Vec<u8> {
            if self.file.write_all(request).is_err() || self.file.flush().is_err() {
                return Vec::new();
            }
            let mut out = Vec::new();
            let mut buf = [0u8; 256];
            // Two silent reads in a row end the wait; a single one is enough in
            // practice but the second costs nothing when the terminal is mute
            // and covers replies that arrive split across a scheduler hiccup.
            let mut quiet = 0;
            while quiet < 2 && out.len() < 4096 {
                match self.file.read(&mut buf) {
                    Ok(0) => quiet += 1,
                    Ok(n) => {
                        out.extend_from_slice(&buf[..n]);
                        if response_is_complete(&out) {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            out
        }
    }

    /// `(rows, cols)` for any tty descriptor.
    pub fn window_size(fd: RawFd) -> Result<(u16, u16)> {
        // SAFETY: winsize is POD; TIOCGWINSZ writes exactly that struct.
        unsafe {
            let mut ws: libc::winsize = std::mem::zeroed();
            if libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) != 0 {
                return Err(std::io::Error::last_os_error()).context("TIOCGWINSZ");
            }
            Ok((ws.ws_row, ws.ws_col))
        }
    }

    /// Push a `(rows, cols)` size onto a pty master, so the child re-lays-out.
    pub fn set_window_size(fd: RawFd, rows: u16, cols: u16) {
        // SAFETY: same POD struct, opposite direction. Errors are ignored: a
        // resize that does not land is cosmetic and must not kill the session.
        unsafe {
            let ws = libc::winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            libc::ioctl(fd, libc::TIOCSWINSZ, &ws);
        }
    }
}

/// No `/dev/tty`, no termios: the same API, shaped so callers need no `cfg`.
///
/// [`Tty::open`] is the single failure point, and it is one every caller
/// already handles — `doctor` turns it into `tty: unavailable: …` plus the
/// existing note, keeping the environment half of the report intact.
#[cfg(not(unix))]
mod imp {
    use anyhow::{bail, Result};

    /// Stand-in for `std::os::fd::RawFd`, which is Unix-only. Nothing on this
    /// platform ever produces a real one — `Tty::open` fails first.
    pub type RawFd = i32;

    pub struct RawMode;

    impl RawMode {
        pub fn enable_timed(_fd: RawFd, _timeout: std::time::Duration) -> Result<Self> {
            bail!(UNSUPPORTED)
        }

        #[allow(dead_code)]
        pub fn enable_blocking(_fd: RawFd) -> Result<Self> {
            bail!(UNSUPPORTED)
        }
    }

    pub struct Tty(());

    impl Tty {
        pub fn open() -> Result<Self> {
            bail!(UNSUPPORTED)
        }

        pub fn fd(&self) -> RawFd {
            -1
        }

        pub fn size(&self) -> Result<(u16, u16)> {
            bail!(UNSUPPORTED)
        }

        #[allow(dead_code)]
        pub fn query(&mut self, _request: &[u8]) -> Vec<u8> {
            Vec::new()
        }
    }

    #[allow(dead_code)]
    pub fn window_size(_fd: RawFd) -> Result<(u16, u16)> {
        bail!(UNSUPPORTED)
    }

    const UNSUPPORTED: &str =
        "terra needs a Unix controlling terminal (/dev/tty) for this; \
         the Windows console API is not wired up yet";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_is_complete_only_when_it_starts_with_esc_and_ends_with_a_final_byte() {
        assert!(response_is_complete(b"\x1b[?62;22c"));
        assert!(response_is_complete(b"\x1b[24;80R"));
        assert!(response_is_complete(b"\x1b[?2026;2$y"));
        assert!(response_is_complete(b"\x1bP>|ghostty\x1b\\"));
        assert!(!response_is_complete(b"\x1b[?62;22"));
        assert!(!response_is_complete(b""));
        // Plain typed text is not a reply, however it ends.
        assert!(!response_is_complete(b"c"));
    }

    #[test]
    #[cfg(unix)]
    fn window_size_on_a_non_tty_is_an_error_not_a_panic() {
        use std::os::fd::AsRawFd;
        // /dev/null is never a tty, so this exercises the failure path without
        // needing (or disturbing) a real terminal.
        let f = std::fs::File::open("/dev/null").expect("/dev/null");
        assert!(window_size(f.as_raw_fd()).is_err());
    }

    /// Off Unix the whole module is a stub, and the contract callers rely on is
    /// that opening the terminal *fails* rather than pretending.
    #[test]
    #[cfg(not(unix))]
    fn without_a_unix_tty_opening_the_terminal_fails_cleanly() {
        assert!(Tty::open().is_err());
        assert!(window_size(-1).is_err());
    }
}
