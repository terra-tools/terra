//! `terra record` — log both directions of a program's terminal I/O.
//!
//! `script(1)` records only what the program writes. The interesting half for
//! terminal-compatibility work is the other one: the replies a terminal sends
//! back (DA1, XTVERSION, DECRPM, cursor position), because that is what makes
//! the same program behave differently under terra and Ghostty. So this runs
//! the program on a pty of our own and logs both directions with timestamps.
//!
//! Why raw `libc`/`windows-sys` and not `portable-pty`: what that crate sells
//! is exactly the two platform backends below plus a thread-per-direction
//! reader API. This file already needs `libc` for termios, `TIOCSWINSZ` and
//! `SIGWINCH`, and `windows-sys` for console modes, and each pty is one call
//! (`forkpty`, `CreatePseudoConsole`) — so the dependency would add a tree
//! without removing much code from here.
//!
//! # Portability
//!
//! Three parts, gated the way `tty.rs` is:
//!
//! * **Recording on Unix** is `forkpty` + `poll` + `SIGWINCH`.
//! * **Recording on Windows** is ConPTY: `CreatePseudoConsole` over a pair of
//!   anonymous pipes, `CreateProcessW` with the pseudoconsole passed through a
//!   proc-thread attribute list, a thread per direction, and
//!   `ResizePseudoConsole` when the console window changes shape. There is no
//!   `SIGWINCH`, so the size is polled on the same tick that waits for the
//!   child.
//! * **Reading a recording back** ([`decode_file`]) is JSON and string work,
//!   portable and unconditional.
//!
//! The log format is one format, not two: both recorders call [`log_line`], so
//! a recording made on Windows decodes on macOS and vice versa.

use crate::escape::{describe_bytes, escape_bytes, unescape_bytes};
use anyhow::{Context, Result};
use std::io::{BufRead, BufWriter, Write};
use std::path::Path;

/// One line of the log. Kept as a function so both the writer and its test see
/// exactly the same formatting.
///
/// `t` is fixed to four decimals rather than left to float formatting: log
/// lines are meant to be diffed, and `0.1` vs `0.1000` would be noise.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
fn log_line(t: f64, dir: &str, bytes: &[u8]) -> String {
    // serde_json does the JSON string quoting; escape_bytes has already made
    // the payload printable ASCII, so the two escapings never fight.
    let payload = serde_json::Value::String(escape_bytes(bytes));
    format!("{{\"t\":{t:.4},\"dir\":\"{dir}\",\"bytes\":{payload}}}")
}

/// Join `argv` into a Windows command line.
///
/// `CreateProcessW` takes one string, and the child splits it again with
/// `CommandLineToArgvW`, so the quoting here has to be that function's exact
/// inverse or `terra record -- git commit -m "two words"` silently becomes two
/// arguments. The rule is the awkward one from the Win32 docs: a run of
/// backslashes is literal unless a quote follows it, in which case it is
/// doubled and the quote escaped.
///
/// Pure, and so tested on every platform even though only Windows calls it.
#[cfg_attr(not(windows), allow(dead_code))]
fn command_line(argv: &[String]) -> String {
    const NEEDS_QUOTES: &[char] = &[' ', '\t', '\n', '\u{b}', '"'];

    let mut out = String::new();
    for (i, arg) in argv.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        if !arg.is_empty() && !arg.contains(NEEDS_QUOTES) {
            out.push_str(arg);
            continue;
        }
        out.push('"');
        let mut backslashes = 0usize;
        for c in arg.chars() {
            match c {
                '\\' => backslashes += 1,
                '"' => {
                    // 2n+1: the run is doubled and one more escapes the quote.
                    for _ in 0..backslashes * 2 + 1 {
                        out.push('\\');
                    }
                    backslashes = 0;
                    out.push('"');
                }
                _ => {
                    for _ in 0..backslashes {
                        out.push('\\');
                    }
                    backslashes = 0;
                    out.push(c);
                }
            }
        }
        // A trailing run would otherwise escape our own closing quote.
        for _ in 0..backslashes * 2 {
            out.push('\\');
        }
        out.push('"');
    }
    out
}

