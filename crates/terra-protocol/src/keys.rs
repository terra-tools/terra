//! Key notation for [`Request::Send`](crate::Request::Send): named keys written
//! inline in the text, `{Enter}` style.
//!
//! ```text
//! terra send 3 "cargo test{Enter}"
//! terra send 3 "{C-c}"
//! terra send 3 "vim x.rs{Enter}ihello{Esc}:wq{Enter}"
//! terra send 3 "y{Enter}{Delay 300}y{Enter}"
//! ```
//!
//! Why braces rather than tmux's positional key words (`send-keys "ls" Enter`):
//! one string carries the whole interaction, so text and keys interleave in an
//! unambiguous order, and the shell only has to quote one argument. `{Delay N}`
//! then falls out for free, which is what makes driving a TUI that needs a beat
//! to redraw a single call instead of a shell loop.
//!
//! **Unknown braces are text.** `{foo}` is not a key name, so it is sent
//! verbatim, and `terra send 3 "echo {foo}"` means what it says. Only `{{` is
//! special beyond the key table: it escapes a literal `{`.
//!
//! That safety net is not enough on its own to make parsing the default,
//! which is why the CLI gates it behind `--keys`. Shell variable names
//! collide with key names — see the `${...}` rule below — so "unknown braces
//! are text" leaves exactly the holes a paste is most likely to fall into.
//! Opt-in makes it a non-issue: you get key parsing only when you asked.
//!
//! **`${...}` is never key notation.** A brace group directly after `$` is
//! shell expansion, so it is text no matter what it spells. Without that rule
//! `${HOME}` lowercases to `home`, hits the key table and turns into `$` plus
//! cursor-home — and the same trap sits under `${END}`, `${UP}`, `${SPACE}`,
//! `${INSERT}`, `${DELETE}` and `${TAB}`.

/// One step of a parsed key string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chunk {
    /// Bytes to write to the PTY.
    Bytes(Vec<u8>),
    /// Pause before the next chunk, in milliseconds.
    Delay(u64),
}

/// Named keys, lowercased at lookup so `{enter}`, `{Enter}` and `{ENTER}` all
/// work. Aliases sit next to their canonical name rather than in a second
/// table, so adding a key is one line.
const KEYS: &[(&str, &[u8])] = &[
    // Return. `\r` (CR) is what a PTY in canonical mode expects; `\n` would
    // insert a literal newline in most line editors instead of submitting.
    ("enter", b"\r"),
    ("cr", b"\r"),
    ("return", b"\r"),
    ("lf", b"\n"),
    ("nl", b"\n"),
    ("tab", b"\t"),
    ("esc", b"\x1b"),
    ("escape", b"\x1b"),
    ("space", b" "),
    ("backspace", b"\x7f"),
    ("bs", b"\x7f"),
    ("delete", b"\x1b[3~"),
    ("del", b"\x1b[3~"),
    // Cursor keys in "normal" (non-application) mode. Every shell line editor
    // and every full-screen program accepts these; application-mode variants
    // (ESC O A) are only correct while DECCKM is set, which we cannot know
    // from here.
    ("up", b"\x1b[A"),
    ("down", b"\x1b[B"),
    ("right", b"\x1b[C"),
    ("left", b"\x1b[D"),
    ("home", b"\x1b[H"),
    ("end", b"\x1b[F"),
    ("pageup", b"\x1b[5~"),
    ("pgup", b"\x1b[5~"),
    ("pagedown", b"\x1b[6~"),
    ("pgdn", b"\x1b[6~"),
    ("insert", b"\x1b[2~"),
    ("f1", b"\x1bOP"),
    ("f2", b"\x1bOQ"),
    ("f3", b"\x1bOR"),
    ("f4", b"\x1bOS"),
    ("f5", b"\x1b[15~"),
    ("f6", b"\x1b[17~"),
    ("f7", b"\x1b[18~"),
    ("f8", b"\x1b[19~"),
    ("f9", b"\x1b[20~"),
    ("f10", b"\x1b[21~"),
    ("f11", b"\x1b[23~"),
    ("f12", b"\x1b[24~"),
];

