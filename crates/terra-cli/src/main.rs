//! `terra` CLI — tmux-style control of the terra app over its unix socket.
//!
//! Every subcommand maps to exactly one [`terra_protocol::Request`], sent with
//! the blocking [`terra_protocol::request`] helper. Output is plain and
//! greppable by default; `--json` prints the raw `Response` instead.
//!
//! Two subcommands are the exception and never touch the socket: `doctor` and
//! `record` talk to the terminal this process is running inside, so that the
//! same binary can be run under terra and under any other terminal and the two
//! outputs diffed.

mod doctor;
mod escape;
mod pretty;
mod record;
mod tty;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use terra_protocol::{request, Request, Response, TabInfo};

#[derive(Parser, Debug)]
#[command(
    name = "terra",
    version,
    about = "control the terra terminal from the command line",
    long_about = None,
)]
struct Cli {
    /// Print the raw JSON response instead of formatted output
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

/// `terra learn` output, generated from the clap command definitions so the
/// command map can never drift from reality. Only the protocol facts
/// (transport, exit codes, workflow) are prose — they have no other source
/// of truth.
fn learn_text() -> String {
    use clap::CommandFactory;
    let cmd = Cli::command();
    let mut commands = String::new();
    for sub in cmd.get_subcommands() {
        let usage: Vec<String> = std::iter::once(format!("terra {}", sub.get_name()))
            .chain(
                sub.get_positionals()
                    .map(|a| format!("<{}>", a.get_id().to_string().to_lowercase())),
            )
            .collect();
        let mut left = usage.join(" ");
        let flags: Vec<String> = sub
            .get_arguments()
            .filter(|a| !a.is_positional() && a.get_long().is_some())
            .map(|a| format!("[--{}]", a.get_long().unwrap()))
            .collect();
        if !flags.is_empty() {
            left = format!("{left} {}", flags.join(" "));
        }
        let about = sub.get_about().map(|a| a.to_string()).unwrap_or_default();
        commands.push_str(&format!("  {left:<44} {about}\n"));
    }
    format!(
        "{name} {version} — {about}\n\
         The GUI is `terra-app`; this CLI controls it over a unix socket.\n\n\
         Commands (from --help, always current)\n\
         --------------------------------------\n\
         {commands}\n\
         A typical agent workflow\n\
         ------------------------\n\
         \x20 id=$(terra new --title \"tests\")\n\
         \x20 terra send \"$id\" \"cargo test{{Enter}}\" --keys\n\
         \x20 terra send \"$id\" \"$(cat patch.txt)\"   # no --keys: literal paste\n\
         \x20 sleep 5; terra capture \"$id\"\n\
         \x20 terra kill \"$id\"\n\n\
         Keys (send --keys)\n\
         ------------------\n\
         Opt-in: {{Home}} is a key name, so ${{HOME}} would otherwise become\n\
         `$` plus cursor-home. Without --keys the text is written byte for\n\
         byte, which is what you want for pasting.\n\
         Vocabulary: {{Enter}} {{Esc}} {{Tab}} {{Space}} {{Backspace}} {{Delete}}\n\
         {{Up}} {{Down}} {{Left}} {{Right}} {{Home}} {{End}} {{PageUp}} {{PageDown}}\n\
         {{Insert}} {{F1}}..{{F12}} {{C-c}} (ctrl) {{M-x}}/{{A-x}} (alt) {{S-Tab}}\n\
         {{Delay 300}}/{{sleep 300}} (pause, max 10s). Names are\n\
         case-insensitive, `{{{{` is a literal brace, and a brace group that\n\
         names nothing is sent as written. --enter still appends a CR and\n\
         composes with --keys.\n\n\
         Pictures of the window\n\
         ----------------------\n\
         \x20 terra select \"$id\"; terra screenshot --out shot.png --pretty\n\
         The rendered window as a PNG (the app captures its own framebuffer),\n\
         optionally composited ray.so-style: rounded card, macOS traffic\n\
         lights, drop shadow, diagonal gradient (--bg '#4f46e5,#ec4899' to\n\
         recolour). The window is brought forward to be drawn, and a window\n\
         that cannot be drawn fails in 2s rather than hanging. For *reading* a\n\
         tab, capture below is better in every way.\n\n\
         Reading a tab precisely\n\
         -----------------------\n\
         \x20 terra capture \"$id\" --cells\n\
         The visible grid as JSON: per-row runs of {{x,text,fg,bg,flags}} plus\n\
         {{cursor:{{row,col,visible}}}}, run-length encoded by style. Colours\n\
         stay unresolved ({{\"indexed\":236}}, {{\"named\":\"Background\"}},\n\
         \"#3a3a3a\") so you see what the program asked for. This answers \"is\n\
         that row's background really grey?\" and \"where is the cursor?\"\n\
         without a screenshot. --scrollback N applies to both forms.\n\n\
         \x20 terra bidi \"$id\" [off|on|auto]\n\
         Per-tab right-to-left reordering (UAX #9). Default off, like every\n\
         other terminal; on for programs that emit logical order; auto uses the\n\
         per-application table in ~/.terra/config.toml. Measured: Claude Code\n\
         needs off (it does its own BiDi), Codex needs on.\n\n\
         Terminal forensics (no socket, no terra-app)\n\
         --------------------------------------------\n\
         \x20 diff <(terra doctor) <(ssh box terra doctor)\n\
         doctor probes the terminal it runs inside — env, size, colour count,\n\
         decoded DA1/DA2/XTVERSION/DECRQM/CPR replies — sorted and stable, so\n\
         two terminals diff cleanly.\n\
         \x20 terra record --out s.jsonl -- codex\n\
         \x20 terra record --decode s.jsonl\n\
         record logs *both* directions of a program's terminal I/O, which\n\
         script(1) cannot; the terminal->program replies are what make a\n\
         program behave differently between terminals. --decode names the\n\
         escape sequences.\n\n\
         Config\n\
         ------\n\
         ~/.terra/config.toml: [font] size, line_height; [text] bidi,\n\
         bidi_base, [text.bidi_quirks]. Every key optional; a broken file\n\
         yields defaults plus a warning. Template: docs/config.example.toml.\n\n\
         Transport\n\
         ---------\n\
         Unix socket: {socket} (override: TERRA_SOCKET). terra-app must be\n\
         running. Remote: `ssh -R /tmp/terra.sock:$HOME/.terra/terra.sock host`\n\
         + TERRA_SOCKET=/tmp/terra.sock on the remote opens tabs locally.\n\n\
         Output & exit codes\n\
         -------------------\n\
         Global --json prints the raw JSON response. stdout = results,\n\
         stderr = errors, never mixed. Exit 0 success, 1 error.\n\
         Per-command detail: terra <command> --help\n",
        name = cmd.get_name(),
        version = cmd.get_version().unwrap_or("dev"),
        about = cmd.get_about().map(|a| a.to_string()).unwrap_or_default(),
        socket = terra_protocol::socket_path().display(),
    )
}

/// JSON form of `learn`, same auto-derived command map.
fn learn_json() -> serde_json::Value {
    use clap::CommandFactory;
    let cmd = Cli::command();
    serde_json::json!({
        "tool": cmd.get_name(),
        "version": cmd.get_version().unwrap_or("dev"),
        "purpose": cmd.get_about().map(|a| a.to_string()),
        "socket": terra_protocol::socket_path().display().to_string(),
        "socket_env": "TERRA_SOCKET",
        "commands": cmd.get_subcommands().map(|s| serde_json::json!({
            "name": s.get_name(),
            "about": s.get_about().map(|a| a.to_string()),
        })).collect::<Vec<_>>(),
        "exit_codes": {"0": "success", "1": "error (stderr has the message)"},
        "json_flag": "--json prints the raw JSON Response",
    })
}

#[derive(Subcommand, Debug)]
enum Command {
    /// List tabs as a table: ID  ACTIVE  TITLE
    Ls,

