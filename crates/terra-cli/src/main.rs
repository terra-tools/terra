//! `terra` CLI — tmux-style control of the terra app over its unix socket.
//!
//! Every subcommand maps to exactly one [`terra_protocol::Request`], sent with
//! the blocking [`terra_protocol::request`] helper. Output is plain and
//! greppable by default; `--json` prints the raw `Response` instead.

use anyhow::Result;
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
         \x20 id=$(terra new --title \"tests\" -- bash)\n\
         \x20 terra send \"$id\" \"cargo test\" --enter\n\
         \x20 sleep 5; terra capture \"$id\"\n\
         \x20 terra kill \"$id\"\n\n\
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

    /// Create a new tab; prints its id
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
}

impl Command {
    fn to_request(&self) -> Request {
        match self {
            // `learn` is answered locally in run(); it never reaches the socket.
            Command::Learn => unreachable!("learn is handled before to_request"),
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
            Command::Send { tab, text, enter } => Request::Send {
                tab: *tab,
                text: text.clone(),
                enter: *enter,
            },
            Command::Capture { tab, scrollback } => Request::Capture {
                tab: *tab,
                scrollback: *scrollback,
            },
            Command::Rename { tab, title } => Request::Rename {
                tab: *tab,
                title: title.clone(),
            },
            Command::Select { tab } => Request::Select { tab: *tab },
        }
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
    let req = cli.command.to_request();
    let resp = request(&req)?;

    if cli.json {
        println!("{}", serde_json::to_string(&resp)?);
        // A failed operation is still an error for the shell, even in --json mode.
        if let Response::Err { error } = &resp {
            eprintln!("terra: {error}");
            std::process::exit(1);
        }
        return Ok(());
    }

    match resp {
        Response::Err { error } => Err(anyhow::anyhow!(error)),
        Response::Ok { tabs, text, tab } => {
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
            Request::Send { tab, text, enter } => {
                assert_eq!(tab, 3);
                assert_eq!(text, "ls -la");
                assert!(enter);
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
            Request::Capture { tab, scrollback } => {
                assert_eq!(tab, 1);
                assert_eq!(scrollback, 0);
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
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
