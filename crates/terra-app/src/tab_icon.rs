//! The little logo on each tab pill: what to draw, and how it gets on screen.
//!
//! Windows Terminal puts a per-tab icon on the left of the title so a row of
//! pills can be read at a glance instead of parsed. terra does the same, and
//! the interesting part is not the drawing but *deciding*: a tab is not "a
//! Python tab", it is a shell that has a Python in it right now and will not in
//! a minute.
//!
//! # Three sources, in order
//!
//! 1. **The foreground processes' arguments.** A wrapped CLI is invisible to
//!    its process name: `codex` is a node script, so the kernel says `node`.
//!    Its argv says `node …/bin/codex`, and [`resolve_argv`] reads the argv of
//!    the whole chain from the innermost process up to the shell before
//!    anything else, because it is the one source that is both specific and
//!    certain.
//! 2. **The foreground process's name.** [`crate::procinfo`] walks the process
//!    tree down from a tab's shell to name whatever is actually running in it —
//!    that machinery exists for the BiDi quirks table, and it is exactly the
//!    question an icon asks. This is the source that gets `htop` right, because
//!    it is the only one that knows `htop` exited.
//! 3. **Text.** The tab's effective title plus the command it was spawned with,
//!    matched on whole words. Wrong more often, but it is all there is when the
//!    process table cannot be read (a sandbox, an unsupported platform), and it
//!    answers instantly for a tab whose command has not been typed yet.
//!
//! Anything unmatched gets a generic `>_` glyph rather than nothing, so the
//! titles in a row stay aligned instead of jittering left and right as
//! programs come and go.
//!
//! # What does *not* get a logo
//!
//! A default shell is not an identity. zsh, bash, sh, dash and cmd wear the
//! generic glyph, because a row of tabs that all say "you are in a shell" says
//! nothing at all and a quiet row makes the one tab running something stand
//! out. `fish` and PowerShell keep their marks: nobody ends up in fish by
//! accident, and on Windows "this is pwsh, not cmd" is real information.
//!
//! # Colour
//!
//! Brand icons keep their brand colour — that is the whole point of
//! recognising them at a glance. Only the generic glyph is tinted to the pill's
//! text colour, because it is chrome rather than a logo. A brand colour too
//! dark to see against terra's dark bar (Rust's is literally `#000000`) is
//! lifted toward white by [`readable_on_dark`]; nothing else is touched.
//!
//! # Rendering, without an SVG renderer
//!
//! egui cannot draw SVG, and pulling in `resvg` to rasterise sixteen fixed
//! shapes at one fixed size would be a heavyweight dependency doing a build
//! step's job. So the SVGs in `assets/tab-icons/` are rasterised once, by hand,
//! to 64px PNGs (see that directory's `LICENSE.md` for the command) and those
//! are what ship. Decoding them needs no new dependency either: `eframe`
//! already exposes a PNG decoder for the window icon.
//!
//! 64px is 2–5× larger than an icon is ever drawn, and egui's bilinear
//! sampling takes one sample per output pixel, which turns a thin stroke into a
//! flickering dotted line. [`resample`] therefore box-filters the master down
//! to the exact pixel size the tab bar will draw it at, and the result is
//! cached per (icon, size) in egui's memory.

use std::collections::HashMap;

use egui::{Color32, ColorImage, Context, Rect, TextureHandle, TextureOptions, Ui};

use crate::procinfo::Foreground;

/// Edge length of every shipped PNG. Assets are square by construction; a file
/// that is not is dropped rather than stretched (see [`master`]).
const MASTER: usize = 64;

/// How often the process table may be consulted, in seconds.
///
/// The walk itself is one `sysctl` for the whole machine, but at 120fps
/// "cheap" still adds up, and no human reads an icon that changed 8ms ago. A
/// tab that has just opened is exempt — see [`IconCache::poll`].
const POLL_SECS: f64 = 1.0;

/// Relative luminance below which a brand colour is unreadable on terra's dark
/// tab bar, and the luminance [`readable_on_dark`] lifts it to.
///
/// Deliberately low: this is a rescue for near-black marks, not a house style.
/// At `0.10` the darkest colour that survives untouched is Python's `#3776AB`
/// (0.166) and the only shipped icon that moves is Rust's `#000000`.
const MIN_LUMINANCE: f32 = 0.10;
const LIFTED_LUMINANCE: f32 = 0.40;

// ---------------------------------------------------------------------------
// The icon set
// ---------------------------------------------------------------------------

/// One shipped icon.
///
/// Small and closed on purpose: every variant is a file in
/// `assets/tab-icons/`, and adding a program that maps to an existing variant
/// costs a row in [`BY_PROCESS`] and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TabIcon {
    Claude,
    /// Deliberately *not* a variant for zsh, bash, sh or dash: a login shell is
    /// the tab's furniture, not its identity, and a row of shell logos says
    /// nothing. Those wear [`Self::Terminal`]. `fish` keeps its mark because
    /// nobody arrives at fish by default, and PowerShell keeps one because on
    /// Windows the distinction between it and `cmd` is real.
    PowerShell,
    Fish,
    Python,
    Node,
    Docker,
    Git,
    Htop,
    Vim,
    Neovim,
    Tmux,
    Rust,
    /// OpenAI's blossom mark, for `codex`.
    OpenAi,
    /// OpenCode's own mark, for `opencode`.
    OpenCode,
    /// The fallback `>_`. Not a logo — see the module docs on colour.
    Terminal,
}