    /// Print a structured self-teaching prompt (for agents)
    Learn,

    /// Create a new tab; prints its id. A trailing command runs via your
    /// default shell and the tab stays open in that shell afterwards
    New {
        /// Initial tab title
        #[arg(long)]
        title: Option<String>,

        /// Working directory for the new tab
        #[arg(long)]
        cwd: Option<String>,

        /// Program and arguments to run instead of the default shell
        #[arg(last = true, allow_hyphen_values = true, value_name = "CMD")]
        command: Vec<String>,
    },

    /// Close a tab (kills its PTY)
    Kill {
        /// Numeric tab id (from `terra ls`)
        tab: u64,
    },

    /// Write text to a tab's PTY
    Send {
        /// Numeric tab id (from `terra ls`)
        tab: u64,
        /// Text to write
        text: String,
        /// Interpret key names in TEXT: `{Enter}`, `{Escape}`, `{C-c}`,
        /// `{Tab}`, `{Up}`, `{Delay 300}` and friends, the way
        /// `tmux send-keys` does. Without it TEXT is written literally,
        /// which is what you want for pasting.
        #[arg(long)]
        keys: bool,
        /// Append a carriage return (like `tmux send-keys ... Enter`)
        #[arg(long)]
        enter: bool,
    },

    /// Print a tab's screen contents to stdout
    Capture {
        /// Numeric tab id (from `terra ls`)
        tab: u64,
        /// Include up to N lines of scrollback above the visible screen
        #[arg(long, default_value_t = 0)]
        scrollback: usize,

        /// Emit the full cell grid with styling as JSON instead of plain text
        #[arg(long)]
        cells: bool,
    },

