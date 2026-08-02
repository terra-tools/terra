//! `terra doctor` — probe the terminal this process is running inside.
//!
//! Nothing here touches the terra IPC socket: the point is to run the same
//! binary under terra, Ghostty, iTerm2 or tmux and `diff` the two reports, so
//! the only party it may talk to is `/dev/tty`.
//!
//! Gathering and formatting are deliberately separate. [`gather`] does all the
//! I/O and returns a plain [`Facts`]; [`render_text`] and [`render_json`] are
//! pure functions of that struct, which is what makes the report testable
//! without a terminal — and what guarantees two runs differ only where the
//! terminals differ.

use crate::escape::{describe_bytes, escape_bytes, name_sequence, split_sequences, Segment};
use crate::tty::{RawMode, Tty, QUERY_TIMEOUT};
use std::collections::BTreeMap;

/// Environment variables worth reporting, in the order they are gathered (the
/// report itself is sorted, so this order is only for readers of the code).
const ENV_KEYS: &[&str] = &[
    "TERM",
    "COLORTERM",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "LANG",
];

/// The probes, as `(report key, bytes to write)`.
///
/// Every one of these is a question a terminal may simply decline to answer;
/// XTVERSION in particular is answered by Ghostty and ignored by most others,
/// which is precisely the kind of difference this command exists to surface.
const PROBES: &[(&str, &[u8])] = &[
    ("da1", b"\x1b[c"),
    ("da2", b"\x1b[>c"),
    ("xtversion", b"\x1b[>0q"),
    ("decrqm_synchronized_output", b"\x1b[?2026$p"),
    ("decrqm_bracketed_paste", b"\x1b[?2004$p"),
    ("cursor_position", b"\x1b[6n"),
];

/// Everything the report is made of. Owning this as data (rather than printing
/// as we go) is what lets the formatting be tested with no tty in sight.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Facts {
    /// Reported environment variables; `None` means unset, which is itself a
    /// meaningful difference between terminals.
    pub env: Vec<(String, Option<String>)>,
    /// `(rows, cols)` from `TIOCGWINSZ`.
    pub size: Option<(u16, u16)>,
    /// What terminfo says, via `tput colors`.
    pub colors: Option<String>,
    /// Probe key -> raw response bytes. An absent key means no response.
    pub responses: Vec<(String, Option<Vec<u8>>)>,
    /// Why the tty could not be used, when it could not.
    pub tty_error: Option<String>,
}

/// Run every probe against `/dev/tty`.
///
/// Never fails: a missing terminal degrades to env-and-size-only facts with
/// `tty_error` set, because `terra doctor | tee report.txt` in CI should still
/// produce the half of the report that does not need a terminal.
pub fn gather() -> Facts {
    let env = ENV_KEYS
        .iter()
        .map(|k| (k.to_string(), std::env::var(k).ok()))
        .collect();
    // `tput` is run before raw mode is entered: it writes to the terminal and
    // reads terminfo, and doing that mid-probe would pollute the input stream.
    let colors = tput_colors();

    let mut facts = Facts {
        env,
        colors,
        ..Facts::default()
    };

    let mut tty = match Tty::open() {
        Ok(t) => t,
        Err(e) => {
            facts.tty_error = Some(format!("{e:#}"));
            return facts;
        }
    };
    facts.size = tty.size().ok();

    // The guard restores termios on every path out of this function, including
    // a panic inside a probe.
    let _raw = match RawMode::enable_timed(tty.fd(), QUERY_TIMEOUT) {
        Ok(g) => g,
        Err(e) => {
            facts.tty_error = Some(format!("{e:#}"));
            return facts;
        }
    };
    for (key, request) in PROBES {
        let reply = tty.query(request);
        facts.responses.push((
            key.to_string(),
            if reply.is_empty() { None } else { Some(reply) },
        ));
    }
    facts
}

