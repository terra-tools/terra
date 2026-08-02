//! Talking to the controlling terminal directly, bypassing stdin/stdout.
//!
//! `doctor` and `record` both need the real terminal rather than stdin/stdout:
//! stdout may be a pipe (`terra doctor > report.txt` must still probe), and a
//! probe must not be answered by a shell that happens to be on stdin. On Unix
//! that handle is `/dev/tty`; on Windows it is `CONIN$`/`CONOUT$`, which name
//! the console itself and are equally immune to redirection.
//!
//! Raw mode is entered through [`RawMode`], whose `Drop` restores what it
//! found. Restoring is not optional: a CLI that returns to a shell with echo
//! off has broken the user's session, so the guard covers early returns, `?`,
//! and panics (which unwind by default).
//!
//! # Portability
//!
//! Following `terra-app`'s `macos.rs`: each platform's real implementation
//! lives in its own `cfg` module, all of them the same shape, and callers name
//! [`Tty`] / [`RawMode`] with no `cfg` at the use site.
//!
//! * **Unix** — termios (`cfmakeraw`, `VMIN`/`VTIME` for the read deadline)
//!   and `TIOCGWINSZ`.
//! * **Windows** — `GetConsoleMode`/`SetConsoleMode` with
//!   `ENABLE_VIRTUAL_TERMINAL_INPUT` on the input buffer and
//!   `ENABLE_VIRTUAL_TERMINAL_PROCESSING` on the screen buffer (without the
//!   latter the probe bytes are printed rather than interpreted), plus
//!   `GetConsoleScreenBufferInfo` for the size. There is no `VMIN`/`VTIME`
//!   equivalent, so the deadline is `WaitForSingleObject` on the input handle
//!   followed by a read of exactly the records already queued — a probe that
//!   nothing answers times out instead of parking the process forever.
//! * **Anything else** — a stub whose `Tty::open` fails with one clear
//!   sentence, which is the path `doctor` already takes in CI.

/// How long a single terminal query may take to answer. Terminals reply in
/// microseconds over a pty; 200ms is generous even over ssh, and it bounds the
/// worst case (a terminal that never answers) at "one blink per probe".
pub const QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

// `window_size`/`set_window_size` are only called by `record`; re-exported
// unconditionally so the two modules keep one import list rather than a `cfg`
// at every use site.
#[allow(unused_imports)]
pub use imp::{window_size, RawMode, Tty};

#[cfg(unix)]
pub use imp::set_window_size;

/// Does this look like a whole reply? Terminal replies end with the final byte
/// of a CSI sequence or with a string terminator; recognising that shaves the
/// timeout off every probe that *does* answer.
///
/// Pure, and therefore compiled and tested everywhere even though only the
/// platform readers call it.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
fn response_is_complete(buf: &[u8]) -> bool {
    match buf.last() {
        Some(b'c') | Some(b'R') | Some(b'y') | Some(b'S') | Some(b'\\') | Some(&0x07) => {
            buf.first() == Some(&0x1b)
        }
        _ => false,
    }
}

/// Portable mirror of the Win32 console mode bits, so the mode arithmetic below
/// is a pure function that compiles and is tested on every platform rather than
/// only where it runs. The Windows module `const`-asserts each value against
/// `windows_sys`, so a mismatch is a build error, not a runtime surprise.
#[allow(dead_code)]
mod console_mode {
    /// `ENABLE_PROCESSED_INPUT`: the console turns ^C into a signal. Raw mode
    /// clears it, matching termios `ISIG`, so ^C reaches the program.
    pub const PROCESSED_INPUT: u32 = 0x0001;
    /// `ENABLE_LINE_INPUT`: reads only complete lines (termios `ICANON`).
    pub const LINE_INPUT: u32 = 0x0002;
    /// `ENABLE_ECHO_INPUT` (termios `ECHO`).
    pub const ECHO_INPUT: u32 = 0x0004;
    /// `ENABLE_VIRTUAL_TERMINAL_INPUT`: keys arrive as VT sequences, which is
    /// what makes a Windows console speak the same language as a pty.
    pub const VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;
    /// `ENABLE_PROCESSED_OUTPUT`: required before VT output processing counts.
    pub const PROCESSED_OUTPUT: u32 = 0x0001;
    /// `ENABLE_VIRTUAL_TERMINAL_PROCESSING`: escape sequences we write are
    /// interpreted instead of printed.
    pub const VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
}