/// Resolve one brace body to its bytes, or `None` if it names nothing — in
/// which case the caller emits the original text verbatim.
fn lookup(body: &str) -> Option<Vec<u8>> {
    let lower = body.trim().to_ascii_lowercase();

    if let Some(bytes) = KEYS.iter().find(|(name, _)| *name == lower) {
        return Some(bytes.1.to_vec());
    }

    // Ctrl: `C-c` -> 0x03. Letters map to 1..=26, and the handful of control
    // codes that are not letters (`C-[` is Esc, `C-\`, `C-]`, `C-^`, `C-_`)
    // follow the same "clear the top three bits" rule.
    if let Some(rest) = lower.strip_prefix("c-") {
        let mut chars = rest.chars();
        let (c, None) = (chars.next()?, chars.next()) else {
            return None;
        };
        return match c {
            'a'..='z' => Some(vec![c as u8 - b'a' + 1]),
            '[' | '\\' | ']' | '^' | '_' => Some(vec![c as u8 & 0x1f]),
            '?' => Some(vec![0x7f]),
            '@' | ' ' => Some(vec![0]),
            _ => None,
        };
    }

    // Alt/Meta: ESC-prefixed, the convention every terminal emulator uses.
    // `M-b` is the word-back binding in readline; `M-Enter` composes with the
    // key table so named keys work under a modifier too.
    if let Some(rest) = lower
        .strip_prefix("m-")
        .or_else(|| lower.strip_prefix("a-"))
    {
        let mut bytes = vec![0x1b];
        bytes.extend(lookup(rest).or_else(|| {
            let mut chars = rest.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii() => Some(vec![c as u8]),
                _ => None,
            }
        })?);
        return Some(bytes);
    }

    // Shift is only meaningful on Tab, where it is the well-known back-tab.
    if lower == "s-tab" {
        return Some(b"\x1b[Z".to_vec());
    }

    // `Delay 250` / `Delay250` is handled by the caller, which needs to emit a
    // different chunk kind; recognising it here keeps the "is this a key?"
    // question in one place.
    None
}

/// Longest pause a single `{Delay}` can ask for.
///
/// The wait happens on the connection thread serving one `terra send`, so an
/// unbounded `{Delay 999999999}` would wedge that thread and hold the client's
/// socket open for weeks. Ten seconds is far past any TUI redraw and still
/// short enough that a typo cannot look like a hang.
pub const MAX_DELAY_MS: u64 = 10_000;

/// `Delay 250`, `delay250`, `sleep 1000` -> milliseconds.
///
/// The clamp lives here rather than at the sleep site so the cap is part of the
/// parsed representation: it is then the same for every consumer and testable
/// without doing any I/O or actually waiting. A non-numeric body still yields
/// `None`, which makes it an unknown brace group and therefore literal text.
fn delay_ms(body: &str) -> Option<u64> {
    let lower = body.trim().to_ascii_lowercase();
    let rest = lower
        .strip_prefix("delay")
        .or_else(|| lower.strip_prefix("sleep"))?;
    let ms: u64 = rest.trim().parse().ok()?;
    // Excess is ignored silently: the caller asked to pause, and refusing the
    // whole send over an over-long pause helps nobody.
    Some(ms.min(MAX_DELAY_MS))
}

