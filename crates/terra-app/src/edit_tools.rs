//! "Edit Settings With …": the coding agents and editors installed on this
//! machine, and how terra hands each of them `~/.terra/config.toml`.
//!
//! terra's settings are a file, so "open the settings" has always meant
//! "open the file" ([`crate::open_config_in_editor`], the ⌘, path). This
//! module is the other half of that idea: if the user already has an agent
//! CLI, the fastest way to change a setting is to *ask* for it, and the agent
//! needs nothing from terra but the path and a sentence of context.
//!
//! # Finding the tools
//!
//! A GUI app launched from Finder inherits launchd's environment, not a
//! terminal's — `PATH` is `/usr/bin:/bin:/usr/sbin:/sbin` and nothing a user
//! installed is on it. That is the same problem `tabs.rs` solves for tabs by
//! spawning a *login* shell, and [`login_path`] solves it here the same way:
//! ask the user's own shell what its `PATH` is, once. [`FALLBACK_DIRS`] covers
//! the case where that fails (no `$SHELL`, a shell that will not start), and
//! is where Homebrew, `~/.local/bin` and the npm global prefix put things
//! anyway.
//!
//! On macOS an editor is also findable as an *application*: plenty of people
//! run VS Code or Cursor without ever installing the `code`/`cursor` shell
//! shim. [`bundle_installed`] asks LaunchServices, so those users get the row
//! too — it just opens the file through `open -a` instead of the CLI.
//!
//! The whole probe costs one shell start plus one `open -Ra` per editor, so it
//! runs **once**, on a background thread ([`prime`]), and everything after
//! that reads [`detected`].

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::tab_icon::TabIcon;

/// What terra tells an agent when it opens a tab to edit the config.
///
/// One constant, shared by every agent entry: `{config}` is the only thing
/// that varies, and it is the config path, not the tool. Both `claude` and
/// `codex` take an initial prompt as a single positional argument, so this
/// arrives as one `argv` entry and the agent starts its first turn on it.
///
/// It points the agent at the *user's own file* and nothing else. terra seeds
/// a missing `~/.terra/config.toml` from the documented example
/// (`crate::ensure_config_file`), so the comments that document the keys are
/// in the file itself — naming `docs/config.example.toml` instead would send
/// the agent hunting for a repository path that an installed user does not
/// have. The claim is deliberately weak ("the comments document the keys",
/// not "every key is documented"): a config that predates a key, or that has
/// been hand-stripped of its comments, must not make the prompt a lie.
///
/// Asking before editing is the other half. This is the *settings* file of the
/// terminal the agent is running inside, so a wrong guess is felt immediately.
pub const EDIT_PROMPT: &str = "Open {config} and help me adjust my Terra settings. Ask me what I want to change before editing. The comments in the file document the keys it supports — keep those comments intact when you edit.";

/// [`EDIT_PROMPT`] with the real, expanded config path in it.
pub fn edit_prompt(config: &Path) -> String {
    EDIT_PROMPT.replace("{config}", &config.display().to_string())
}

/// Directories searched when the login shell cannot be asked, and in addition
/// to whatever it says. The three places a user-installed CLI actually lands
/// on macOS, plus the system ones.
const FALLBACK_DIRS: &[&str] = &[
    "~/.local/bin",
    "~/bin",
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
];

/// One tool terra can hand the config file to.
///
/// Declaration order is the order the rows appear in, everywhere: agents
/// first (they *change* the file for you), then editors (they show it to you).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EditTool {
    ClaudeCode,
    Codex,
    VsCode,
    Cursor,
}

impl EditTool {
    pub const ALL: &'static [Self] = &[Self::ClaudeCode, Self::Codex, Self::VsCode, Self::Cursor];

