//! `terra record` — log both directions of a program's terminal I/O.
//!
//! `script(1)` records only what the program writes. The interesting half for
//! terminal-compatibility work is the other one: the replies a terminal sends
//! back (DA1, XTVERSION, DECRPM, cursor position), because that is what makes
//! the same program behave differently under terra and Ghostty. So this runs
//! the program on a pty of our own and logs both directions with timestamps.
//!
//! Why raw `libc` and not `portable-pty`: the crate's value is portability
//! across Windows ConPTY and a thread-per-direction reader API. terra records
//! on unix only, this file already needs `libc` for termios, `TIOCSWINSZ` and
//! `SIGWINCH`, and the pty part is one `forkpty` call — so the dependency
//! would add a tree without removing any code from here.
//!
//! # Portability
//!
//! The two halves split cleanly and are gated accordingly:
//!
//! * **Recording** ([`record`]) is `forkpty` + `poll` + `SIGWINCH`, so it is
//!   `cfg(unix)`. Elsewhere it is a stub that fails with one clear sentence —
//!   the subcommand still exists, still parses, and still tells you why it
//!   cannot run, rather than vanishing from `--help` on Windows.
//! * **Reading a recording back** ([`decode_file`]) is JSON and string work.
//!   It is portable, unconditional, and stays useful on Windows: a recording
//!   taken on a Mac can be decoded and diffed anywhere.

use crate::escape::{describe_bytes, escape_bytes, unescape_bytes};
use anyhow::{Context, Result};
use std::io::{BufRead, BufWriter, Write};
use std::path::Path;

/// One line of the log. Kept as a function so both the writer and its test see
/// exactly the same formatting.
///
/// `t` is fixed to four decimals rather than left to float formatting: log
/// lines are meant to be diffed, and `0.1` vs `0.1000` would be noise.
#[cfg_attr(not(unix), allow(dead_code))]
fn log_line(t: f64, dir: &str, bytes: &[u8]) -> String {
    // serde_json does the JSON string quoting; escape_bytes has already made
    // the payload printable ASCII, so the two escapings never fight.
    let payload = serde_json::Value::String(escape_bytes(bytes));
    format!("{{\"t\":{t:.4},\"dir\":\"{dir}\",\"bytes\":{payload}}}")
}

/// Record `argv` running on a fresh pty, writing JSONL to `out`.
///
/// Returns the exit code to propagate: the child's own status, or 128+signal
/// when it died to a signal, matching shell convention.
#[cfg(unix)]
pub fn record(out: &Path, argv: &[String]) -> Result<i32> {
    unix::record(out, argv)
}

/// No pty here. Fails loudly and specifically rather than being absent: the
/// subcommand is still listed, `--decode` still works, and the message names
/// the missing piece instead of leaving the user to guess.
#[cfg(not(unix))]
pub fn record(_out: &Path, _argv: &[String]) -> Result<i32> {
    anyhow::bail!(
        "terra record is not supported on this platform yet: it needs a Unix pty \
         (forkpty) and termios. `terra record --decode <file>` does work here, so a \
         recording taken on macOS or Linux can still be read and diffed."
    )
}

