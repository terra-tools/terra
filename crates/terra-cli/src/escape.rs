//! Byte escaping and escape-sequence naming, shared by `doctor` and `record`.
//!
//! Two separate jobs live here on purpose:
//!
//! * [`escape_bytes`] / [`unescape_bytes`] are the *storage* format. A terminal
//!   stream is arbitrary bytes — half-written UTF-8, 0x80-0xff from latin-1
//!   programs, raw control codes — but a JSONL file must be valid UTF-8. Lossy
//!   UTF-8 decoding would silently fold every invalid byte onto U+FFFD, which
//!   is exactly the kind of detail a terminal diff is looking for, so instead
//!   every byte outside printable ASCII becomes `\xNN`. That is lossless and
//!   round-trips, which is what lets `--decode` re-parse its own log.
//! * [`name_sequence`] / [`describe_bytes`] are the *reading* format: they turn
//!   the stored bytes back into something a human can diff by eye.

/// Escape a byte string into printable ASCII, losslessly.
///
/// Printable ASCII passes through so the common case stays readable in a plain
/// `less`; backslash is doubled; everything else (control codes, high bytes,
/// invalid UTF-8) becomes `\xNN` with lowercase hex.
pub fn escape_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

/// Inverse of [`escape_bytes`]. Unknown escapes are kept verbatim rather than
/// dropped, so a hand-edited log still decodes instead of losing data.
pub fn unescape_bytes(s: &str) -> Vec<u8> {
    let src = s.as_bytes();
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if src[i] == b'\\' && i + 1 < src.len() {
            match src[i + 1] {
                b'\\' => {
                    out.push(b'\\');
                    i += 2;
                    continue;
                }
                b'x' if i + 3 < src.len() => {
                    let hex = std::str::from_utf8(&src[i + 2..i + 4]).ok();
                    if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                        out.push(v);
                        i += 4;
                        continue;
                    }
                }
                _ => {}
            }
        }
        out.push(src[i]);
        i += 1;
    }
    out
}

/// One chunk of a terminal byte stream: either an escape sequence or the plain
/// text between sequences.
#[derive(Debug, PartialEq, Eq)]
pub enum Segment<'a> {
    Text(&'a [u8]),
    Esc(&'a [u8]),
}

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

/// Split a byte stream into escape sequences and the text between them.
///
/// A sequence truncated by the end of the chunk is still returned as `Esc` —
/// recordings are chunked by read boundaries, so an incomplete tail is normal
/// and dropping it would hide the very sequence being investigated.
pub fn split_sequences(bytes: &[u8]) -> Vec<Segment<'_>> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut text_start = 0;
    while i < bytes.len() {
        if bytes[i] != ESC {
            i += 1;
            continue;
        }
        if i > text_start {
            out.push(Segment::Text(&bytes[text_start..i]));
        }
        let end = sequence_end(bytes, i);
        out.push(Segment::Esc(&bytes[i..end]));
        i = end;
        text_start = end;
    }
    if text_start < bytes.len() {
        out.push(Segment::Text(&bytes[text_start..]));
    }
    out
}

/// Index one past the end of the escape sequence starting at `start`.
fn sequence_end(bytes: &[u8], start: usize) -> usize {
    let n = bytes.len();
    match bytes.get(start + 1) {
        // CSI: parameters and intermediates, then a final byte 0x40..=0x7e.
        Some(b'[') => {
            let mut i = start + 2;
            while i < n && !(0x40..=0x7e).contains(&bytes[i]) {
                i += 1;
            }
            (i + 1).min(n)
        }
        // String-ish introducers run until ST (ESC \) or BEL.
        Some(b']') | Some(b'P') | Some(b'_') | Some(b'^') | Some(b'X') => {
            let mut i = start + 2;
            while i < n {
                if bytes[i] == BEL {
                    return i + 1;
                }
                if bytes[i] == ESC && bytes.get(i + 1) == Some(&b'\\') {
                    return i + 2;
                }
                i += 1;
            }
            n
        }
        // Two-byte escapes (ESC =, ESC >, ESC (B, ...); the charset ones take a
        // third byte, which the generic +2 leaves as text — harmless for naming.
        Some(_) => (start + 2).min(n),
        None => n,
    }
}