    /// The command that runs it. Also the tab suffix for an agent tab
    /// (`config · claude`) and the palette action's id (`config.edit.claude`).
    pub const fn cli(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
            Self::VsCode => "code",
            Self::Cursor => "cursor",
        }
    }

    /// The macOS application this tool also ships as, for the users who have
    /// the app but never ran "Install 'code' command in PATH". Agents are
    /// CLIs and nothing else, so they have none.
    pub const fn bundle(self) -> Option<&'static str> {
        match self {
            Self::ClaudeCode | Self::Codex => None,
            Self::VsCode => Some("Visual Studio Code"),
            Self::Cursor => Some("Cursor"),
        }
    }

    /// What the row says, after "Edit Settings With".
    pub const fn label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::VsCode => "VS Code",
            Self::Cursor => "Cursor",
        }
    }

    /// Stable id fragment: the palette action is `config.edit.<slug>`, and the
    /// macOS menu item's tag is this variant's index in [`Self::ALL`].
    pub const fn slug(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
            Self::VsCode => "vscode",
            Self::Cursor => "cursor",
        }
    }

    pub const fn icon(self) -> TabIcon {
        match self {
            Self::ClaudeCode => TabIcon::Claude,
            Self::Codex => TabIcon::OpenAi,
            Self::VsCode => TabIcon::VsCode,
            Self::Cursor => TabIcon::Cursor,
        }
    }

    /// An *agent* gets a terra tab and a prompt; an *editor* just gets the
    /// file. The distinction is the whole difference between the two kinds of
    /// row, so it lives here rather than being re-derived at each call site.
    pub const fn is_agent(self) -> bool {
        matches!(self, Self::ClaudeCode | Self::Codex)
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|t| t.slug() == slug)
    }
}

/// A tool that is actually installed, and how to reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub tool: EditTool,
    /// The resolved CLI, absolute — terra's own `PATH` cannot find it again.
    /// `None` for an editor found only as a macOS application bundle.
    pub cli: Option<PathBuf>,
}

/// The installed subset of [`EditTool::ALL`], in declaration order.
///
/// Pure: the filesystem arrives as `resolve` (a program name -> its absolute
/// path) and `bundle` (an application name -> is it installed), so the whole
/// policy — CLI first, application bundle as a fallback, agents never having
/// one — is testable without either.
pub fn detect_with(
    resolve: impl Fn(&str) -> Option<PathBuf>,
    bundle: impl Fn(&str) -> bool,
) -> Vec<Found> {
    EditTool::ALL
        .iter()
        .filter_map(|tool| match resolve(tool.cli()) {
            Some(path) => Some(Found {
                tool: *tool,
                cli: Some(path),
            }),
            None if tool.bundle().is_some_and(&bundle) => Some(Found {
                tool: *tool,
                cli: None,
            }),
            None => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The real probe
// ---------------------------------------------------------------------------

static DETECTED: OnceLock<Vec<Found>> = OnceLock::new();

/// Start the probe on a background thread, so the shell start it does is not
/// on the way to the first frame. Safe to call more than once.
pub fn prime() {
    if DETECTED.get().is_some() {
        return;
    }
    std::thread::Builder::new()
        .name("terra-edit-tools".to_owned())
        .spawn(|| {
            let found = detect();
            log::debug!(
                "terra: settings editors: {:?}",
                found.iter().map(|f| f.tool).collect::<Vec<_>>()
            );
            let _ = DETECTED.set(found);
        })
        .ok();
}

/// The probe's answer, or `None` while it is still running.
///
/// Non-blocking on purpose: the macOS menu is built from this on whichever
/// frame it first becomes available, and a UI thread must never wait on a
/// subprocess.
pub fn ready() -> Option<&'static [Found]> {
    DETECTED.get().map(Vec::as_slice)
}

/// The probe's answer, empty until it lands. What the palette reads — by the
/// time a human has pressed ⇧⌘P the thread started at launch is long done,
/// and an empty list simply means "no extra rows this once".
pub fn detected() -> &'static [Found] {
    ready().unwrap_or(&[])
}

/// Where a detected tool lives, if it was found at all.
pub fn found(tool: EditTool) -> Option<&'static Found> {
    detected().iter().find(|f| f.tool == tool)
}

fn detect() -> Vec<Found> {
    let dirs = search_dirs();
    detect_with(|program| lookup(&dirs, program), bundle_installed)
}

/// First directory in `dirs` holding an executable file called `program`.
fn lookup(dirs: &[PathBuf], program: &str) -> Option<PathBuf> {
    dirs.iter()
        .map(|dir| dir.join(program))
        .find(|path| is_executable(path))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Every directory worth looking in: the login shell's `PATH`, then terra's
/// own, then [`FALLBACK_DIRS`] — deduplicated, order preserved.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push = |dir: PathBuf| {
        if !dir.as_os_str().is_empty() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };
    for raw in login_path().iter().chain(std::env::var("PATH").iter()) {
        for dir in std::env::split_paths(raw) {
            push(dir);
        }
    }
    for dir in FALLBACK_DIRS {
        push(expand_tilde(dir));
    }
    dirs
}