/// Parse key notation into the steps needed to replay it.
///
/// Never fails: anything unrecognised is text. Adjacent text is coalesced so a
/// plain string is exactly one [`Chunk::Bytes`].
pub fn parse(text: &str) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = Vec::new();
    let mut buf: Vec<u8> = Vec::new();
    let src: Vec<char> = text.chars().collect();
    let mut i = 0;

    let flush = |buf: &mut Vec<u8>, out: &mut Vec<Chunk>| {
        if !buf.is_empty() {
            out.push(Chunk::Bytes(std::mem::take(buf)));
        }
    };

    while i < src.len() {
        if src[i] != '{' {
            let mut b = [0u8; 4];
            buf.extend(src[i].encode_utf8(&mut b).as_bytes());
            i += 1;
            continue;
        }

        // `{{` -> a literal brace.
        if src.get(i + 1) == Some(&'{') {
            buf.push(b'{');
            i += 2;
            continue;
        }

        // `${...}` is shell expansion, never key notation. Checked after `{{`
        // so `${{Enter}` still escapes to a literal `${Enter}`: the escape has
        // already decided the brace is text, and there is no lookup left to
        // suppress. Emitting only the `{` here leaves the body to be scanned as
        // ordinary text, so a nested `${a{Enter}b}` still gets its key.
        if i > 0 && src[i - 1] == '$' {
            buf.push(b'{');
            i += 1;
            continue;
        }

        // Find the closing brace. An unclosed `{` is just text — the common
        // case is shell brace expansion the user did not think twice about.
        let Some(close) = (i + 1..src.len()).find(|&j| src[j] == '}') else {
            buf.push(b'{');
            i += 1;
            continue;
        };

        let body: String = src[i + 1..close].iter().collect();

        if let Some(ms) = delay_ms(&body) {
            flush(&mut buf, &mut out);
            out.push(Chunk::Delay(ms));
            i = close + 1;
        } else if let Some(bytes) = lookup(&body) {
            buf.extend(bytes);
            i = close + 1;
        } else {
            // Not a key: emit `{body}` exactly as written.
            buf.push(b'{');
            buf.extend(body.as_bytes());
            buf.push(b'}');
            i = close + 1;
        }
    }

    flush(&mut buf, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(text: &str) -> Vec<u8> {
        parse(text)
            .into_iter()
            .flat_map(|c| match c {
                Chunk::Bytes(b) => b,
                Chunk::Delay(_) => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn plain_text_is_one_chunk_of_itself() {
        assert_eq!(
            parse("cargo test"),
            vec![Chunk::Bytes(b"cargo test".to_vec())]
        );
    }

    #[test]
    fn enter_becomes_a_carriage_return() {
        assert_eq!(bytes("cargo test{Enter}"), b"cargo test\r");
    }

    #[test]
    fn key_names_are_case_insensitive() {
        assert_eq!(bytes("{enter}"), bytes("{ENTER}"));
        assert_eq!(bytes("{Enter}"), b"\r");
    }

    #[test]
    fn ctrl_letters_map_to_their_control_codes() {
        assert_eq!(bytes("{C-c}"), b"\x03");
        assert_eq!(bytes("{C-d}"), b"\x04");
        assert_eq!(bytes("{C-a}"), b"\x01");
        assert_eq!(bytes("{C-z}"), b"\x1a");
    }

    #[test]
    fn alt_prefixes_with_escape_and_composes_with_named_keys() {
        assert_eq!(bytes("{M-b}"), b"\x1bb");
        assert_eq!(bytes("{M-Enter}"), b"\x1b\r");
    }

    #[test]
    fn arrow_keys_use_normal_mode_sequences() {
        assert_eq!(bytes("{Up}{Up}{Enter}"), b"\x1b[A\x1b[A\r");
    }

    /// The property that lets key parsing be the default: a brace group that
    /// names nothing is text, so scripts written before this existed are safe.
    #[test]
    fn an_unknown_brace_group_is_sent_verbatim() {
        assert_eq!(bytes("echo {foo}"), b"echo {foo}");
        assert_eq!(bytes("${HOME}"), b"${HOME}");
        assert_eq!(bytes("cp a.{txt,bak}"), b"cp a.{txt,bak}");
    }

    /// The collision list, exhaustively: every key name that is also a variable
    /// people really have in their environment. One example would not cover it,
    /// because each of these lowercases onto a different key-table row.
    #[test]
    fn a_shell_expansion_is_text_even_when_it_spells_a_key() {
        for name in [
            "HOME", "END", "UP", "DOWN", "LEFT", "RIGHT", "SPACE", "INSERT", "DELETE", "DEL",
            "TAB", "ESC", "ENTER", "RETURN", "CR", "LF", "NL", "BS", "F1", "F2", "PAGEUP",
        ] {
            let text = format!("${{{name}}}");
            assert_eq!(bytes(&text), text.as_bytes(), "{text} must stay literal");
        }
    }

    /// `${HOME}` in the middle of a real command line, with a key after it, so
    /// the rule cannot be satisfied by giving up on key parsing after a `$`.
    #[test]
    fn a_shell_expansion_does_not_disable_the_keys_around_it() {
        assert_eq!(bytes("cd ${HOME}/src{Enter}"), b"cd ${HOME}/src\r");
        assert_eq!(bytes("echo $HOME{Enter}"), b"echo $HOME\r");
        // Only a brace *directly* after `$` is expansion.
        assert_eq!(bytes("cost is $ {Enter}"), b"cost is $ \r");
    }

    /// `{{` already decides the brace is text, so `$` has no lookup left to
    /// suppress there — and the parser has no backslash escape at all, so `\$`
    /// is simply two literal characters followed by the same `${` rule.
    #[test]
    fn the_dollar_rule_composes_with_the_brace_escape_and_backslashes() {
        assert_eq!(bytes("${{Enter}"), b"${Enter}");
        assert_eq!(bytes("\\${HOME}"), b"\\${HOME}");
        assert_eq!(bytes("${a{Enter}b}"), b"${a\rb}");
    }

    #[test]
    fn an_unclosed_brace_is_text() {
        assert_eq!(bytes("echo {"), b"echo {");
        assert_eq!(bytes("awk '{print $1'"), b"awk '{print $1'");
    }

    #[test]
    fn a_doubled_brace_escapes_one_literal_brace() {
        assert_eq!(bytes("{{Enter}"), b"{Enter}");
        assert_eq!(bytes("{{}"), b"{}");
    }

    #[test]
    fn delay_becomes_its_own_chunk_and_splits_the_text() {
        assert_eq!(
            parse("y{Enter}{Delay 300}n{Enter}"),
            vec![
                Chunk::Bytes(b"y\r".to_vec()),
                Chunk::Delay(300),
                Chunk::Bytes(b"n\r".to_vec()),
            ]
        );
    }

    #[test]
    fn delay_accepts_the_spellings_people_actually_type() {
        assert_eq!(parse("{Delay 50}"), vec![Chunk::Delay(50)]);
        assert_eq!(parse("{delay50}"), vec![Chunk::Delay(50)]);
        assert_eq!(parse("{sleep 1000}"), vec![Chunk::Delay(1000)]);
    }

    /// The pause runs on the connection thread, so the cap is what stops a
    /// stray digit from holding a client socket open indefinitely.
    #[test]
    fn an_over_long_delay_is_clamped_to_the_cap() {
        assert_eq!(parse("{Delay 999999999}"), vec![Chunk::Delay(MAX_DELAY_MS)]);
        assert_eq!(parse("{sleep 10001}"), vec![Chunk::Delay(MAX_DELAY_MS)]);
        // Anything under the cap is passed through untouched.
        assert_eq!(parse("{Delay 300}"), vec![Chunk::Delay(300)]);
        assert_eq!(parse("{Delay 0}"), vec![Chunk::Delay(0)]);
        assert_eq!(parse("{Delay 10000}"), vec![Chunk::Delay(MAX_DELAY_MS)]);
    }

    /// A body that is not a number is not a delay, so it falls through to the
    /// unknown-brace rule and is sent verbatim — unchanged by the clamp.
    #[test]
    fn a_non_numeric_delay_is_text() {
        assert_eq!(bytes("{Delay abc}"), b"{Delay abc}");
        assert_eq!(bytes("{Delay -5}"), b"{Delay -5}");
        assert_eq!(bytes("{Delay}"), b"{Delay}");
    }

    /// `terra send 3 ""` must be a successful no-op. Zero chunks is how that
    /// reaches the sender, which then writes nothing and reports `Ok`.
    #[test]
    fn empty_text_parses_to_no_chunks_at_all() {
        assert_eq!(parse(""), Vec::new());
        assert!(parse("").is_empty());
    }

    #[test]
    fn non_ascii_text_survives_intact() {
        assert_eq!(bytes("echo שלום{Enter}"), "echo שלום\r".as_bytes());
        assert_eq!(bytes("echo 🌱{Enter}"), "echo 🌱\r".as_bytes());
    }

    #[test]
    fn adjacent_text_and_keys_coalesce_into_one_chunk() {
        assert_eq!(
            parse("a{Tab}b{Esc}"),
            vec![Chunk::Bytes(b"a\tb\x1b".to_vec())]
        );
    }

    #[test]
    fn a_multi_char_ctrl_body_is_not_a_key() {
        assert_eq!(bytes("{C-cc}"), b"{C-cc}");
    }
}