#[cfg(unix)]
mod unix {
    use super::log_line;
    use crate::tty::{set_window_size, window_size, RawMode, Tty};
    use anyhow::{bail, Context, Result};
    use std::ffi::CString;
    use std::io::{BufWriter, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    /// Set by the `SIGWINCH` handler; the forwarding loop turns it into a
    /// `TIOCSWINSZ` on the child's pty. A flag is all a signal handler may
    /// safely touch, hence the deferral.
    static RESIZED: AtomicBool = AtomicBool::new(false);

    extern "C" fn on_sigwinch(_: libc::c_int) {
        RESIZED.store(true, Ordering::Relaxed);
    }

    /// Fallback pty size when the recorder itself has no terminal (output piped
    /// in CI). 80x24 is what every program assumes anyway.
    const DEFAULT_SIZE: (u16, u16) = (24, 80);

    /// How long the forwarding loop blocks in `poll` before rechecking flags.
    /// It only bounds SIGWINCH latency, so it can be lazy.
    const POLL_INTERVAL_MS: libc::c_int = 200;

    pub fn record(out: &Path, argv: &[String]) -> Result<i32> {
        if argv.is_empty() {
            bail!("nothing to record: pass the program after `--`, e.g. terra record --out s.jsonl -- vim");
        }
        // Build the child's argv before forking: after `fork` only
        // async-signal-safe work is legal, and allocating a CString is not.
        let cargs: Vec<CString> = argv
            .iter()
            .map(|a| CString::new(a.as_bytes()).context("argument contains a NUL byte"))
            .collect::<Result<_>>()?;
        let mut cptrs: Vec<*const libc::c_char> = cargs.iter().map(|a| a.as_ptr()).collect();
        cptrs.push(std::ptr::null());

        let mut file = BufWriter::new(
            std::fs::File::create(out).with_context(|| format!("create {}", out.display()))?,
        );

        // The controlling terminal, if there is one: it gives the pty its size
        // and is where raw mode has to be applied so keystrokes reach the child
        // intact.
        let tty = Tty::open().ok();
        let (rows, cols) = tty
            .as_ref()
            .and_then(|t| t.size().ok())
            .unwrap_or(DEFAULT_SIZE);

        let mut master: RawFd = -1;
        let mut ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: forkpty writes the master fd through `master` and returns in
        // both processes; the child branch below only calls execvp/_exit.
        //
        // `&mut ws` rather than `&ws` because macOS declares the parameter
        // `*mut winsize` while glibc declares it `const *`; one `&mut`
        // coerces to both, where `&` would not compile on macOS. Hence the
        // allow — clippy only sees the glibc signature when cross-checking.
        #[allow(clippy::unnecessary_mut_passed)]
        let pid = unsafe {
            libc::forkpty(
                &mut master,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut ws,
            )
        };
        if pid < 0 {
            return Err(std::io::Error::last_os_error()).context("forkpty");
        }
        if pid == 0 {
            // SAFETY: child of a fork, only async-signal-safe calls from here.
            unsafe {
                libc::execvp(cptrs[0], cptrs.as_ptr());
                // execvp only returns on failure; the parent sees this as an exit.
                libc::_exit(127);
            }
        }
        // SAFETY: forkpty gave us ownership of this descriptor.
        let master = unsafe { OwnedFd::from_raw_fd(master) };

        // SAFETY: installing a handler that only sets an atomic flag.
        unsafe {
            libc::signal(
                libc::SIGWINCH,
                on_sigwinch as *const () as libc::sighandler_t,
            )
        };

        // Raw mode on the real terminal, so ^C, arrow keys and paste go to the
        // child untouched. The guard restores it on every exit path, panics
        // included — leaving a shell in raw mode is worse than losing the log.
        let _raw = tty
            .as_ref()
            .and_then(|t| RawMode::enable_blocking(t.fd()).ok());
        let tty_fd = tty.as_ref().map_or(libc::STDIN_FILENO, |t| t.fd());

        let start = Instant::now();
        let mut buf = [0u8; 8192];
        // Input is read from stdin, not from the `/dev/tty` handle: macOS
        // `poll(2)` answers POLLNVAL for a descriptor opened on /dev/tty, which
        // silently kills the terminal->program direction (measured — it was
        // doing exactly that). stdin is an ordinary pty-slave descriptor and
        // polls correctly, and it is also the right thing to forward when input
        // is redirected.
        // Goes -1 once that input is at EOF, which makes poll skip it; without
        // that the loop would spin on the permanently-readable EOF.
        let mut input_fd = libc::STDIN_FILENO;
        loop {
            if RESIZED.swap(false, Ordering::Relaxed) {
                if let Ok((r, c)) = window_size(tty_fd) {
                    set_window_size(master.as_raw_fd(), r, c);
                }
            }
            let mut fds = [
                libc::pollfd {
                    fd: master.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: input_fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: both descriptors are open for the duration of the call.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, POLL_INTERVAL_MS) };
            if ready < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    // SIGWINCH landed; the top of the loop handles it.
                    continue;
                }
                break;
            }
            // Only POLLIN means "a read will not block". A pty master or a tty
            // can come back with POLLHUP alone, and reading on that would park
            // the process in `read(2)` forever — which is exactly how an earlier
            // version of this loop hung after the child exited.
            if hung_up(fds[0].revents) {
                break;
            }
            if fds[0].revents & libc::POLLIN != 0 {
                match read_fd(master.as_raw_fd(), &mut buf) {
                    Some(n) => {
                        // Program -> terminal. Forwarded first so the user never
                        // waits on the logger.
                        write_all_fd(libc::STDOUT_FILENO, &buf[..n]);
                        let _ = writeln!(
                            file,
                            "{}",
                            log_line(start.elapsed().as_secs_f64(), "out", &buf[..n])
                        );
                    }
                    // EOF or EIO on the pty: the child is gone.
                    None => break,
                }
            }
            if hung_up(fds[1].revents) {
                input_fd = -1;
                continue;
            }
            if fds[1].revents & libc::POLLIN != 0 {
                match read_fd(input_fd, &mut buf) {
                    Some(n) => {
                        // Terminal -> program: keystrokes *and* the query replies
                        // this whole command exists to capture.
                        write_all_fd(master.as_raw_fd(), &buf[..n]);
                        let _ = writeln!(
                            file,
                            "{}",
                            log_line(start.elapsed().as_secs_f64(), "in", &buf[..n])
                        );
                    }
                    // Input at EOF: stop watching it, but let the child run on —
                    // it may have plenty left to say.
                    None => input_fd = -1,
                }
            }
        }
        let _ = file.flush();