/// `tput colors`, trimmed. `None` when tput is missing or unhappy — reporting
/// nothing beats reporting a guess.
fn tput_colors() -> Option<String> {
    let out = std::process::Command::new("tput")
        .arg("colors")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Decoded reading of a probe reply, or `None` when nothing recognises it.
///
/// The reply's first escape sequence carries the meaning; anything after it is
/// stray input the user typed while the probe ran.
fn decode(response: &[u8]) -> Option<String> {
    split_sequences(response)
        .into_iter()
        .find_map(|seg| match seg {
            Segment::Esc(e) => name_sequence(e),
            Segment::Text(_) => None,
        })
}

const NO_RESPONSE: &str = "(no response)";
const UNSET: &str = "(unset)";
const UNKNOWN: &str = "(unknown)";

/// The report: one `key: value` per line, sorted, no timestamps or other
/// run-to-run noise, so `diff <(terra doctor) <(ssh box terra doctor)` shows
/// only real terminal differences.
///
/// Keys are always present even when the value is missing — a probe that goes
/// unanswered prints `(no response)` rather than vanishing, so the two sides of
/// a diff stay line-for-line aligned.
pub fn render_text(facts: &Facts) -> String {
    let mut lines: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in &facts.env {
        lines.insert(format!("env.{k}"), v.clone().unwrap_or(UNSET.into()));
    }
    lines.insert(
        "color.count".into(),
        facts.colors.clone().unwrap_or(UNKNOWN.into()),
    );
    lines.insert(
        "size.rows".into(),
        facts.size.map_or(UNKNOWN.into(), |(r, _)| r.to_string()),
    );
    lines.insert(
        "size.cols".into(),
        facts.size.map_or(UNKNOWN.into(), |(_, c)| c.to_string()),
    );
    for (key, response) in &facts.responses {
        let (raw, decoded) = match response {
            Some(bytes) => (
                escape_bytes(bytes),
                decode(bytes).unwrap_or(UNKNOWN.to_string()),
            ),
            None => (NO_RESPONSE.to_string(), NO_RESPONSE.to_string()),
        };
        lines.insert(format!("query.{key}.raw"), raw);
        lines.insert(format!("query.{key}.decoded"), decoded);
    }
    match &facts.tty_error {
        None => {
            lines.insert("tty".into(), "ok".into());
        }
        Some(e) => {
            lines.insert("tty".into(), format!("unavailable: {e}"));
            lines.insert(
                "tty.note".into(),
                "query probes and size need a controlling terminal; run terra doctor \
                 directly in the terminal under test"
                    .into(),
            );
            // Keep the query keys in the output even with no tty, so a report
            // taken in CI still lines up against one taken in a terminal.
            for (key, _) in PROBES {
                lines.insert(format!("query.{key}.raw"), NO_RESPONSE.into());
                lines.insert(format!("query.{key}.decoded"), NO_RESPONSE.into());
            }
        }
    }
    lines
        .into_iter()
        .map(|(k, v)| format!("{k}: {v}\n"))
        .collect()
}

/// Same facts, machine-readable. Uses the same key names as the text report so
/// a script and a human are looking at the same thing.
pub fn render_json(facts: &Facts) -> serde_json::Value {
    let env: serde_json::Map<String, serde_json::Value> = facts
        .env
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                v.clone().map_or(serde_json::Value::Null, Into::into),
            )
        })
        .collect();
    let queries: serde_json::Map<String, serde_json::Value> = facts
        .responses
        .iter()
        .map(|(k, r)| {
            let value = match r {
                Some(bytes) => serde_json::json!({
                    "raw": escape_bytes(bytes),
                    "decoded": decode(bytes),
                    "pretty": describe_bytes(bytes),
                }),
                None => serde_json::json!({ "raw": null, "decoded": null, "pretty": null }),
            };
            (k.clone(), value)
        })
        .collect();
    serde_json::json!({
        "env": env,
        "size": facts.size.map(|(r, c)| serde_json::json!({"rows": r, "cols": c})),
        "colors": facts.colors,
        "queries": queries,
        "tty_error": facts.tty_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Facts with every field populated, so the formatting test does not need a
    /// terminal (and cannot be flaky on a machine that has none).
    fn sample() -> Facts {
        Facts {
            env: vec![
                ("TERM".into(), Some("xterm-256color".into())),
                ("COLORTERM".into(), Some("truecolor".into())),
                ("TERM_PROGRAM".into(), Some("ghostty".into())),
                ("TERM_PROGRAM_VERSION".into(), None),
                ("LANG".into(), Some("en_US.UTF-8".into())),
            ],
            size: Some((40, 120)),
            colors: Some("256".into()),
            responses: vec![
                ("da1".into(), Some(b"\x1b[?62;22c".to_vec())),
                ("da2".into(), Some(b"\x1b[>1;4000;0c".to_vec())),
                ("xtversion".into(), None),
                (
                    "decrqm_synchronized_output".into(),
                    Some(b"\x1b[?2026;2$y".to_vec()),
                ),
            ],
            tty_error: None,
        }
    }

    #[test]
    fn the_report_is_byte_identical_across_runs_of_the_same_facts() {
        assert_eq!(render_text(&sample()), render_text(&sample()));
    }

    #[test]
    fn the_report_is_sorted_and_one_key_value_pair_per_line() {
        let out = render_text(&sample());
        let keys: Vec<&str> = out
            .lines()
            .map(|l| l.split_once(": ").expect("key: value").0)
            .collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted, "report must be sorted:\n{out}");
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn the_report_shows_raw_bytes_and_a_decoding_for_every_probe() {
        let out = render_text(&sample());
        assert!(out.contains("query.da1.raw: \\x1b[?62;22c\n"), "{out}");
        assert!(
            out.contains("query.da1.decoded: DA1 response: 62;22\n"),
            "{out}"
        );
        assert!(
            out.contains(
                "query.decrqm_synchronized_output.decoded: DECRPM synchronized-output = reset\n"
            ),
            "{out}"
        );
        assert!(out.contains("size.rows: 40\n") && out.contains("size.cols: 120\n"));
        assert!(out.contains("env.TERM: xterm-256color\n"));
    }

    #[test]
    fn an_unanswered_probe_says_so_instead_of_disappearing() {
        let out = render_text(&sample());
        assert!(
            out.contains("query.xtversion.raw: (no response)\n"),
            "{out}"
        );
        assert!(
            out.contains("query.xtversion.decoded: (no response)\n"),
            "{out}"
        );
        assert!(out.contains("env.TERM_PROGRAM_VERSION: (unset)\n"), "{out}");
    }

    #[test]
    fn without_a_tty_the_report_keeps_its_shape_and_explains_itself() {
        let facts = Facts {
            env: vec![("TERM".into(), Some("dumb".into()))],
            tty_error: Some("open /dev/tty: No such device".into()),
            ..Facts::default()
        };
        let out = render_text(&facts);
        assert!(out.contains("tty: unavailable: open /dev/tty"), "{out}");
        assert!(out.contains("tty.note: query probes"), "{out}");
        // Every probe key still appears, so this report diffs against a real one.
        for (key, _) in PROBES {
            assert!(
                out.contains(&format!("query.{key}.raw: (no response)")),
                "{key} missing from:\n{out}"
            );
        }
        assert!(out.contains("size.rows: (unknown)"), "{out}");
    }

    #[test]
    fn json_carries_the_same_facts_as_the_text_report() {
        let v = render_json(&sample());
        assert_eq!(v["env"]["TERM"], "xterm-256color");
        assert!(v["env"]["TERM_PROGRAM_VERSION"].is_null());
        assert_eq!(v["size"]["rows"], 40);
        assert_eq!(v["colors"], "256");
        assert_eq!(v["queries"]["da1"]["raw"], "\\x1b[?62;22c");
        assert_eq!(v["queries"]["da1"]["decoded"], "DA1 response: 62;22");
        assert!(v["queries"]["xtversion"]["raw"].is_null());
        assert!(v["tty_error"].is_null());
    }

    #[test]
    fn a_reply_with_typed_ahead_text_still_decodes_the_sequence() {
        // The user hitting a key mid-probe must not blind the decoder.
        assert_eq!(
            decode(b"x\x1b[24;80R").as_deref(),
            Some("CPR response: row=24 col=80")
        );
        assert_eq!(decode(b"garbage"), None);
    }
}