    /// Save a PNG of the terra window.
    ///
    /// The app captures its own framebuffer, so this is the rendered window —
    /// fonts, colours, the tab bar, the palette if it is open — rather than the
    /// text `capture` returns. It shows whatever the window is showing, so pick
    /// the tab with `terra select` first.
    ///
    /// The window is brought forward to be drawn: eframe does not run at all
    /// while it is occluded, so a screenshot is the one request that needs the
    /// window in front of you. A minimised window fails after two seconds
    /// rather than hanging.
    ///
    ///     terra screenshot --out shot.png
    ///     terra screenshot --out shot.png --pretty
    ///
    /// `--pretty` composites the window ray.so-style: a rounded card with
    /// macOS traffic lights and a drop shadow, on a diagonal gradient.
    Screenshot {
        /// File to write the PNG to. Required unless --json, which prints the
        /// image as base64 in the Response instead of writing it anywhere.
        /// (No `-`: a PNG on a terminal's stdout is not useful to anyone.)
        #[arg(long, value_name = "PATH")]
        out: Option<std::path::PathBuf>,

        /// Composite the window on a gradient card with a shadow
        #[arg(long)]
        pretty: bool,

        /// Gradient colours for --pretty: one or two CSS hex colours,
        /// e.g. `--bg '#4f46e5,#ec4899'`. One colour means a flat background.
        #[arg(long, value_name = "HEX[,HEX]", requires = "pretty")]
        bg: Option<String>,
    },

    /// Set a tab's title
    Rename {
        /// Numeric tab id (from `terra ls`)
        tab: u64,
        /// New title
        title: String,
    },

    /// Focus a tab in the GUI
    Select {
        /// Numeric tab id (from `terra ls`)
        tab: u64,
    },

    /// Show or set whether a tab reorders right-to-left text.
    ///
    /// A terminal cannot tell logical-order text from visual-order text: both
    /// arrive as the same bytes, so the program's intent has to be declared
    /// rather than detected.
    ///
    /// `off` (the default) paints the bytes in the order they arrive, which is
    /// what every other terminal does. `on` applies the Unicode bidirectional
    /// algorithm, for programs that emit logical order and expect the terminal
    /// to reorder. `auto` looks the running program up in the per-application
    /// table in ~/.terra/config.toml.
    ///
    /// Measured: Claude Code needs `off`, Codex needs `on`.
    ///
    /// With no MODE this prints the tab's current mode; with one it sets the
    /// mode and prints the new value.
    Bidi {
        /// Numeric tab id (from `terra ls`)
        tab: u64,
        /// New mode: off, on, or auto (omit to query)
        #[arg(value_parser = ["off", "on", "auto"])]
        mode: Option<String>,
    },

