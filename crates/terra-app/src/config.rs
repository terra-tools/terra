//! `~/.terra/config.toml` — terra's configuration file.
//!
//! # Contract
//!
//! **Loading can never fail.** terra is a GUI: a process that refuses to start
//! because of a typo in a config file is a process that appears to do nothing
//! at all. Every error path here yields the compiled-in defaults plus a
//! warning; there is no `Result` in this module's public API.
//!
//! # Layers
//!
//! Three, resolved highest-first:
//!
//! 1. **session** — runtime overrides, e.g. toggling BiDi from the command
//!    palette. In memory only; gone when terra exits.
//! 2. **file** — `$TERRA_CONFIG`, else `~/.terra/config.toml`.
//! 3. **default** — the constants below, which are exactly the values terra
//!    hardcoded before this module existed.
//!
//! [`ConfigStore::reload`] re-reads the file while keeping the session layer
//! on top, so a reload never silently undoes something the user just toggled.
//!
//! # Why session overrides are not written back
//!
//! Round-tripping through `toml::to_string` would destroy the user's comments
//! and key order, so honest write-back means `toml_edit` and a surgical
//! patcher. Worse, every terra window shares one file and they would clobber
//! each other with no locking. A session font-size bump that evaporates on
//! restart is the predictable behaviour; the file stays unambiguously
//! hand-authored.
//!
//! # Fault isolation
//!
//! The file is parsed to a `toml::Table` first and each section is
//! deserialized independently, so a bad `[font]` costs you the font defaults
//! and nothing else. Unknown keys are ignored — and reported — rather than
//! rejected. `[profile.<name>]` goes one level further and is deserialized
//! *per profile*, so one broken profile costs you that profile and leaves the
//! others working.

use egui_term::BidiBase;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Defaults — the values terra hardcoded before this module existed
// ---------------------------------------------------------------------------

pub const DEFAULT_FONT_SIZE: f32 = 15.0;
pub const DEFAULT_LINE_HEIGHT: f32 = 1.3;
/// Do not reorder unless asked to.
///
/// This is a deliberate change from terra's original always-on behaviour, for
/// three reasons. It is byte-for-byte what every other terminal does — Ghostty
/// pins CoreText to embedding level 0 — so a config file that says nothing
/// gets the output every other tool on the machine was tested against, and
/// nothing an existing user relies on regresses into garbage. It is also the
/// only honest answer: a terminal receives one byte stream and UAX #9 gives
/// logical and visual order the *same* representation, so which one an
/// application meant is undecidable from the text. Some CLIs emit logical
/// order and expect the terminal to reorder; others run their own BiDi and
/// emit visual order, where reordering again double-reverses into nonsense.
/// Guessing is wrong half the time and wrong differently on each redraw; a
/// predictable default plus an explicit per-application table (`BidiQuirks`)
/// is wrong never.
pub const DEFAULT_BIDI: BidiMode = BidiMode::Off;
/// Show a per-tab icon in the tab bar.
///
/// On by default: an icon is how a row of pills stops being read and starts
/// being recognised. It is a switch rather than a fixed behaviour because the
/// primary detection source is a walk of the process table, and a user who
/// wants terra to look at nothing outside its own tabs — or who simply finds
/// logos in chrome noisy — should be able to say so. Off costs the walk too:
/// nothing polls when this is false.
pub const DEFAULT_TAB_ICONS: bool = true;
/// Autodetect the paragraph direction per row.
///
/// `Ltr` keeps a shell prompt provably immobile, but it strands RTL sentence
/// punctuation on the wrong side — the `?` of `היי מה קורה?` resolves to the
/// paragraph level and is left at the visual right. `Auto` is measured not to
/// move a prompt either: rule P2 stops at the first strong character, which
/// for `→ terra git:(main) ✗ …` is the `t` of `terra`.
pub const DEFAULT_BIDI_BASE: BidiBase = BidiBase::Auto;

/// Parse a `[text] bidi_base` value.
fn parse_bidi_base(name: &str) -> Option<BidiBase> {
    match name {
        "ltr" => Some(BidiBase::Ltr),
        "auto" => Some(BidiBase::Auto),
        "rtl" => Some(BidiBase::Rtl),
        _ => None,
    }
}

/// The spelling [`parse_bidi_base`] accepts, so a session override can round
/// trip through the same all-`Option` wire type the file layer uses.
fn bidi_base_name(base: BidiBase) -> &'static str {
    match base {
        BidiBase::Ltr => "ltr",
        BidiBase::Auto => "auto",
        BidiBase::Rtl => "rtl",
    }
}

// ---------------------------------------------------------------------------
// BiDi mode and the per-application quirks table
// ---------------------------------------------------------------------------

/// Whether the terminal reorders right-to-left text for a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BidiMode {
    /// Never reorder. Byte-for-byte what every other terminal does today.
    #[default]
    Off,
    /// Always reorder — for applications that emit logical order.
    On,
    /// Consult the quirks table for the tab's foreground process; fall back
    /// to `Off` when nothing matches.
    Auto,
}

impl BidiMode {
    /// Parse a config value. `None` for anything unrecognised.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "off" => Some(Self::Off),
            "on" => Some(Self::On),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }

    /// The spelling `parse` accepts, so a session override round-trips.
    ///
    /// The session layer stores its overrides in the same all-`Option`,
    /// all-strings wire shape the file layer uses, so anything it writes has
    /// to be a name [`BidiMode::parse`] takes back — otherwise a runtime
    /// toggle would resolve straight back to the default.
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::Auto => "auto",
        }
    }
}

/// Per-application overrides, keyed on a lowercased command basename.
///
/// This is a *compatibility* table, not language detection. Nothing here
/// inspects the text: the decision is made from which process is running in
/// the tab, because that process — not the bytes — is what determines whether
/// the stream is already in visual order.
#[derive(Debug, Clone, PartialEq)]
pub struct BidiQuirks {
    entries: BTreeMap<String, BidiMode>,
}

/// The default table is the shipped one, not an empty map, so a
/// `Config::default()` built anywhere in the app — the headless path, a test,
/// a window opened before the file was read — still knows about the
/// applications terra was actually measured against.
impl Default for BidiQuirks {
    fn default() -> Self {
        let mut quirks = Self {
            entries: BTreeMap::new(),
        };
        for (command, mode) in DEFAULT_QUIRKS {
            quirks.insert(command, *mode);
        }
        quirks
    }
}