/// Raw input mode: drop the cooked-mode bits, add VT input.
#[cfg_attr(not(windows), allow(dead_code))]
fn raw_input_mode(saved: u32) -> u32 {
    (saved & !(console_mode::PROCESSED_INPUT | console_mode::LINE_INPUT | console_mode::ECHO_INPUT))
        | console_mode::VIRTUAL_TERMINAL_INPUT
}

/// Output mode that interprets escape sequences. Purely additive: everything
/// else about the user's console is left as found.
#[cfg_attr(not(windows), allow(dead_code))]
fn vt_output_mode(saved: u32) -> u32 {
    saved | console_mode::PROCESSED_OUTPUT | console_mode::VIRTUAL_TERMINAL_PROCESSING
}

/// `(rows, cols)` from a console `SMALL_RECT` (`srWindow`), which is inclusive
/// on both ends — a 80x25 console reports `Right = 79`, `Bottom = 24`.
///
/// A degenerate rectangle yields 0, and callers treat that as "no size" rather
/// than resizing a pty to nothing.
#[cfg_attr(not(windows), allow(dead_code))]
fn size_from_window_rect(left: i16, top: i16, right: i16, bottom: i16) -> (u16, u16) {
    let rows = (i32::from(bottom) - i32::from(top) + 1).clamp(0, u16::MAX as i32) as u16;
    let cols = (i32::from(right) - i32::from(left) + 1).clamp(0, u16::MAX as i32) as u16;
    (rows, cols)
}

