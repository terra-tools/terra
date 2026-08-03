//! Per-tab transcripts: a bounded ring of the raw bytes a tab's child process
//! wrote, plus the escape-stripper that turns them back into readable text.
//!
//! # Why this exists
//!
//! `terra capture --scrollback` reads the terminal's grid, and a full-screen
//! program leaves nothing there to read: the alternate screen has no
//! scrollback by definition, and when the program exits the primary screen is
//! restored exactly as it was. Everything Claude Code, `htop` or `less` ever
//! painted is gone the moment it is overwritten.
//!
//! terra is the process those bytes arrived at, so the cheapest possible fix
//! is to keep them. [`Ring`] holds the last `[tabs] transcript_kb` kilobytes
//! of one tab's child→terminal stream, overwriting oldest-first, and
//! [`render`] strips the escape sequences back out so the result reads like
//! text.
//!
//! # In memory only
//!
//! A transcript is every byte a program printed — command output, file
//! contents, whatever scrolled past. It is never written to disk, never
//! copied into a log, and dies with the tab. The only way out is
//! `terra transcript <tab>`, over the same `0700` socket as everything else.
//!
//! # Fidelity
//!
//! [`render`] is a stripper, not a terminal. It deletes escape sequences and
//! keeps the printable text between them, so cursor motion is not replayed: a
//! program that repaints one screen a hundred times leaves a hundred copies of
//! it, in the order they were painted. That is the honest answer for a
//! *history* — the repeated frames are what actually happened — but it is not
//! what the screen looked like. For the current screen, `terra capture` is
//! still the right tool.

use std::sync::{Arc, Mutex};

/// A fixed-capacity byte ring: push at the end, overwrite the oldest.
///
/// Deliberately dumb — bytes in, bytes out, in order. It knows nothing about
/// terminals, so [`render`] can be tested against it in isolation and the
/// wraparound can be tested without a PTY.
///
/// Allocation is lazy: `Ring::new(cap)` allocates nothing, and the buffer
/// grows to `cap` only as bytes actually arrive. A tab that never prints
/// anything therefore costs nothing beyond the struct itself, which matters
/// when the cap is a megabyte and the window holds twenty tabs.
#[derive(Debug)]
pub struct Ring {
    /// Maximum bytes retained. `0` disables the ring: every push is dropped
    /// and nothing is ever allocated.
    cap: usize,
    /// The bytes. Shorter than `cap` until the ring first fills; exactly `cap`
    /// long forever after.
    buf: Vec<u8>,
    /// Index of the oldest byte, and so also of the next byte to overwrite.
    /// Always `0` while `buf.len() < cap`.
    start: usize,
}

impl Ring {
    /// A ring holding at most `cap` bytes. Allocates nothing until the first
    /// [`push`](Self::push).
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            buf: Vec::new(),
            start: 0,
        }
    }

    /// Append `data`, dropping as much of the oldest content as it takes to
    /// fit.
    ///
    /// A single write larger than the whole ring keeps only its last `cap`
    /// bytes — the same thing that would happen if it arrived in pieces.
    pub fn push(&mut self, data: &[u8]) {
        if self.cap == 0 || data.is_empty() {
            return;
        }

        // One write bigger than the ring: everything before its tail is
        // already destined to be overwritten by the rest of this very write.
        let data = if data.len() > self.cap {
            &data[data.len() - self.cap..]
        } else {
            data
        };

        // Still filling for the first time: plain append, no wraparound
        // possible, and this is where the buffer actually grows.
        if self.buf.len() < self.cap {
            let room = self.cap - self.buf.len();
            let take = room.min(data.len());
            self.buf.extend_from_slice(&data[..take]);
            if take == data.len() {
                return;
            }
            // The rest wraps into the (now full) buffer, starting at 0.
            return self.overwrite(&data[take..]);
        }

        self.overwrite(data)
    }

    /// Write over the oldest bytes of a full buffer, wrapping at the end.
    /// `data.len() <= cap == buf.len()` is guaranteed by the caller.
    fn overwrite(&mut self, data: &[u8]) {
        let tail = self.cap - self.start;
        if data.len() <= tail {
            self.buf[self.start..self.start + data.len()].copy_from_slice(data);
        } else {
            self.buf[self.start..].copy_from_slice(&data[..tail]);
            self.buf[..data.len() - tail].copy_from_slice(&data[tail..]);
        }
        self.start = (self.start + data.len()) % self.cap;
    }

    /// Everything the ring holds, oldest byte first.
    pub fn snapshot(&self) -> Vec<u8> {
        if self.start == 0 {
            return self.buf.clone();
        }
        let mut out = Vec::with_capacity(self.buf.len());
        out.extend_from_slice(&self.buf[self.start..]);
        out.extend_from_slice(&self.buf[..self.start]);
        out
    }

    /// Bytes currently retained. Reaches — and then stays at — the cap.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.buf.len()
    }
}