/// Human name for a DEC private mode number, used by both `doctor`'s DECRQM
/// report and `record --decode`. Only modes that actually differ between
/// terminals are worth naming.
pub fn private_mode_name(mode: u16) -> Option<&'static str> {
    Some(match mode {
        1 => "cursor-keys-application",
        7 => "autowrap",
        12 => "cursor-blink",
        25 => "cursor-visible",
        1000 => "mouse-click-tracking",
        1002 => "mouse-drag-tracking",
        1003 => "mouse-any-tracking",
        1004 => "focus-reporting",
        1006 => "mouse-sgr-encoding",
        1049 => "alt-screen",
        2004 => "bracketed-paste",
        2026 => "synchronized-output",
        2027 => "grapheme-clustering",
        2031 => "color-scheme-updates",
        _ => return None,
    })
}

/// Decoded meaning of a DECRPM reply value (the `<v>` in `ESC [ ? m ; v $ y`).
pub fn decrpm_state(value: u16) -> &'static str {
    match value {
        0 => "not recognised",
        1 => "set",
        2 => "reset",
        3 => "permanently set",
        4 => "permanently reset",
        _ => "unknown",
    }
}

/// Name an escape sequence, e.g. `ESC[?2026$p` -> `DECRQM synchronized-output`.
///
/// Returns `None` for sequences with no interesting name; callers fall back to
/// printing the raw escaped bytes alone.
pub fn name_sequence(seq: &[u8]) -> Option<String> {
    if seq.first() != Some(&ESC) {
        return None;
    }
    match seq.get(1) {
        Some(b'[') => name_csi(&seq[2..]),
        Some(b']') => name_osc(&seq[2..]),
        Some(b'P') => {
            let body = strip_terminator(&seq[2..]);
            if body.starts_with(b">|") {
                Some(format!(
                    "XTVERSION response: {}",
                    String::from_utf8_lossy(&body[2..])
                ))
            } else {
                Some("DCS".to_string())
            }
        }
        Some(b'=') => Some("keypad application mode".to_string()),
        Some(b'>') => Some("keypad numeric mode".to_string()),
        Some(b'c') => Some("RIS full reset".to_string()),
        Some(b'7') => Some("DECSC save cursor".to_string()),
        Some(b'8') => Some("DECRC restore cursor".to_string()),
        _ => None,
    }
}

/// Drop a trailing ST (`ESC \`) or BEL from a string-sequence body.
fn strip_terminator(body: &[u8]) -> &[u8] {
    if body.ends_with(&[ESC, b'\\']) {
        &body[..body.len() - 2]
    } else if body.ends_with(&[BEL]) {
        &body[..body.len() - 1]
    } else {
        body
    }
}

/// Split `1;2;3` style parameters into numbers (empty parameter = 0, per ECMA-48).
fn params(body: &[u8]) -> Vec<u16> {
    String::from_utf8_lossy(body)
        .split(';')
        .map(|p| p.trim().parse::<u16>().unwrap_or(0))
        .collect()
}