/// Decode UTF-16 code units from console key events into UTF-8 bytes.
///
/// `pending` carries a lone high surrogate across calls: an astral character
/// arrives as two key events and `ReadConsoleInput` can hand them back in two
/// different batches, so decoding each batch independently would corrupt it.
/// Unpaired surrogates that never get their partner are dropped rather than
/// replaced, because a replacement character in a byte-exact recording is worse
/// than a missing one.
#[cfg_attr(not(windows), allow(dead_code))]
fn utf16_to_bytes(units: &[u16], pending: &mut Option<u16>) -> Vec<u8> {
    const HIGH_SURROGATES: std::ops::Range<u16> = 0xd800..0xdc00;

    let mut buf: Vec<u16> = pending
        .take()
        .into_iter()
        .chain(units.iter().copied())
        .collect();
    // A trailing high surrogate is the first half of a pair whose second half
    // has not been read yet; hold it back for the next call.
    if let Some(&last) = buf.last() {
        if HIGH_SURROGATES.contains(&last) {
            *pending = Some(last);
            buf.pop();
        }
    }
    let mut out = Vec::with_capacity(buf.len());
    for ch in std::char::decode_utf16(buf).flatten() {
        let mut enc = [0u8; 4];
        out.extend_from_slice(ch.encode_utf8(&mut enc).as_bytes());
    }
    out
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

/// The Windows console, through `CONIN$`/`CONOUT$` and `SetConsoleMode`.
///
/// Same shape as the Unix module. The one structural difference is invisible
/// to callers: a console's input and output are two separate handles, so
/// [`Tty`] holds both and [`RawMode`] — which is handed only the input handle,
/// to keep `RawMode::enable_*(tty.fd(), …)` identical across platforms — opens
/// its own `CONOUT$` for the duration.
#[cfg(windows)]
mod imp {
    use super::{
        console_mode, raw_input_mode, response_is_complete, size_from_window_rect, utf16_to_bytes,
        vt_output_mode, QUERY_TIMEOUT,
    };
    use anyhow::{Context, Result};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{
        GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, WriteFile, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetConsoleScreenBufferInfo, GetNumberOfConsoleInputEvents,
        ReadConsoleInputW, SetConsoleMode, CONSOLE_MODE, CONSOLE_SCREEN_BUFFER_INFO, INPUT_RECORD,
        KEY_EVENT,
    };
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    /// The portable mirror in [`super::console_mode`] is only sound if it
    /// matches the real header. Checked here, at build time.
    const _: () = {
        assert!(
            console_mode::PROCESSED_INPUT
                == windows_sys::Win32::System::Console::ENABLE_PROCESSED_INPUT
        );
        assert!(console_mode::LINE_INPUT == windows_sys::Win32::System::Console::ENABLE_LINE_INPUT);
        assert!(console_mode::ECHO_INPUT == windows_sys::Win32::System::Console::ENABLE_ECHO_INPUT);
        assert!(
            console_mode::VIRTUAL_TERMINAL_INPUT
                == windows_sys::Win32::System::Console::ENABLE_VIRTUAL_TERMINAL_INPUT
        );
        assert!(
            console_mode::PROCESSED_OUTPUT
                == windows_sys::Win32::System::Console::ENABLE_PROCESSED_OUTPUT
        );
        assert!(
            console_mode::VIRTUAL_TERMINAL_PROCESSING
                == windows_sys::Win32::System::Console::ENABLE_VIRTUAL_TERMINAL_PROCESSING
        );
    };

    /// Stands in for Unix's `RawFd`: what [`Tty::fd`] hands to [`RawMode`] and
    /// [`window_size`]. Borrowed, never owned — the [`Tty`] outlives it.
    pub type RawFd = HANDLE;

    /// At most this many input records are pulled out of the console in one
    /// go. Bounded so a paste cannot make us allocate without limit; whatever
    /// is left over is read on the next pass.
    const MAX_RECORDS: u32 = 512;

    /// NUL-terminated UTF-16, the only string form `CreateFileW` accepts.
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Open one of the console's two pseudo-files. `CONIN$`/`CONOUT$` name the
    /// console itself, so — like `/dev/tty` — they are unaffected by a
    /// redirected stdin or stdout, and they fail when there is no console at
    /// all, which is exactly the signal `doctor` wants.
    fn open_console(name: &str) -> Result<OwnedHandle> {
        let path = wide(name);
        // SAFETY: `path` is NUL-terminated and outlives the call; every other
        // argument is a plain constant.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(std::io::Error::last_os_error()).with_context(|| format!("open {name}"));
        }
        // SAFETY: a fresh, valid handle that nothing else owns.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    fn raw(h: &OwnedHandle) -> HANDLE {
        h.as_raw_handle() as HANDLE
    }

    fn get_mode(h: HANDLE) -> Result<CONSOLE_MODE> {
        let mut mode: CONSOLE_MODE = 0;
        // SAFETY: `h` is a live console handle; `mode` is a u32 we own.
        if unsafe { GetConsoleMode(h, &mut mode) } == 0 {
            return Err(std::io::Error::last_os_error()).context("GetConsoleMode");
        }
        Ok(mode)
    }

    fn set_mode(h: HANDLE, mode: CONSOLE_MODE) -> Result<()> {
        // SAFETY: `h` is a live console handle.
        if unsafe { SetConsoleMode(h, mode) } == 0 {
            return Err(std::io::Error::last_os_error()).context("SetConsoleMode");
        }
        Ok(())
    }

    /// Restores both console modes when dropped.
    ///
    /// Both, because making a console behave like a terminal takes two changes
    /// in two places: raw input on the input buffer and VT interpretation on
    /// the screen buffer. Leaving either behind is the Windows equivalent of
    /// returning to a shell with echo off.
    pub struct RawMode {
        input: HANDLE,
        saved_input: CONSOLE_MODE,
        output: OwnedHandle,
        saved_output: CONSOLE_MODE,
    }

    impl RawMode {
        /// Raw mode for probing. The deadline is not a property of the mode on
        /// Windows — there is no `VTIME` — so the timeout is applied by
        /// [`Tty::read_timed`] instead; the argument is accepted to keep one
        /// signature across platforms.
        pub fn enable_timed(input: RawFd, _timeout: Duration) -> Result<Self> {
            Self::enable(input)
        }

        /// Raw mode for pass-through forwarding. Identical here: a Windows
        /// console read is bounded by the caller's wait, not by the mode.
        pub fn enable_blocking(input: RawFd) -> Result<Self> {
            Self::enable(input)
        }

        fn enable(input: RawFd) -> Result<Self> {
            let saved_input = get_mode(input).context("read console input mode")?;
            let output = open_console("CONOUT$")?;
            let saved_output = get_mode(raw(&output)).context("read console output mode")?;

            set_mode(input, raw_input_mode(saved_input)).context("enter raw console input")?;
            let guard = Self {
                input,
                saved_input,
                output,
                saved_output,
            };
            // From here the guard owns the restore, so a failure below still
            // puts the input buffer back.
            set_mode(raw(&guard.output), vt_output_mode(saved_output))
                .context("enable console VT output")?;
            Ok(guard)
        }
    }

    impl Drop for RawMode {
        fn drop(&mut self) {
            // Restoring exactly what we read at construction; a failure here
            // has nowhere to go and nothing useful to do.
            let _ = set_mode(self.input, self.saved_input);
            let _ = set_mode(raw(&self.output), self.saved_output);
        }
    }

    /// The console, opened read/write on both of its handles.
    pub struct Tty {
        input: OwnedHandle,
        output: OwnedHandle,
        /// Half of a surrogate pair carried between reads. See
        /// [`super::utf16_to_bytes`].
        pending: Option<u16>,
    }

    impl Tty {
        /// Open the console. Fails when the process has none (a service, a
        /// detached build agent), which callers degrade on instead of dying.
        pub fn open() -> Result<Self> {
            Ok(Self {
                input: open_console("CONIN$")?,
                output: open_console("CONOUT$")?,
                pending: None,
            })
        }

        /// The input handle — what [`RawMode`] and reads work on.
        pub fn fd(&self) -> RawFd {
            raw(&self.input)
        }

        /// Window size as `(rows, cols)` from the screen buffer's visible
        /// window. `dwSize` is the *buffer*, which on a default console is
        /// 9001 lines tall; `srWindow` is what the user can see.
        pub fn size(&self) -> Result<(u16, u16)> {
            window_size(raw(&self.output))
        }

        /// Bytes typed (or replied) since the last call, waiting at most
        /// `timeout` for the first of them.
        ///
        /// `None` means the wait expired with the console silent — the caller's
        /// deadline, and the reason a probe nothing answers cannot hang.
        /// `Some(vec)` means records were consumed, possibly yielding no bytes
        /// (a focus change or a key release carries none).
        ///
        /// Only key-down events with a character are kept: `ReadConsoleInput`
        /// also reports key releases, mouse motion and focus, none of which a
        /// pty would ever have delivered.
        pub fn read_timed(&mut self, timeout: Duration) -> Option<Vec<u8>> {
            let ms = timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32;
            // SAFETY: `fd()` is a live handle for the lifetime of the call.
            if unsafe { WaitForSingleObject(self.fd(), ms) } != WAIT_OBJECT_0 {
                return None;
            }
            // The handle signals for *any* record, including ones that carry no
            // bytes, so read only what is queued: `ReadConsoleInput` blocks
            // when asked for more than that.
            let mut available: u32 = 0;
            // SAFETY: live handle, u32 out-parameter we own.
            if unsafe { GetNumberOfConsoleInputEvents(self.fd(), &mut available) } == 0 {
                return None;
            }
            if available == 0 {
                return Some(Vec::new());
            }
            let mut records = vec![INPUT_RECORD::default(); available.min(MAX_RECORDS) as usize];
            let mut read: u32 = 0;
            // SAFETY: `records` has `len()` slots and we ask for exactly that
            // many; `read` is a u32 out-parameter we own.
            let ok = unsafe {
                ReadConsoleInputW(
                    self.fd(),
                    records.as_mut_ptr(),
                    records.len() as u32,
                    &mut read,
                )
            };
            if ok == 0 {
                return None;
            }
            let units: Vec<u16> = records[..read as usize]
                .iter()
                .filter_map(|r| {
                    if u32::from(r.EventType) != KEY_EVENT {
                        return None;
                    }
                    // SAFETY: EventType says this record's union holds a
                    // KEY_EVENT_RECORD, which is what the field discriminates.
                    let key = unsafe { r.Event.KeyEvent };
                    // SAFETY: uChar is a union of two 16-bit fields laid over
                    // each other; UnicodeChar is the one ReadConsoleInputW
                    // fills.
                    let ch = unsafe { key.uChar.UnicodeChar };
                    (key.bKeyDown != 0 && ch != 0).then_some(ch)
                })
                .collect();
            Some(utf16_to_bytes(&units, &mut self.pending))
        }

        /// Write a query and collect the answer until the console goes quiet.
        ///
        /// The caller must already hold a [`RawMode`], otherwise the reply is
        /// line-buffered and echoed into the user's scrollback. An empty result
        /// means "no response" — never an error, because "this terminal does
        /// not implement this query" is a finding, not a failure.
        pub fn query(&mut self, request: &[u8]) -> Vec<u8> {
            if write_all(raw(&self.output), request).is_err() {
                return Vec::new();
            }
            let mut out = Vec::new();
            // Two expired waits in a row end the probe. Records that carry no
            // bytes do not count as silence, so a stray mouse move cannot cut a
            // slow reply short.
            let mut quiet = 0;
            while quiet < 2 && out.len() < 4096 {
                match self.read_timed(QUERY_TIMEOUT) {
                    None => quiet += 1,
                    Some(bytes) => {
                        out.extend_from_slice(&bytes);
                        if response_is_complete(&out) {
                            break;
                        }
                    }
                }
            }
            out
        }
    }

    /// `(rows, cols)` for a console screen-buffer handle.
    pub fn window_size(output: RawFd) -> Result<(u16, u16)> {
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = Default::default();
        // SAFETY: `output` is a live screen-buffer handle and the struct is
        // POD, written wholly by the call.
        if unsafe { GetConsoleScreenBufferInfo(output, &mut info) } == 0 {
            return Err(std::io::Error::last_os_error()).context("GetConsoleScreenBufferInfo");
        }
        let w = info.srWindow;
        Ok(size_from_window_rect(w.Left, w.Top, w.Right, w.Bottom))
    }

    /// `WriteFile` until the whole slice is out.
    pub fn write_all(h: HANDLE, mut buf: &[u8]) -> std::io::Result<()> {
        while !buf.is_empty() {
            let mut written: u32 = 0;
            // SAFETY: reading at most `buf.len()` bytes from `buf`; `written`
            // is a u32 out-parameter we own.
            let ok = unsafe {
                WriteFile(
                    h,
                    buf.as_ptr(),
                    buf.len() as u32,
                    &mut written,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
            if written == 0 {
                return Err(std::io::Error::from(std::io::ErrorKind::WriteZero));
            }
            buf = &buf[written as usize..];
        }
        Ok(())
    }
}

/// No `/dev/tty`, no console: the same API, shaped so callers need no `cfg`.
///
/// [`Tty::open`] is the single failure point, and it is one every caller
/// already handles — `doctor` turns it into `tty: unavailable: …` plus the
/// existing note, keeping the environment half of the report intact.
#[cfg(not(any(unix, windows)))]
mod imp {
    use anyhow::{bail, Result};

    /// Stand-in for the platform's terminal handle. Nothing here ever produces
    /// a real one — `Tty::open` fails first.
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

    const UNSUPPORTED: &str = "terra needs a terminal to talk to for this: a Unix controlling \
         terminal (/dev/tty) or a Windows console (CONIN$/CONOUT$)";
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
    fn raw_console_input_drops_cooked_mode_and_asks_for_vt() {
        // A stock console input mode: processed + line + echo + a bit we must
        // not touch (ENABLE_INSERT_MODE, 0x20).
        let saved = 0x0001 | 0x0002 | 0x0004 | 0x0020;
        let raw = raw_input_mode(saved);
        assert_eq!(raw & 0x0001, 0, "^C must reach the program");
        assert_eq!(raw & 0x0002, 0, "no line buffering");
        assert_eq!(raw & 0x0004, 0, "no echo");
        assert_eq!(raw & 0x0200, 0x0200, "keys as VT sequences");
        assert_eq!(raw & 0x0020, 0x0020, "unrelated bits are left alone");
        // Idempotent: entering raw mode twice is the same mode.
        assert_eq!(raw_input_mode(raw), raw);
    }

    #[test]
    fn vt_output_only_adds_bits() {
        let saved = 0x0002; // ENABLE_WRAP_AT_EOL_OUTPUT
        let vt = vt_output_mode(saved);
        assert_eq!(vt & 0x0002, 0x0002, "the caller's bits survive");
        assert_eq!(vt & 0x0004, 0x0004, "VT processing on");
        assert_eq!(vt & 0x0001, 0x0001, "processed output on");
        assert_eq!(vt_output_mode(vt), vt);
    }

    #[test]
    fn a_console_window_rect_is_inclusive_on_both_edges() {
        // An 80x25 console: Right=79, Bottom=24.
        assert_eq!(size_from_window_rect(0, 0, 79, 24), (25, 80));
        // Scrolled buffers put the window somewhere other than the origin.
        assert_eq!(size_from_window_rect(0, 300, 119, 341), (42, 120));
        // Degenerate/inverted rectangles clamp instead of wrapping around.
        assert_eq!(size_from_window_rect(0, 0, -1, -1), (0, 0));
        assert_eq!(size_from_window_rect(10, 10, 0, 0), (0, 0));
    }

    #[test]
    fn console_key_events_decode_to_the_bytes_a_pty_would_have_carried() {
        let mut pending = None;
        // A DA1 reply arrives one UTF-16 unit per key event.
        let units: Vec<u16> = "\x1b[?62;22c".encode_utf16().collect();
        assert_eq!(
            utf16_to_bytes(&units, &mut pending),
            b"\x1b[?62;22c".to_vec()
        );
        assert!(pending.is_none());
        // Non-ASCII becomes UTF-8, as it would over a pty.
        assert_eq!(utf16_to_bytes(&[0x00e9], &mut pending), "é".as_bytes());
    }

    #[test]
    fn a_surrogate_pair_split_across_two_reads_still_decodes() {
        // U+1F600, the two halves delivered in separate batches.
        let mut pending = None;
        assert!(utf16_to_bytes(&[0xd83d], &mut pending).is_empty());
        assert_eq!(pending, Some(0xd83d));
        assert_eq!(utf16_to_bytes(&[0xde00], &mut pending), "😀".as_bytes());
        assert!(pending.is_none());
        // And in one batch, with text either side.
        let mut pending = None;
        let units: Vec<u16> = "a😀b".encode_utf16().collect();
        assert_eq!(utf16_to_bytes(&units, &mut pending), "a😀b".as_bytes());
        assert!(pending.is_none());
    }

    #[test]
    fn an_unpaired_surrogate_is_dropped_rather_than_corrupting_the_stream() {
        let mut pending = None;
        // A low surrogate with no high half before it is not decodable.
        assert_eq!(utf16_to_bytes(&[0xdc00, b'x' as u16], &mut pending), b"x");
        assert!(pending.is_none());
        // A high surrogate followed by ordinary text: the orphan goes, the
        // text stays.
        let mut pending = Some(0xd83du16);
        assert_eq!(utf16_to_bytes(&[b'x' as u16], &mut pending), b"x");
        assert!(pending.is_none());
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

    /// Off Unix and Windows the whole module is a stub, and the contract
    /// callers rely on is that opening the terminal *fails* rather than
    /// pretending.
    #[test]
    #[cfg(not(any(unix, windows)))]
    fn without_a_terminal_opening_it_fails_cleanly() {
        assert!(Tty::open().is_err());
        assert!(window_size(-1).is_err());
    }
}