impl BidiQuirks {
    /// Case-insensitive: `Codex` and `codex` are the same program, and the
    /// basename we are handed comes from whatever the user typed.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, command: &str) -> Option<BidiMode> {
        self.entries.get(&command.to_lowercase()).copied()
    }

    /// Insert one entry, overriding whatever a lower layer said for that key.
    /// Per key, so a user table naming one tool keeps the shipped rest.
    fn insert(&mut self, command: &str, mode: BidiMode) {
        self.entries.insert(command.to_lowercase(), mode);
    }
}

/// Shipped defaults, seeded from what was measured today. User entries in
/// `[text.bidi_quirks]` override these per key; they do not replace the table.
pub const DEFAULT_QUIRKS: &[(&str, BidiMode)] = &[
    ("claude", BidiMode::Off), // runs its own BiDi, emits visual order
    ("codex", BidiMode::On),   // emits logical order
];

/// Resolve whether to reorder, for a tab whose foreground command is
/// `command` (already a lowercased basename; `None` when unknown).
///
/// An explicit `on`/`off` is the user's stated answer and never consults the
/// table. Under `auto` a missing entry means `Off`, and so does an entry that
/// itself says `auto`: the table is the last place to ask, and "ask again"
/// there resolves to the same safe default as knowing nothing.
/// Whether `mode` means "reorder", given the quirks table and whatever
/// command is running. Split out from [`should_reorder`] so a caller that
/// already has a per-tab override can resolve it without a whole [`Config`].
/// Whether a tab with no override of its own should reorder.
pub fn should_reorder(cfg: &Config, command: Option<&str>) -> bool {
    should_reorder_mode(cfg.text.bidi, &cfg.text.quirks, command)
}

pub fn should_reorder_mode(mode: BidiMode, quirks: &BidiQuirks, command: Option<&str>) -> bool {
    match mode {
        BidiMode::Off => false,
        BidiMode::On => true,
        // A quirk that itself says `auto` carries no opinion, same as a
        // missing entry: fall through to the default rather than recursing.
        BidiMode::Auto => matches!(command.and_then(|c| quirks.get(c)), Some(BidiMode::On)),
    }
}

/// Below this the terminal is unreadable; above it a cell no longer fits.
const FONT_SIZE_RANGE: (f32, f32) = (6.0, 72.0);
/// `TerminalFont::new` already floors at 0.5; match it so the warning fires
/// here, where we can name the key, rather than being silently clamped later.
const LINE_HEIGHT_RANGE: (f32, f32) = (0.5, 3.0);

/// Every key terra understands, for unknown-key reporting.
///
/// `text.bidi_quirks` is listed as an ordinary key because that is all the
/// walk below sees: a direct key of `[text]` that happens to hold a table.
/// Its own keys are deliberately *not* enumerable — any command basename is
/// legal — so nothing descends into it, and `resolve` polices its values.
///
/// `[profile]` is absent on purpose: it is a table *of* tables whose names are
/// arbitrary, so [`report_unknown_keys`] descends into it by hand and checks
/// each profile's keys against [`PROFILE_KEYS`] instead.
const KNOWN: &[(&str, &[&str])] = &[
    ("font", &["size", "line_height"]),
    ("text", &["bidi", "bidi_base", "bidi_quirks"]),
    ("tabs", &["icons"]),
];

/// The section holding the named ways to open a tab: `[profile.<name>]`.
const PROFILE_SECTION: &str = "profile";

/// Every key a `[profile.<name>]` table may carry.
const PROFILE_KEYS: &[&str] = &["command", "cwd", "title"];

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

/// A named way to open a tab — `[profile.htop]` and friends.
///
/// Resolved from [`ProfileFile`]: the one difference is `command`, which is a
/// *string* in the file (so it reads like the command you would type) and an
/// argv here, because that is what `terra new -- cmd` and
/// `TabManager::open` take. Going through the same argv means a profile and a
/// `--` command are the same code path, quoting included, rather than two.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    /// The name in the section header, kept so a resolved profile can name
    /// itself in a menu, a palette entry or an error.
    pub name: String,
    /// Program + args. Empty means "just the default shell".
    pub command: Vec<String>,
    pub cwd: Option<String>,
    pub title: Option<String>,
}

/// Split a profile's `command` into argv the way a shell splits a command
/// line: whitespace separates words, `'…'` and `"…"` group them.
///
/// Deliberately *not* a shell: no expansion, no escapes, no operators. A
/// profile names a program to run, and anything needing a pipeline can name a
/// shell explicitly. `None` for an unterminated quote, which is the one input
/// that has no sensible reading at all.
fn split_command(line: &str) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut word = String::new();
    // Tracked separately from `word.is_empty()` so `""` stays an argument.
    let mut started = false;
    let mut quote: Option<char> = None;

    for c in line.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                started = true;
            }
            None if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            None => {
                word.push(c);
                started = true;
            }
        }
    }

    if quote.is_some() {
        return None;
    }
    if started {
        out.push(word);
    }
    Some(out)
}

/// Expand a leading `~` to `home`. Only the leading one, and only when it is
/// the whole path or is followed by a separator, so `~user` and a file
/// literally called `~` are left alone rather than silently repointed.
fn expand_tilde(path: &str, home: Option<&str>) -> String {
    let Some(home) = home.filter(|h| !h.is_empty()) else {
        return path.to_owned();
    };
    match path {
        "~" => home.to_owned(),
        _ => match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
            Some(rest) => format!("{}/{rest}", home.trim_end_matches(['/', '\\'])),
            None => path.to_owned(),
        },
    }
}

/// Look a profile up by name, or say which names there are.
///
/// The error is the whole point: a typo'd `terra new --profile htpo` must not
/// silently open a plain shell, and the fix is one `ls` away only if the reply
/// carries it.
pub fn resolve_profile<'a>(
    profiles: &'a BTreeMap<String, Profile>,
    name: &str,
) -> Result<&'a Profile, String> {
    if let Some(profile) = profiles.get(name) {
        return Ok(profile);
    }
    if profiles.is_empty() {
        return Err(format!(
            "unknown profile {name:?}; no [profile.<name>] sections are defined in your terra config"
        ));
    }
    let known: Vec<&str> = profiles.keys().map(String::as_str).collect();
    Err(format!(
        "unknown profile {name:?}; known profiles: {}",
        known.join(", ")
    ))
}