fn expand_tilde(dir: &str) -> PathBuf {
    match (dir.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(dir),
    }
}

/// What `$SHELL -lc 'printf %s "$PATH"'` says.
///
/// A login shell is what reads `.zprofile`/`.profile`, which is where every
/// PATH-extending installer writes — the same reasoning as `tabs::login_args`,
/// and the reason a tab can run `claude` when terra's own process cannot.
/// `None` if there is no `$SHELL`, if it will not start, or if it answers with
/// nothing; the caller still has [`FALLBACK_DIRS`].
#[cfg(unix)]
fn login_path() -> Option<String> {
    let shell = std::env::var("SHELL").ok()?;
    let out = std::process::Command::new(shell)
        .args(["-lc", "printf %s \"$PATH\""])
        .output()
        .ok()?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!path.is_empty()).then_some(path)
}

#[cfg(not(unix))]
fn login_path() -> Option<String> {
    None
}

/// Is an application named `name` installed? LaunchServices knows, wherever
/// the bundle actually sits — `/Applications`, `~/Applications`, a volume.
#[cfg(target_os = "macos")]
fn bundle_installed(name: &str) -> bool {
    std::process::Command::new("/usr/bin/open")
        .arg("-Ra")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(not(target_os = "macos"))]
fn bundle_installed(_name: &str) -> bool {
    false
}