/// A tab's transcript, in the shape the PTY reader and the IPC threads share
/// it: the reader pushes, an IPC thread snapshots.
pub type Shared = Arc<Mutex<Ring>>;

/// Take the transcript lock, ignoring poisoning — the same rule the tab lock
/// follows: a panic on one thread must not silently disable readback.
pub fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Ring> {
    shared.lock().unwrap_or_else(|err| err.into_inner())
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

/// Strip the escape sequences out of a raw transcript, leaving the text.
///
/// Handled: CSI (`ESC [ … final`), OSC / DCS / SOS / PM / APC (`ESC ] P X ^ _`
/// … `BEL` or `ESC \`), charset designators (`ESC ( B` and friends), and the
/// two-byte `ESC <char>` escapes. `\t` and `\n` survive; `\r\n` collapses to
/// `\n` and a lone `\r` becomes one, because a carriage return in a raw stream
/// means "paint over the line just written" and a newline is the readable form
/// of that. Other C0 controls are dropped.
///
/// Not handled, on purpose: the 8-bit C1 forms (`0x9b` as CSI), which cannot
/// be told apart from UTF-8 continuation bytes without decoding first, and
/// cursor motion of any kind — see the module docs on fidelity.
///
/// Invalid UTF-8 (which a truncated ring produces at its front edge by
/// construction) becomes U+FFFD rather than an error.
pub fn render(raw: &[u8]) -> String {
    let mut out: Vec<u8> = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let byte = raw[i];
        match byte {
            ESC => i = skip_escape(raw, i),
            b'\r' => {
                // `\r\n` is one line ending, not two.
                if raw.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                out.push(b'\n');
                i += 1;
            }
            b'\n' | b'\t' => {
                out.push(byte);
                i += 1;
            }
            // Everything else in C0, plus DEL: not text.
            0x00..=0x1f | 0x7f => i += 1,
            _ => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Index just past the escape sequence starting at `i` (which is an `ESC`).
/// An unterminated sequence — the ring's front edge cut one in half, or the
/// program was killed mid-write — consumes the rest of the input, which is
/// the reading that cannot leak stray bracket-noise into the text.
fn skip_escape(raw: &[u8], i: usize) -> usize {
    let Some(&kind) = raw.get(i + 1) else {
        return raw.len();
    };
    match kind {
        // CSI: parameter and intermediate bytes, then one final byte.
        b'[' => {
            let mut j = i + 2;
            while j < raw.len() && !(0x40..=0x7e).contains(&raw[j]) {
                j += 1;
            }
            (j + 1).min(raw.len())
        }
        // String sequences, terminated by BEL or ST (`ESC \`).
        b']' | b'P' | b'X' | b'^' | b'_' => {
            let mut j = i + 2;
            while j < raw.len() {
                if raw[j] == BEL {
                    return j + 1;
                }
                if raw[j] == ESC {
                    // `ESC \` ends it; any other ESC means the terminator was
                    // lost, and the new sequence starts here.
                    return if raw.get(j + 1) == Some(&b'\\') {
                        j + 2
                    } else {
                        j
                    };
                }
                j += 1;
            }
            raw.len()
        }
        // Charset designation: `ESC ( B`, `ESC ) 0`, …
        b'(' | b')' | b'*' | b'+' => (i + 3).min(raw.len()),
        // Everything else is `ESC <one byte>`: ESC 7, ESC =, ESC M, …
        _ => i + 2,
    }
}

/// The last `n` lines of `text`, or all of it when `n` is `None`.
///
/// Lines, not bytes, because the rendered form is text and "the last 40 lines"
/// is the question people actually ask of it. A trailing newline does not
/// count as an extra empty line.
pub fn tail_lines(text: &str, n: Option<usize>) -> String {
    let Some(n) = n else {
        return text.to_string();
    };
    if n == 0 {
        return String::new();
    }
    let body = text.strip_suffix('\n').unwrap_or(text);
    let total = body.split('\n').count();
    let skip = total.saturating_sub(n);
    let mut out: String = body.split('\n').skip(skip).collect::<Vec<_>>().join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// The last `n` *bytes* of `raw`, or all of it when `n` is `None`.
///
/// Bytes rather than lines: the raw form has no lines to speak of — a
/// full-screen program can repaint for minutes without emitting a single
/// newline — so a byte count is the only limit that means anything there.
pub fn tail_bytes(raw: &[u8], n: Option<usize>) -> &[u8] {
    match n {
        Some(n) if n < raw.len() => &raw[raw.len() - n..],
        _ => raw,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_of(cap: usize, writes: &[&[u8]]) -> Vec<u8> {
        let mut ring = Ring::new(cap);
        for write in writes {
            ring.push(write);
        }
        ring.snapshot()
    }

    #[test]
    fn a_fresh_ring_holds_nothing_and_has_allocated_nothing() {
        let ring = Ring::new(1024);
        assert_eq!(ring.len(), 0);
        assert!(ring.snapshot().is_empty());
        // The point of the lazy allocation: a tab that never prints costs the
        // struct and no buffer at all.
        assert_eq!(ring.buf.capacity(), 0);
    }

    #[test]
    fn bytes_come_back_in_the_order_they_went_in() {
        assert_eq!(snapshot_of(16, &[b"abc", b"de", b"f"]), b"abcdef");
    }

    /// The exact boundary: filling the ring to the last byte must not wrap,
    /// and must not report truncation.
    #[test]
    fn filling_the_ring_exactly_keeps_everything() {
        let mut ring = Ring::new(4);
        ring.push(b"abcd");
        assert_eq!(ring.snapshot(), b"abcd");
        assert_eq!(ring.len(), 4);

        // …and one byte past it drops exactly one byte from the front.
        ring.push(b"e");
        assert_eq!(ring.snapshot(), b"bcde");
    }

    #[test]
    fn a_write_that_straddles_the_end_wraps_around() {
        // 4-byte ring: "abcd" fills it, then "efg" overwrites a, b, c.
        assert_eq!(snapshot_of(4, &[b"abcd", b"efg"]), b"defg");
        // A write that lands exactly on the end and then continues.
        assert_eq!(snapshot_of(4, &[b"ab", b"cdef"]), b"cdef");
        assert_eq!(snapshot_of(4, &[b"abc", b"defg"]), b"defg");
    }

    #[test]
    fn many_wraps_still_leave_the_last_cap_bytes() {
        let mut ring = Ring::new(5);
        // 26 single-byte writes over a 5-byte ring: five full wraps and a bit.
        for c in b'a'..=b'z' {
            ring.push(&[c]);
        }
        assert_eq!(ring.snapshot(), b"vwxyz");
        assert_eq!(ring.len(), 5);

        // The same in three-byte writes, which never divide the cap evenly.
        let mut ring = Ring::new(5);
        for chunk in b"abcdefghijklmnopqr".chunks(3) {
            ring.push(chunk);
        }
        assert_eq!(ring.snapshot(), b"nopqr");
    }

    #[test]
    fn one_write_bigger_than_the_ring_keeps_its_tail() {
        assert_eq!(snapshot_of(4, &[b"abcdefghij"]), b"ghij");
        // …including when the ring already held something.
        assert_eq!(snapshot_of(4, &[b"xy", b"abcdefghij"]), b"ghij");
        // …and the ring stays usable afterwards.
        assert_eq!(snapshot_of(4, &[b"abcdefghij", b"kl"]), b"ijkl");
    }

    #[test]
    fn a_zero_capacity_ring_never_allocates_and_never_holds_anything() {
        let mut ring = Ring::new(0);
        ring.push(b"lots and lots of output");
        assert!(ring.snapshot().is_empty());
        assert_eq!(ring.len(), 0);
        assert_eq!(ring.buf.capacity(), 0);
    }

    #[test]
    fn a_one_byte_ring_holds_the_last_byte() {
        assert_eq!(snapshot_of(1, &[b"abc"]), b"c");
        assert_eq!(snapshot_of(1, &[b"a", b"b"]), b"b");
    }

    // --- rendering ---------------------------------------------------------

    #[test]
    fn plain_text_survives_rendering_untouched() {
        assert_eq!(render(b"hello\tworld\n"), "hello\tworld\n");
    }

    #[test]
    fn csi_sequences_are_stripped() {
        // Colour, cursor motion, erase, and the alt-screen switch itself.
        let raw = b"\x1b[?1049h\x1b[2J\x1b[H\x1b[1;32mgreen\x1b[0m\x1b[?1049l";
        assert_eq!(render(raw), "green");
        // Parameters with intermediates (`ESC [ ? 2 5 h`) and no parameters.
        assert_eq!(render(b"a\x1b[Kb\x1b[?25lc"), "abc");
    }

    #[test]
    fn osc_titles_are_stripped_whichever_terminator_they_use() {
        assert_eq!(render(b"\x1b]0;my title\x07text"), "text");
        assert_eq!(render(b"\x1b]0;my title\x1b\\text"), "text");
        // A DCS/APC string goes the same way.
        assert_eq!(render(b"\x1bPq#0;2;0;0;0\x1b\\ok"), "ok");
        assert_eq!(render(b"\x1b_G a=T\x1b\\ok"), "ok");
    }

    #[test]
    fn short_escapes_and_charset_designators_are_stripped() {
        assert_eq!(render(b"\x1b(B\x1b)0abc"), "abc");
        assert_eq!(render(b"\x1b7save\x1b8"), "save");
        assert_eq!(render(b"\x1b=x\x1b>y"), "xy");
    }

    #[test]
    fn an_unterminated_escape_eats_itself_rather_than_leaking_noise() {
        assert_eq!(render(b"text\x1b[38;5;"), "text");
        assert_eq!(render(b"text\x1b]0;unfinished"), "text");
        assert_eq!(render(b"text\x1b"), "text");
        // A lost string terminator must not swallow the rest of the file:
        // the next ESC ends it.
        assert_eq!(render(b"\x1b]0;lost\x1b[1mkept"), "kept");
    }

    #[test]
    fn carriage_returns_become_line_breaks() {
        // A progress bar repainting one line reads as its successive states.
        assert_eq!(render(b"10%\r20%\r30%\r\n"), "10%\n20%\n30%\n");
        assert_eq!(render(b"a\r\nb\n"), "a\nb\n");
    }

    #[test]
    fn other_control_bytes_are_dropped() {
        assert_eq!(render(b"a\x00b\x08c\x7fd"), "abcd");
    }

    #[test]
    fn invalid_utf8_at_the_front_edge_becomes_a_replacement_char() {
        // What a truncated ring looks like: a multi-byte char cut in half.
        let text = render("é".as_bytes().split_at(1).1);
        assert_eq!(text, "\u{fffd}");
        assert_eq!(render("héllo".as_bytes()), "héllo");
    }

    /// The end-to-end shape of the feature: an alt-screen program's frames
    /// survive in the ring even though the screen was cleared between them.
    #[test]
    fn a_repainted_screen_leaves_one_copy_per_frame() {
        let mut ring = Ring::new(4096);
        ring.push(b"\x1b[?1049h");
        for frame in 1..=3 {
            ring.push(b"\x1b[2J\x1b[H");
            ring.push(format!("frame {frame}\r\n").as_bytes());
        }
        ring.push(b"\x1b[?1049l");
        assert_eq!(render(&ring.snapshot()), "frame 1\nframe 2\nframe 3\n");
    }

    // --- tails -------------------------------------------------------------

    #[test]
    fn tail_lines_keeps_the_last_n() {
        assert_eq!(tail_lines("a\nb\nc\n", Some(2)), "b\nc\n");
        assert_eq!(tail_lines("a\nb\nc", Some(2)), "b\nc");
        assert_eq!(tail_lines("a\nb\nc\n", Some(9)), "a\nb\nc\n");
        assert_eq!(tail_lines("a\nb\nc\n", None), "a\nb\nc\n");
        assert_eq!(tail_lines("a\nb\nc\n", Some(0)), "");
        assert_eq!(tail_lines("", Some(3)), "");
    }

    #[test]
    fn tail_bytes_keeps_the_last_n() {
        assert_eq!(tail_bytes(b"abcdef", Some(2)), b"ef");
        assert_eq!(tail_bytes(b"abcdef", Some(99)), b"abcdef");
        assert_eq!(tail_bytes(b"abcdef", None), b"abcdef");
        assert_eq!(tail_bytes(b"abcdef", Some(0)), b"");
    }
}