        let mut status: libc::c_int = 0;
        // SAFETY: `pid` is our child; waitpid fills in `status`.
        unsafe { libc::waitpid(pid, &mut status, 0) };
        Ok(exit_code(status))
    }

    /// Shell-convention exit code for a `wait(2)` status.
    fn exit_code(status: libc::c_int) -> i32 {
        if libc::WIFSIGNALED(status) {
            128 + libc::WTERMSIG(status)
        } else {
            libc::WEXITSTATUS(status)
        }
    }

    /// Did poll report the far end gone (or the fd unusable) with nothing left
    /// to read? `POLLIN` wins when both are set: pending bytes are still worth
    /// having.
    fn hung_up(revents: libc::c_short) -> bool {
        revents & libc::POLLIN == 0
            && revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
    }

    /// `read(2)` that retries on EINTR. `None` means end-of-stream or a real
    /// error.
    fn read_fd(fd: RawFd, buf: &mut [u8]) -> Option<usize> {
        loop {
            // SAFETY: writing at most buf.len() bytes into buf.
            let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return None;
            }
            if n == 0 {
                return None;
            }
            return Some(n as usize);
        }
    }

    /// `write(2)` until the whole slice is out, retrying short writes and EINTR.
    /// Errors are dropped: a broken stdout must not take the session down.
    fn write_all_fd(fd: RawFd, mut buf: &[u8]) {
        while !buf.is_empty() {
            // SAFETY: reading at most buf.len() bytes from buf.
            let n = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
            if n <= 0 {
                let err = std::io::Error::last_os_error();
                if n < 0 && err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return;
            }
            buf = &buf[n as usize..];
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_signalled_child_reports_128_plus_the_signal() {
            // Encoded the way wait(2) does: low bits carry the signal number.
            assert_eq!(exit_code(libc::SIGINT), 128 + libc::SIGINT);
            // Normal exits carry their code in the high byte.
            assert_eq!(exit_code(3 << 8), 3);
            assert_eq!(exit_code(0), 0);
        }
    }
}

/// Pretty form of one JSONL record, or `None` for a line that is not one.
///
/// Kept pure so `--decode` is testable and so a malformed line (a truncated
/// last record after a crash) is skipped rather than fatal.
fn decode_line(line: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let t = v.get("t")?.as_f64()?;
    let dir = v.get("dir")?.as_str()?;
    let bytes = unescape_bytes(v.get("bytes")?.as_str()?);
    Some(format!("{t:>9.4}  {dir:<3}  {}", describe_bytes(&bytes)))
}

/// Pretty-print a recording to stdout, naming escape sequences where possible.
pub fn decode_file(path: &Path) -> Result<()> {
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut out = BufWriter::new(std::io::stdout().lock());
    for line in std::io::BufReader::new(file).lines() {
        let line = line.context("read recording")?;
        if line.trim().is_empty() {
            continue;
        }
        match decode_line(&line) {
            Some(pretty) => writeln!(out, "{pretty}")?,
            None => writeln!(out, "?  {line}")?,
        }
    }
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_line_is_json_with_a_fixed_width_timestamp() {
        let line = log_line(0.0123, "out", b"\x1b[?2026$p");
        assert_eq!(line, r#"{"t":0.0123,"dir":"out","bytes":"\\x1b[?2026$p"}"#);
        // Fixed decimals, so identical events diff identically.
        assert!(log_line(0.1, "in", b"").starts_with(r#"{"t":0.1000,"#));
    }

    #[test]
    fn a_log_line_survives_bytes_that_are_not_utf8() {
        let line = log_line(1.0, "in", &[0xff, 0x00, b'"', b'\\']);
        let v: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(
            unescape_bytes(v["bytes"].as_str().unwrap()),
            vec![0xff, 0x00, b'"', b'\\']
        );
    }

    #[test]
    fn every_byte_value_round_trips_through_a_log_line() {
        let all: Vec<u8> = (0..=255u8).collect();
        let line = log_line(2.5, "out", &all);
        let v: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(unescape_bytes(v["bytes"].as_str().unwrap()), all);
    }

    #[test]
    fn decoding_names_the_sequences_in_a_record() {
        let line = log_line(0.0123, "out", b"\x1b[?2026$p");
        assert_eq!(
            decode_line(&line).unwrap(),
            "   0.0123  out  ESC[?2026$p  DECRQM synchronized-output"
        );
        let reply = log_line(0.0456, "in", b"\x1b[?2026;2$y");
        assert_eq!(
            decode_line(&reply).unwrap(),
            "   0.0456  in   ESC[?2026;2$y  DECRPM synchronized-output = reset"
        );
    }

    #[test]
    fn a_malformed_line_decodes_to_none_rather_than_panicking() {
        assert!(decode_line("").is_none());
        assert!(decode_line("{\"t\":0.1}").is_none());
        assert!(decode_line("{\"t\":0.1,\"dir\":\"in\"}").is_none());
        assert!(decode_line("not json at all").is_none());
    }

    /// The stub has to *fail*, not silently succeed with an empty recording —
    /// a caller that ignores this would produce a file nobody could tell from
    /// a real one.
    #[test]
    #[cfg(not(unix))]
    fn recording_is_refused_with_an_explanation_off_unix() {
        let err = record(Path::new("unused.jsonl"), &["vim".to_string()]).unwrap_err();
        assert!(err.to_string().contains("not supported on this platform"));
    }
}