// ---------------------------------------------------------------------------
// Wire types — mirror the TOML exactly; every field optional
// ---------------------------------------------------------------------------

/// A parsed config file, or a parsed set of session overrides. `None` means
/// "not specified here", which is what lets a lower layer show through.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct ConfigFile {
    pub font: FontFile,
    pub text: TextFile,
    pub tabs: TabsFile,
    /// `[profile.<name>]`, keyed on the name in the section header. Filled by
    /// [`parse`] one profile at a time; always empty in the session layer,
    /// which has no runtime toggle that could write a profile.
    pub profiles: BTreeMap<String, ProfileFile>,
}

/// One `[profile.<name>]` table, exactly as written.
///
/// `command` is a single string rather than an array because that is how a
/// command is written everywhere else the user meets one — in a shell, in
/// `terra new -- htop -d 5`. [`split_command`] turns it into argv.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct ProfileFile {
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct FontFile {
    pub size: Option<f32>,
    pub line_height: Option<f32>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct TextFile {
    /// Free-form on the wire so an unrecognised name warns and falls back,
    /// rather than serde rejecting the whole `[text]` table over it.
    pub bidi: Option<String>,
    /// Likewise free-form.
    pub bidi_base: Option<String>,
    /// `[text.bidi_quirks]`. Keys are arbitrary command basenames, so the map
    /// is open; values are strings for the same reason as above — one typo'd
    /// entry must cost you that entry, not the table and not `[text]`.
    pub bidi_quirks: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct TabsFile {
    /// Typed as `bool` rather than free-form: unlike `bidi`, there is no
    /// vocabulary to get wrong here, and TOML's own `true`/`false` is the only
    /// spelling. A non-boolean costs the `[tabs]` section its defaults and
    /// warns, which `section` already does.
    pub icons: Option<bool>,
}

// ---------------------------------------------------------------------------
// Resolved types — what the app reads; no Options, no egui
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub font: FontConfig,
    pub text: TextConfig,
    pub tabs: TabsConfig,
    /// The named ways to open a tab, by name. A `BTreeMap` so every consumer —
    /// the chevron menu, the palette, the unknown-profile error — lists them
    /// alphabetically without sorting again.
    pub profiles: BTreeMap<String, Profile>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FontConfig {
    pub size: f32,
    pub line_height: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextConfig {
    /// Whether to reorder right-to-left text for display (UAX #9). Ask
    /// [`should_reorder`] rather than reading this directly, since under
    /// `Auto` the answer depends on the tab's foreground process.
    pub bidi: BidiMode,
    /// The paragraph direction each row is resolved against.
    pub bidi_base: BidiBase,
    /// The shipped quirks table with the user's `[text.bidi_quirks]` merged
    /// over it, key by key.
    pub quirks: BidiQuirks,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TabsConfig {
    /// Whether the tab bar draws a per-tab icon. See [`DEFAULT_TAB_ICONS`].
    pub icons: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font: FontConfig {
                size: DEFAULT_FONT_SIZE,
                line_height: DEFAULT_LINE_HEIGHT,
            },
            text: TextConfig {
                bidi: DEFAULT_BIDI,
                bidi_base: DEFAULT_BIDI_BASE,
                quirks: BidiQuirks::default(),
            },
            tabs: TabsConfig {
                icons: DEFAULT_TAB_ICONS,
            },
            profiles: BTreeMap::new(),
        }
    }
}

/// A runtime override. A closed enum rather than a generic `set(key, value)`,
/// so every toggle the UI can perform is enumerable and type-checked.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEdit {
    /// `Some` sets an override; `None` clears it back to the file/default.
    FontSize(Option<f32>),
    BidiBase(Option<BidiBase>),
}

// ---------------------------------------------------------------------------
// Path
// ---------------------------------------------------------------------------

/// Resolve the config path. Honors `TERRA_CONFIG`, else `~/.terra/config.toml`.
///
/// Deliberately mirrors [`terra_protocol::socket_path`], including its `/tmp`
/// fallback for a missing `HOME`. `~/.terra` is computed independently of the
/// socket path, because `TERRA_SOCKET` may relocate the socket without
/// relocating the config.
pub fn config_path() -> PathBuf {
    resolve_path(
        std::env::var("TERRA_CONFIG").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// The pure core of [`config_path`], so the precedence is testable without
/// mutating process-global environment.
fn resolve_path(terra_config: Option<&str>, home: Option<&str>) -> PathBuf {
    match terra_config {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from(home.unwrap_or("/tmp"))
            .join(".terra")
            .join("config.toml"),
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Deserialize one `[section]` on its own, so a type error in it cannot take
/// the rest of the file down with it.
fn section<T>(root: &toml::Table, name: &str, warnings: &mut Vec<String>) -> T
where
    T: serde::de::DeserializeOwned + Default,
{
    match root.get(name) {
        None => T::default(),
        Some(value) => value.clone().try_into().unwrap_or_else(|e| {
            warnings.push(format!("[{name}]: {e}; using defaults for it"));
            T::default()
        }),
    }
}

/// Report keys terra does not understand. Serde ignores them silently by
/// default, which is the right *behaviour* — a typo must not brick startup —
/// but silence would leave the user staring at a setting that does nothing.
fn report_unknown_keys(root: &toml::Table, warnings: &mut Vec<String>) {
    for (table, value) in root {
        // `[profile]` holds one sub-table per profile, and the profile names
        // are the user's own words — so the check is one level deeper.
        if table == PROFILE_SECTION {
            for (name, profile) in value.as_table().into_iter().flatten() {
                let Some(keys) = profile.as_table() else {
                    continue; // reported by `profiles_section`
                };
                for key in keys.keys() {
                    if !PROFILE_KEYS.contains(&key.as_str()) {
                        warnings.push(format!("unknown key `profile.{name}.{key}`"));
                    }
                }
            }
            continue;
        }
        let Some(known) = KNOWN.iter().find(|(n, _)| n == table) else {
            warnings.push(format!("unknown section [{table}]"));
            continue;
        };
        let Some(entries) = value.as_table() else {
            continue; // a type error; `section` reports it with a better message
        };
        for key in entries.keys() {
            if !known.1.contains(&key.as_str()) {
                warnings.push(format!("unknown key `{table}.{key}`"));
            }
        }
    }
}

/// Parse config text. Never fails: a syntax error discards the whole file,
/// since a failed parse yields no usable tree to salvage sections from.
pub fn parse(text: &str) -> (ConfigFile, Vec<String>) {
    let mut warnings = Vec::new();
    let root: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            warnings.push(format!("could not parse config: {e}"));
            return (ConfigFile::default(), warnings);
        }
    };
    report_unknown_keys(&root, &mut warnings);
    let file = ConfigFile {
        font: section(&root, "font", &mut warnings),
        text: section(&root, "text", &mut warnings),
        tabs: section(&root, "tabs", &mut warnings),
        profiles: profiles_section(&root, &mut warnings),
    };
    (file, warnings)
}

/// Deserialize `[profile.<name>]` one profile at a time.
///
/// [`section`] would take the whole table down over a single `command = 5`,
/// and losing every profile because one of them has a typo is exactly the
/// failure mode this module exists to avoid.
fn profiles_section(
    root: &toml::Table,
    warnings: &mut Vec<String>,
) -> BTreeMap<String, ProfileFile> {
    let mut out = BTreeMap::new();
    let Some(value) = root.get(PROFILE_SECTION) else {
        return out;
    };
    let Some(table) = value.as_table() else {
        warnings.push(
            "[profile] must hold one [profile.<name>] table per profile; ignoring it".to_owned(),
        );
        return out;
    };
    for (name, profile) in table {
        match profile.clone().try_into::<ProfileFile>() {
            Ok(parsed) => {
                out.insert(name.clone(), parsed);
            }
            Err(e) => warnings.push(format!("[profile.{name}]: {e}; skipping this profile")),
        }
    }
    out
}

/// Clamp `value` into `range`, warning under the key's name if it moved.
fn clamped(value: f32, range: (f32, f32), key: &str, warnings: &mut Vec<String>) -> f32 {
    if !value.is_finite() {
        warnings.push(format!("`{key}` is not a number; using the default"));
        return f32::NAN; // caller substitutes; see `resolve`
    }
    let out = value.clamp(range.0, range.1);
    if out != value {
        warnings.push(format!(
            "`{key}` {value} is out of range {}..={}; using {out}",
            range.0, range.1
        ));
    }
    out
}

/// Collapse the layers into the values the app reads.
///
/// `session` wins over `file` wins over the compiled default, per field —
/// not per section, so overriding the font size never resets the line height.
pub fn resolve(file: &ConfigFile, session: &ConfigFile, warnings: &mut Vec<String>) -> Config {
    let size = session
        .font
        .size
        .or(file.font.size)
        .map(|v| clamped(v, FONT_SIZE_RANGE, "font.size", warnings))
        .filter(|v| v.is_finite())
        .unwrap_or(DEFAULT_FONT_SIZE);

    let line_height = session
        .font
        .line_height
        .or(file.font.line_height)
        .map(|v| clamped(v, LINE_HEIGHT_RANGE, "font.line_height", warnings))
        .filter(|v| v.is_finite())
        .unwrap_or(DEFAULT_LINE_HEIGHT);

    let bidi_base = session
        .text
        .bidi_base
        .as_deref()
        .or(file.text.bidi_base.as_deref())
        .map(|name| {
            parse_bidi_base(name).unwrap_or_else(|| {
                warnings.push(format!(
                    "`text.bidi_base` {name:?} is not one of ltr/auto/rtl; using {}",
                    bidi_base_name(DEFAULT_BIDI_BASE)
                ));
                DEFAULT_BIDI_BASE
            })
        })
        .unwrap_or(DEFAULT_BIDI_BASE);

    let bidi = session
        .text
        .bidi
        .as_deref()
        .or(file.text.bidi.as_deref())
        .map(|name| {
            BidiMode::parse(name).unwrap_or_else(|| {
                warnings.push(format!(
                    "`text.bidi` {name:?} is not one of off/on/auto; using {}",
                    DEFAULT_BIDI.name()
                ));
                DEFAULT_BIDI
            })
        })
        .unwrap_or(DEFAULT_BIDI);

    let icons = session
        .tabs
        .icons
        .or(file.tabs.icons)
        .unwrap_or(DEFAULT_TAB_ICONS);

    Config {
        font: FontConfig { size, line_height },
        text: TextConfig {
            bidi,
            bidi_base,
            quirks: quirks(file, session, warnings),
        },
        tabs: TabsConfig { icons },
        profiles: profiles(file, warnings),
    }
}

/// Turn the parsed `[profile.<name>]` tables into resolved [`Profile`]s.
///
/// Only the file layer has profiles: the session layer exists for runtime
/// toggles, and there is no toggle that invents a profile. One unusable
/// profile is dropped with a warning, exactly as one unusable quirk is.
fn profiles(file: &ConfigFile, warnings: &mut Vec<String>) -> BTreeMap<String, Profile> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok();
    let mut out = BTreeMap::new();
    for (name, profile) in &file.profiles {
        if name.trim().is_empty() {
            warnings.push("a profile with an empty name cannot be opened; ignoring it".to_owned());
            continue;
        }
        let command = match profile.command.as_deref() {
            None => Vec::new(),
            Some(line) => match split_command(line) {
                Some(argv) => argv,
                None => {
                    warnings.push(format!(
                        "`profile.{name}.command` {line:?} has an unterminated quote; skipping this profile"
                    ));
                    continue;
                }
            },
        };
        out.insert(
            name.clone(),
            Profile {
                name: name.clone(),
                command,
                cwd: profile
                    .cwd
                    .as_deref()
                    .map(|cwd| expand_tilde(cwd, home.as_deref())),
                title: profile.title.clone(),
            },
        );
    }
    out
}

/// Merge the quirks tables: shipped defaults first, then the file, then the
/// session — per key, so naming one tool in `[text.bidi_quirks]` adjusts the
/// shipped table rather than replacing it and silently losing the rest.
fn quirks(file: &ConfigFile, session: &ConfigFile, warnings: &mut Vec<String>) -> BidiQuirks {
    let mut out = BidiQuirks::default();
    for layer in [&file.text.bidi_quirks, &session.text.bidi_quirks] {
        for (command, name) in layer.iter().flatten() {
            match BidiMode::parse(name) {
                Some(mode) => out.insert(command, mode),
                // One bad entry loses that entry only. The user named a
                // program they care about; the other programs are unrelated.
                None => warnings.push(format!(
                    "`text.bidi_quirks.{command}` {name:?} is not one of off/on/auto; ignoring it"
                )),
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// The loaded config plus its session overrides.
pub struct ConfigStore {
    path: PathBuf,
    /// Whether `path` came from `TERRA_CONFIG`. An explicit path that does
    /// not exist is a user mistake worth reporting; a missing default path is
    /// simply the normal case.
    explicit_path: bool,
    file: ConfigFile,
    session: ConfigFile,
    resolved: Config,
    generation: u64,
    warnings: Vec<String>,
}

impl ConfigStore {
    /// Read the config file and resolve it. Infallible.
    pub fn load() -> Self {
        let explicit_path = std::env::var("TERRA_CONFIG").is_ok_and(|p| !p.is_empty());
        let mut store = Self {
            path: config_path(),
            explicit_path,
            file: ConfigFile::default(),
            session: ConfigFile::default(),
            resolved: Config::default(),
            generation: 0,
            warnings: Vec::new(),
        };
        store.reload();
        store
    }

    /// A store with no file behind it, for tests and for the headless path.
    pub fn in_memory() -> Self {
        Self {
            path: PathBuf::new(),
            explicit_path: false,
            file: ConfigFile::default(),
            session: ConfigFile::default(),
            resolved: Config::default(),
            generation: 0,
            warnings: Vec::new(),
        }
    }

    pub fn get(&self) -> &Config {
        &self.resolved
    }

    /// Bumped only when the *resolved* config actually changes, so callers
    /// can cache derived objects (fonts, themes) against it.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Apply a runtime override. `None` clears it back to the file value.
    pub fn apply(&mut self, edit: SessionEdit) {
        match edit {
            SessionEdit::FontSize(v) => self.session.font.size = v,
            SessionEdit::BidiBase(v) => {
                self.session.text.bidi_base = v.map(|b| bidi_base_name(b).to_owned());
            }
        }
        self.reresolve();
    }

    /// Drop every session override, returning to what the file says.
    pub fn clear_session(&mut self) {
        self.session = ConfigFile::default();
        self.reresolve();
    }

    /// Re-read the file, keeping the session layer on top.
    pub fn reload(&mut self) {
        self.warnings.clear();
        self.file = ConfigFile::default();

        if self.path.as_os_str().is_empty() {
            self.reresolve();
            return;
        }

        match std::fs::read_to_string(&self.path) {
            Ok(text) => {
                let (file, mut warnings) = parse(&text);
                self.file = file;
                self.warnings.append(&mut warnings);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // The default path not existing is the normal case and must
                // stay silent. An explicit TERRA_CONFIG that does not exist
                // is a mistake, and falling back without a word would hide it.
                if self.explicit_path {
                    self.warnings.push(format!(
                        "TERRA_CONFIG points at {}, which does not exist",
                        self.path.display()
                    ));
                }
            }
            Err(e) => self
                .warnings
                .push(format!("could not read {}: {e}", self.path.display())),
        }

        self.reresolve();
        log::debug!(
            "terra: {} bidi quirk(s) active{}",
            self.resolved.text.quirks.len(),
            if self.resolved.text.quirks.is_empty() {
                " (none)"
            } else {
                ""
            }
        );
        for warning in &self.warnings {
            log::warn!("terra: config: {warning}");
        }
    }

    fn reresolve(&mut self) {
        let mut warnings = Vec::new();
        let resolved = resolve(&self.file, &self.session, &mut warnings);
        self.warnings.extend(warnings);
        if resolved != self.resolved {
            self.resolved = resolved;
            self.generation += 1;
        }
    }
}

impl Default for ConfigStore {
    fn default() -> Self {
        Self::in_memory()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(text: &str) -> Config {
        let (file, _) = parse(text);
        resolve(&file, &ConfigFile::default(), &mut Vec::new())
    }

    fn warnings_for(text: &str) -> Vec<String> {
        let (file, mut w) = parse(text);
        resolve(&file, &ConfigFile::default(), &mut w);
        w
    }

    /// The defaults must equal what terra hardcoded before config existed,
    /// or adopting the file silently restyles every existing user's terminal.
    /// `bidi` is the one deliberate exception; see the test below it.
    #[test]
    fn the_defaults_match_the_previously_hardcoded_values() {
        let c = Config::default();
        assert_eq!(c.font.size, 15.0);
        assert_eq!(c.font.line_height, 1.3);
        assert_eq!(c.text.bidi_base, BidiBase::Auto);
    }

    /// Reordering by default is the one thing we changed on purpose. A stock
    /// terra must now emit exactly what every other terminal emits, because
    /// reordering a stream that is already in visual order double-reverses it
    /// into garbage, and the byte stream cannot tell us which one it is.
    #[test]
    fn the_default_is_off_so_nothing_changes_for_existing_users() {
        assert_eq!(DEFAULT_BIDI, BidiMode::Off);
        assert_eq!(Config::default().text.bidi, BidiMode::Off);
        assert_eq!(resolved("").text.bidi, BidiMode::Off);
        assert!(!should_reorder(&Config::default(), None));
        assert!(
            !should_reorder(&Config::default(), Some("codex")),
            "a quirk entry does not fire unless the mode is auto"
        );
    }

    #[test]
    fn all_three_bidi_modes_parse_and_round_trip() {
        for mode in [BidiMode::Off, BidiMode::On, BidiMode::Auto] {
            assert_eq!(BidiMode::parse(mode.name()), Some(mode), "{mode:?}");
            let text = format!("[text]\nbidi = \"{}\"\n", mode.name());
            assert_eq!(resolved(&text).text.bidi, mode, "{mode:?}");
        }
    }

    #[test]
    fn an_unknown_bidi_mode_warns_and_falls_back() {
        let c = resolved("[text]\nbidi = \"sometimes\"\nbidi_base = \"rtl\"\n");
        assert_eq!(c.text.bidi, DEFAULT_BIDI);
        assert_eq!(
            c.text.bidi_base,
            BidiBase::Rtl,
            "the rest of [text] survived"
        );
        let w = warnings_for("[text]\nbidi = \"sometimes\"\n");
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("sometimes"), "{w:?}");
    }

    /// The whole point of the quirks table: the decision is keyed on the
    /// process, never on the text, because logical and visual order are the
    /// same bytes. Claude Code runs its own BiDi and emits visual order, so
    /// reordering it again is what breaks it; Codex emits logical order.
    #[test]
    fn auto_consults_the_quirks_table_for_the_foreground_command() {
        let c = resolved("[text]\nbidi = \"auto\"\n");
        assert!(!should_reorder(&c, Some("claude")));
        assert!(should_reorder(&c, Some("codex")));
        assert!(!should_reorder(&c, None), "unknown process, safe default");
        assert!(!should_reorder(&c, Some("vim")), "not in the table");
    }

    /// An explicit answer is the user's answer. If they had to state it, they
    /// know something the table does not.
    #[test]
    fn an_explicit_mode_ignores_the_quirks_table() {
        let on = resolved("[text]\nbidi = \"on\"\n");
        assert!(should_reorder(&on, Some("claude")));
        assert!(should_reorder(&on, None));

        let off = resolved("[text]\nbidi = \"off\"\n");
        assert!(!should_reorder(&off, Some("codex")));
    }

    /// The user's table is merged over the shipped one, not swapped for it —
    /// otherwise adding one tool would silently unteach terra the rest.
    #[test]
    fn user_quirks_override_the_shipped_defaults_without_replacing_them() {
        let c = resolved("[text.bidi_quirks]\nmytool = \"on\"\nclaude = \"on\"\n");
        assert_eq!(c.text.quirks.get("mytool"), Some(BidiMode::On), "added");
        assert_eq!(c.text.quirks.get("claude"), Some(BidiMode::On), "replaced");
        assert_eq!(c.text.quirks.get("codex"), Some(BidiMode::On), "untouched");
        assert_eq!(c.text.quirks.len(), DEFAULT_QUIRKS.len() + 1);
        assert!(!c.text.quirks.is_empty());

        let stock = Config::default().text.quirks;
        assert_eq!(stock.get("claude"), Some(BidiMode::Off));
        assert_eq!(stock.len(), DEFAULT_QUIRKS.len());
    }

    /// The keys of `[text.bidi_quirks]` are arbitrary command names, so an
    /// unrecognised *value* is the only thing that can be wrong there — and
    /// it must cost the user that one program, not the table.
    #[test]
    fn an_unknown_quirk_value_drops_only_that_entry_and_warns() {
        let text = "[text.bidi_quirks]\nmytool = \"sideways\"\nother = \"on\"\n";
        let c = resolved(text);
        assert_eq!(c.text.quirks.get("mytool"), None, "dropped");
        assert_eq!(c.text.quirks.get("other"), Some(BidiMode::On), "survived");
        assert_eq!(c.text.quirks.get("codex"), Some(BidiMode::On), "survived");

        let w = warnings_for(text);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("mytool"), "the warning names the key: {w:?}");
        assert!(w[0].contains("sideways"), "{w:?}");
    }

    /// The basename we are handed is whatever the user typed to launch the
    /// program, and `Codex` and `codex` are the same program.
    #[test]
    fn a_quirk_lookup_is_case_insensitive_on_the_command_name() {
        let c = resolved("[text]\nbidi = \"auto\"\n\n[text.bidi_quirks]\nMyTool = \"on\"\n");
        assert_eq!(c.text.quirks.get("mytool"), Some(BidiMode::On));
        assert_eq!(c.text.quirks.get("MYTOOL"), Some(BidiMode::On));
        assert_eq!(c.text.quirks.get("Codex"), Some(BidiMode::On));
        assert!(should_reorder(&c, Some("CODEX")));
        assert!(!should_reorder(&c, Some("Claude")));
    }

    #[test]
    fn an_empty_config_yields_defaults_without_complaint() {
        assert_eq!(resolved(""), Config::default());
        assert!(warnings_for("").is_empty());
    }

    /// A syntax error leaves no tree to salvage, so the whole file goes —
    /// but terra still starts.
    #[test]
    fn a_syntax_error_yields_defaults_and_one_warning() {
        let text = "[font\nsize = 20";
        assert_eq!(resolved(text), Config::default());
        let w = warnings_for(text);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("could not parse"), "{w:?}");
    }

    /// The point of per-section deserialization: one bad table must not cost
    /// you the settings in every other table.
    #[test]
    fn a_bad_value_in_one_section_leaves_the_other_sections_intact() {
        let c = resolved("[font]\nsize = \"big\"\n\n[text]\nbidi = \"on\"\n");
        assert_eq!(c.font.size, DEFAULT_FONT_SIZE, "font fell back");
        assert_eq!(c.text.bidi, BidiMode::On, "but [text] survived");
        assert!(warnings_for("[font]\nsize = \"big\"\n")[0].contains("[font]"));
    }

    /// Fallback is per *field*, not per section.
    #[test]
    fn a_partial_table_falls_back_field_by_field() {
        let c = resolved("[font]\nsize = 20.0\n");
        assert_eq!(c.font.size, 20.0);
        assert_eq!(c.font.line_height, DEFAULT_LINE_HEIGHT);
    }

    #[test]
    fn unknown_keys_are_ignored_but_reported() {
        let c = resolved("[font]\nszie = 20.0\nsize = 18.0\n");
        assert_eq!(c.font.size, 18.0, "the valid key still applies");
        let w = warnings_for("[font]\nszie = 20.0\n");
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("font.szie"), "{w:?}");
    }

    #[test]
    fn an_unknown_section_is_reported() {
        let w = warnings_for("[colours]\nbg = \"black\"\n");
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("[colours]"), "{w:?}");
    }

    #[test]
    fn an_out_of_range_font_size_is_clamped_and_warned() {
        let c = resolved("[font]\nsize = 900.0\n");
        assert_eq!(c.font.size, 72.0);
        assert!(warnings_for("[font]\nsize = 900.0\n")[0].contains("72"));

        assert_eq!(resolved("[font]\nsize = 1.0\n").font.size, 6.0);
        assert_eq!(
            resolved("[font]\nline_height = 0.1\n").font.line_height,
            0.5
        );
    }

    /// TOML has no NaN literal, but a session override is set from code and
    /// arithmetic can produce one. It must not reach the font machinery.
    #[test]
    fn a_non_finite_value_falls_back_to_the_default() {
        let mut session = ConfigFile::default();
        session.font.size = Some(f32::NAN);
        let c = resolve(&ConfigFile::default(), &session, &mut Vec::new());
        assert_eq!(c.font.size, DEFAULT_FONT_SIZE);
    }

    #[test]
    fn the_bidi_base_defaults_to_auto_and_parses_all_three_names() {
        assert_eq!(resolved("").text.bidi_base, BidiBase::Auto);
        for (name, want) in [
            ("ltr", BidiBase::Ltr),
            ("auto", BidiBase::Auto),
            ("rtl", BidiBase::Rtl),
        ] {
            let text = format!("[text]\nbidi_base = \"{name}\"\n");
            assert_eq!(resolved(&text).text.bidi_base, want, "{name}");
        }
    }

    /// An unrecognised direction must warn and fall back rather than take the
    /// whole `[text]` table down with it — `bidi` is set in the same table
    /// here, and it has to survive.
    #[test]
    fn an_unknown_bidi_base_falls_back_and_warns() {
        let c = resolved("[text]\nbidi_base = \"sideways\"\nbidi = \"on\"\n");
        assert_eq!(c.text.bidi_base, DEFAULT_BIDI_BASE);
        assert_eq!(c.text.bidi, BidiMode::On, "the rest of [text] survived");
        let w = warnings_for("[text]\nbidi_base = \"sideways\"\n");
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("sideways"), "{w:?}");
    }

    /// The session layer stores the direction in the same all-`Option` wire
    /// shape as the file, so the name it writes has to be one `parse_bidi_base`
    /// accepts — otherwise a runtime cycle would resolve back to the default.
    #[test]
    fn a_session_bidi_base_round_trips_through_its_own_spelling() {
        for base in [BidiBase::Ltr, BidiBase::Auto, BidiBase::Rtl] {
            assert_eq!(parse_bidi_base(bidi_base_name(base)), Some(base));
            let mut store = ConfigStore::in_memory();
            store.apply(SessionEdit::BidiBase(Some(base)));
            assert_eq!(store.get().text.bidi_base, base);
        }
    }

    #[test]
    fn a_session_override_beats_the_file_value() {
        let (file, _) = parse("[text]\nbidi = \"on\"\n");
        let mut session = ConfigFile::default();
        session.text.bidi = Some("off".to_owned());
        assert_eq!(
            resolve(&file, &session, &mut Vec::new()).text.bidi,
            BidiMode::Off
        );
    }

    #[test]
    fn clearing_a_session_override_restores_the_file_value() {
        let mut store = ConfigStore::in_memory();
        store.file = parse("[font]\nsize = 20.0\n").0;
        store.reresolve();
        assert_eq!(store.get().font.size, 20.0);

        store.apply(SessionEdit::FontSize(Some(11.0)));
        assert_eq!(store.get().font.size, 11.0);

        store.apply(SessionEdit::FontSize(None));
        assert_eq!(store.get().font.size, 20.0, "the file value came back");
    }

    /// A reload must not silently undo what the user just toggled.
    #[test]
    fn reloading_keeps_session_overrides_on_top() {
        let mut store = ConfigStore::in_memory();
        store.apply(SessionEdit::BidiBase(Some(BidiBase::Rtl)));
        assert_eq!(store.get().text.bidi_base, BidiBase::Rtl);

        store.reload();
        assert_eq!(
            store.get().text.bidi_base,
            BidiBase::Rtl,
            "the session layer survived"
        );

        store.clear_session();
        assert_eq!(store.get().text.bidi_base, DEFAULT_BIDI_BASE);
    }

    /// Callers cache fonts and themes against the generation, so it must
    /// track real change, not merely "something was called".
    #[test]
    fn the_generation_advances_only_when_the_resolved_config_changes() {
        let mut store = ConfigStore::in_memory();
        let start = store.generation();

        store.apply(SessionEdit::BidiBase(Some(DEFAULT_BIDI_BASE)));
        assert_eq!(store.generation(), start, "a no-op edit changed nothing");

        store.apply(SessionEdit::BidiBase(Some(BidiBase::Rtl)));
        assert_eq!(store.generation(), start + 1);

        store.reload();
        assert_eq!(store.generation(), start + 1, "a no-op reload is free");
    }

    #[test]
    fn terra_config_wins_over_the_home_path() {
        assert_eq!(
            resolve_path(Some("/etc/terra.toml"), Some("/home/x")),
            PathBuf::from("/etc/terra.toml")
        );
        assert_eq!(
            resolve_path(None, Some("/home/x")),
            PathBuf::from("/home/x/.terra/config.toml")
        );
        // An empty TERRA_CONFIG is treated as unset, not as the empty path.
        assert_eq!(
            resolve_path(Some(""), Some("/home/x")),
            PathBuf::from("/home/x/.terra/config.toml")
        );
        // No HOME still yields a path rather than panicking.
        assert_eq!(
            resolve_path(None, None),
            PathBuf::from("/tmp/.terra/config.toml")
        );
    }

    // --- profiles ---------------------------------------------------------

    #[test]
    fn a_profile_carries_its_command_cwd_and_title() {
        let c = resolved(
            "[profile.htop]\ncommand = \"htop -d 5\"\ncwd = \"/tmp\"\ntitle = \"system\"\n",
        );
        let p = c.profiles.get("htop").expect("htop is defined");
        assert_eq!(p.name, "htop");
        assert_eq!(p.command, vec!["htop", "-d", "5"]);
        assert_eq!(p.cwd.as_deref(), Some("/tmp"));
        assert_eq!(p.title.as_deref(), Some("system"));
        assert!(warnings_for("[profile.htop]\ncommand = \"htop\"\n").is_empty());
    }

    /// Every key is optional, including `command` — a profile that only names
    /// a directory is a perfectly good "open a shell over there".
    #[test]
    fn a_profile_may_name_only_a_directory_or_a_title() {
        let c = resolved("[profile.docs]\ncwd = \"/tmp/docs\"\n");
        let p = c.profiles.get("docs").unwrap();
        assert!(p.command.is_empty(), "no command means the default shell");
        assert_eq!(p.cwd.as_deref(), Some("/tmp/docs"));
        assert!(p.title.is_none());
    }

    #[test]
    fn an_empty_config_has_no_profiles() {
        assert!(resolved("").profiles.is_empty());
        assert!(resolved("[font]\nsize = 12.0\n").profiles.is_empty());
    }

    /// The reason profiles are deserialized one at a time: a typo in one must
    /// not take away the others.
    #[test]
    fn a_malformed_profile_is_skipped_and_the_rest_survive() {
        let text = "[profile.bad]\ncommand = 5\n\n[profile.good]\ncommand = \"htop\"\n";
        let c = resolved(text);
        assert!(!c.profiles.contains_key("bad"), "dropped");
        assert_eq!(c.profiles.get("good").unwrap().command, vec!["htop"]);

        let w = warnings_for(text);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("profile.bad"), "the warning names it: {w:?}");
    }

    /// The other way a profile can be unusable: a command line that does not
    /// close its quote has no reading at all.
    #[test]
    fn an_unterminated_quote_in_a_command_skips_only_that_profile() {
        let text = "[profile.bad]\ncommand = \"echo 'hi\"\n\n[profile.ok]\ncommand = \"ls\"\n";
        let c = resolved(text);
        assert!(!c.profiles.contains_key("bad"));
        assert!(c.profiles.contains_key("ok"));
        let w = warnings_for(text);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("unterminated"), "{w:?}");
    }

    #[test]
    fn an_unknown_key_in_a_profile_is_reported_but_the_profile_still_works() {
        let text = "[profile.p]\ncommand = \"ls\"\nshel = \"fish\"\n";
        assert_eq!(
            resolved(text).profiles.get("p").unwrap().command,
            vec!["ls"]
        );
        let w = warnings_for(text);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("profile.p.shel"), "{w:?}");
    }

    /// `[profile]` given as anything but a table of tables.
    #[test]
    fn a_profile_section_of_the_wrong_shape_is_reported_not_fatal() {
        let text = "profile = 3\n\n[font]\nsize = 20.0\n";
        let c = resolved(text);
        assert!(c.profiles.is_empty());
        assert_eq!(c.font.size, 20.0, "the rest of the file survived");
        assert_eq!(warnings_for(text).len(), 1);
    }

    /// A command string is split the way a shell splits one, so a profile and
    /// `terra new -- …` reach `TabManager::open` in exactly the same shape.
    #[test]
    fn a_command_string_splits_into_argv_honouring_quotes() {
        assert_eq!(split_command("htop"), Some(vec!["htop".to_owned()]));
        assert_eq!(
            split_command("  ls   -la  "),
            Some(vec!["ls".to_owned(), "-la".to_owned()])
        );
        assert_eq!(
            split_command("git commit -m 'two words'"),
            Some(vec![
                "git".to_owned(),
                "commit".to_owned(),
                "-m".to_owned(),
                "two words".to_owned()
            ])
        );
        assert_eq!(
            split_command(r#"say "it's fine""#),
            Some(vec!["say".to_owned(), "it's fine".to_owned()])
        );
        // An empty quoted word is still a word.
        assert_eq!(
            split_command("echo ''"),
            Some(vec!["echo".to_owned(), String::new()])
        );
        assert_eq!(split_command(""), Some(Vec::new()));
        assert_eq!(split_command("   "), Some(Vec::new()));
        // The one input with no sensible reading.
        assert_eq!(split_command("echo 'hi"), None);
        assert_eq!(split_command(r#"echo "hi"#), None);
    }

    #[test]
    fn a_leading_tilde_in_a_profile_cwd_expands_to_home() {
        assert_eq!(expand_tilde("~/src", Some("/home/ada")), "/home/ada/src");
        assert_eq!(expand_tilde("~", Some("/home/ada")), "/home/ada");
        // Not a home reference: left exactly as written.
        assert_eq!(expand_tilde("~ada/src", Some("/home/ada")), "~ada/src");
        assert_eq!(expand_tilde("/tmp/~", Some("/home/ada")), "/tmp/~");
        // No HOME to expand against.
        assert_eq!(expand_tilde("~/src", None), "~/src");
        assert_eq!(expand_tilde("~/src", Some("")), "~/src");
    }

    /// An unknown name must never quietly open a plain shell, and the reply
    /// has to carry the fix.
    #[test]
    fn resolving_an_unknown_profile_names_the_known_ones() {
        let profiles =
            resolved("[profile.htop]\n\n[profile.build]\ncommand = \"cargo build\"\n").profiles;
        assert_eq!(resolve_profile(&profiles, "htop").unwrap().name, "htop");

        let err = resolve_profile(&profiles, "htpo").unwrap_err();
        assert!(err.contains("htpo"), "{err}");
        // Alphabetical, because the map is a BTreeMap.
        assert!(err.contains("build, htop"), "{err}");

        let err = resolve_profile(&BTreeMap::new(), "htop").unwrap_err();
        assert!(err.contains("no [profile."), "{err}");
    }

    /// Tab icons are on unless the file says otherwise — a file that says
    /// nothing about them must not disable them.
    #[test]
    fn tab_icons_default_on_and_are_switchable_off() {
        assert!(Config::default().tabs.icons);
        assert!(resolved("").tabs.icons);
        assert!(resolved("[font]\nsize = 12.0\n").tabs.icons);
        assert!(!resolved("[tabs]\nicons = false\n").tabs.icons);
        assert!(resolved("[tabs]\nicons = true\n").tabs.icons);
    }

    /// The fault-isolation contract, applied to the new section: a `[tabs]`
    /// that will not deserialize costs you tab icons, not your font.
    #[test]
    fn a_non_boolean_icons_value_warns_and_keeps_the_default() {
        let text = "[font]\nsize = 12.0\n\n[tabs]\nicons = \"yes\"\n";
        assert!(resolved(text).tabs.icons);
        assert_eq!(resolved(text).font.size, 12.0);
        let warnings = warnings_for(text);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("[tabs]"), "{warnings:?}");
    }

    #[test]
    fn an_unknown_key_under_tabs_is_reported() {
        let warnings = warnings_for("[tabs]\nicon = true\n");
        assert_eq!(warnings, vec!["unknown key `tabs.icon`".to_string()]);
    }

    /// Every key named in the shipped example must be one terra understands,
    /// or the docs teach a setting that does nothing.
    #[test]
    fn the_documented_example_config_parses_without_warnings() {
        let example = include_str!("../../../docs/config.example.toml");
        let (_, warnings) = parse(example);
        assert!(warnings.is_empty(), "{warnings:?}");
    }
}