/// Name the body of a CSI sequence (everything after `ESC [`).
fn name_csi(body: &[u8]) -> Option<String> {
    let (&final_byte, rest) = body.split_last()?;
    // Intermediate bytes (0x20..=0x2f) sit between the parameters and the final
    // byte; `$p` / `$y` are the DECRQM / DECRPM pair.
    let (rest, intermediate) = match rest.split_last() {
        Some((&i, head)) if (0x20..=0x2f).contains(&i) => (head, Some(i)),
        _ => (rest, None),
    };
    let private = rest.first() == Some(&b'?') || rest.first() == Some(&b'>');
    let prefix = if private { rest[0] } else { 0 };
    let nums = params(if private { &rest[1..] } else { rest });

    let named = |m: u16| {
        private_mode_name(m)
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("mode {m}"))
    };

    Some(match (prefix, intermediate, final_byte) {
        (b'?', Some(b'$'), b'p') => format!("DECRQM {}", named(nums[0])),
        (b'?', Some(b'$'), b'y') => format!(
            "DECRPM {} = {}",
            named(nums[0]),
            decrpm_state(*nums.get(1).unwrap_or(&0))
        ),
        (b'?', None, b'h') => format!("DECSET {}", named(nums[0])),
        (b'?', None, b'l') => format!("DECRST {}", named(nums[0])),
        (b'?', None, b'c') => format!(
            "DA1 response: {}",
            nums.iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(";")
        ),
        // `ESC [ > 0 q`: the `0` is a parameter, not an intermediate, so this
        // arrives here as prefix `>` with no intermediate.
        (b'>', None, b'q') => "XTVERSION query".to_string(),
        (b'>', None, b'c') if rest.len() == 1 => "DA2 query".to_string(),
        (b'>', None, b'c') => format!(
            "DA2 response: type={} version={} hw={}",
            nums.first().unwrap_or(&0),
            nums.get(1).unwrap_or(&0),
            nums.get(2).unwrap_or(&0)
        ),
        (0, None, b'c') => "DA1 query".to_string(),
        (0, None, b'n') if nums.first() == Some(&6) => "CPR query (cursor position)".to_string(),
        (0, None, b'R') => format!(
            "CPR response: row={} col={}",
            nums.first().unwrap_or(&0),
            nums.get(1).unwrap_or(&0)
        ),
        (0, None, b'm') => "SGR".to_string(),
        (0, None, b'H') => "cursor position".to_string(),
        (0, None, b'J') => "erase in display".to_string(),
        (0, None, b'K') => "erase in line".to_string(),
        (0, None, b'A') | (0, None, b'B') | (0, None, b'C') | (0, None, b'D') => {
            "cursor move".to_string()
        }
        (0, Some(b' '), b'q') => "DECSCUSR cursor style".to_string(),
        _ => return None,
    })
}

/// Name the body of an OSC sequence (everything after `ESC ]`).
fn name_osc(body: &[u8]) -> Option<String> {
    let body = strip_terminator(body);
    let num: u16 = String::from_utf8_lossy(body)
        .split(';')
        .next()?
        .parse()
        .ok()?;
    let what = match num {
        0 => "set icon name and window title",
        1 => "set icon name",
        2 => "set window title",
        7 => "report working directory",
        8 => "hyperlink",
        10 => "foreground colour",
        11 => "background colour",
        52 => "clipboard",
        133 => "shell integration prompt mark",
        _ => "OSC",
    };
    Some(format!("OSC {num} {what}"))
}