/// Hand `path` to an editor: its CLI when there is one, else the macOS
/// application bundle. Agents do not come through here — they get a tab.
pub fn open_file_with(tool: EditTool, path: &Path) {
    let Some(found) = found(tool) else {
        log::warn!("terra: {} is not installed", tool.label());
        return;
    };
    let mut command = match (&found.cli, tool.bundle()) {
        (Some(cli), _) => {
            let mut c = std::process::Command::new(cli);
            c.arg(path);
            c
        }
        (None, Some(bundle)) => {
            let mut c = std::process::Command::new("/usr/bin/open");
            c.arg("-a").arg(bundle).arg(path);
            c
        }
        // `detect_with` cannot produce this: no CLI and no bundle is not found.
        (None, None) => return,
    };
    if let Err(err) = command.spawn() {
        log::warn!(
            "terra: cannot open {} in {}: {err}",
            path.display(),
            tool.label()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(paths: &[&str], bundles: &[&str]) -> Vec<Found> {
        let paths: Vec<String> = paths.iter().map(|p| (*p).to_owned()).collect();
        let bundles: Vec<String> = bundles.iter().map(|b| (*b).to_owned()).collect();
        detect_with(
            |program| {
                paths
                    .contains(&program.to_owned())
                    .then(|| PathBuf::from("/opt/homebrew/bin").join(program))
            },
            |name| bundles.contains(&name.to_owned()),
        )
    }

    /// Nothing installed means no rows at all — the feature is entirely
    /// invisible on a machine that has none of the four.
    #[test]
    fn an_empty_machine_offers_nothing() {
        assert!(fake(&[], &[]).is_empty());
    }

    /// A CLI on PATH is the preferred way in, and the absolute path is kept:
    /// terra's own PATH is launchd's and could not find it a second time.
    #[test]
    fn a_cli_on_path_is_found_with_its_absolute_location() {
        let found = fake(&["claude", "code"], &[]);
        assert_eq!(
            found,
            vec![
                Found {
                    tool: EditTool::ClaudeCode,
                    cli: Some(PathBuf::from("/opt/homebrew/bin/claude")),
                },
                Found {
                    tool: EditTool::VsCode,
                    cli: Some(PathBuf::from("/opt/homebrew/bin/code")),
                },
            ]
        );
    }

    /// The point of the bundle probe: VS Code installed, its shell shim never
    /// installed. The row still appears, with no CLI behind it.
    #[test]
    fn an_editor_without_its_shim_is_found_as_an_application() {
        let found = fake(&[], &["Visual Studio Code", "Cursor"]);
        assert_eq!(
            found.iter().map(|f| f.tool).collect::<Vec<_>>(),
            [EditTool::VsCode, EditTool::Cursor]
        );
        assert!(found.iter().all(|f| f.cli.is_none()));
    }

    /// An agent is a CLI and nothing else: an application of the same name
    /// must not conjure a row that would then have nothing to run.
    #[test]
    fn an_agent_is_never_found_as_an_application() {
        assert!(fake(&[], &["Claude Code", "Codex"]).is_empty());
    }

    /// The CLI wins when both exist, because `code <file>` beats round-tripping
    /// through LaunchServices.
    #[test]
    fn the_cli_wins_over_the_bundle() {
        let found = fake(&["cursor"], &["Cursor"]);
        assert_eq!(found.len(), 1);
        assert!(found[0].cli.is_some());
    }

    /// Rows come out in declaration order — agents first — whatever order the
    /// probe happened to find them in.
    #[test]
    fn rows_keep_their_declared_order() {
        let found = fake(&["cursor", "code", "codex", "claude"], &[]);
        assert_eq!(
            found.iter().map(|f| f.tool).collect::<Vec<_>>(),
            EditTool::ALL
        );
    }

    /// One prompt, one substitution: the real expanded path of the user's own
    /// config, and nothing the user does not have on disk.
    #[test]
    fn the_prompt_carries_the_expanded_config_path() {
        let prompt = edit_prompt(Path::new("/Users/me/.terra/config.toml"));
        assert!(prompt.starts_with("Open /Users/me/.terra/config.toml and help me adjust"));
        assert!(!prompt.contains("{config}"));
        // Both agents are handed the *same* sentence — nothing about it is
        // parameterised by which one is being asked.
        assert_eq!(
            edit_prompt(Path::new("/x/config.toml")),
            edit_prompt(Path::new("/x/config.toml"))
        );
    }

    /// The prompt names the user's own file and only that. A repository path
    /// is not something an installed user has, and sending an agent to look
    /// for one wastes its first turn — the seeded config carries the same
    /// comments (see `crate::ensure_config_file`).
    #[test]
    fn the_prompt_names_no_repository_path() {
        let prompt = edit_prompt(Path::new("/Users/me/.terra/config.toml"));
        assert!(!prompt.contains("docs/"));
        assert!(!prompt.contains("config.example.toml"));
        // It asks first, and says the comments are the documentation without
        // promising that every key is present in any given file.
        assert!(prompt.contains("Ask me what I want to change before editing."));
        assert!(prompt.contains("comments in the file document the keys"));
        assert!(!prompt.contains("Every key"));
    }

    /// Slugs are the wire between the palette's action ids, the macOS menu's
    /// tags and this enum, so the round trip has to hold for every variant.
    #[test]
    fn every_tool_round_trips_through_its_slug() {
        for tool in EditTool::ALL {
            assert_eq!(EditTool::from_slug(tool.slug()), Some(*tool));
        }
        assert_eq!(EditTool::from_slug("emacs"), None);
    }

    /// Agents wear their own mark, editors theirs — the same marks the tabs
    /// wear, so a "config · claude" tab and the row that opened it match.
    #[test]
    fn each_tool_wears_its_own_brand() {
        let icons: Vec<TabIcon> = EditTool::ALL.iter().map(|t| t.icon()).collect();
        assert_eq!(
            icons,
            [
                TabIcon::Claude,
                TabIcon::OpenAi,
                TabIcon::VsCode,
                TabIcon::Cursor
            ]
        );
        // And the agents' marks are exactly what a tab running that CLI
        // resolves to on its own, which is what keeps the two in step.
        assert_eq!(
            crate::tab_icon::from_process("claude"),
            Some(EditTool::ClaudeCode.icon())
        );
        assert_eq!(
            crate::tab_icon::from_process("codex"),
            Some(EditTool::Codex.icon())
        );
    }

    /// `~` in the fallback list is expanded against `$HOME`; an absolute entry
    /// is left alone.
    #[test]
    fn a_tilde_directory_expands_against_home() {
        let home = std::env::var("HOME").unwrap_or_default();
        if !home.is_empty() {
            assert_eq!(
                expand_tilde("~/.local/bin"),
                PathBuf::from(&home).join(".local/bin")
            );
        }
        assert_eq!(expand_tilde("/usr/bin"), PathBuf::from("/usr/bin"));
    }

    /// Whatever the environment, the fallback directories are always searched,
    /// so a launchd-minimal PATH still finds a Homebrew install.
    #[test]
    fn the_fallback_directories_are_always_searched() {
        let dirs = search_dirs();
        for fallback in FALLBACK_DIRS {
            assert!(
                dirs.contains(&expand_tilde(fallback)),
                "{fallback} missing from {dirs:?}"
            );
        }
        // Deduplicated: /usr/bin is both a fallback and on every real PATH.
        let mut sorted = dirs.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), dirs.len());
    }
}