    /// Report what the terminal running this command supports.
    ///
    /// Does not talk to terra at all — it probes the terminal it is running
    /// inside, so it works the same in terra, Ghostty, iTerm2 or over ssh. Run
    /// it in two terminals and diff the output to see exactly where they differ:
    ///
    ///     diff <(terra doctor) <(ssh box terra doctor)
    ///
    /// Reports TERM and friends, the window size, the terminfo colour count,
    /// and the replies to DA1, DA2, XTVERSION, DECRQM for synchronized output
    /// and bracketed paste, and a cursor position request — each as raw bytes
    /// plus a decoding. Output is one `key: value` per line, sorted, with no
    /// run-to-run noise; a query nothing answers prints `(no response)` after a
    /// short timeout rather than hanging.
    ///
    /// Without a controlling terminal (piped into CI) the environment section
    /// still prints and the probes say so.
    Doctor,

    /// Record both directions of a program's terminal I/O to a JSON Lines file.
    ///
    /// Unlike `script(1)`, which sees only what the program writes, this also
    /// captures what the terminal writes back — the query replies that make a
    /// program behave differently under different terminals.
    ///
    ///     terra record --out session.jsonl -- codex --yolo
    ///
    /// The program runs on a pty and stays fully interactive: keystrokes,
    /// resizes and the exit status all pass through. Each chunk is logged as
    /// {"t":seconds,"dir":"out"|"in","bytes":"..."} where `out` is written by
    /// the program toward the terminal, `in` by the terminal toward the
    /// program, and `bytes` escapes every non-printable byte as \xNN so the
    /// file is valid UTF-8 whatever came through.
    ///
    ///     terra record --decode session.jsonl
    ///
    /// prints a recording back with escape sequences named
    /// (`ESC[?2026$p  DECRQM synchronized-output`), so two recordings taken in
    /// two terminals can be diffed directly.
    Record {
        /// File to write the JSON Lines recording to
        #[arg(long, value_name = "PATH", required_unless_present = "decode")]
        out: Option<String>,

        /// Pretty-print an existing recording instead of making one
        #[arg(long, value_name = "PATH", conflicts_with = "out")]
        decode: Option<String>,

        /// Program and arguments to record
        #[arg(last = true, allow_hyphen_values = true, value_name = "CMD")]
        command: Vec<String>,
    },
}

impl Command {
    fn to_request(&self) -> Request {
        match self {
            // These are answered locally in run(); they never reach the socket.
            Command::Learn => unreachable!("learn is handled before to_request"),
            Command::Doctor => unreachable!("doctor is handled before to_request"),
            Command::Record { .. } => unreachable!("record is handled before to_request"),
            Command::Ls => Request::List,
            Command::New {
                title,
                cwd,
                command,
            } => Request::New {
                title: title.clone(),
                command: command.clone(),
                cwd: cwd.clone(),
            },
            Command::Kill { tab } => Request::Kill { tab: *tab },
            Command::Send {
                tab,
                text,
                enter,
                keys,
            } => Request::Send {
                tab: *tab,
                text: text.clone(),
                enter: *enter,
                keys: *keys,
            },
            Command::Capture {
                tab,
                scrollback,
                cells,
            } => Request::Capture {
                tab: *tab,
                scrollback: *scrollback,
                cells: *cells,
            },
            Command::Rename { tab, title } => Request::Rename {
                tab: *tab,
                title: title.clone(),
            },
            Command::Screenshot { .. } => Request::Screenshot,
            Command::Select { tab } => Request::Select { tab: *tab },
            Command::Bidi { tab, mode } => Request::Bidi {
                tab: *tab,
                mode: mode.clone(),
            },
        }
    }
}

/// Turn a server-side error into one the reader can act on.
///
/// The wire protocol is additive, so a `terra` newer than the running
/// `terra-app` is a normal state of the world — you upgrade the CLI, or build
/// it from a checkout, and the installed app is still last week's. serde
/// answers an unknown `cmd` with "unknown variant `screenshot`", which is
/// accurate and completely unhelpful about what to do next. Neither side
/// crashes; only the message needs work.
fn explain(error: &str) -> String {
    if error.contains("unknown variant") {
        format!(
            "{error}\n\
             the running terra app does not know this command — it is older than \
             this CLI. Upgrade it with `just upgrade` (or restart it from a build \
             of this checkout)."
        )
    } else {
        error.to_string()
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("terra: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    if matches!(cli.command, Command::Learn) {
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&learn_json())?);
        } else {
            print!("{}", learn_text());
        }
        return Ok(());
    }
    // The terminal-inspection commands are self-contained: no socket, no
    // running terra-app needed.
    match &cli.command {
        Command::Doctor => {
            let facts = doctor::gather();
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&doctor::render_json(&facts))?
                );
            } else {
                print!("{}", doctor::render_text(&facts));
            }
            return Ok(());
        }
        Command::Record {
            out,
            decode,
            command,
        } => {
            if let Some(path) = decode {
                return record::decode_file(std::path::Path::new(path));
            }
            // clap's required_unless_present guarantees `out` is here.
            let out = out.as_deref().unwrap_or_default();
            let code = record::record(std::path::Path::new(out), command)?;
            // The recorder is transparent, so it exits as the child did.
            std::process::exit(code);
        }
        _ => {}
    }

    // Checked before the request rather than after: taking a screenshot and
    // only then discovering there is nowhere to put it steals the window's
    // focus for nothing. clap cannot express it — `--json` is a global flag,
    // and `required_unless_present` does not see it when it is written before
    // the subcommand (`terra --json screenshot`).
    if let Command::Screenshot { out: None, .. } = &cli.command {
        if !cli.json {
            anyhow::bail!(
                "screenshot needs --out <PATH> (or --json, to get the base64 payload instead)"
            );
        }
    }

    let req = cli.command.to_request();
    let resp = request(&req)?;

    if cli.json {
        println!("{}", serde_json::to_string(&resp)?);
        // A failed operation is still an error for the shell, even in --json mode.
        if let Response::Err { error } = &resp {
            eprintln!("terra: {}", explain(error));
            std::process::exit(1);
        }
        return Ok(());
    }

    match resp {
        Response::Err { error } => Err(anyhow::anyhow!(explain(&error))),
        Response::Ok {
            tabs,
            text,
            tab,
            png,
        } => {
            match cli.command {
                Command::Ls => {
                    let tabs = tabs.unwrap_or_default();
                    let table = format_tabs(&tabs);
                    if !table.is_empty() {
                        println!("{table}");
                    }
                }
                Command::New { .. } => {
                    if let Some(id) = tab {
                        println!("{id}");
                    }
                }
                Command::Capture { .. } => {
                    if let Some(text) = text {
                        // Capture output is already newline-separated; don't
                        // add a second trailing newline.
                        print!("{text}");
                        if !text.ends_with('\n') {
                            println!();
                        }
                    }
                }
                Command::Screenshot { out, pretty, bg } => {
                    let encoded = png.context(
                        "the app answered without an image — is it a newer or older terra?",
                    )?;
                    let image = terra_protocol::decode_png(&encoded)?;
                    let image = if pretty {
                        let bg = match bg.as_deref() {
                            Some(spec) => pretty::parse_bg(spec)?,
                            None => pretty::DEFAULT_BG,
                        };
                        pretty::encode(&pretty::compose(&pretty::decode(&image)?, bg))?
                    } else {
                        image
                    };
                    // clap's `required_unless_present = "json"` guarantees this,
                    // and --json returned long before here.
                    let out = out.context("--out is required")?;
                    std::fs::write(&out, &image)
                        .with_context(|| format!("writing {}", out.display()))?;
                }
                Command::Bidi { .. } => {
                    // The app answers both the query and the set with the
                    // resulting mode.
                    if let Some(mode) = text {
                        println!("{mode}");
                    }
                }
                // kill / send / rename / select: silence on success.
                _ => {}
            }
            Ok(())
        }
    }
}