/// Render bytes for human reading: escape sequences shown as `ESC[...` with
/// their name appended, plain text shown escaped.
pub fn describe_bytes(bytes: &[u8]) -> String {
    let mut parts = Vec::new();
    for seg in split_sequences(bytes) {
        match seg {
            Segment::Text(t) => parts.push(escape_bytes(t)),
            Segment::Esc(e) => {
                // `ESC` reads better than `\x1b` when the point is to eyeball a
                // sequence, and it keeps the name attached to its sequence.
                let shown = format!("ESC{}", escape_bytes(&e[1..]));
                match name_sequence(e) {
                    Some(name) => parts.push(format!("{shown}  {name}")),
                    None => parts.push(shown),
                }
            }
        }
    }
    parts.join("  ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_ascii_survives_escaping_unchanged() {
        assert_eq!(escape_bytes(b"hello world!"), "hello world!");
    }

    #[test]
    fn control_and_high_bytes_become_hex() {
        assert_eq!(escape_bytes(b"\x1b[0m"), "\\x1b[0m");
        assert_eq!(escape_bytes(&[0x00, 0x0a, 0xff]), "\\x00\\x0a\\xff");
        assert_eq!(escape_bytes(b"a\\b"), "a\\\\b");
    }

    #[test]
    fn escaping_round_trips_every_byte_value() {
        let all: Vec<u8> = (0..=255u8).collect();
        assert_eq!(unescape_bytes(&escape_bytes(&all)), all);
    }

    #[test]
    fn escaping_round_trips_invalid_utf8_and_backslashes() {
        // A truncated UTF-8 sequence plus a literal `\x41` that must not be
        // confused with the byte 0x41 on the way back.
        for case in [
            b"\xe2\x82".to_vec(),
            b"\\x41".to_vec(),
            b"\xff\\\xfe".to_vec(),
            b"caf\xc3\xa9".to_vec(),
            vec![],
        ] {
            assert_eq!(unescape_bytes(&escape_bytes(&case)), case, "{case:?}");
        }
    }

    #[test]
    fn unknown_escapes_are_kept_verbatim_when_unescaping() {
        assert_eq!(unescape_bytes("a\\q"), b"a\\q".to_vec());
        assert_eq!(unescape_bytes("\\x"), b"\\x".to_vec());
    }

    #[test]
    fn sequences_split_away_from_surrounding_text() {
        let segs = split_sequences(b"hi\x1b[31mred\x1b[0m");
        assert_eq!(
            segs,
            vec![
                Segment::Text(b"hi"),
                Segment::Esc(b"\x1b[31m"),
                Segment::Text(b"red"),
                Segment::Esc(b"\x1b[0m"),
            ]
        );
    }

    #[test]
    fn osc_runs_to_its_string_terminator() {
        let segs = split_sequences(b"\x1b]0;title\x07rest");
        assert_eq!(
            segs,
            vec![Segment::Esc(b"\x1b]0;title\x07"), Segment::Text(b"rest")]
        );
        let segs = split_sequences(b"\x1bP>|ghostty 1.0\x1b\\");
        assert_eq!(segs, vec![Segment::Esc(b"\x1bP>|ghostty 1.0\x1b\\")]);
    }

    #[test]
    fn a_truncated_sequence_is_still_reported() {
        assert_eq!(
            split_sequences(b"\x1b[?20"),
            vec![Segment::Esc(b"\x1b[?20")]
        );
        assert_eq!(split_sequences(b"\x1b"), vec![Segment::Esc(b"\x1b")]);
    }

    #[test]
    fn queries_and_replies_get_their_names() {
        let cases: Vec<(&[u8], &str)> = vec![
            (b"\x1b[c", "DA1 query"),
            (b"\x1b[?62;22c", "DA1 response: 62;22"),
            (b"\x1b[>c", "DA2 query"),
            (b"\x1b[>1;4000;0c", "DA2 response: type=1 version=4000 hw=0"),
            (b"\x1b[>0q", "XTVERSION query"),
            (
                b"\x1bP>|ghostty 1.0.1\x1b\\",
                "XTVERSION response: ghostty 1.0.1",
            ),
            (b"\x1b[?2026$p", "DECRQM synchronized-output"),
            (b"\x1b[?2004$p", "DECRQM bracketed-paste"),
            (b"\x1b[?2026;2$y", "DECRPM synchronized-output = reset"),
            (b"\x1b[?2004;1$y", "DECRPM bracketed-paste = set"),
            (b"\x1b[?1234$p", "DECRQM mode 1234"),
            (b"\x1b[6n", "CPR query (cursor position)"),
            (b"\x1b[24;80R", "CPR response: row=24 col=80"),
            (b"\x1b[?1049h", "DECSET alt-screen"),
            (b"\x1b[?25l", "DECRST cursor-visible"),
            (b"\x1b]0;vim\x07", "OSC 0 set icon name and window title"),
            (b"\x1b[0m", "SGR"),
        ];
        for (seq, want) in cases {
            assert_eq!(name_sequence(seq).as_deref(), Some(want), "{seq:?}");
        }
    }

    #[test]
    fn unnamed_sequences_return_none() {
        assert_eq!(name_sequence(b"\x1b[?7z"), None);
        assert_eq!(name_sequence(b"not an escape"), None);
    }

    #[test]
    fn describing_a_chunk_names_each_sequence_in_place() {
        let out = describe_bytes(b"\x1b[?2026$p\x1b[6n");
        assert_eq!(
            out,
            "ESC[?2026$p  DECRQM synchronized-output  ESC[6n  CPR query (cursor position)"
        );
    }
}