/// Exit code to propagate for a Windows process exit status.
///
/// Windows exit codes are a full `u32` — `cmd`'s `ERRORLEVEL` and the fatal
/// NTSTATUS values (`0xc000013a` for Ctrl-C) both use the whole range — so the
/// bits are passed through rather than truncated to a byte the way a Unix
/// `wait(2)` status is.
///
/// Pure, and so tested on every platform even though only Windows calls it.
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_exit_code(status: u32) -> i32 {
    status as i32
}

/// Record `argv` running on a fresh pty, writing JSONL to `out`.
///
/// Returns the exit code to propagate: the child's own status, or 128+signal
/// when it died to a signal, matching shell convention.
#[cfg(unix)]
pub fn record(out: &Path, argv: &[String]) -> Result<i32> {
    unix::record(out, argv)
}

/// Record `argv` on a ConPTY pseudoconsole, writing the same JSONL to `out`.
#[cfg(windows)]
pub fn record(out: &Path, argv: &[String]) -> Result<i32> {
    windows::record(out, argv)
}

/// No pty here. Fails loudly and specifically rather than being absent: the
/// subcommand is still listed, `--decode` still works, and the message names
/// the missing piece instead of leaving the user to guess.
#[cfg(not(any(unix, windows)))]
pub fn record(_out: &Path, _argv: &[String]) -> Result<i32> {
    anyhow::bail!(
        "terra record is not supported on this platform yet: it needs a Unix pty \
         (forkpty) or a Windows pseudoconsole (ConPTY). `terra record --decode <file>` \
         does work here, so a recording taken elsewhere can still be read and diffed."
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

/// Recording through a ConPTY pseudoconsole.
///
/// The shape is the same as the Unix module — put the child on a pty, forward
/// both directions, log both, propagate size changes and the exit status — but
/// none of the mechanism is shared, because Windows has neither `fork` nor a
/// descriptor you can `poll`:
///
/// * The pty is `CreatePseudoConsole` over two anonymous pipes; the child gets
///   it through a proc-thread attribute list rather than by inheriting a
///   controlling terminal.
/// * There is no `poll`, so each direction gets a thread, and a channel
///   funnels both into a single writer thread — which is also what keeps the
///   log's timestamps monotonic without a lock around the file.
/// * There is no `SIGWINCH`, so the console size is compared against the last
///   one on the same tick that polls the child for exit.
///
/// The attribute-list dance (size the list with a null pointer first,
/// `UpdateProcThreadAttribute` with `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`,
/// `EXTENDED_STARTUPINFO_PRESENT`) follows `alacritty_terminal`'s
/// `tty/windows/conpty.rs`, which is the closest known-good example. Two
/// deliberate differences: this closes its copies of the pty-side pipe ends
/// straight after `CreatePseudoConsole` (as Microsoft's own sample does —
/// alacritty leaks them via `into_raw_handle`), and it deletes the attribute
/// list once the process exists.
#[cfg(windows)]
mod windows {
    use super::{command_line, log_line, windows_exit_code};
    use crate::tty::{RawMode, Tty};
    use anyhow::{bail, Context, Result};
    use std::fs::File;
    use std::io::{BufWriter, Read, Write};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{HANDLE, S_OK, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Console::{
        ClosePseudoConsole, CreatePseudoConsole, GetConsoleMode, GetStdHandle, ResizePseudoConsole,
        COORD, HPCON, STD_INPUT_HANDLE,
    };
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
        InitializeProcThreadAttributeList, UpdateProcThreadAttribute, WaitForSingleObject,
        EXTENDED_STARTUPINFO_PRESENT, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
        STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW,
    };

    /// Fallback pty size when the recorder itself has no console (output piped
    /// in CI). 80x24 is what every program assumes anyway.
    const DEFAULT_SIZE: (u16, u16) = (24, 80);

    /// How long the main loop waits on the child before rechecking the console
    /// size. It only bounds resize latency, so it can be lazy.
    const TICK: Duration = Duration::from_millis(100);

    /// I/O buffer, matching the Unix recorder's.
    const CHUNK: usize = 8192;

    /// What the two forwarding threads send the log writer. Timestamps are
    /// taken by the sender, at the moment the bytes moved, so the file stays in
    /// real time order even though two threads produce it.
    enum Msg {
        Line(f64, &'static str, Vec<u8>),
        Stop,
    }

    /// RAII pseudoconsole. `ClosePseudoConsole` also closes the pty ends of
    /// both pipes, which is what finally gives the output reader its EOF.
    struct Conpty(HPCON);

    impl Drop for Conpty {
        fn drop(&mut self) {
            // Blocks until the output pipe has been drained, so the reader
            // thread must still be alive here — see the shutdown order below.
            //
            // SAFETY: a handle we created and have not closed.
            unsafe { ClosePseudoConsole(self.0) }
        }
    }

    /// One anonymous pipe as `(read, write)`, with default buffering.
    fn pipe() -> Result<(OwnedHandle, OwnedHandle)> {
        let mut read: HANDLE = std::ptr::null_mut();
        let mut write: HANDLE = std::ptr::null_mut();
        // SAFETY: two handle out-parameters we own; null attributes means the
        // default (non-inheritable) security, which is what we want — the
        // pseudoconsole duplicates the ends it needs rather than inheriting.
        if unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) } == 0 {
            return Err(std::io::Error::last_os_error()).context("CreatePipe");
        }
        // SAFETY: two fresh valid handles that nothing else owns.
        unsafe {
            Ok((
                OwnedHandle::from_raw_handle(read),
                OwnedHandle::from_raw_handle(write),
            ))
        }
    }

    fn raw(h: &OwnedHandle) -> HANDLE {
        h.as_raw_handle() as HANDLE
    }

    fn coord(rows: u16, cols: u16) -> COORD {
        COORD {
            X: cols.min(i16::MAX as u16) as i16,
            Y: rows.min(i16::MAX as u16) as i16,
        }
    }

    /// Drain the channel into the recording. Owning the file in one thread is
    /// what keeps the two directions from interleaving mid-line.
    fn write_log(mut file: BufWriter<File>, rx: Receiver<Msg>) {
        while let Ok(msg) = rx.recv() {
            match msg {
                Msg::Line(t, dir, bytes) => {
                    let _ = writeln!(file, "{}", log_line(t, dir, &bytes));
                }
                Msg::Stop => break,
            }
        }
        let _ = file.flush();
    }

    /// Program -> terminal. Forwarded to stdout first so the user never waits
    /// on the logger.
    fn forward_output(mut conout: File, tx: Sender<Msg>, start: Instant) {
        let mut buf = [0u8; CHUNK];
        let mut stdout = std::io::stdout();
        loop {
            // Ok(0) is EOF; an error here is the broken pipe left behind by
            // ClosePseudoConsole. Either way the child is done talking.
            let n = match conout.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let _ = stdout.write_all(&buf[..n]);
            let _ = stdout.flush();
            let _ = tx.send(Msg::Line(
                start.elapsed().as_secs_f64(),
                "out",
                buf[..n].to_vec(),
            ));
        }
    }

    /// Terminal -> program: keystrokes *and* the query replies this whole
    /// command exists to capture.
    ///
    /// `console` picks where input comes from, reproducing the split the Unix
    /// side gets for free by reading stdin: an interactive run is read from the
    /// console as VT-decoded input records, so arrow keys and the terminal's
    /// query replies survive, while `terra record ... < input.txt` reads the
    /// redirected stdin as the ordinary byte stream it is.
    ///
    /// This thread is never joined. The console branch stops on `stop` within
    /// one tick; the stdin branch may sit in a blocking read forever, and
    /// waiting for a pipe that nobody will ever close is not worth delaying the
    /// exit status for.
    fn forward_input(
        mut conin: File,
        tx: Sender<Msg>,
        start: Instant,
        stop: Arc<AtomicBool>,
        console: bool,
    ) {
        let mut send = |bytes: Vec<u8>| -> bool {
            if conin.write_all(&bytes).is_err() || conin.flush().is_err() {
                return false;
            }
            tx.send(Msg::Line(start.elapsed().as_secs_f64(), "in", bytes))
                .is_ok()
        };

        // The console is opened here rather than moved in: a console handle is
        // not `Send`, and the mode set by the main thread's `RawMode` belongs
        // to the console's input buffer, so it applies to this handle too.
        if let Some(mut tty) = console.then(Tty::open).and_then(Result::ok) {
            while !stop.load(Ordering::Relaxed) {
                match tty.read_timed(TICK) {
                    // The wait expired, or records with no bytes in them.
                    None => continue,
                    Some(bytes) if bytes.is_empty() => continue,
                    Some(bytes) => {
                        if !send(bytes) {
                            return;
                        }
                    }
                }
            }
            return;
        }
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; CHUNK];
        loop {
            match stdin.read(&mut buf) {
                // Input at EOF: stop forwarding, but let the child run on — it
                // may have plenty left to say.
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    if !send(buf[..n].to_vec()) {
                        return;
                    }
                }
            }
        }
    }

    /// Is our stdin the console itself, rather than a redirected file or pipe?
    ///
    /// `GetConsoleMode` is the standard test: it is the one call that only
    /// succeeds on a console handle.
    fn stdin_is_console() -> bool {
        // SAFETY: GetStdHandle is infallible in the sense that matters here —
        // a bad value simply fails GetConsoleMode; `mode` is a u32 we own.
        unsafe {
            let mut mode: u32 = 0;
            GetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), &mut mode) != 0
        }
    }

    /// Start `argv` attached to `pty`, returning its process handle.
    fn spawn(pty: &Conpty, argv: &[String]) -> Result<OwnedHandle> {
        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        // Set with all three handles left null, so the child inherits none of
        // ours: everything it needs comes through the pseudoconsole.
        startup.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;

        // First call sizes the list; it is *expected* to fail, having been
        // given nowhere to write.
        let mut size: usize = 0;
        // SAFETY: null list plus a size out-parameter is the documented way to
        // ask for the required length.
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        }
        if size == 0 {
            return Err(std::io::Error::last_os_error()).context("size proc-thread attribute list");
        }
        let mut list: Box<[u8]> = vec![0u8; size].into_boxed_slice();
        // The list is an opaque blob, not a struct, so the alignment lint does
        // not apply — same cast alacritty makes.
        #[allow(clippy::cast_ptr_alignment)]
        {
            startup.lpAttributeList = list.as_mut_ptr() as _;
        }
        // SAFETY: `list` is `size` bytes, exactly what the sizing call asked
        // for, and outlives every use below.
        if unsafe { InitializeProcThreadAttributeList(startup.lpAttributeList, 1, 0, &mut size) }
            == 0
        {
            return Err(std::io::Error::last_os_error())
                .context("InitializeProcThreadAttributeList");
        }
        // Deletes the list however this function leaves.
        let _list_guard = AttributeList(startup.lpAttributeList);

        // SAFETY: the HPCON outlives the child (its guard is held by the
        // caller), and the size is that of the value being passed by pointer.
        let ok = unsafe {
            UpdateProcThreadAttribute(
                startup.lpAttributeList,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                pty.0 as *const std::ffi::c_void,
                std::mem::size_of::<HPCON>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error()).context("attach pseudoconsole");
        }

        // Mutable: CreateProcessW takes the command line as a writable buffer
        // and may scribble on it.
        let mut cmdline: Vec<u16> = command_line(argv)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: `cmdline` is NUL-terminated and writable, `startup` and
        // `info` are ours, and every pointer outlives the call. A null
        // application name means the command line's first token is resolved
        // against PATH, which is what `terra record -- foo` should do.
        let ok = unsafe {
            CreateProcessW(
                std::ptr::null(),
                cmdline.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0, // inherit handles: no
                EXTENDED_STARTUPINFO_PRESENT,
                std::ptr::null(),
                std::ptr::null(),
                &startup.StartupInfo as *const STARTUPINFOW,
                &mut info,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("run {}", argv[0]));
        }
        // SAFETY: two fresh handles CreateProcessW just gave us. The thread
        // handle is of no use here and is closed by the drop below.
        unsafe {
            let _thread = OwnedHandle::from_raw_handle(info.hThread);
            Ok(OwnedHandle::from_raw_handle(info.hProcess))
        }
    }

    /// Frees a proc-thread attribute list on the way out, including the error
    /// paths between initialising it and handing it to `CreateProcessW`.
    struct AttributeList(*mut std::ffi::c_void);

    impl Drop for AttributeList {
        fn drop(&mut self) {
            // SAFETY: initialised by us, and the process it configured has
            // already been created.
            unsafe { DeleteProcThreadAttributeList(self.0) }
        }
    }

    pub fn record(out: &Path, argv: &[String]) -> Result<i32> {
        if argv.is_empty() {
            bail!("nothing to record: pass the program after `--`, e.g. terra record --out s.jsonl -- pwsh");
        }
        let file = BufWriter::new(
            std::fs::File::create(out).with_context(|| format!("create {}", out.display()))?,
        );

        // The console, if there is one: it gives the pty its size and is where
        // raw mode has to be applied so keystrokes reach the child intact.
        let tty = Tty::open().ok();
        let mut size = tty
            .as_ref()
            .and_then(|t| t.size().ok())
            .filter(|&(r, c)| r > 0 && c > 0)
            .unwrap_or(DEFAULT_SIZE);

        let (conin_read, conin_write) = pipe().context("input pipe")?;
        let (conout_read, conout_write) = pipe().context("output pipe")?;

        let mut handle: HPCON = 0;
        // SAFETY: both pipe ends are live, and `handle` is ours to fill.
        let hr = unsafe {
            CreatePseudoConsole(
                coord(size.0, size.1),
                raw(&conin_read),
                raw(&conout_write),
                0,
                &mut handle,
            )
        };
        if hr != S_OK {
            bail!("CreatePseudoConsole failed (HRESULT {hr:#010x})");
        }
        let pty = Conpty(handle);
        // The pseudoconsole duplicated what it needs; our copies of the pty-side
        // ends must go, or the output reader would never see EOF.
        drop(conin_read);
        drop(conout_write);

        // The output reader starts before the child, not after: dropping `pty`
        // on a failed spawn would otherwise block in `ClosePseudoConsole`
        // waiting for a pipe nobody is draining.
        let start = Instant::now();
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let logger = std::thread::spawn(move || write_log(file, rx));
        let reader = {
            let tx = tx.clone();
            std::thread::spawn(move || forward_output(File::from(conout_read), tx, start))
        };

        let child = spawn(&pty, argv)?;

        // Raw mode on the real console, so ^C, arrow keys and paste go to the
        // child untouched, and so escape sequences we pass through are
        // interpreted rather than printed. The guard restores both console
        // modes on every exit path, panics included.
        let _raw = tty
            .as_ref()
            .and_then(|t| RawMode::enable_blocking(t.fd()).ok());

        {
            let tx = tx.clone();
            let stop = Arc::clone(&stop);
            let console = stdin_is_console();
            std::thread::spawn(move || {
                forward_input(File::from(conin_write), tx, start, stop, console)
            });
        }

        // Wait for the child, checking the console's shape on every tick. A
        // ConPTY has no SIGWINCH, and polling here costs one cheap call per
        // tenth of a second.
        loop {
            // SAFETY: `child` is live for the whole loop.
            if unsafe { WaitForSingleObject(raw(&child), TICK.as_millis() as u32) } == WAIT_OBJECT_0
            {
                break;
            }
            if let Some(current) = tty.as_ref().and_then(|t| t.size().ok()) {
                if current != size && current.0 > 0 && current.1 > 0 {
                    size = current;
                    // SAFETY: the pseudoconsole is alive until `pty` drops.
                    unsafe { ResizePseudoConsole(pty.0, coord(size.0, size.1)) };
                }
            }
        }

        stop.store(true, Ordering::Relaxed);
        // Order matters: closing the pseudoconsole flushes the child's last
        // output and then breaks the pipe, which is what ends the reader — so
        // it has to happen while the reader is still draining, and the reader
        // has to be joined before the log is closed.
        drop(pty);
        let _ = reader.join();
        let _ = tx.send(Msg::Stop);
        let _ = logger.join();

        let mut status: u32 = 0;
        // SAFETY: `child` is still open; `status` is a u32 we own.
        if unsafe { GetExitCodeProcess(raw(&child), &mut status) } == 0 {
            return Err(std::io::Error::last_os_error()).context("GetExitCodeProcess");
        }
        Ok(windows_exit_code(status))
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

    #[test]
    fn a_plain_command_line_is_joined_with_spaces() {
        assert_eq!(command_line(&["pwsh".into()]), "pwsh");
        assert_eq!(
            command_line(&["git".into(), "status".into(), "--short".into()]),
            "git status --short"
        );
        assert_eq!(command_line(&[]), "");
    }

    /// The child re-splits this string with `CommandLineToArgvW`, so anything
    /// that round-trips wrong here silently changes the recorded program's
    /// arguments.
    #[test]
    fn arguments_that_need_quoting_get_it() {
        // Whitespace and empty arguments must survive as one argument each.
        assert_eq!(
            command_line(&["git".into(), "-m".into(), "two words".into()]),
            r#"git -m "two words""#
        );
        assert_eq!(command_line(&["x".into(), String::new()]), r#"x """#);
        assert_eq!(command_line(&["a\tb".into()]), "\"a\tb\"");
        // A quote is escaped, and so is the backslash run in front of it.
        assert_eq!(command_line(&[r#"say "hi""#.into()]), r#""say \"hi\"""#);
        assert_eq!(command_line(&[r#"a\"b"#.into()]), r#""a\\\"b""#);
        // Backslashes not followed by a quote stay literal...
        assert_eq!(command_line(&[r"C:\dir\file".into()]), r"C:\dir\file");
        assert_eq!(command_line(&[r"C:\my dir\x".into()]), r#""C:\my dir\x""#);
        // ...but a trailing run is doubled, or it would escape our own quote.
        assert_eq!(command_line(&[r"C:\my dir\".into()]), r#""C:\my dir\\""#);
    }

    #[test]
    fn a_windows_exit_status_passes_through_whole() {
        assert_eq!(windows_exit_code(0), 0);
        assert_eq!(windows_exit_code(3), 3);
        // Ctrl-C kills a console process with this NTSTATUS; the bits are kept
        // rather than folded onto a byte, so `echo %ERRORLEVEL%` still matches.
        assert_eq!(windows_exit_code(0xc000_013a) as u32, 0xc000_013a);
        assert_eq!(windows_exit_code(u32::MAX) as u32, u32::MAX);
    }

    /// The stub has to *fail*, not silently succeed with an empty recording —
    /// a caller that ignores this would produce a file nobody could tell from
    /// a real one.
    #[test]
    #[cfg(not(any(unix, windows)))]
    fn recording_is_refused_with_an_explanation_off_unix() {
        let err = record(Path::new("unused.jsonl"), &["vim".to_string()]).unwrap_err();
        assert!(err.to_string().contains("not supported on this platform"));
    }
}