impl TabIcon {
    /// A stable name, used to key the texture cache. Never shown to the user.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::PowerShell => "powershell",
            Self::Fish => "fishshell",
            Self::Python => "python",
            Self::Node => "nodedotjs",
            Self::Docker => "docker",
            Self::Git => "git",
            Self::Htop => "htop",
            Self::Vim => "vim",
            Self::Neovim => "neovim",
            Self::Tmux => "tmux",
            Self::Rust => "rust",
            Self::OpenAi => "openai",
            Self::OpenCode => "opencode",
            Self::Terminal => "terminal",
        }
    }

    /// The 64px master, compiled into the binary.
    pub const fn png(self) -> &'static [u8] {
        match self {
            Self::Claude => include_bytes!("../assets/tab-icons/claude-64.png"),
            Self::PowerShell => include_bytes!("../assets/tab-icons/powershell-64.png"),
            Self::Fish => include_bytes!("../assets/tab-icons/fishshell-64.png"),
            Self::Python => include_bytes!("../assets/tab-icons/python-64.png"),
            Self::Node => include_bytes!("../assets/tab-icons/nodedotjs-64.png"),
            Self::Docker => include_bytes!("../assets/tab-icons/docker-64.png"),
            Self::Git => include_bytes!("../assets/tab-icons/git-64.png"),
            Self::Htop => include_bytes!("../assets/tab-icons/htop-64.png"),
            Self::Vim => include_bytes!("../assets/tab-icons/vim-64.png"),
            Self::Neovim => include_bytes!("../assets/tab-icons/neovim-64.png"),
            Self::Tmux => include_bytes!("../assets/tab-icons/tmux-64.png"),
            Self::Rust => include_bytes!("../assets/tab-icons/rust-64.png"),
            Self::OpenAi => include_bytes!("../assets/tab-icons/openai-64.png"),
            Self::OpenCode => include_bytes!("../assets/tab-icons/opencode-64.png"),
            Self::Terminal => include_bytes!("../assets/tab-icons/terminal-64.png"),
        }
    }

    /// Whether this is the "I have no idea" glyph, which is drawn as chrome:
    /// tinted to the pill's text colour and faded back, so a row of unmatched
    /// tabs stays quiet.
    pub const fn is_generic(self) -> bool {
        matches!(self, Self::Terminal)
    }

    /// Every variant, so the asset test can prove that each one has a file
    /// behind it that decodes. Nothing at runtime wants the whole set —
    /// textures are built lazily, for the icons actually on screen.
    #[cfg(test)]
    pub const ALL: &'static [Self] = &[
        Self::Claude,
        Self::PowerShell,
        Self::Fish,
        Self::Python,
        Self::Node,
        Self::Docker,
        Self::Git,
        Self::Htop,
        Self::Vim,
        Self::Neovim,
        Self::Tmux,
        Self::Rust,
        Self::OpenAi,
        Self::OpenCode,
        Self::Terminal,
    ];
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// Process basename -> icon, matched exactly.
///
/// Exact rather than substring, because this side of the lookup gets a real
/// executable name and the cost of a wrong answer is a tab wearing someone
/// else's logo. Version suffixes are handled by [`strip_version_suffix`]
/// instead of being enumerated, so `python3.13` needs no row of its own.
const BY_PROCESS: &[(&str, TabIcon)] = &[
    ("claude", TabIcon::Claude),
    // The default shells, mapped to the generic glyph on purpose — see
    // [`TabIcon::PowerShell`]. They are rows rather than omissions because
    // `resolve` needs to tell "this tab is a shell" from "no idea", and because
    // a shell is a *host*: it yields to anything the text knows better.
    ("zsh", TabIcon::Terminal),
    ("bash", TabIcon::Terminal),
    ("sh", TabIcon::Terminal),
    ("dash", TabIcon::Terminal),
    ("cmd", TabIcon::Terminal),
    ("pwsh", TabIcon::PowerShell),
    ("powershell", TabIcon::PowerShell),
    ("fish", TabIcon::Fish),
    ("python", TabIcon::Python),
    ("ipython", TabIcon::Python),
    ("pip", TabIcon::Python),
    ("uv", TabIcon::Python),
    ("poetry", TabIcon::Python),
    ("node", TabIcon::Node),
    ("npm", TabIcon::Node),
    ("npx", TabIcon::Node),
    ("pnpm", TabIcon::Node),
    ("yarn", TabIcon::Node),
    ("bun", TabIcon::Node),
    ("deno", TabIcon::Node),
    ("docker", TabIcon::Docker),
    ("docker-compose", TabIcon::Docker),
    ("podman", TabIcon::Docker),
    ("git", TabIcon::Git),
    ("lazygit", TabIcon::Git),
    ("tig", TabIcon::Git),
    ("gh", TabIcon::Git),
    ("htop", TabIcon::Htop),
    ("btop", TabIcon::Htop),
    ("top", TabIcon::Htop),
    ("vim", TabIcon::Vim),
    ("vi", TabIcon::Vim),
    ("nvim", TabIcon::Neovim),
    ("neovim", TabIcon::Neovim),
    ("tmux", TabIcon::Tmux),
    ("screen", TabIcon::Tmux),
    ("zellij", TabIcon::Tmux),
    ("cargo", TabIcon::Rust),
    ("rustc", TabIcon::Rust),
    ("rustup", TabIcon::Rust),
    ("rust-analyzer", TabIcon::Rust),
    ("codex", TabIcon::OpenAi),
    ("opencode", TabIcon::OpenCode),
];

/// Keyword -> icon for the text fallback, **in priority order**.
///
/// Order is load-bearing where one keyword is a suffix of another under
/// [`contains_word`]'s boundary rules: `nvim` must be tried before `vim`.
const BY_KEYWORD: &[(&str, TabIcon)] = &[
    ("claude", TabIcon::Claude),
    // Before `codex`: `opencode` contains no `codex`, but keeping the pair
    // adjacent is how the next agent CLI gets added without a surprise.
    ("opencode", TabIcon::OpenCode),
    ("codex", TabIcon::OpenAi),
    ("lazygit", TabIcon::Git),
    ("ipython", TabIcon::Python),
    ("nvim", TabIcon::Neovim),
    ("neovim", TabIcon::Neovim),
    ("htop", TabIcon::Htop),
    ("btop", TabIcon::Htop),
    ("docker", TabIcon::Docker),
    ("podman", TabIcon::Docker),
    ("python", TabIcon::Python),
    ("node", TabIcon::Node),
    ("npm", TabIcon::Node),
    ("pnpm", TabIcon::Node),
    ("yarn", TabIcon::Node),
    ("cargo", TabIcon::Rust),
    ("rustc", TabIcon::Rust),
    ("tmux", TabIcon::Tmux),
    ("zellij", TabIcon::Tmux),
    ("vim", TabIcon::Vim),
    ("git", TabIcon::Git),
    ("pwsh", TabIcon::PowerShell),
    ("powershell", TabIcon::PowerShell),
    ("fish", TabIcon::Fish),
];

/// Drop a trailing version, so one row covers a whole family: `python3.13`,
/// `python3` and `python` are the same program, and `node22` is `node`.
///
/// Never returns the empty string — a name that is *all* digits is a name we
/// have no business rewriting.
fn strip_version_suffix(name: &str) -> &str {
    let trimmed = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    if trimmed.is_empty() {
        name
    } else {
        trimmed
    }
}