/// Render tabs as an aligned `ID  ACTIVE  TITLE` table (no trailing newline).
///
/// Returns an empty string when there are no tabs, so `ls` on an empty app
/// prints nothing at all (greppable, pipe-friendly).
fn format_tabs(tabs: &[TabInfo]) -> String {
    if tabs.is_empty() {
        return String::new();
    }

    const H_ID: &str = "ID";
    const H_ACTIVE: &str = "ACTIVE";

    let id_w = tabs
        .iter()
        .map(|t| t.id.to_string().len())
        .chain(std::iter::once(H_ID.len()))
        .max()
        .unwrap_or(H_ID.len());
    let active_w = H_ACTIVE.len();

    let mut out = format!("{H_ID:<id_w$}  {H_ACTIVE:<active_w$}  TITLE");
    for t in tabs {
        let marker = if t.active { "*" } else { "" };
        let id = t.id;
        out.push('\n');
        out.push_str(&format!(
            "{id:<id_w$}  {marker:<active_w$}  {}",
            t.title.replace(['\n', '\t'], " ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn tab(id: u64, title: &str, active: bool) -> TabInfo {
        TabInfo {
            id,
            title: title.to_string(),
            active,
        }
    }

    #[test]
    fn empty_table_is_empty() {
        assert_eq!(format_tabs(&[]), "");
    }

    #[test]
    fn table_has_header_and_active_marker() {
        let out = format_tabs(&[tab(1, "zsh", false), tab(2, "vim", true)]);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "ID  ACTIVE  TITLE");
        let title_col = lines[0].find("TITLE").unwrap();
        assert_eq!(&lines[1][title_col..], "zsh");
        assert_eq!(&lines[2][title_col..], "vim");
        assert!(lines[1].starts_with("1 "));
        assert!(!lines[1][..title_col].contains('*'));
        assert!(lines[2].starts_with("2 "));
        assert!(lines[2][..title_col].contains('*'));
    }

    #[test]
    fn wide_ids_stay_aligned() {
        let out = format_tabs(&[tab(7, "a", false), tab(1234, "b", true)]);
        let lines: Vec<&str> = out.lines().collect();
        // Every row's TITLE starts at the same column.
        let title_col = lines[0].find("TITLE").unwrap();
        assert_eq!(&lines[1][title_col..], "a");
        assert_eq!(&lines[2][title_col..], "b");
        assert!(lines[2][..title_col].contains('*'));
    }

    #[test]
    fn newlines_in_titles_do_not_break_rows() {
        let out = format_tabs(&[tab(1, "a\nb", false)]);
        assert_eq!(out.lines().count(), 2);
        assert!(out.ends_with("a b"));
    }

    #[test]
    fn new_maps_trailing_command() {
        let cli = Cli::parse_from([
            "terra", "new", "--title", "build", "--cwd", "/tmp", "--", "cargo", "test",
        ]);
        match cli.command.to_request() {
            Request::New {
                title,
                command,
                cwd,
            } => {
                assert_eq!(title.as_deref(), Some("build"));
                assert_eq!(cwd.as_deref(), Some("/tmp"));
                assert_eq!(command, vec!["cargo".to_string(), "test".to_string()]);
            }
            other => panic!("expected New, got {other:?}"),
        }
    }

    #[test]
    fn new_command_keeps_hyphenated_args() {
        match Cli::parse_from(["terra", "new", "--", "ls", "-la", "--color=auto"])
            .command
            .to_request()
        {
            Request::New { command, .. } => {
                assert_eq!(command, vec!["ls", "-la", "--color=auto"]);
            }
            other => panic!("expected New, got {other:?}"),
        }
    }

    #[test]
    fn new_without_command_is_empty_vec() {
        let cli = Cli::parse_from(["terra", "new"]);
        match cli.command.to_request() {
            Request::New {
                title,
                command,
                cwd,
            } => {
                assert!(title.is_none());
                assert!(cwd.is_none());
                assert!(command.is_empty());
            }
            other => panic!("expected New, got {other:?}"),
        }
    }

    #[test]
    fn send_enter_flag() {
        let cli = Cli::parse_from(["terra", "send", "3", "ls -la", "--enter"]);
        match cli.command.to_request() {
            Request::Send {
                tab,
                text,
                enter,
                keys,
            } => {
                assert_eq!(tab, 3);
                assert_eq!(text, "ls -la");
                assert!(enter);
                assert!(!keys, "text is literal unless --keys is given");
            }
            other => panic!("expected Send, got {other:?}"),
        }
    }

    /// `--keys` is what turns `{C-c}` into a control character rather than
    /// six literal ones, so it must reach the request.
    #[test]
    fn send_keys_flag_is_forwarded() {
        let cli = Cli::parse_from(["terra", "send", "3", "{C-c}", "--keys"]);
        match cli.command.to_request() {
            Request::Send { keys, text, .. } => {
                assert!(keys);
                assert_eq!(text, "{C-c}");
            }
            other => panic!("expected Send, got {other:?}"),
        }
    }

    #[test]
    fn capture_scrollback_defaults_to_zero() {
        match Cli::parse_from(["terra", "capture", "1"])
            .command
            .to_request()
        {
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
        match Cli::parse_from(["terra", "capture", "1", "--scrollback", "500"])
            .command
            .to_request()
        {
            Request::Capture { scrollback, .. } => assert_eq!(scrollback, 500),
            other => panic!("expected Capture, got {other:?}"),
        }
    }

    #[test]
    fn screenshot_maps_to_the_request_and_keeps_its_flags_local() {
        match Cli::parse_from(["terra", "screenshot", "--out", "shot.png"]).command {
            Command::Screenshot { out, pretty, bg } => {
                assert_eq!(out.as_deref(), Some(std::path::Path::new("shot.png")));
                assert!(!pretty);
                assert!(bg.is_none());
            }
            other => panic!("expected Screenshot, got {other:?}"),
        }
        assert!(matches!(
            Cli::parse_from(["terra", "screenshot", "--out", "a.png"])
                .command
                .to_request(),
            Request::Screenshot
        ));
    }

    /// `--out` is how the image gets anywhere, so it is required — except in
    /// `--json` mode, which prints the payload instead of writing a file.
    #[test]
    fn screenshot_needs_somewhere_to_put_the_image() {
        // The parser accepts it either way — the requirement is `run`'s,
        // because clap cannot see a global flag written before the subcommand.
        assert!(Cli::try_parse_from(["terra", "screenshot"]).is_ok());
        assert!(Cli::try_parse_from(["terra", "--json", "screenshot"]).is_ok());
        assert!(Cli::try_parse_from(["terra", "screenshot", "--json"]).is_ok());
        // …and the flag really does land on the parsed command in both spellings.
        assert!(Cli::parse_from(["terra", "--json", "screenshot"]).json);
        assert!(Cli::parse_from(["terra", "screenshot", "--json"]).json);
    }

    /// `--bg` only means anything to the compositor, so asking for it without
    /// `--pretty` is a mistake worth catching at the parser.
    #[test]
    fn a_background_without_pretty_is_rejected() {
        assert!(
            Cli::try_parse_from(["terra", "screenshot", "--out", "a.png", "--bg", "#000"]).is_err()
        );
        assert!(Cli::try_parse_from([
            "terra",
            "screenshot",
            "--out",
            "a.png",
            "--pretty",
            "--bg",
            "#000,#fff",
        ])
        .is_ok());
    }

    /// An app older than this CLI answers `screenshot` with serde's "unknown
    /// variant". That must not be the last word the user reads.
    #[test]
    fn an_unknown_verb_error_says_what_to_do_about_it() {
        let explained = explain("bad request: unknown variant `screenshot`, expected one of …");
        assert!(explained.contains("older than this CLI"), "{explained}");
        assert!(explained.contains("just upgrade"), "{explained}");
        // Everything else is passed through untouched.
        assert_eq!(explain("no such tab: 7"), "no such tab: 7");
    }

    #[test]
    fn json_flag_is_global() {
        assert!(Cli::parse_from(["terra", "ls", "--json"]).json);
        assert!(Cli::parse_from(["terra", "--json", "ls"]).json);
        assert!(!Cli::parse_from(["terra", "ls"]).json);
    }

    #[test]
    fn simple_subcommands_map_cleanly() {
        assert!(matches!(
            Cli::parse_from(["terra", "kill", "2"]).command.to_request(),
            Request::Kill { tab: 2 }
        ));
        assert!(matches!(
            Cli::parse_from(["terra", "select", "9"])
                .command
                .to_request(),
            Request::Select { tab: 9 }
        ));
        assert!(matches!(
            Cli::parse_from(["terra", "ls"]).command.to_request(),
            Request::List
        ));
        match Cli::parse_from(["terra", "rename", "4", "logs"])
            .command
            .to_request()
        {
            Request::Rename { tab, title } => {
                assert_eq!(tab, 4);
                assert_eq!(title, "logs");
            }
            other => panic!("expected Rename, got {other:?}"),
        }
    }

    #[test]
    fn bidi_with_no_mode_is_a_query() {
        match Cli::parse_from(["terra", "bidi", "3"]).command.to_request() {
            Request::Bidi { tab, mode } => {
                assert_eq!(tab, 3);
                assert!(mode.is_none());
            }
            other => panic!("expected Bidi, got {other:?}"),
        }
    }

    #[test]
    fn bidi_with_a_mode_sets_it() {
        for want in ["off", "on", "auto"] {
            match Cli::parse_from(["terra", "bidi", "5", want])
                .command
                .to_request()
            {
                Request::Bidi { tab, mode } => {
                    assert_eq!(tab, 5);
                    assert_eq!(mode.as_deref(), Some(want));
                }
                other => panic!("expected Bidi, got {other:?}"),
            }
        }
    }

    #[test]
    fn an_invalid_bidi_mode_is_rejected_by_the_parser() {
        let err = Cli::try_parse_from(["terra", "bidi", "1", "ON"]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("off"),
            "clap error should list the modes: {msg}"
        );
    }

    #[test]
    fn doctor_takes_no_arguments_and_honours_the_global_json_flag() {
        assert!(matches!(
            Cli::parse_from(["terra", "doctor"]).command,
            Command::Doctor
        ));
        assert!(Cli::parse_from(["terra", "doctor", "--json"]).json);
    }

    #[test]
    fn record_needs_an_out_path_or_a_recording_to_decode() {
        // Neither given: clap must refuse rather than silently record nowhere.
        assert!(Cli::try_parse_from(["terra", "record", "--", "vim"]).is_err());
        // Both given: they are mutually exclusive modes.
        assert!(Cli::try_parse_from([
            "terra", "record", "--out", "a.jsonl", "--decode", "b.jsonl",
        ])
        .is_err());
    }

    #[test]
    fn record_keeps_the_recorded_command_intact() {
        match Cli::parse_from([
            "terra",
            "record",
            "--out",
            "session.jsonl",
            "--",
            "codex",
            "--yolo",
            "-v",
        ])
        .command
        {
            Command::Record {
                out,
                decode,
                command,
            } => {
                assert_eq!(out.as_deref(), Some("session.jsonl"));
                assert!(decode.is_none());
                assert_eq!(command, vec!["codex", "--yolo", "-v"]);
            }
            other => panic!("expected Record, got {other:?}"),
        }
    }

    #[test]
    fn record_decode_reads_an_existing_recording() {
        match Cli::parse_from(["terra", "record", "--decode", "session.jsonl"]).command {
            Command::Record {
                out,
                decode,
                command,
            } => {
                assert!(out.is_none());
                assert_eq!(decode.as_deref(), Some("session.jsonl"));
                assert!(command.is_empty());
            }
            other => panic!("expected Record, got {other:?}"),
        }
    }

    #[test]
    fn the_learn_map_lists_the_terminal_inspection_commands() {
        // `learn` is generated from clap, so new subcommands must show up in it
        // without anyone remembering to edit prose.
        let text = learn_text();
        assert!(text.contains("terra doctor"), "{text}");
        assert!(text.contains("terra record"), "{text}");
        assert!(text.contains("[--out]"), "{text}");
    }

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