/// The icon for a foreground process basename, or `None` for one we do not
/// recognise.
///
/// `name` is what [`crate::procinfo::foreground_command`] returns — already
/// lowercased and stripped of a login shell's leading `-` — but this normalises
/// again anyway, so it is equally usable on a raw `argv[0]`.
pub fn from_process(name: &str) -> Option<TabIcon> {
    let lowered = name.trim().to_ascii_lowercase();
    let name = lowered.rsplit(['/', '\\']).next().unwrap_or(&lowered);
    let name = name.strip_prefix('-').unwrap_or(name);
    let name = name.strip_suffix(".exe").unwrap_or(name);
    let lookup = |n: &str| BY_PROCESS.iter().find(|(k, _)| *k == n).map(|(_, i)| *i);
    lookup(name).or_else(|| lookup(strip_version_suffix(name)))
}

/// Whether `needle` occurs in `haystack` as a word rather than as any old run
/// of letters.
///
/// The point is that a tab sitting in `~/src/gitlab-runner` is not a Git tab.
/// So the character before the match must not be alphanumeric — but the one
/// *after* may be a digit, because `python3` and `node22` are still Python and
/// Node. `haystack` must already be lowercase.
fn contains_word(haystack: &str, needle: &str) -> bool {
    let mut from = 0;
    while let Some(offset) = haystack[from..].find(needle) {
        let start = from + offset;
        let end = start + needle.len();
        let before_ok = haystack[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        let after_ok = haystack[end..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphabetic());
        if before_ok && after_ok {
            return true;
        }
        // Advance past this occurrence's first character, not past the whole
        // match: overlapping candidates are rare but free to allow.
        from = start + haystack[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// The icon suggested by free text — a tab title, a spawn command, or both
/// joined. `None` when nothing matches.
pub fn from_text(text: &str) -> Option<TabIcon> {
    let text = text.to_ascii_lowercase();
    BY_KEYWORD
        .iter()
        .find(|(word, _)| contains_word(&text, word))
        .map(|(_, icon)| *icon)
}

/// Icons that belong to interpreters and shells — *hosts* that are frequently
/// just the runtime of something more specific. claude code is a node script,
/// so its foreground process says `node`; half the CLI world says `python`.
/// When the process says "host" and the text names a guest, the guest is the
/// tab's real identity.
///
/// The plain shells used to be here too. They are hosts in exactly this sense,
/// but they now resolve to the generic glyph, which [`resolve`] already treats
/// as yielding — so listing them would be saying the same thing twice.
fn is_host(icon: TabIcon) -> bool {
    matches!(icon, TabIcon::Node | TabIcon::Python | TabIcon::Fish)
}

/// Whether an icon is specific enough to end the search: a real brand mark for
/// a program that is not merely hosting another one.
fn is_specific(icon: TabIcon) -> bool {
    !is_host(icon) && !icon.is_generic()
}

/// The icon for a tab: its foreground process if we recognise it — except that
/// an interpreter, or a shell (which resolves to the generic glyph), yields to a
/// more specific text match — else its text, else the generic glyph.
///
/// Pure. [`resolve_argv`] is the entry point that also gets a look at the
/// process's arguments; this one is what it falls back to.
pub fn resolve(foreground: Option<&str>, text: &str) -> TabIcon {
    match foreground.and_then(from_process) {
        Some(icon) if is_specific(icon) => icon,
        // A host *or* the generic glyph: both mean "something is running here
        // that the process name does not identify", and the title may.
        Some(weak) => from_text(text).unwrap_or(weak),
        None => from_text(text).unwrap_or(TabIcon::Terminal),
    }
}

/// Extensions worth looking through, because the file that carries them is a
/// program rather than data: `codex.js` is codex. Kept short on purpose — this
/// runs on argument *paths*, where a longer list would start eating filenames.
const SCRIPT_EXTENSIONS: &[&str] = &["js", "mjs", "cjs", "ts", "py", "rb"];

/// `…/codex.js` -> `…/codex`, and everything else unchanged.
fn strip_script_extension(arg: &str) -> &str {
    match arg.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && SCRIPT_EXTENSIONS.contains(&ext) => stem,
        _ => arg,
    }
}

/// The icon one argv entry names on its own, or `None`.
///
/// Two attempts, narrowest first. The basename is the reliable one: `codex`,
/// `codex.js`, `/opt/homebrew/bin/codex` are all the row `codex` in
/// [`BY_PROCESS`]. The second is for the launcher whose script is named after
/// nothing (`…/claude-code/cli.js`, `…/codex/index.js`) — there the identity is
/// in a *directory* of the path, so the whole entry is run through the keyword
/// matcher, which is word-boundary anchored and so does not fire on
/// `~/src/gitlab-runner`. It is tried only on entries that are paths, and only
/// for a specific icon: a host or the generic glyph from a path is no evidence
/// at all.
fn from_argv_entry(arg: &str) -> Option<TabIcon> {
    let by_name = from_process(arg).or_else(|| from_process(strip_script_extension(arg)));
    by_name
        .or_else(|| arg.contains(['/', '\\']).then(|| from_text(arg)).flatten())
        .filter(|icon| is_specific(*icon))
}

/// [`resolve`], with the foreground process's arguments consulted first.
///
/// This is what makes a wrapped CLI recognisable no matter what the title says.
/// `codex` and `claude` are node scripts: the process table calls them `node`,
/// and if the tab's title is still the shell's — `~/src/terra`, because the
/// program never set one — the old chain had nothing left to go on and fell all
/// the way to the generic glyph. The argv says `node …/bin/codex`, which is not
/// ambiguous.
///
/// `argv` is the whole foreground chain innermost-first (see
/// [`crate::procinfo::chain_to_shell`]), not one process, because the deepest
/// process is not always the interesting one — a live `codex` session is
/// `zsh → node → codex → node_repl`, and its identity is one step up from the
/// bottom. Taking the first *recognisable* entry is innermost-wins with a
/// fallback rather than a second rule.
///
/// Arguments win over the title because they are what the kernel says is
/// running, and lose to nothing: they are only consulted for a *specific* icon,
/// so a bare `node` REPL (`node`, `/usr/local/bin/node`) matches nothing here
/// and drops through to [`resolve`], which gives it Node's mark as before.
pub fn resolve_argv(foreground: Option<&str>, argv: &[String], text: &str) -> TabIcon {
    match argv.iter().find_map(|arg| from_argv_entry(arg)) {
        Some(icon) => icon,
        None => resolve(foreground, text),
    }
}

/// The title as the pill should draw it. TUIs that predate the icons prefix
/// their titles with a status glyph doing an icon's job — claude code's
/// `✳ Claude Code` spinner is the canonical case — and next to a real brand
/// icon that reads as two icons. When the pill wears a brand icon, one
/// leading symbol character (anything from the arrows/dingbats planes up,
/// never a letter, digit or ASCII mark) plus its trailing space is dropped.
/// Paint-time only: capture, rename, `ls` and the window title keep the
/// original string.
pub fn display_title(title: &str, icon: Option<TabIcon>) -> &str {
    if icon.is_none_or(TabIcon::is_generic) {
        return title;
    }
    let mut chars = title.chars();
    match (chars.next(), chars.next()) {
        (Some(marker), Some(' ')) if (marker as u32) >= 0x2190 && !marker.is_alphanumeric() => {
            chars.as_str()
        }
        _ => title,
    }
}

// ---------------------------------------------------------------------------
// Per-tab cache
// ---------------------------------------------------------------------------

/// What [`IconCache::poll`] needs to know about one tab.
pub struct TabFacts<'a> {
    pub id: u64,
    /// The tab's shell pid, for the process-tree walk.
    pub shell_pid: Option<u32>,
    /// Effective title and spawn command, already joined — see [`resolve`].
    pub text: &'a str,
}

/// One icon per tab, with the process lookup throttled.
///
/// The text half is recomputed every poll because it is a handful of substring
/// searches and a title can change at any moment; only the process half, which
/// is a syscall, is rate-limited.
#[derive(Default)]
pub struct IconCache {
    /// Last known foreground process per tab — its name and argv. Kept between
    /// polls so a throttled frame still resolves against the primary source.
    foreground: HashMap<u64, Option<Foreground>>,
    icons: HashMap<u64, TabIcon>,
    checked: f64,
}

impl IconCache {
    /// The icon for `id`, or `None` when icons are off or the tab has not been
    /// polled yet.
    pub fn get(&self, id: u64) -> Option<TabIcon> {
        self.icons.get(&id).copied()
    }

    /// Forget everything — for the config kill-switch being turned off, so
    /// turning it back on does not paint a stale row for a frame.
    pub fn clear(&mut self) {
        self.foreground.clear();
        self.icons.clear();
        self.checked = f64::NEG_INFINITY;
    }

    /// Refresh from `tabs`. `now` is a monotonically increasing seconds clock
    /// (egui's `input.time`).
    ///
    /// `lookup` maps a batch of shell pids to their foreground processes; it is
    /// a parameter so this is testable without a process table, and so the
    /// whole batch costs one snapshot rather than one per tab.
    pub fn poll<F>(&mut self, now: f64, tabs: &[TabFacts<'_>], lookup: F)
    where
        F: FnOnce(&[u32]) -> Vec<Option<Foreground>>,
    {
        // A tab that opened since the last poll must not wait out the interval
        // wearing the wrong icon, so a new id forces the walk.
        let unseen = tabs.iter().any(|t| !self.foreground.contains_key(&t.id));
        if unseen || now - self.checked >= POLL_SECS {
            self.checked = now;
            let pids: Vec<u32> = tabs.iter().filter_map(|t| t.shell_pid).collect();
            let found = lookup(&pids);
            let mut found = found.into_iter();
            let mut fresh = HashMap::with_capacity(tabs.len());
            for tab in tabs {
                let process = match tab.shell_pid {
                    // `lookup` is contractually one answer per pid, in order;
                    // a short answer degrades to "no opinion" rather than
                    // shifting every later tab onto the wrong process.
                    Some(_) => found.next().flatten(),
                    None => None,
                };
                fresh.insert(tab.id, process);
            }
            self.foreground = fresh;
        }

        self.icons = tabs
            .iter()
            .map(|tab| {
                let fg = self.foreground.get(&tab.id).and_then(Option::as_ref);
                let name = fg.map(|f| f.name.as_str());
                let argv = fg.map_or(&[][..], |f| f.argv.as_slice());
                (tab.id, resolve_argv(name, argv, tab.text))
            })
            .collect();
    }
}

// ---------------------------------------------------------------------------
// Pixels
// ---------------------------------------------------------------------------

/// sRGB -> linear, for one channel in `0..=1`.
fn to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// linear -> sRGB, the inverse of [`to_linear`].
fn to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Lift a brand colour that would vanish into terra's dark tab bar.
///
/// Blends toward white *in linear light*, which makes the amount solvable
/// rather than tuned: mixing a colour of luminance `l` with white by `t` gives
/// `l + t(1 - l)`, so the `t` that reaches [`LIFTED_LUMINANCE`] is one
/// division. Hue is preserved for anything that has one; `#000000` has none and
/// comes out a neutral light grey, which is the correct answer for Rust's mark
/// on a dark background.
///
/// Colours at or above [`MIN_LUMINANCE`] are returned byte-for-byte unchanged —
/// this must never quietly restyle a brand that was already legible.
pub fn readable_on_dark(rgb: [u8; 3]) -> [u8; 3] {
    let lin = rgb.map(|c| to_linear(f32::from(c) / 255.0));
    let luminance = 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2];
    if luminance >= MIN_LUMINANCE {
        return rgb;
    }
    let t = ((LIFTED_LUMINANCE - luminance) / (1.0 - luminance)).clamp(0.0, 1.0);
    lin.map(|c| (to_srgb(c + t * (1.0 - c)).clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// Box-filter `src` (a `sw`x`sw` square of premultiplied RGBA) down to `dw`x`dw`.
///
/// Area-averaging rather than point sampling: the masters are 64px and a tab
/// draws them at 26, so one sample per output pixel would drop three quarters
/// of every stroke and alias differently at every DPI. Fractional box edges are
/// weighted, so no ratio is a special case and `dw > sw` degenerates to
/// something sane rather than reading out of bounds.
///
/// Premultiplied in, premultiplied out — averaging straight alpha would halo
/// every edge with the icon's colour at zero coverage.
fn resample(src: &[u8], sw: usize, dw: usize) -> Vec<u8> {
    if dw == sw {
        return src.to_vec();
    }
    let scale = sw as f32 / dw as f32;
    let mut out = vec![0u8; dw * dw * 4];
    for y in 0..dw {
        let (y0, y1) = (y as f32 * scale, (y + 1) as f32 * scale);
        for x in 0..dw {
            let (x0, x1) = (x as f32 * scale, (x + 1) as f32 * scale);
            let mut acc = [0.0f32; 4];
            let mut weight = 0.0f32;
            for sy in y0 as usize..(y1.ceil() as usize).min(sw) {
                let wy = (y1.min(sy as f32 + 1.0) - y0.max(sy as f32)).max(0.0);
                for sx in x0 as usize..(x1.ceil() as usize).min(sw) {
                    let wx = (x1.min(sx as f32 + 1.0) - x0.max(sx as f32)).max(0.0);
                    let w = wx * wy;
                    let at = (sy * sw + sx) * 4;
                    for (channel, slot) in acc.iter_mut().enumerate() {
                        *slot += w * f32::from(src[at + channel]);
                    }
                    weight += w;
                }
            }
            let at = (y * dw + x) * 4;
            if weight > 0.0 {
                for (channel, slot) in acc.iter().enumerate() {
                    out[at + channel] = (slot / weight).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
    out
}

/// Decode an icon's master PNG into premultiplied RGBA, with a dark brand
/// colour lifted and the generic glyph reduced to a white mask.
///
/// The generic glyph is drawn black in the asset and tinted at paint time, so
/// its colour is thrown away here and only its coverage kept: a white
/// premultiplied mask multiplied by a tint *is* the tint.
///
/// `None` for an asset that will not decode or is not square — a missing icon
/// is a missing icon, never a panic on a UI thread.
fn master(icon: TabIcon) -> Option<Vec<u8>> {
    let image = eframe::icon_data::from_png_bytes(icon.png())
        .inspect_err(|err| log::warn!("terra: tab icon {}: {err}", icon.key()))
        .ok()?;
    if image.width as usize != MASTER || image.height as usize != MASTER {
        log::warn!(
            "terra: tab icon {} is {}x{}, expected {MASTER}x{MASTER}",
            icon.key(),
            image.width,
            image.height
        );
        return None;
    }
    let generic = icon.is_generic();
    let mut out = Vec::with_capacity(image.rgba.len());
    for pixel in image.rgba.chunks_exact(4) {
        let alpha = u32::from(pixel[3]);
        let rgb = if generic {
            [255, 255, 255]
        } else {
            readable_on_dark([pixel[0], pixel[1], pixel[2]])
        };
        for channel in rgb {
            out.push((u32::from(channel) * alpha / 255) as u8);
        }
        out.push(pixel[3]);
    }
    Some(out)
}

/// The texture for `icon` at `px` physical pixels, built once and then kept
/// alive in egui's memory.
///
/// The handle has to be *stored*, not just returned: dropping the last one
/// frees the texture, so a cache that only remembered ids would upload a fresh
/// copy every frame.
fn texture(ctx: &Context, icon: TabIcon, px: usize) -> Option<TextureHandle> {
    let key = egui::Id::new(("terra_tab_icon", icon.key(), px));
    if let Some(handle) = ctx.data(|d| d.get_temp::<TextureHandle>(key)) {
        return Some(handle);
    }
    let pixels = resample(&master(icon)?, MASTER, px);
    let image = ColorImage::new(
        [px, px],
        pixels
            .chunks_exact(4)
            .map(|p| Color32::from_rgba_premultiplied(p[0], p[1], p[2], p[3]))
            .collect(),
    );
    let handle = ctx.load_texture(icon.key(), image, TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(key, handle.clone()));
    Some(handle)
}

/// Draw `icon` inside `rect`.
///
/// `tint` is applied to the generic glyph only; brand icons carry their own
/// colour and take just its alpha, so an inactive pill can fade its icon back
/// without bleaching the logo.
pub fn paint(ui: &Ui, icon: TabIcon, rect: Rect, tint: Color32) {
    paint_on(ui.ctx(), ui.painter(), icon, rect, tint);
}

/// [`paint`] for a caller that has no `Ui` — the floating drag ghost lives on
/// its own layer and only ever holds a context and a painter.
pub fn paint_on(
    ctx: &egui::Context,
    painter: &egui::Painter,
    icon: TabIcon,
    rect: Rect,
    tint: Color32,
) {
    let ppp = ctx.pixels_per_point();
    // Land on whole device pixels: a 13px glyph straddling a half pixel loses
    // the crispness the box filter was for.
    let snap = |v: f32| (v * ppp).round() / ppp;
    let rect = Rect::from_min_size(
        egui::pos2(snap(rect.min.x), snap(rect.min.y)),
        egui::Vec2::splat(snap(rect.width())),
    );
    // Never upscale the master: past 64px egui's own filtering is as good as
    // anything we would do here, and the texture cache stops growing.
    let px = ((rect.width() * ppp).round() as usize).clamp(1, MASTER);
    let Some(texture) = texture(ctx, icon, px) else {
        return;
    };
    let color = if icon.is_generic() {
        tint
    } else {
        Color32::WHITE.gamma_multiply(tint.a() as f32 / 255.0)
    };
    painter.image(
        texture.id(),
        rect,
        Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        color,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One process as the lookup would report it: a name and, optionally, the
    /// argv the kernel gave for it.
    fn process(name: &str, argv: &[&str]) -> Option<Foreground> {
        Some(Foreground {
            name: name.to_string(),
            argv: argv.iter().map(|a| (*a).to_string()).collect(),
        })
    }

    #[test]
    fn a_recognised_foreground_process_wins_over_the_title() {
        // The title says the tab is sitting in a Rust checkout; the process
        // table says htop is on screen. The process table is right.
        assert_eq!(
            resolve(Some("htop"), "~/src/terra  cargo"),
            TabIcon::Htop,
            "the foreground process is the primary source"
        );
    }

    #[test]
    fn an_interpreter_yields_to_what_it_is_running() {
        // claude code is a node script: the process table says `node`, the
        // title says claude. The title names the tab's real identity.
        assert_eq!(resolve(Some("node"), "✳ Claude Code"), TabIcon::Claude);
        assert_eq!(resolve(Some("python3"), "aider main.py"), TabIcon::Python);
        // A bare interpreter with nothing more specific keeps its own mark.
        assert_eq!(resolve(Some("node"), "node"), TabIcon::Node);
        assert_eq!(resolve(Some("node"), "~"), TabIcon::Node);
    }

    #[test]
    fn the_title_is_consulted_only_when_the_process_is_unknown() {
        assert_eq!(
            resolve(Some("some-inhouse-tool"), "docker ps"),
            TabIcon::Docker
        );
        assert_eq!(resolve(None, "claude"), TabIcon::Claude);
    }

    #[test]
    fn nothing_recognised_still_yields_a_glyph() {
        let icon = resolve(Some("some-inhouse-tool"), "~/Documents/terra");
        assert_eq!(icon, TabIcon::Terminal);
        assert!(icon.is_generic());
    }

    #[test]
    fn a_login_shell_and_a_full_path_name_the_same_program() {
        // All four spellings of a plain shell reach the same row — which is
        // now the generic glyph, not a shell logo.
        assert_eq!(from_process("-zsh"), Some(TabIcon::Terminal));
        assert_eq!(from_process("/bin/zsh"), Some(TabIcon::Terminal));
        assert_eq!(
            from_process(r"C:\Program Files\Git\bin\bash.exe"),
            Some(TabIcon::Terminal)
        );
        assert_eq!(from_process("  ZSH  "), Some(TabIcon::Terminal));
        // The same normalisation on a shell that *does* have a mark, so this
        // still tests the path/dash/case handling and not just one constant.
        assert_eq!(from_process("/usr/local/bin/fish"), Some(TabIcon::Fish));
        assert_eq!(from_process("-fish"), Some(TabIcon::Fish));
        assert_eq!(
            from_process(r"C:\Program Files\PowerShell\7\pwsh.exe"),
            Some(TabIcon::PowerShell)
        );
    }

    /// A default shell is the tab's furniture. zsh, bash, sh, dash and cmd all
    /// wear the quiet `>_`; fish and PowerShell are chosen, and keep a mark.
    #[test]
    fn a_plain_shell_is_chrome_rather_than_a_brand() {
        for shell in ["zsh", "bash", "sh", "dash", "cmd"] {
            assert_eq!(from_process(shell), Some(TabIcon::Terminal), "{shell}");
            assert_eq!(resolve(Some(shell), "~/src/terra"), TabIcon::Terminal);
            assert!(resolve(Some(shell), "~/src/terra").is_generic());
        }
        assert_eq!(resolve(Some("fish"), "~/src/terra"), TabIcon::Fish);
        assert_eq!(resolve(Some("pwsh"), "~"), TabIcon::PowerShell);
        assert_eq!(resolve(Some("powershell"), "~"), TabIcon::PowerShell);
        assert_eq!(resolve(None, "pwsh -NoLogo"), TabIcon::PowerShell);
        assert!(!TabIcon::PowerShell.is_generic());
        assert!(!TabIcon::Fish.is_generic());
    }

    /// The shells resolve to the generic glyph, and the generic glyph must
    /// yield to the title exactly as an interpreter does — otherwise a tab
    /// running `docker` under zsh would be stuck wearing `>_`.
    #[test]
    fn a_shell_yields_to_what_the_title_says_is_running() {
        assert_eq!(resolve(Some("zsh"), "docker ps"), TabIcon::Docker);
        assert_eq!(resolve(Some("-bash"), "✳ Claude Code"), TabIcon::Claude);
        // ...and a fish tab, whose icon is a real one, yields the same way.
        assert_eq!(resolve(Some("fish"), "docker ps"), TabIcon::Docker);
        // A shell with nothing to yield to keeps the glyph.
        assert_eq!(resolve(Some("zsh"), "~"), TabIcon::Terminal);
    }

    #[test]
    fn a_versioned_interpreter_needs_no_row_of_its_own() {
        assert_eq!(from_process("python3.13"), Some(TabIcon::Python));
        assert_eq!(from_process("python3"), Some(TabIcon::Python));
        assert_eq!(from_process("node22"), Some(TabIcon::Node));
        // The stripper must not eat a name that is only digits, and must not
        // invent a match for one.
        assert_eq!(from_process("7"), None);
    }

    #[test]
    fn an_unrelated_program_gets_no_icon_from_the_process_table() {
        assert_eq!(from_process("ssh"), None);
        assert_eq!(from_process("some-inhouse-tool"), None);
        assert_eq!(from_process(""), None);
    }

    /// The two coding agents that ship a mark of their own. `codex` wears
    /// OpenAI's blossom; `opencode` wears OpenCode's — neither falls back to
    /// the generic glyph, and neither borrows the other's.
    #[test]
    fn the_agent_clis_each_wear_their_own_mark() {
        assert_eq!(from_process("codex"), Some(TabIcon::OpenAi));
        assert_eq!(from_process("opencode"), Some(TabIcon::OpenCode));
        assert_eq!(resolve(Some("codex"), ""), TabIcon::OpenAi);
        assert_eq!(resolve(Some("opencode"), ""), TabIcon::OpenCode);
        // …and by title alone, before the process has even started.
        assert_eq!(resolve(None, "codex --help"), TabIcon::OpenAi);
        assert_eq!(resolve(None, "opencode run"), TabIcon::OpenCode);
        assert!(!TabIcon::OpenAi.is_generic());
        assert!(!TabIcon::OpenCode.is_generic());
    }

    /// The failure this exists for: `codex` is a node script, so the process
    /// table says `node`, and a tab whose title is still the shell's (`~/src`)
    /// had nothing left to identify it. The argv is not ambiguous.
    #[test]
    fn a_wrapped_cli_is_recognised_from_its_arguments_whatever_the_title_says() {
        let codex = [
            "/opt/homebrew/bin/node".to_string(),
            "node".to_string(),
            "/opt/homebrew/lib/node_modules/@openai/codex/bin/codex.js".to_string(),
        ];
        assert_eq!(
            resolve_argv(Some("node"), &codex, "~/src/terra"),
            TabIcon::OpenAi
        );
        // The `.js` is not the point: the same launcher without an extension,
        // as an `env node` shebang script produces, resolves identically.
        let bare = [
            "/usr/bin/node".to_string(),
            "/Users/me/.nvm/bin/codex".to_string(),
        ];
        assert_eq!(resolve_argv(Some("node"), &bare, "~"), TabIcon::OpenAi);
        // A script named after nothing — the identity is a directory of the
        // path, which the word-boundary keyword matcher still finds.
        let claude = [
            "/usr/bin/node".to_string(),
            "node".to_string(),
            "/Users/me/.claude/local/node_modules/claude-code/cli.js".to_string(),
        ];
        assert_eq!(
            resolve_argv(Some("node"), &claude, "~/src"),
            TabIcon::Claude
        );
    }

    /// Arguments are only consulted for a *specific* answer, so a bare
    /// interpreter — whose argv is nothing but its own name — still lands on
    /// the old chain and keeps the interpreter's mark.
    #[test]
    fn a_bare_interpreter_is_not_talked_out_of_its_own_icon_by_its_own_argv() {
        let repl = ["/usr/local/bin/node".to_string(), "node".to_string()];
        assert_eq!(resolve_argv(Some("node"), &repl, "~"), TabIcon::Node);
        // ...and the title still outranks a host, exactly as before.
        assert_eq!(
            resolve_argv(Some("node"), &repl, "✳ Claude Code"),
            TabIcon::Claude
        );
        // A plain shell's argv says shell, which is no evidence: still `>_`.
        let shell = ["/bin/zsh".to_string(), "-zsh".to_string()];
        assert_eq!(
            resolve_argv(Some("zsh"), &shell, "~/src"),
            TabIcon::Terminal
        );
        // No argv at all is the pre-argv behaviour, exactly.
        assert_eq!(resolve_argv(Some("htop"), &[], "~/src"), TabIcon::Htop);
        assert_eq!(resolve_argv(None, &[], "docker ps"), TabIcon::Docker);
    }

    /// Earlier arguments win, so an interpreter never shadows its script and a
    /// script never shadows the *file* it was given.
    #[test]
    fn the_first_specific_argument_wins() {
        let editing = [
            "/usr/bin/vim".to_string(),
            "vim".to_string(),
            "/Users/me/src/docker-compose.yml".to_string(),
        ];
        assert_eq!(resolve_argv(Some("vim"), &editing, ""), TabIcon::Vim);
        // The argument that names a program beats the one that names data,
        // in the order the kernel listed them.
        let wrapped = [
            "/usr/bin/env".to_string(),
            "/usr/bin/python3".to_string(),
            "/tmp/htop-notes.py".to_string(),
        ];
        assert_eq!(resolve_argv(Some("env"), &wrapped, ""), TabIcon::Htop);
    }

    /// An argument that is not a path is matched by name only: the keyword
    /// sweep is what makes a *path* readable, and running it over ordinary
    /// words would re-import every false positive the process table avoids.
    #[test]
    fn a_plain_argument_is_not_scanned_as_free_text() {
        assert_eq!(from_argv_entry("commit-to-git"), None);
        assert_eq!(from_argv_entry("gitlab"), None);
        assert_eq!(from_argv_entry("/usr/bin/gitlab-runner"), None);
        assert_eq!(from_argv_entry("git"), Some(TabIcon::Git));
        // A host or the generic glyph is not an answer, at either stage.
        assert_eq!(from_argv_entry("/usr/local/bin/node"), None);
        assert_eq!(from_argv_entry("/bin/zsh"), None);
    }

    #[test]
    fn only_a_script_extension_is_looked_through() {
        assert_eq!(strip_script_extension("codex.js"), "codex");
        assert_eq!(strip_script_extension("/a/b/manage.py"), "/a/b/manage");
        // Not an extension we know: left alone, so `python3.13` still reaches
        // the version stripper intact.
        assert_eq!(strip_script_extension("python3.13"), "python3.13");
        assert_eq!(strip_script_extension("archive.tar.gz"), "archive.tar.gz");
        assert_eq!(strip_script_extension(".js"), ".js");
        assert_eq!(strip_script_extension("codex"), "codex");
    }

    /// The cache is what the tab bar actually calls, so the argv has to survive
    /// the trip through it — including the throttled frames in between.
    #[test]
    fn the_cache_resolves_a_tab_from_the_argv_it_was_handed() {
        let mut cache = IconCache::default();
        cache.poll(
            0.0,
            &[TabFacts {
                id: 1,
                shell_pid: Some(7),
                text: "~/src/terra",
            }],
            |_| {
                process("node", &["/usr/bin/node", "node", "/tmp/codex/index.js"])
                    .map_or_else(Vec::new, |f| vec![Some(f)])
            },
        );
        assert_eq!(cache.get(1), Some(TabIcon::OpenAi));
        // A throttled frame answers from the same remembered argv.
        cache.poll(
            0.1,
            &[TabFacts {
                id: 1,
                shell_pid: Some(7),
                text: "~/src/terra",
            }],
            |_| panic!("the walk ran again inside the interval"),
        );
        assert_eq!(cache.get(1), Some(TabIcon::OpenAi));
    }

    /// The whole reason the text fallback matches words and not substrings.
    #[test]
    fn a_path_that_merely_contains_a_keyword_is_not_a_match() {
        assert_eq!(from_text("~/src/gitlab-runner"), None);
        assert_eq!(from_text("digits"), None);
        assert_eq!(from_text("nodemon watch"), None);
        // ...but a real word still matches, at either end and in the middle.
        assert_eq!(from_text("git"), Some(TabIcon::Git));
        assert_eq!(from_text("run git status"), Some(TabIcon::Git));
        assert_eq!(from_text("~/src/git"), Some(TabIcon::Git));
    }

    #[test]
    fn a_trailing_digit_does_not_break_a_keyword() {
        assert_eq!(from_text("python3 -m http.server"), Some(TabIcon::Python));
        assert_eq!(from_text("node22 server.js"), Some(TabIcon::Node));
    }

    /// `nvim` contains `vim`, and `lazygit` contains `git`: the more specific
    /// keyword has to be tried first or both tabs get the wrong logo.
    #[test]
    fn the_more_specific_keyword_wins() {
        assert_eq!(from_text("nvim src/main.rs"), Some(TabIcon::Neovim));
        assert_eq!(from_text("lazygit"), Some(TabIcon::Git));
        assert_eq!(from_text("ipython"), Some(TabIcon::Python));
        // and the general one still works on its own
        assert_eq!(from_text("vim src/main.rs"), Some(TabIcon::Vim));
    }

    #[test]
    fn matching_is_case_insensitive_on_both_sources() {
        assert_eq!(from_process("Python3"), Some(TabIcon::Python));
        assert_eq!(from_text("Docker Desktop"), Some(TabIcon::Docker));
    }

    #[test]
    fn every_shipped_asset_decodes_to_a_square_master_with_something_in_it() {
        for icon in TabIcon::ALL {
            let pixels = master(*icon).unwrap_or_else(|| panic!("{} did not decode", icon.key()));
            assert_eq!(pixels.len(), MASTER * MASTER * 4, "{}", icon.key());
            let covered = pixels.chunks_exact(4).filter(|p| p[3] > 0).count();
            assert!(covered > MASTER, "{} is blank", icon.key());
        }
    }

    /// The generic glyph is tinted at paint time, so its master must be a
    /// white mask; a brand icon must *not* have been bleached the same way.
    #[test]
    fn only_the_generic_glyph_is_reduced_to_a_mask() {
        let generic = master(TabIcon::Terminal).expect("decode");
        for p in generic.chunks_exact(4).filter(|p| p[3] == 255) {
            assert_eq!([p[0], p[1], p[2]], [255, 255, 255]);
        }
        let claude = master(TabIcon::Claude).expect("decode");
        assert!(
            claude
                .chunks_exact(4)
                .any(|p| p[3] == 255 && [p[0], p[1], p[2]] != [255, 255, 255]),
            "the brand colour was thrown away"
        );
    }

    #[test]
    fn a_legible_brand_colour_is_returned_untouched() {
        for colour in [
            [0xD9, 0x77, 0x57], // claude
            [0x37, 0x76, 0xAB], // python, the darkest one shipped
            [0x00, 0x90, 0x20], // htop
            [0x24, 0x96, 0xED], // docker
        ] {
            assert_eq!(readable_on_dark(colour), colour);
        }
    }

    #[test]
    fn a_black_brand_colour_is_lifted_until_it_can_be_seen() {
        let lifted = readable_on_dark([0, 0, 0]);
        assert!(lifted[0] > 140, "{lifted:?} is still too dark");
        // Neutral in, neutral out — a lift must not invent a hue.
        assert_eq!(lifted[0], lifted[1]);
        assert_eq!(lifted[1], lifted[2]);
    }

    #[test]
    fn a_lift_keeps_the_hue_it_was_given() {
        let lifted = readable_on_dark([0x20, 0x00, 0x00]);
        assert!(
            lifted[0] > lifted[1] && lifted[1] == lifted[2],
            "{lifted:?}"
        );
    }

    #[test]
    fn resampling_a_flat_square_changes_nothing_but_its_size() {
        let src = vec![200u8; 8 * 8 * 4];
        let out = resample(&src, 8, 3);
        assert_eq!(out.len(), 3 * 3 * 4);
        assert!(out.iter().all(|&c| c == 200), "{out:?}");
    }

    /// Two source pixels averaged into one is the property that stops a 64px
    /// master from aliasing when a tab draws it at 26.
    #[test]
    fn resampling_averages_rather_than_dropping_pixels() {
        // A 2x2 with one opaque pixel, down to 1x1: a point sample would
        // return 0 or 255, the box filter returns the coverage.
        let mut src = vec![0u8; 2 * 2 * 4];
        src[0..4].copy_from_slice(&[255, 255, 255, 255]);
        let out = resample(&src, 2, 1);
        assert_eq!(out, vec![64, 64, 64, 64]);
    }

    #[test]
    fn resampling_to_the_same_size_is_the_identity() {
        let src: Vec<u8> = (0..4 * 4 * 4).map(|i| i as u8).collect();
        assert_eq!(resample(&src, 4, 4), src);
    }

    #[test]
    fn the_process_table_is_not_consulted_more_than_once_a_second() {
        let mut cache = IconCache::default();
        let mut calls = 0;
        let poll = |cache: &mut IconCache, now: f64, calls: &mut i32| {
            cache.poll(
                now,
                &[TabFacts {
                    id: 1,
                    shell_pid: Some(42),
                    text: "~/src",
                }],
                |pids| {
                    *calls += 1;
                    assert_eq!(pids, [42]);
                    vec![process("htop", &[])]
                },
            );
        };
        poll(&mut cache, 0.0, &mut calls); // first sight of the tab
        poll(&mut cache, 0.1, &mut calls);
        poll(&mut cache, 0.9, &mut calls);
        assert_eq!(calls, 1, "the walk ran again inside the interval");
        poll(&mut cache, 1.5, &mut calls);
        assert_eq!(calls, 2);
        // And the throttled frames still had an answer to draw.
        assert_eq!(cache.get(1), Some(TabIcon::Htop));
    }

    /// A tab opened between polls must not sit there wearing the wrong icon
    /// until the interval elapses.
    #[test]
    fn a_new_tab_forces_a_walk_even_inside_the_interval() {
        let mut cache = IconCache::default();
        let one = [TabFacts {
            id: 1,
            shell_pid: Some(1),
            text: "",
        }];
        cache.poll(0.0, &one, |_| vec![process("zsh", &[])]);
        let mut called = false;
        let two = [
            TabFacts {
                id: 1,
                shell_pid: Some(1),
                text: "",
            },
            TabFacts {
                id: 2,
                shell_pid: Some(2),
                text: "",
            },
        ];
        cache.poll(0.05, &two, |_| {
            called = true;
            vec![process("zsh", &[]), process("docker", &[])]
        });
        assert!(called);
        assert_eq!(cache.get(2), Some(TabIcon::Docker));
    }

    /// A tab whose title changed gets a fresh answer without a syscall.
    #[test]
    fn the_text_half_is_re_evaluated_on_every_poll() {
        let mut cache = IconCache::default();
        cache.poll(
            0.0,
            &[TabFacts {
                id: 1,
                shell_pid: None,
                text: "~/src",
            }],
            |_| vec![],
        );
        assert_eq!(cache.get(1), Some(TabIcon::Terminal));
        cache.poll(
            0.1,
            &[TabFacts {
                id: 1,
                shell_pid: None,
                text: "docker ps",
            }],
            |_| panic!("no pid, no walk"),
        );
        assert_eq!(cache.get(1), Some(TabIcon::Docker));
    }

    #[test]
    fn a_closed_tab_is_dropped_rather_than_remembered() {
        let mut cache = IconCache::default();
        cache.poll(
            0.0,
            &[TabFacts {
                id: 1,
                shell_pid: None,
                text: "docker",
            }],
            |_| vec![],
        );
        cache.poll(2.0, &[], |_| vec![]);
        assert_eq!(cache.get(1), None);
    }

    /// A short answer from the lookup must not slide every later tab onto
    /// someone else's process.
    #[test]
    fn a_truncated_lookup_degrades_to_no_opinion() {
        let mut cache = IconCache::default();
        cache.poll(
            0.0,
            &[
                TabFacts {
                    id: 1,
                    shell_pid: Some(1),
                    text: "",
                },
                TabFacts {
                    id: 2,
                    shell_pid: Some(2),
                    text: "",
                },
            ],
            |_| vec![process("htop", &[])],
        );
        assert_eq!(cache.get(1), Some(TabIcon::Htop));
        assert_eq!(cache.get(2), Some(TabIcon::Terminal));
    }
}

#[cfg(test)]
mod display_title_tests {
    use super::*;

    #[test]
    fn a_status_glyph_yields_to_a_brand_icon() {
        assert_eq!(
            display_title("✳ Claude Code", Some(TabIcon::Claude)),
            "Claude Code"
        );
        assert_eq!(
            display_title("✻ Display random emojis", Some(TabIcon::Claude)),
            "Display random emojis"
        );
    }

    #[test]
    fn without_a_brand_icon_the_title_is_untouched() {
        assert_eq!(display_title("✳ Claude Code", None), "✳ Claude Code");
        assert_eq!(
            display_title("✳ Claude Code", Some(TabIcon::Terminal)),
            "✳ Claude Code"
        );
    }

    #[test]
    fn ordinary_titles_never_lose_their_first_word() {
        assert_eq!(display_title("~ home", Some(TabIcon::Docker)), "~ home");
        assert_eq!(display_title("[1] build", Some(TabIcon::Git)), "[1] build");
        assert_eq!(display_title("א שלום", Some(TabIcon::Claude)), "א שלום");
    }
}
