//! Font setup: JetBrains Mono (Ghostty's default) as the monospace face.
//!
//! # Why there is no Apple Color Emoji here
//!
//! epaint 0.35 rasterizes glyphs by pulling *outlines* out of `skrifa`
//! (`OutlineGlyphCollection::get` + `outline.draw(..)`) and filling them with
//! `vello_cpu` in solid **white**; the resulting coverage mask is then tinted
//! with the text colour by the tessellator. There is no call to
//! `skrifa`'s `color_glyphs()` / `bitmap_strikes()` anywhere in the crate, so
//! COLR, CBDT and sbix colour tables are simply never read.
//!
//! `/System/Library/Fonts/Apple Color Emoji.ttc` is an sbix font: its `glyf`
//! entries are empty stubs and all the artwork lives in the `sbix` bitmap
//! table. Loading it would therefore *hide* the working monochrome emoji
//! (its `cmap` claims the codepoints) and draw nothing at all, so we
//! deliberately don't.
//!
//! What we do instead: start from [`egui::FontDefinitions::default()`], which
//! already ships `NotoEmoji-Regular` plus `emoji-icon-font` as fallbacks in
//! both the `Monospace` and `Proportional` families, and only *prepend* our
//! own faces. Emoji stay monochrome but they do render.
//!
//! # Why we bundle a second Noto Emoji anyway
//!
//! epaint's copy of `NotoEmoji-Regular.ttf` is a *subset*: 887 codepoints,
//! frozen at the noto-emoji v2.034 release. It has 😀 U+1F600 but not 🙂
//! U+1F642, so a slightly-smiling face came out as tofu — and no macOS system
//! font can rescue it, because the only local face with U+1F642 is Apple Color
//! Emoji, which is off-limits for the sbix reason above.
//!
//! So we embed the current upstream monochrome Noto Emoji (1489 codepoints)
//! and put it in front of epaint's subset. It is outline-only — `glyf`/`gvar`,
//! no `sbix`/`CBDT`/`COLR` — which is exactly what epaint can rasterize.

/// Family name for the bold face, for callers that want it explicitly.
pub const BOLD_FAMILY: &str = "JetBrains Mono Bold";

/// Family for chrome text (tab titles, hints): the system UI face at its
/// regular weight, falling back to egui's proportional font.
pub const UI_FAMILY: &str = "terra-ui";
/// Same face at medium weight — macOS titles the active tab in SF Pro Medium.
pub const UI_MEDIUM_FAMILY: &str = "terra-ui-medium";

const JETBRAINS_MONO_REGULAR: &[u8] = include_bytes!("../assets/JetBrainsMono-Regular.ttf");
const JETBRAINS_MONO_BOLD: &[u8] = include_bytes!("../assets/JetBrainsMono-Bold.ttf");

/// The full monochrome Noto Emoji, embedded (not read from disk) so it also
/// works from inside a packaged `.app`, exactly like JetBrains Mono.
///
/// Upstream ships this as a single variable file with one `wght` axis rather
/// than as static weights — hence Google Fonts' file name, kept verbatim.
/// Licence text lives next to it in `assets/NotoEmoji-LICENSE.txt`.
const NOTO_EMOJI: &[u8] = include_bytes!("../assets/NotoEmoji-VariableFont_wght.ttf");

/// Key for our emoji face in `FontDefinitions::font_data`.
const EMOJI_FACE: &str = "Noto Emoji";

/// Key epaint registers *its* (subset) Noto Emoji under. We insert ourselves
/// directly in front of this entry — see [`install_emoji`].
const EPAINT_EMOJI_FACE: &str = "NotoEmoji-Regular";

/// The `wght` value we pin the variable emoji face to. This is already the
/// axis default, so it changes nothing today; it is stated so a future upstream
/// re-release that moves the default cannot silently re-weight every emoji.
const EMOJI_WGHT_REGULAR: f32 = 400.0;

/// Shrink factor for the emoji face — the same value epaint applies to its own
/// Noto Emoji (`FontTweak { scale: 0.81, .. }`), and for the same reason.
///
/// Noto Emoji is drawn *outside* the em square: glyphs advance 2600/2048 =
/// 1.27 em and the `hhea` ascender is 1900/2048 = 0.93 em, against JetBrains
/// Mono's 1.02 em. Left at 1.0 the emoji face would still fit the row box, but
/// each glyph would be 1.27 em wide where the grid reserves two 0.6 em cells
/// (1.2 em) for a wide char, so emoji would bleed into their neighbours.
///
/// At 0.81 a wide char's glyph advances 1.03 em inside its 1.2 em slot, and its
/// ink runs from -0.13 em to +0.69 em about the baseline — centred on 0.28 em,
/// which is within a hair of JetBrains Mono's x-height centre (550/2 = 0.275 em).
/// That is why no `y_offset_factor` is set: the emoji already sit optically on
/// the same line as lowercase text. (Measured from the two fonts' `glyf`
/// bounding boxes; epaint scales outlines by `font_size * scale / units_per_em`,
/// so these ratios are the whole story.)
///
/// Matching epaint's 0.81 also keeps the two emoji files the same size on
/// screen, which matters for the three codepoints epaint's subset has and ours
/// does not (U+00A0, U+25CA, U+FEFF — none of them actually emoji).
const EMOJI_SCALE: f32 = 0.81;

/// Directories searched for every system face, best first.
///
/// # Why directories and file names rather than whole paths
///
/// The same file lives in a different directory on every distribution — and on
/// Windows in a different directory depending on whether it was installed for
/// the machine or for the user — while its *name* is stable everywhere. Listing
/// the two axes separately and taking their cross product means a face is found
/// wherever it happens to be, without the table repeating each file name once
/// per layout. A directory that does not exist simply produces paths that do not
/// open, which is the pre-existing silent-skip path.
///
/// Entries may contain `${VAR}`, expanded from the environment by [`expand`];
/// an unset variable drops that directory rather than probing a literal
/// `${VAR}` path. This is how `%SystemRoot%` is honoured instead of assuming
/// the C: drive, and how the per-user font directories are reached at all.
///
/// The cost is one `open` per (directory, file name) pair per missing face —
/// on the order of a hundred failed syscalls once, at startup.
#[cfg(target_os = "macos")]
const FONT_DIRS: &[&str] = &[
    "/System/Library/Fonts",
    "/System/Library/Fonts/Supplemental",
    // Machine- and user-installed faces, searched last so a system file always
    // wins and the measured macOS table keeps resolving to exactly the files
    // its tests read.
    "/Library/Fonts",
    "${HOME}/Library/Fonts",
];

#[cfg(windows)]
const FONT_DIRS: &[&str] = &[
    // `%SystemRoot%\Fonts` is the documented system font directory
    // (`FOLDERID_Fonts`, default path `%windir%\Fonts` — see
    // learn.microsoft.com/windows/win32/shell/knownfolderid). Windows sets both
    // spellings of the variable and its environment block is case-insensitive,
    // so the second entry only matters on an environment missing the first.
    r"${SystemRoot}\Fonts",
    r"${WINDIR}\Fonts",
    // Per-user fonts. Since Windows 10 1809 the Explorer "Install" verb (as
    // opposed to "Install for all users") drops the file here and registers it
    // under `HKCU\Software\Microsoft\Windows NT\CurrentVersion\Fonts` — which
    // is where a user-installed Hebrew or Nerd font lands on a machine where
    // the user is not an administrator. Microsoft documents this only in
    // support answers, not in the Win32 reference, and there is no
    // `KNOWNFOLDERID` for it; the path is nonetheless what every Windows 10/11
    // machine uses.
    r"${LOCALAPPDATA}\Microsoft\Windows\Fonts",
];

#[cfg(all(unix, not(target_os = "macos")))]
const FONT_DIRS: &[&str] = &[
    // Fontconfig would be the right answer here; a directory list is the honest
    // approximation until this crate takes that dependency. See the note on
    // [`SCRIPT_FALLBACKS`] for what fontconfig would actually cost.
    //
    // Debian/Ubuntu file the packages under `truetype/<family>` (and unifont
    // under `opentype/`), Arch under `TTF`/`OTF` or a bare family directory,
    // Fedora under a directory named after the *package*. Verified against the
    // published file lists of `fonts-dejavu-core`, `fonts-noto-core`,
    // `fonts-liberation`, `fonts-unifont` (packages.debian.org), `ttf-dejavu`,
    // `noto-fonts`, `ttf-liberation` (archlinux.org) and the corresponding
    // Fedora RPMs.
    "/usr/share/fonts/truetype/dejavu",
    "/usr/share/fonts/truetype/noto",
    "/usr/share/fonts/truetype/liberation",
    "/usr/share/fonts/truetype/unifont",
    "/usr/share/fonts/opentype/unifont",
    "/usr/share/fonts/TTF",
    "/usr/share/fonts/OTF",
    "/usr/share/fonts/dejavu",
    "/usr/share/fonts/dejavu-sans-fonts",
    "/usr/share/fonts/dejavu-sans-mono-fonts",
    "/usr/share/fonts/noto",
    "/usr/share/fonts/google-noto",
    "/usr/share/fonts/liberation",
    "/usr/share/fonts/liberation-sans-fonts",
    "/usr/share/fonts/liberation-mono-fonts",
    "/usr/share/fonts/unifont",
    "/usr/share/fonts/cantarell",
    "/usr/share/fonts/opentype/cantarell",
    "/usr/share/fonts",
    "/usr/local/share/fonts",
    // Per-user fonts, from the XDG base directory spec and the two legacy
    // locations fontconfig still honours. Only the top level is searched: a
    // face filed under `~/.local/share/fonts/NerdFonts/` is not found, because
    // walking user directories at startup is a different (and unbounded) cost.
    "${XDG_DATA_HOME}/fonts",
    "${HOME}/.local/share/fonts",
    "${HOME}/.fonts",
];

#[cfg(not(any(unix, windows)))]
const FONT_DIRS: &[&str] = &[];

/// macOS ships the system UI face as a single *variable* TrueType file (not a
/// `.ttc` collection), so it needs no face index — just variation coordinates.
#[cfg(target_os = "macos")]
const SF_PRO_FILE: &str = "SFNS.ttf";

/// Candidate file names for the system UI face, best first; the first that is
/// found in some [`FONT_DIRS`] entry and parses wins. See [`install_ui_family`].
///
/// One entry on macOS (the name is fixed and the face is variable), several
/// elsewhere: Windows 11 ships a variable Segoe UI but Windows 10 only the
/// static one, and on Linux there is no single system UI font at all.
#[cfg(target_os = "macos")]
const UI_FONT_FILES: &[&str] = &[SF_PRO_FILE];

#[cfg(windows)]
const UI_FONT_FILES: &[&str] = &[
    // Windows 11's variable Segoe UI — the only one of these with a `wght`
    // axis, and therefore the only one that yields a real medium weight.
    // Documented as new in Windows 11 (Segoe UI Variable, `SegUIVar.ttf`, in
    // learn.microsoft.com/typography/fonts/windows_11_font_list), so on
    // Windows 10 the static Segoe UI below is what is found.
    "SegUIVar.ttf",
    "segoeui.ttf",
    "arial.ttf",
];

#[cfg(all(unix, not(target_os = "macos")))]
const UI_FONT_FILES: &[&str] = &[
    "DejaVuSans.ttf",
    "LiberationSans-Regular.ttf",
    "NotoSans-Regular.ttf",
    "Cantarell-VF.otf",
];

#[cfg(not(any(unix, windows)))]
const UI_FONT_FILES: &[&str] = &[];

/// Name the UI face is registered under. Cosmetic — it is a key in
/// `FontDefinitions::font_data` — but it shows up in egui debug output, so it
/// should not claim to be SF Pro on a machine that has never seen it.
#[cfg(target_os = "macos")]
const UI_FACE: &str = "SF Pro";
#[cfg(target_os = "macos")]
const UI_FACE_MEDIUM: &str = "SF Pro Medium";
#[cfg(not(target_os = "macos"))]
const UI_FACE: &str = "System UI";
#[cfg(not(target_os = "macos"))]
const UI_FACE_MEDIUM: &str = "System UI Medium";

/// A system face that covers a script none of our own fonts do.
struct ScriptFallback {
    /// Key under which the face is registered in `FontDefinitions::font_data`.
    name: &'static str,
    /// File names this face may go by, best first; [`read_fallback`] looks for
    /// each of them in every [`FONT_DIRS`] entry and takes the first that
    /// exists and parses. Usually one name — the second is for faces whose
    /// container differs by distribution (`unifont.otf` vs `unifont.ttf`) or
    /// whose family ships under two names (Cascadia Mono / Cascadia Code).
    files: &'static [&'static str],
    /// Face to use inside a `.ttc` collection; `0` for a plain `.ttf`.
    index: u32,
}

/// Faces appended *behind* every family, so JetBrains Mono and SF Pro still
/// win for Latin and these are only consulted for codepoints nothing ahead of
/// them has.
///
/// JetBrains Mono stops at Latin/Greek/Cyrillic and egui's built-ins add
/// little beyond emoji, so any other script arrives as tofu unless it has an
/// entry here. Note that Menlo — the obvious guess for a Mac monospace
/// fallback — has no Hebrew either; Arial Hebrew is what the system's own
/// cascade resolves to.
///
/// The grid draws one glyph per cell, centred (`egui_term::view`), so a
/// proportional fallback still lands in the right place horizontally; only the
/// glyph's own width differs from a monospace face. Vertically it does *not*
/// land in the right place on its own — see [`baseline_offset_factor`].
///
/// The last three entries are not about *scripts* at all: TUIs paint dingbats,
/// geometric shapes and technical symbols that lie outside any monospace font's
/// repertoire — Claude Code alone uses `✳ ✻ ✽ ◒ ⚒ ☒` (U+2733, 273B, 273D, 25D2,
/// 2692, 2612), `⎿` (U+23BF) and `⏺` (U+23FA). JetBrains Mono has none of them
/// and egui's built-ins add nothing there, so without these they are tofu.
/// Menlo carries the dingbats — and, being a monospace face, draws them at the
/// cell width. Apple Symbols is the system's own technical-symbol face and is
/// where `⎿` comes from. `⏺` is the awkward one: U+23FA exists in exactly two
/// faces on this machine — `Apple Color Emoji.ttc`, which is off-limits for the
/// sbix reason documented at the top of this file (loading it would claim every
/// emoji codepoint and then draw nothing), and STIX Two Math. So STIX is the
/// only way to reach `⏺` at all.
///
/// `⏺` U+23FA is a special case worth stating once: the bundled Noto Emoji has
/// it (as does epaint's `emoji-icon-font`), so on *every* platform it is served
/// from a face terra ships rather than from anything below. The macOS STIX
/// entry predates that and is kept because it is measured and harmless.
///
/// Every platform has its own table below; the machinery around them
/// ([`read_fallback`], [`baseline_offset_factor`], [`install_script_fallbacks`])
/// is shared. A missing file is still skipped rather than being an error, but
/// it is no longer silent: [`warn_about_uncovered_scripts`] logs whatever ends
/// up with no covering face at all.
#[cfg(target_os = "macos")]
const SCRIPT_FALLBACKS: &[ScriptFallback] = &[
    // Arial Hebrew is kept at scale 1.0 deliberately. Measured against the
    // cell, which is JetBrains Mono's `m` advance, 600/1000 = 0.600 em:
    // 25 of the 27 Hebrew letters advance 0.247 em (vav, yod, final nun) to
    // 0.602 em (he) and so already sit inside the cell; only shin (0.694 em)
    // and tav (0.643 em) overflow, by 0.047 and 0.022 em per side once the
    // glyph is centred. Shrinking to fit those two would need
    // scale = 0.600/0.694 = 0.865, which drags Arial Hebrew's x-height from
    // 520/1000 down to 450/1000 against JetBrains Mono's 550 — 18% short, so
    // every Hebrew letter would read as undersized to spare two of them a
    // hairline overlap. Not worth it; see `hebrew_is_left_unscaled_because_
    // shrinking_it_to_fit_costs_more_than_it_saves`.
    //
    // Nor is the face itself the problem. SF Hebrew (the system UI Hebrew)
    // covers the same 87 codepoints but is wider still (shin 0.745 em);
    // New Peninim MT and Raanana do fit at 1.0 but are condensed and drop the
    // cantillation marks (U+0591..U+05AF) and half the presentation forms
    // (U+FB1D..U+FB4F) that Arial Hebrew carries; Courier New is the one
    // genuinely monospaced Hebrew here — every letter exactly 0.600 em — but
    // it is a thin typewriter serif against a sans grid and its letters are
    // 13% shorter again. Arial Hebrew keeps the widest repertoire (it is also
    // the only one of them with ₪ U+20AA, which JetBrains Mono lacks) and is
    // what the system's own cascade picks.
    ScriptFallback {
        name: "Arial Hebrew",
        files: &["ArialHB.ttc"],
        index: 0,
    },
    // Face 0 of the collection is Menlo-Regular (0/1/2/3 are Regular, Bold,
    // Italic, Bold Italic); the italics don't even carry the dingbats.
    ScriptFallback {
        name: "Menlo",
        files: &["Menlo.ttc"],
        index: 0,
    },
    ScriptFallback {
        name: "Apple Symbols",
        files: &["Apple Symbols.ttf"],
        index: 0,
    },
    // Note the file name: macOS ships this as `STIXTwoMath.otf`, without the
    // `-Regular` suffix the family's other members use.
    ScriptFallback {
        name: "STIX Two Math",
        files: &["STIXTwoMath.otf"],
        index: 0,
    },
];

/// Linux fallbacks. Same job as the macOS table, chosen from what distributions
/// actually ship rather than from one fixed system font set.
///
/// There is no `/System/Library/Fonts` here and no guarantee any given face is
/// installed — a plain `ubuntu:24.04` image ships **no font package at all**
/// (its OCI manifest lists none), and the GitHub Actions Ubuntu runners have
/// only `fonts-noto-color-emoji`. So every entry here may legitimately be
/// absent; that is the pre-existing silent-skip path, now reported by
/// [`warn_about_uncovered_scripts`] instead of leaving the user with
/// unexplained boxes.
///
/// Ordering mirrors the macOS one: the script face, then the monospace face
/// (dingbats at cell width), then the broad symbol faces, then the last resort.
///
/// * **Noto Sans Hebrew** is the Hebrew equivalent of Arial Hebrew and what
///   fontconfig's own cascade picks on a modern desktop. 88 codepoints in
///   U+0590..U+05FF (measured against the release TTF from
///   `notofonts.github.io`). Debian/Ubuntu `fonts-noto-core`, Fedora
///   `google-noto-sans-hebrew-fonts`, Arch `noto-fonts`.
/// * **DejaVu Sans Mono** is the Menlo of this table: monospaced, so it draws
///   the dingbats at the cell width, and it carries every symbol terra needs
///   except U+23BF and U+23FA — but it has **no Hebrew whatsoever** (0
///   codepoints in the block, measured against the 2.37 release), which is why
///   it cannot lead. On Debian sid and Ubuntu 24.04 it moved out of
///   `fonts-dejavu-core` into its own `fonts-dejavu-mono` package; Fedora
///   `dejavu-sans-mono-fonts`, Arch `ttf-dejavu`.
/// * **DejaVu Sans** is the closest thing Linux has to a universal fallback:
///   54 Hebrew codepoints, so it doubles as the Hebrew backstop on a machine
///   with no Noto, plus `✳ ✻ ✽ ◒ ⚒ ☒ ❯`. It has neither U+23BF nor U+23FA
///   (upstream `unicover.txt` puts its Miscellaneous Technical coverage at
///   25%, and the release TTF confirms both are missing).
/// * **Noto Sans Symbols** — the *first* Symbols font, not Symbols 2 — is the
///   only Noto face with `⎿` U+23BF. Symbols 2 does not have it. Both live in
///   `fonts-noto-core` on Debian/Ubuntu and `noto-fonts` on Arch; Fedora
///   splits them into `google-noto-sans-symbols-fonts` and
///   `google-noto-sans-symbols-2-fonts`.
/// * **Noto Sans Symbols 2** carries `⏺` U+23FA (which Symbols 1 lacks) and
///   most of the dingbats — though not `⚒` U+2692.
/// * **Unifont** is the last resort: a bitmap-derived outline face with
///   near-complete BMP coverage — it alone has both U+23BF and U+23FA — ugly
///   but never tofu. Last in the list, so it only ever serves codepoints
///   nothing else has. Debian/Ubuntu `fonts-unifont` ships it as
///   `unifont.otf` under `opentype/`, **not** as a `.ttf`; Fedora
///   `unifont-fonts` likewise `.otf`. It is not in Arch's official repos at
///   all (AUR only), hence both file names are probed.
///
/// Coverage claims above are measured against upstream release archives, not
/// against a running distribution: the *files* are known good, whether a given
/// machine has them is what [`read_fallback`] finds out at startup.
///
/// # Why not fontconfig
///
/// Fontconfig is the correct answer to "which installed face covers U+05D0" and
/// this list is an approximation of it. Taking it would mean either
/// `yeslogic-fontconfig-sys` (a C `libfontconfig` dependency: `pkg-config` and
/// headers at build time, or its `dlopen` feature to defer that to runtime) or
/// `font-kit`, which additionally pulls FreeType in unconditionally on Linux.
/// The pure-Rust alternative, `fontdb`, does *not* call fontconfig: its
/// "system fonts" are its own hardcoded directory list — i.e. this list, with
/// someone else maintaining it. So the choice is a C library on the build
/// (or run) path versus a directory list, and terra currently keeps the list.
#[cfg(all(unix, not(target_os = "macos")))]
const SCRIPT_FALLBACKS: &[ScriptFallback] = &[
    ScriptFallback {
        name: "Noto Sans Hebrew",
        files: &["NotoSansHebrew-Regular.ttf"],
        index: 0,
    },
    ScriptFallback {
        name: "DejaVu Sans Mono",
        files: &["DejaVuSansMono.ttf"],
        index: 0,
    },
    ScriptFallback {
        name: "DejaVu Sans",
        files: &["DejaVuSans.ttf"],
        index: 0,
    },
    ScriptFallback {
        name: "Noto Sans Symbols",
        files: &["NotoSansSymbols-Regular.ttf"],
        index: 0,
    },
    ScriptFallback {
        name: "Noto Sans Symbols 2",
        files: &["NotoSansSymbols2-Regular.ttf"],
        index: 0,
    },
    ScriptFallback {
        name: "Unifont",
        files: &["unifont.otf", "unifont.ttf"],
        index: 0,
    },
];

/// Windows fallbacks, from the set Microsoft documents as shipping with the OS.
///
/// File names only; the directories they are looked up in are [`FONT_DIRS`],
/// which resolves `%SystemRoot%` rather than assuming `C:\Windows` and also
/// covers the per-user font directory.
///
/// Every face here is in the *main* table of
/// `learn.microsoft.com/typography/fonts/windows_10_font_list` and its Windows
/// 11 counterpart — i.e. always installed — as opposed to the "Fonts included
/// in Feature On Demand (FOD) packages" section further down those pages, which
/// is where the extra Hebrew typefaces (David, Miriam, Narkisim, …) live. Terra
/// deliberately depends on none of the FOD faces: the Hebrew Supplemental Fonts
/// package is only installed once the user adds Hebrew to their language
/// settings, so a fallback resting on it would be tofu on exactly the machine
/// that has not got round to that yet.
///
/// * **Segoe UI** (`segoeui.ttf`) is the system UI face and covers Hebrew:
///   its font page lists `Hebr` in both `dlng` and `slng` and code page 1255,
///   and it carries the Hebrew OpenType layout tables.
/// * **Cascadia Mono** is the Menlo of this table — monospaced, so it draws
///   dingbats at the cell width. **Not expected to be found on a clean
///   install**: although the Windows 11 font list names it, it is delivered
///   inside the Windows Terminal MSIX package and registered through that
///   package's `SharedFonts` manifest extension, so the file sits under
///   `C:\Program Files\WindowsApps\…` and not in the fonts directory. It is
///   probed anyway because a user who installs the upstream release gets
///   `CascadiaMono.ttf` (or `CascadiaCode.ttf`) in the per-user font
///   directory, and finding it there is free.
/// * **Segoe UI Symbol** (`seguisym.ttf`) is Windows' own technical-symbol
///   face — the counterpart to Apple Symbols, and where `⎿` U+23BF and the
///   dingbats come from.
/// * **Tahoma** is a second Hebrew source (`Hebr` in `dlng` *and* `slng`, code
///   pages 1255 and 862), in case a future release trims Segoe UI.
/// * **Arial** is the backstop; `slng` includes `Hebr`.
///
/// # What is and is not verified
///
/// *Verified from Microsoft's documentation:* that all four of Segoe UI, Segoe
/// UI Symbol, Tahoma and Arial are always installed on Windows 10 and 11, and
/// that Segoe UI, Tahoma and Arial declare Hebrew support.
///
/// *Not verified, and not verifiable from a Mac:* the per-codepoint symbol
/// coverage. Microsoft publishes no cmap tables. Third-party coverage data
/// (fileformat.info) says Segoe UI Symbol has U+23BF, U+2733, U+273B, U+273D,
/// U+25D2, U+2692, U+2612 and U+276F but **not** U+23FA — which is survivable
/// only because U+23FA comes from the bundled Noto Emoji on every platform
/// (see [`REQUIRED_COVERAGE`]). That claim is a snapshot of one font version
/// and is exactly what `windows_fallbacks_cover_the_scripts_the_grid_needs`
/// checks on a real Windows machine; this table is a hypothesis until that
/// test runs.
#[cfg(windows)]
const SCRIPT_FALLBACKS: &[ScriptFallback] = &[
    ScriptFallback {
        name: "Segoe UI",
        files: &["segoeui.ttf"],
        index: 0,
    },
    ScriptFallback {
        name: "Cascadia Mono",
        files: &["CascadiaMono.ttf", "CascadiaCode.ttf"],
        index: 0,
    },
    ScriptFallback {
        name: "Segoe UI Symbol",
        files: &["seguisym.ttf"],
        index: 0,
    },
    ScriptFallback {
        name: "Tahoma",
        files: &["tahoma.ttf"],
        index: 0,
    },
    ScriptFallback {
        name: "Arial",
        files: &["arial.ttf"],
        index: 0,
    },
];

/// Nothing known about this platform's fonts: the families still build, they
/// just have no system faces behind them.
#[cfg(not(any(unix, windows)))]
const SCRIPT_FALLBACKS: &[ScriptFallback] = &[];

/// `wght` values of SF Pro's own named instances (read out of its `fvar`
/// table): "Regular" is 400 and "Medium" is 510 — note the axis runs 1..=1000,
/// not the usual 100..=900.
const SF_WGHT_REGULAR: f32 = 400.0;
const SF_WGHT_MEDIUM: f32 = 510.0;

/// SF Pro's `opsz` axis runs 17..=96 and *defaults to 28* — i.e. to the Display
/// design, which is drawn tight for headlines. Pinning it to the low end picks
/// the Text design (looser spacing, more open counters), which is what macOS
/// uses for 13px chrome like window and tab titles.
const SF_OPSZ_TEXT: f32 = 17.0;

/// Whether [`UI_MEDIUM_FAMILY`] resolves to a genuinely heavier face.
///
/// False when the system font could not be read or parsed, in which case both
/// UI families fall through to egui's single-weight proportional font and
/// callers have to fake weight (see `ui::paint_faux_medium`).
static REAL_UI_MEDIUM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// True once [`install`] has registered a real medium-weight UI face.
pub fn has_real_ui_medium() -> bool {
    REAL_UI_MEDIUM.load(std::sync::atomic::Ordering::Relaxed)
}

/// Substitute `${VAR}` references from the environment, or `None` if any of
/// them is unset.
///
/// This is what lets [`FONT_DIRS`] name `%SystemRoot%\Fonts` and the per-user
/// font directories without hardcoding a drive letter or a home path. An unset
/// variable drops the whole directory — probing a literal `${LOCALAPPDATA}\…`
/// would only waste a syscall — and a malformed reference (`${` with no `}`)
/// does the same rather than being pasted through.
///
/// Deliberately not a general shell expansion: no `$VAR`, no `~`, no nesting.
/// The table is ours, so the only requirement is that it can say "this
/// variable, here".
fn expand(path: &str) -> Option<String> {
    let mut out = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(at) = rest.find("${") {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 2..];
        let end = tail.find('}')?;
        out.push_str(&std::env::var(&tail[..end]).ok()?);
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// Read the first of `files` found in any `dirs` entry that carries font magic
/// and really contains face `index`.
///
/// File names are the outer loop: a name earlier in the list is preferred
/// wherever it lives, rather than a whole directory being preferred over a
/// better-named file inside the next one. A path that exists but is not a font
/// (or is a collection without our face index) is skipped rather than ending
/// the search, so one bad file cannot mask a good one further along. A
/// directory whose `${VAR}` is unset drops out entirely.
///
/// `dirs` is a parameter rather than [`FONT_DIRS`] read directly so the search
/// can be exercised against a temporary tree on any platform; [`find_font`] is
/// the only caller that matters.
fn find_font_in(dirs: &[&str], files: &[&str], index: u32) -> Option<Vec<u8>> {
    files.iter().find_map(|file| {
        dirs.iter().find_map(|dir| {
            let path = std::path::Path::new(&expand(dir)?).join(file);
            let bytes = std::fs::read(path).ok()?;
            (index < sfnt_face_count(&bytes)?).then_some(bytes)
        })
    })
}

/// [`find_font_in`] over this platform's [`FONT_DIRS`].
fn find_font(files: &[&str], index: u32) -> Option<Vec<u8>> {
    find_font_in(FONT_DIRS, files, index)
}

/// Read the first system UI font in [`UI_FONT_FILES`] that exists and carries
/// font magic. Any failure is silent: the caller falls back to the default
/// font.
///
/// The parse check is [`sfnt_face_count`] rather than "does it have a `wght`
/// axis", which is what it used to be. On macOS that is the same answer — SFNS
/// is variable — but elsewhere most system UI faces are static, and rejecting
/// them would mean *no* UI font at all rather than one without a real medium
/// weight. Which of the two we got is [`has_weight_axis`]'s job.
fn read_system_ui_font() -> Option<Vec<u8>> {
    find_font(UI_FONT_FILES, 0)
}

/// Whether a face exposes the `wght` axis, i.e. whether asking for a medium
/// weight will actually produce one.
///
/// `variation_axes` parses the file through skrifa and yields an empty list if
/// that fails, so a static *or* unparsable font answers `false` — and the
/// caller registers it untweaked, which is right either way.
fn has_weight_axis(bytes: &[u8]) -> bool {
    egui::FontData::from_owned(bytes.to_vec())
        .variation_axes()
        .iter()
        .any(|axis| axis.tag == egui::epaint::text::Tag::new(b"wght"))
}

/// Number of faces in an sfnt file: 1 for a bare `.ttf`/`.otf`, the collection
/// count for a `.ttc`. `None` when the magic says this is not a font at all.
fn sfnt_face_count(bytes: &[u8]) -> Option<u32> {
    let tag: [u8; 4] = bytes.get(..4)?.try_into().ok()?;
    match &tag {
        // Collection: the face count is a u32 at offset 8.
        b"ttcf" => Some(u32::from_be_bytes(bytes.get(8..12)?.try_into().ok()?)),
        b"\x00\x01\x00\x00" | b"OTTO" | b"true" | b"typ1" => Some(1),
        _ => None,
    }
}

/// Read a fallback face, but only hand back bytes epaint can actually parse:
/// the file must carry font magic and must really contain the face index we
/// ask for. A face that is simply not installed yields `None` — the family
/// keeps what it had, which is the same tofu as before rather than a crash on
/// startup, and [`warn_about_uncovered_scripts`] is what tells the user.
///
/// See [`find_font`] for the search order over [`ScriptFallback::files`] and
/// [`FONT_DIRS`].
fn read_fallback(f: &ScriptFallback) -> Option<Vec<u8>> {
    find_font(f.files, f.index)
}

/// Big-endian sfnt readers that yield `None` past the end of the file, so a
/// truncated or hostile font is skipped rather than panicking in a slice index.
fn be_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn be_i16(bytes: &[u8], at: usize) -> Option<i16> {
    Some(i16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// Byte offset of the sfnt header for `face` inside `bytes`.
///
/// A bare `.ttf`/`.otf` starts its header at 0; a `.ttc` puts a face count at
/// offset 8 and then an array of u32 offsets from offset 12, one per face —
/// the same layout [`sfnt_face_count`] reads the count out of.
fn face_offset(bytes: &[u8], face: u32) -> Option<u32> {
    if bytes.get(..4)? == b"ttcf" {
        (face < be_u32(bytes, 8)?).then(|| be_u32(bytes, 12 + 4 * face as usize))?
    } else {
        (face == 0).then_some(0)
    }
}

/// Offset of `tag` in the face's table directory: a u16 table count at
/// header+4, then 16-byte records (tag, checksum, offset, length) from
/// header+12.
fn table(bytes: &[u8], header: u32, tag: &[u8; 4]) -> Option<u32> {
    let header = header as usize;
    let count = be_u16(bytes, header + 4)?;
    (0..count as usize).find_map(|i| {
        let rec = header + 12 + 16 * i;
        (bytes.get(rec..rec + 4)? == tag).then(|| be_u32(bytes, rec + 8))?
    })
}

/// Whether a `cmap` format 4 subtable at `sub` maps `cp` to a real glyph.
///
/// Format 4 is the BMP-only segmented mapping: parallel arrays of segment
/// ends, starts, deltas and range offsets, the last of which either is 0
/// (glyph = codepoint + delta) or is itself an offset *from its own slot*
/// into a shared glyph id array.
fn format4_glyph(bytes: &[u8], sub: usize, cp: u32) -> Option<u32> {
    let cp = u16::try_from(cp).ok()?;
    let seg_x2 = be_u16(bytes, sub + 6)? as usize;
    let ends = sub + 14;
    let starts = ends + seg_x2 + 2; // + the reservedPad u16
    let deltas = starts + seg_x2;
    let ranges = deltas + seg_x2;

    for i in (0..seg_x2).step_by(2) {
        if be_u16(bytes, ends + i)? < cp || be_u16(bytes, starts + i)? > cp {
            continue;
        }
        let start = be_u16(bytes, starts + i)?;
        let delta = be_u16(bytes, deltas + i)?;
        let range = be_u16(bytes, ranges + i)?;
        let glyph = if range == 0 {
            cp.wrapping_add(delta)
        } else {
            let at = ranges + i + range as usize + 2 * (cp - start) as usize;
            match be_u16(bytes, at)? {
                0 => 0,
                g => g.wrapping_add(delta),
            }
        };
        return (glyph != 0).then_some(u32::from(glyph));
    }
    None
}

/// Whether a `cmap` format 12 subtable at `sub` maps `cp`: a u32 group
/// count at sub+12 and then 12-byte (start, end, start glyph) groups. This
/// is the only format that reaches past the BMP, but system fonts use it for
/// BMP codepoints too, so it has to be checked either way.
fn format12_glyph(bytes: &[u8], sub: usize, cp: u32) -> Option<u32> {
    let groups = be_u32(bytes, sub + 12)? as usize;
    for i in 0..groups {
        let g = sub + 16 + 12 * i;
        let (start, end) = (be_u32(bytes, g)?, be_u32(bytes, g + 4)?);
        if (start..=end).contains(&cp) {
            let glyph = be_u32(bytes, g + 8)?.wrapping_add(cp - start);
            return (glyph != 0).then_some(glyph);
        }
    }
    None
}

/// The glyph face `face` of the font in `bytes` maps `cp` to, if any.
///
/// Walks every `cmap` encoding record rather than picking one: Menlo and
/// Apple Symbols hold their symbol coverage in a format 4 (3,1) subtable
/// while STIX also carries a format 12 (3,10) one, and which subtable a
/// given codepoint lives in is not something we want to hard-code.
fn glyph_of(bytes: &[u8], face: u32, cp: u32) -> Option<u32> {
    let header = face_offset(bytes, face)?;
    let cmap = table(bytes, header, b"cmap")? as usize;
    let count = be_u16(bytes, cmap + 2)?;
    (0..count as usize).find_map(|i| {
        let rec = cmap + 4 + 8 * i;
        let sub = cmap + be_u32(bytes, rec + 4)? as usize;
        match be_u16(bytes, sub) {
            Some(4) => format4_glyph(bytes, sub, cp),
            Some(12) => format12_glyph(bytes, sub, cp),
            _ => None,
        }
    })
}

/// Whether face `face` of the font in `bytes` has a glyph for `cp`.
///
/// This is the same question epaint asks when it walks a family looking for
/// the first face that can draw a character, so "no face in the family covers
/// `cp`" is exactly "the user sees a box".
fn covers(bytes: &[u8], face: u32, cp: u32) -> bool {
    glyph_of(bytes, face, cp).is_some()
}

/// A run of characters terra has to be able to draw, and what to tell the user
/// when nothing installed can.
struct Requirement {
    /// Named in the warning, so it has to mean something to a user reading a
    /// log line rather than to this file.
    what: &'static str,
    codepoints: &'static [u32],
    /// What to install. Platform-specific and necessarily approximate, but a
    /// package name is the difference between a warning and an actionable one.
    fix: &'static str,
}

/// Every Hebrew letter including the five final forms — the whole set the grid
/// has to place, not a sample of it. A face that stops halfway through the
/// alphabet is exactly the failure this catches.
const HEBREW_LETTERS: &[u32] = &[
    0x05D0, 0x05D1, 0x05D2, 0x05D3, 0x05D4, 0x05D5, 0x05D6, 0x05D7, 0x05D8, 0x05D9, 0x05DA, 0x05DB,
    0x05DC, 0x05DD, 0x05DE, 0x05DF, 0x05E0, 0x05E1, 0x05E2, 0x05E3, 0x05E4, 0x05E5, 0x05E6, 0x05E7,
    0x05E8, 0x05E9, 0x05EA,
];

/// The symbols TUIs paint that no monospace face carries — `✳ ✻ ✽ ◒ ⚒ ☒` plus
/// `⎿` and `⏺`, the set Claude Code alone uses, and `❯`, which JetBrains Mono
/// does have and which is therefore also a check that this list is being
/// evaluated against the real families rather than against nothing.
const TUI_SYMBOLS: &[u32] = &[
    0x2733, 0x273B, 0x273D, 0x25D2, 0x2692, 0x2612, 0x23BF, 0x23FA, 0x276F,
];

/// What the fallback tables exist to cover. Checked against the *assembled*
/// families at startup, so it accounts for the bundled faces and epaint's
/// built-ins as well as for [`SCRIPT_FALLBACKS`] — U+23FA, for instance, is
/// covered by the bundled Noto Emoji on every platform and must not be
/// reported missing merely because no system face has it.
const REQUIRED_COVERAGE: &[Requirement] = &[
    Requirement {
        what: "Hebrew",
        codepoints: HEBREW_LETTERS,
        fix: HEBREW_FIX,
    },
    Requirement {
        what: "the TUI symbol set",
        codepoints: TUI_SYMBOLS,
        fix: SYMBOL_FIX,
    },
];

#[cfg(target_os = "macos")]
const HEBREW_FIX: &str = "Arial Hebrew is missing from /System/Library/Fonts";
#[cfg(target_os = "macos")]
const SYMBOL_FIX: &str = "Menlo, Apple Symbols or STIX Two Math is missing from \
                          /System/Library/Fonts";

#[cfg(windows)]
const HEBREW_FIX: &str = "Segoe UI, Tahoma and Arial all carry Hebrew and all ship with \
                          Windows — check %SystemRoot%\\Fonts";
#[cfg(windows)]
const SYMBOL_FIX: &str = "install Segoe UI Symbol (seguisym.ttf, part of Windows) or any \
                          Nerd Font into %LOCALAPPDATA%\\Microsoft\\Windows\\Fonts";

#[cfg(all(unix, not(target_os = "macos")))]
const HEBREW_FIX: &str = "install fonts-noto-core (Debian/Ubuntu), \
                          google-noto-sans-hebrew-fonts (Fedora) or noto-fonts (Arch)";
#[cfg(all(unix, not(target_os = "macos")))]
const SYMBOL_FIX: &str = "install fonts-noto-core and fonts-unifont (Debian/Ubuntu), \
                          google-noto-sans-symbols{,-2}-fonts and unifont-fonts (Fedora) \
                          or noto-fonts (Arch)";

#[cfg(not(any(unix, windows)))]
const HEBREW_FIX: &str = "terra knows no font locations on this platform";
#[cfg(not(any(unix, windows)))]
const SYMBOL_FIX: &str = "terra knows no font locations on this platform";

/// How many missing codepoints to spell out before summarising. Enough to see
/// the shape of the gap, few enough that a machine with no fonts at all does
/// not print the Hebrew alphabet into the log.
const MAX_LISTED: usize = 4;

/// Which codepoints of which [`REQUIRED_COVERAGE`] entries no face in `family`
/// can draw. Entries that are fully covered are dropped, so an empty result
/// means the user will see text everywhere terra knows to look.
///
/// The family is walked exactly as epaint walks it — first face with the
/// codepoint wins — so this answers the user's question ("will I see boxes?")
/// rather than the table's ("did the files load?"). A face named in the family
/// but absent from `font_data` is skipped; that cannot happen today, but it
/// would otherwise be an index panic during startup.
fn missing_coverage(
    fonts: &egui::FontDefinitions,
    family: &egui::FontFamily,
) -> Vec<(&'static Requirement, Vec<u32>)> {
    let Some(names) = fonts.families.get(family) else {
        return Vec::new();
    };
    let faces: Vec<_> = names
        .iter()
        .filter_map(|name| fonts.font_data.get(name))
        .map(|data| (data.font.as_ref(), data.index))
        .collect();

    REQUIRED_COVERAGE
        .iter()
        .filter_map(|req| {
            let missing: Vec<u32> = req
                .codepoints
                .iter()
                .copied()
                .filter(|&cp| !faces.iter().any(|(bytes, index)| covers(bytes, *index, cp)))
                .collect();
            (!missing.is_empty()).then_some((req, missing))
        })
        .collect()
}

/// [`missing_coverage`] as ready-to-log sentences.
fn uncovered(fonts: &egui::FontDefinitions, family: &egui::FontFamily) -> Vec<String> {
    missing_coverage(fonts, family)
        .into_iter()
        .map(|(req, missing)| {
            let listed: Vec<String> = missing
                .iter()
                .take(MAX_LISTED)
                .map(|cp| format!("U+{cp:04X}"))
                .collect();
            let more = match missing.len().saturating_sub(MAX_LISTED) {
                0 => String::new(),
                n => format!(" and {n} more"),
            };
            format!(
                "no installed font covers {} ({}{}) — {} will render as empty boxes; {}",
                req.what,
                listed.join(" "),
                more,
                if missing.len() == 1 {
                    "it".to_owned()
                } else {
                    format!("{} characters", missing.len())
                },
                req.fix,
            )
        })
        .collect()
}

/// Log whatever the assembled monospace family cannot draw.
///
/// The terminal grid is [`egui::FontFamily::Monospace`], so that is the family
/// that decides whether the user sees text or boxes; the other families are
/// built from the same faces plus more.
///
/// Phrased and routed like `config.rs`'s warnings (`log::warn!("terra: <area>:
/// {warning}")`) so both arrive the same way. It is a log line rather than a
/// `Config::warnings()` entry because nothing here comes from the user's
/// config file — it is a property of the machine, and there is no config error
/// for the user to go and fix.
fn warn_about_uncovered_scripts(fonts: &egui::FontDefinitions) {
    for warning in uncovered(fonts, &egui::FontFamily::Monospace) {
        log::warn!("terra: fonts: {warning}");
    }
}

/// A face's `hhea` ascender and the row height epaint derives from it, both in
/// em: `(ascender, ascender - descender + lineGap) / head.unitsPerEm`.
///
/// These are exactly the two numbers `FontImpl::styled_metrics` reads
/// (`epaint-0.35.0/src/text/font.rs`), which is why `OS/2` is ignored here even
/// where it disagrees — the goal is to predict what epaint will do, not to pick
/// the better-authored table.
fn vertical_metrics(bytes: &[u8], face: u32) -> Option<(f32, f32)> {
    let header = face_offset(bytes, face)?;
    let units_per_em = f32::from(be_u16(bytes, table(bytes, header, b"head")? as usize + 18)?);
    let hhea = table(bytes, header, b"hhea")? as usize;
    let ascender = f32::from(be_i16(bytes, hhea + 4)?);
    let descender = f32::from(be_i16(bytes, hhea + 6)?);
    let line_gap = f32::from(be_i16(bytes, hhea + 8)?);

    (units_per_em > 0.0).then(|| {
        (
            ascender / units_per_em,
            (ascender - descender + line_gap) / units_per_em,
        )
    })
}

/// The `y_offset_factor` that puts a fallback face's baseline where JetBrains
/// Mono's is, in the family JetBrains Mono leads.
///
/// # Why any of this is needed
///
/// epaint does **not** share one baseline across the faces of a family. It
/// places each glyph at
///
/// ```text
/// y = ascent_face + (row_height_family - row_height_face) / 2
/// ```
///
/// (`epaint-0.35.0/src/text/text_layout.rs`, "we always center the difference"),
/// where the *family* metrics come from the family's **first** face only
/// (`Font::styled_metrics` takes `cached_family.fonts.first()`). So every
/// fallback is vertically centred against the leading face rather than aligned
/// to it, and any face whose `hhea` box is shorter than JetBrains Mono's comes
/// out floating above the text it is meant to sit beside.
///
/// Arial Hebrew is the worst of ours. JetBrains Mono is 1020/-300 per 1000 upem
/// (ascent 1.020 em, row height 1.320 em); Arial Hebrew is 730/-335 (0.730 em,
/// 1.065 em), so epaint draws its baseline at
/// `0.730 + (1.320 - 1.065)/2 = 0.858 em` against JetBrains Mono's `1.020 em`
/// — **0.163 em, or 2.3 pt at the default 14 pt, too high**. Hebrew letters are
/// 0.518 em tall, so instead of topping out just below the Latin x-height
/// (0.550) they end up at 0.680, near cap height (0.730): the whole script
/// reads as hovering, which is what makes punctuation next to it — a `?` drawn
/// from JetBrains Mono, on the real baseline — look wrong. Menlo is off by only
/// 0.014 em, but Apple Symbols by 0.194 and STIX Two Math by 0.223.
///
/// # Why it is computed and not a constant
///
/// Every input except JetBrains Mono is a macOS system file whose metrics Apple
/// has changed across releases. A number measured here would be silently stale
/// on the next OS, whereas the subtraction is cheap and always right. `0.0` on
/// any parse failure, which is the old behaviour.
///
/// The factor is divided by `scale` because epaint computes the shift as
/// `font_size * tweak.scale * tweak.y_offset_factor`, while the misalignment it
/// has to cancel is a plain fraction of `font_size`.
///
/// The correction is exact for [`egui::FontFamily::Monospace`] — the terminal
/// grid, which is what this is for. The UI families lead with SF Pro (0.967 em
/// ascent, 1.178 em row height), close enough to JetBrains Mono that the same
/// shift leaves tab titles within 0.02 em of their baseline.
fn baseline_offset_factor(bytes: &[u8], face: u32, scale: f32) -> f32 {
    let Some((grid_ascent, grid_row_height)) = vertical_metrics(JETBRAINS_MONO_REGULAR, 0) else {
        return 0.0;
    };
    let Some((ascent, row_height)) = vertical_metrics(bytes, face) else {
        return 0.0;
    };
    if scale <= 0.0 {
        return 0.0;
    }

    let baseline = scale * ascent + 0.5 * (grid_row_height - scale * row_height);
    (grid_ascent - baseline) / scale
}

/// Register every available [`SCRIPT_FALLBACKS`] entry, returning the names to
/// append to the family lists.
fn install_script_fallbacks(fonts: &mut egui::FontDefinitions) -> Vec<String> {
    use std::sync::Arc;

    let mut names = Vec::new();
    for f in SCRIPT_FALLBACKS {
        let Some(bytes) = read_fallback(f) else {
            continue;
        };
        let tweak = egui::FontTweak {
            y_offset_factor: baseline_offset_factor(&bytes, f.index, 1.0),
            ..Default::default()
        };
        let mut data = egui::FontData::from_owned(bytes).tweak(tweak);
        data.index = f.index;
        fonts.font_data.insert(f.name.to_owned(), Arc::new(data));
        names.push(f.name.to_owned());
    }
    names
}

/// Register [`NOTO_EMOJI`] and slot it into both built-in families.
///
/// # Where it goes, and why
///
/// Directly *in front of* epaint's `NotoEmoji-Regular` and *behind* everything
/// already ahead of that — which is exactly the pair of constraints we have:
///
/// * Behind JetBrains Mono (and Hack, and Ubuntu-Light in the proportional
///   list, and SF Pro once [`install_ui_family`] prepends it), because epaint
///   picks the *first* face in the family that has the codepoint. Noto Emoji
///   carries a handful of non-emoji characters; ahead of our own faces it could
///   quietly restyle them.
/// * Ahead of epaint's subset, which is the entire point: the subset claims
///   U+1F600 and would otherwise keep winning, while U+1F642 stayed tofu.
///
/// Anchoring on the subset's name rather than a fixed index means the ordering
/// survives epaint changing the contents of its default families; if the name
/// ever disappears we append, which is still behind our own faces.
///
/// This also runs *before* [`SCRIPT_FALLBACKS`] are appended, so the emoji face
/// ends up ahead of Menlo/Apple Symbols/STIX. That is the status quo, not a new
/// decision: epaint's emoji fonts were already in front of them, and they
/// already supplied `✳` U+2733, `⚒` U+2692 and `⏺` U+23FA. Serving those three
/// from one emoji face instead of two is if anything more consistent.
fn install_emoji(fonts: &mut egui::FontDefinitions) {
    use std::sync::Arc;

    let tweak = egui::FontTweak {
        scale: EMOJI_SCALE,
        coords: egui::epaint::text::VariationCoords::new([(b"wght", EMOJI_WGHT_REGULAR)]),
        ..Default::default()
    };
    fonts.font_data.insert(
        EMOJI_FACE.to_owned(),
        Arc::new(egui::FontData::from_static(NOTO_EMOJI).tweak(tweak)),
    );

    for family in [egui::FontFamily::Monospace, egui::FontFamily::Proportional] {
        let list = fonts.families.entry(family).or_default();
        let at = list
            .iter()
            .position(|name| name == EPAINT_EMOJI_FACE)
            .unwrap_or(list.len());
        list.insert(at, EMOJI_FACE.to_owned());
    }
}

/// A *variable* system UI face pinned to one weight and to the Text optical
/// size. Only applied when [`has_weight_axis`] says the coordinates mean
/// something; on a static face the tweak would be dead weight.
fn sf_pro_at(weight: f32) -> egui::FontTweak {
    egui::FontTweak {
        coords: egui::epaint::text::VariationCoords::new([
            (b"wght", weight),
            (b"opsz", SF_OPSZ_TEXT),
        ]),
        ..Default::default()
    }
}

/// Install JetBrains Mono as the first monospace font, keeping egui's built-in
/// fonts (including the emoji fallbacks) behind it, and pin the glyph
/// anti-aliasing settings that make light-on-dark text render at the right
/// brightness (see [`pin_text_rendering`]).
pub fn install(ctx: &egui::Context) {
    use std::sync::Arc;

    pin_text_rendering(ctx);

    // `default()` brings Hack, Ubuntu-Light, NotoEmoji-Regular and
    // emoji-icon-font along; we only push our faces in front of them.
    let mut fonts = egui::FontDefinitions::default();

    fonts.font_data.insert(
        "JetBrains Mono".to_owned(),
        Arc::new(egui::FontData::from_static(JETBRAINS_MONO_REGULAR)),
    );
    fonts.font_data.insert(
        BOLD_FAMILY.to_owned(),
        Arc::new(egui::FontData::from_static(JETBRAINS_MONO_BOLD)),
    );

    // First in the monospace list => `FontId::monospace(..)` picks it up, and
    // anything it lacks (emoji, √, box drawing) still falls through to the
    // egui defaults.
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, "JetBrains Mono".to_owned());

    // Ahead of epaint's subset emoji face, behind everything else.
    install_emoji(&mut fonts);

    // Script fallbacks go on the *end* of both built-in families, before the
    // derived families are cloned off them below, so the bold and UI families
    // inherit the same coverage.
    let script = install_script_fallbacks(&mut fonts);
    for family in [egui::FontFamily::Monospace, egui::FontFamily::Proportional] {
        fonts
            .families
            .entry(family)
            .or_default()
            .extend(script.iter().cloned());
    }

    // A named family so bold monospace text can be requested explicitly.
    let mut bold_family = vec![BOLD_FAMILY.to_owned()];
    bold_family.extend(
        fonts
            .families
            .get(&egui::FontFamily::Monospace)
            .cloned()
            .unwrap_or_default(),
    );
    fonts
        .families
        .insert(egui::FontFamily::Name(BOLD_FAMILY.into()), bold_family);

    install_ui_family(&mut fonts);

    // After every face is registered, so this reports what the user will
    // actually see rather than which files happened to be missing.
    warn_about_uncovered_scripts(&fonts);

    ctx.set_fonts(fonts);
}

/// Register [`UI_FAMILY`] / [`UI_MEDIUM_FAMILY`], preferring the macOS system
/// face so tab titles are set in the same font (and the same weights) as the
/// native tab bars they imitate.
///
/// Both families are always registered, with egui's proportional list appended
/// behind whatever we found: if the system font is missing or unparsable the
/// families still resolve — to the default font, at its one weight — so callers
/// never have to branch on availability, and nothing here can panic.
fn install_ui_family(fonts: &mut egui::FontDefinitions) {
    use std::sync::Arc;

    let fallback = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();

    let mut regular = Vec::new();
    let mut medium = Vec::new();

    if let Some(bytes) = read_system_ui_font() {
        if has_weight_axis(&bytes) {
            // The same file twice, at two points on its weight axis.
            fonts.font_data.insert(
                UI_FACE.to_owned(),
                Arc::new(
                    egui::FontData::from_owned(bytes.clone()).tweak(sf_pro_at(SF_WGHT_REGULAR)),
                ),
            );
            fonts.font_data.insert(
                UI_FACE_MEDIUM.to_owned(),
                Arc::new(egui::FontData::from_owned(bytes).tweak(sf_pro_at(SF_WGHT_MEDIUM))),
            );
            regular.push(UI_FACE.to_owned());
            medium.push(UI_FACE_MEDIUM.to_owned());
            REAL_UI_MEDIUM.store(true, std::sync::atomic::Ordering::Relaxed);
        } else {
            // A static face: one weight is all there is, so both families lead
            // with it and `has_real_ui_medium()` stays false, which is exactly
            // the signal `ui::paint_faux_medium` already reads. Better a
            // single-weight system font than egui's default proportional face.
            fonts.font_data.insert(
                UI_FACE.to_owned(),
                Arc::new(egui::FontData::from_owned(bytes)),
            );
            regular.push(UI_FACE.to_owned());
            medium.push(UI_FACE.to_owned());
        }
    }

    regular.extend(fallback.iter().cloned());
    medium.extend(fallback);

    fonts
        .families
        .insert(egui::FontFamily::Name(UI_FAMILY.into()), regular);
    fonts
        .families
        .insert(egui::FontFamily::Name(UI_MEDIUM_FAMILY.into()), medium);
}

/// Pin the glyph coverage -> alpha transfer function to the dark-mode curve.
///
/// # Why
///
/// egui/wgpu alpha-blends in **gamma (sRGB-encoded) space**: the fragment
/// shader computes `vertex_color * atlas_alpha` on the 0-255 sRGB values and
/// the blender does `src + dst * (1 - a)` on those same encoded values
/// (`egui-wgpu-0.35.0/src/egui.wgsl`, `fs_main_gamma_framebuffer`). For
/// light-on-dark text that systematically *under*-lights partially covered
/// pixels: a 50%-covered pixel should end up at ~73% of the text colour
/// (sRGB(0.5) in linear light), not 50%.
///
/// epaint compensates for this when it bakes the coverage mask into the font
/// atlas, via `FontColorTransferFunction`
/// (`epaint-0.35.0/src/image.rs`, and `TextOptions::color_transfer_function`
/// in `epaint-0.35.0/src/text/mod.rs`):
///
/// * `Off` / `Gamma(1.0)` — `alpha = coverage`. egui's **light**-mode default
///   (`Visuals::light()`, `egui-0.35.0/src/style.rs`), correct for dark text on
///   a light background.
/// * `TwoCoverageMinusCoverageSq` — `alpha = 2c - c²` (≈ `c^0.5`, i.e. roughly
///   the sRGB encoding curve). egui's **dark**-mode default
///   (`Visuals::dark()`), correct for light text on a dark background.
///
/// egui picks between them from the *system* theme, because
/// `Options::theme_preference` defaults to `ThemePreference::System`
/// (`egui-0.35.0/src/memory/mod.rs`). Terra's terminal surface is
/// unconditionally dark (`#1e1e1e`), so on a macOS account set to Light
/// appearance egui would bake `alpha = coverage` and every anti-aliased glyph
/// edge would come out visibly thinner and dimmer than Ghostty's — which
/// always gamma-corrects light-on-dark coverage.
///
/// So: pin the preference to Dark, and additionally force the dark-mode curve
/// into *both* theme styles so the setting survives a later `set_visuals` /
/// theme flip from anywhere else in the app.
fn pin_text_rendering(ctx: &egui::Context) {
    use egui::epaint::FontColorTransferFunction;

    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.all_styles_mut(|style| {
        style.visuals.text_options.color_transfer_function =
            FontColorTransferFunction::DARK_MODE_DEFAULT;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cp`'s advance width in em, as epaint will scale it: `hmtx` looked up
    /// through `cmap`, over `head.unitsPerEm`. Glyphs past
    /// `hhea.numberOfHMetrics` share the last entry's advance, which is how
    /// monospace faces store a single width for the whole file.
    #[cfg(target_os = "macos")]
    fn advance_em(bytes: &[u8], face: u32, cp: u32) -> Option<f32> {
        let header = face_offset(bytes, face)?;
        let units_per_em = f32::from(be_u16(bytes, table(bytes, header, b"head")? as usize + 18)?);
        let metrics = be_u16(bytes, table(bytes, header, b"hhea")? as usize + 34)?;
        let hmtx = table(bytes, header, b"hmtx")? as usize;
        let glyph = glyph_of(bytes, face, cp)?.min(u32::from(metrics.checked_sub(1)?));
        let advance = f32::from(be_u16(bytes, hmtx + 4 * glyph as usize)?);

        (units_per_em > 0.0).then_some(advance / units_per_em)
    }

    /// `OS/2.sxHeight` in em — the height of the lowercase letters the Hebrew
    /// has to look the same size as. Only version 2 and later carry it.
    #[cfg(target_os = "macos")]
    fn x_height_em(bytes: &[u8], face: u32) -> Option<f32> {
        let header = face_offset(bytes, face)?;
        let units_per_em = f32::from(be_u16(bytes, table(bytes, header, b"head")? as usize + 18)?);
        let os2 = table(bytes, header, b"OS/2")? as usize;
        if be_u16(bytes, os2)? < 2 || units_per_em <= 0.0 {
            return None;
        }
        let x_height = f32::from(be_i16(bytes, os2 + 86)?);

        Some(x_height / units_per_em)
    }

    /// The width of one terminal cell, in em.
    ///
    /// `egui_term::TerminalFont::font_measure` takes it from `glyph_width` of
    /// `m` in the monospace family, which resolves to JetBrains Mono, so this
    /// is the same number the grid lays out with rather than a stand-in.
    #[cfg(target_os = "macos")]
    fn cell_width_em() -> f32 {
        advance_em(JETBRAINS_MONO_REGULAR, 0, 'm' as u32).expect("JetBrains Mono has no m")
    }

    /// Arial Hebrew as `install_script_fallbacks` actually registers it, so
    /// the tests below read the tweak that ships rather than a constant.
    #[cfg(target_os = "macos")]
    fn registered_hebrew() -> (Vec<u8>, egui::FontTweak) {
        let hebrew = &SCRIPT_FALLBACKS[0];
        assert_eq!(hebrew.name, "Arial Hebrew", "the Hebrew face moved");
        let bytes = read_fallback(hebrew).expect("Arial Hebrew is missing");

        let mut fonts = egui::FontDefinitions::default();
        install_script_fallbacks(&mut fonts);
        let tweak = fonts.font_data[hebrew.name].tweak.clone();

        (bytes, tweak)
    }

    /// The families exactly as [`install`] assembles them, so a test can ask
    /// what the user will really see instead of re-deriving it.
    ///
    /// Kept in step with `install` by construction: it calls the same four
    /// steps in the same order. The `Context`-taking half of `install`
    /// (`pin_text_rendering`, `set_fonts`) needs a running egui and is what is
    /// left out.
    fn assembled_families() -> egui::FontDefinitions {
        use std::sync::Arc;

        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "JetBrains Mono".to_owned(),
            Arc::new(egui::FontData::from_static(JETBRAINS_MONO_REGULAR)),
        );
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .insert(0, "JetBrains Mono".to_owned());
        install_emoji(&mut fonts);
        let script = install_script_fallbacks(&mut fonts);
        for family in [egui::FontFamily::Monospace, egui::FontFamily::Proportional] {
            fonts
                .families
                .entry(family)
                .or_default()
                .extend(script.iter().cloned());
        }
        install_ui_family(&mut fonts);
        fonts
    }

    /// Names of the [`SCRIPT_FALLBACKS`] entries actually present on this
    /// machine, for tests that have to distinguish "absent, fine" from
    /// "present and wrong". macOS does not need it: there every entry is a
    /// system file that must be there, so its tests assert outright.
    #[cfg(not(target_os = "macos"))]
    fn installed_fallbacks() -> Vec<&'static str> {
        SCRIPT_FALLBACKS
            .iter()
            .filter(|f| read_fallback(f).is_some())
            .map(|f| f.name)
            .collect()
    }

    #[test]
    fn sfnt_face_count_reads_each_container() {
        assert_eq!(sfnt_face_count(b"\x00\x01\x00\x00rest"), Some(1));
        assert_eq!(sfnt_face_count(b"OTTOrest"), Some(1));
        // .ttc: face count is the u32 at offset 8.
        assert_eq!(
            sfnt_face_count(b"ttcf\x00\x02\x00\x00\x00\x00\x00\x09"),
            Some(9)
        );
        // Not a font, and truncated headers, must be rejected rather than
        // handed to epaint.
        assert_eq!(sfnt_face_count(b"#!/bin/sh"), None);
        assert_eq!(sfnt_face_count(b"ttcf\x00\x02"), None);
        assert_eq!(sfnt_face_count(b""), None);
    }

    /// A fallback whose file is missing must be skipped, not panicked on:
    /// users can and do delete supplemental system fonts — and on Linux the
    /// *normal* case is that no directory in the list has the file.
    #[test]
    fn missing_fallback_file_is_skipped() {
        assert!(read_fallback(&ScriptFallback {
            name: "nope",
            files: &["definitely-not-a-font.ttc"],
            index: 0,
        })
        .is_none());
        // Every name missing is still just "not available", never a panic.
        assert!(read_fallback(&ScriptFallback {
            name: "nope",
            files: &["nope-one.ttf", "nope-two.ttf"],
            index: 0,
        })
        .is_none());
        assert!(read_fallback(&ScriptFallback {
            name: "nope",
            files: &[],
            index: 0,
        })
        .is_none());
        // A name that cannot be a file name at all must not escape the
        // directory list into some absolute path.
        assert!(find_font(&["../../../etc/hosts"], 0).is_none());
    }

    /// The point of the directory list: a face is found wherever the
    /// distribution happens to put it, and directories ahead of it that are
    /// absent — or hold a file of the same name that is not a font — do not
    /// stop the search.
    ///
    /// Driven through the real [`find_font_in`] against a temporary directory
    /// tree, one arm of which is reached through `${…}` expansion — the same
    /// mechanism `%SystemRoot%` and the per-user font directories use.
    #[test]
    fn a_fallback_takes_the_first_readable_directory_and_skips_the_rest() {
        let root = std::env::temp_dir().join("terra-font-fallback-probe");
        let (decoy_dir, real_dir) = (root.join("decoy"), root.join("real"));
        std::fs::create_dir_all(&decoy_dir).expect("temp dir");
        std::fs::create_dir_all(&real_dir).expect("temp dir");
        std::fs::write(decoy_dir.join("probe.ttf"), b"#!/bin/sh\n").expect("write decoy");
        std::fs::write(real_dir.join("second.ttf"), JETBRAINS_MONO_BOLD).expect("write font");
        std::fs::write(real_dir.join("probe.ttf"), JETBRAINS_MONO_REGULAR).expect("write font");
        std::env::set_var("TERRA_FONT_TEST_DIR", &real_dir);

        let dirs = &[
            // Missing, then present-but-not-a-font, then the real one — and
            // that last one only exists after expansion.
            "/definitely/not/a/font/directory",
            decoy_dir.to_str().expect("utf-8 temp dir"),
            "${TERRA_FONT_TEST_DIR}",
            "${TERRA_NOT_SET_ANYWHERE}",
        ];

        let found = find_font_in(dirs, &["probe.ttf"], 0).expect("the font should have been found");
        assert_eq!(found, JETBRAINS_MONO_REGULAR);

        // File names are the outer loop: the first *name* wins wherever it
        // lives, even though both names sit in the same directory.
        let found = find_font_in(dirs, &["probe.ttf", "second.ttf"], 0).expect("no font found");
        assert_eq!(found, JETBRAINS_MONO_REGULAR);
        let found = find_font_in(dirs, &["second.ttf", "probe.ttf"], 0).expect("no font found");
        assert_eq!(found, JETBRAINS_MONO_BOLD);

        // A face index the file does not have is still refused, whichever
        // directory the file was found in.
        assert!(find_font_in(dirs, &["probe.ttf"], u32::MAX).is_none());
        // And a directory that only exists as an unset variable finds nothing
        // rather than probing a literal `${…}` path.
        assert!(find_font_in(&["${TERRA_NOT_SET_ANYWHERE}"], &["probe.ttf"], 0).is_none());

        std::env::remove_var("TERRA_FONT_TEST_DIR");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `${VAR}` expansion is what keeps `%SystemRoot%` and the per-user font
    /// directories out of the source as literals. An unset or malformed
    /// reference must drop the directory rather than produce a path with a
    /// `${…}` in it, which would be probed forever and never found.
    #[test]
    fn environment_references_expand_or_drop_the_directory() {
        std::env::set_var("TERRA_FONT_TEST_VAR", "/opt/fonts");

        assert_eq!(
            expand("/usr/share/fonts").as_deref(),
            Some("/usr/share/fonts")
        );
        assert_eq!(
            expand("${TERRA_FONT_TEST_VAR}/noto").as_deref(),
            Some("/opt/fonts/noto")
        );
        assert_eq!(
            expand("${TERRA_FONT_TEST_VAR}/a/${TERRA_FONT_TEST_VAR}").as_deref(),
            Some("/opt/fonts/a//opt/fonts")
        );
        assert_eq!(expand("${TERRA_NOT_SET_ANYWHERE}/fonts"), None);
        assert_eq!(expand("${unterminated/fonts"), None);
        // A bare `$` or `}` is not a reference and must survive untouched.
        assert_eq!(expand("/fonts/$weird}").as_deref(), Some("/fonts/$weird}"));

        std::env::remove_var("TERRA_FONT_TEST_VAR");
        assert_eq!(expand("${TERRA_FONT_TEST_VAR}/noto"), None);
    }

    /// Every directory terra probes has to be either absolute or an
    /// environment reference that becomes absolute — a relative entry would
    /// resolve against the shell's working directory, which for a terminal is
    /// wherever the user happened to launch it from.
    #[test]
    fn every_font_directory_is_absolute_or_environment_rooted() {
        for dir in FONT_DIRS {
            assert!(
                dir.starts_with('/') || dir.starts_with("${"),
                "{dir} is neither absolute nor environment-rooted"
            );
            if let Some(expanded) = expand(dir) {
                assert!(
                    std::path::Path::new(&expanded).is_absolute(),
                    "{dir} expands to the relative path {expanded}"
                );
            }
        }
    }

    /// File names are names, not paths: a separator in the table would make
    /// the directory list meaningless for that entry (and, with `..`, let it
    /// point anywhere).
    #[test]
    fn every_fallback_names_a_bare_file() {
        for f in SCRIPT_FALLBACKS
            .iter()
            .map(|f| f.files)
            .chain([UI_FONT_FILES])
        {
            for file in f {
                assert!(
                    !file.contains('/') && !file.contains('\\'),
                    "{file} is a path, not a file name"
                );
            }
        }
    }

    /// An out-of-range face index would make epaint read past the collection.
    ///
    /// Runs everywhere now that the table is per-platform: entries whose files
    /// are all absent are skipped, so on a bare Linux container this asserts
    /// nothing — but on a machine that *has* the fonts it is the same check it
    /// always was.
    #[test]
    fn fallback_face_index_is_bounds_checked() {
        for f in SCRIPT_FALLBACKS {
            if read_fallback(f).is_none() {
                continue; // not installed here
            }
            assert!(
                read_fallback(&ScriptFallback {
                    index: u32::MAX,
                    ..*f
                })
                .is_none(),
                "{} accepted an out-of-range face",
                f.name
            );
        }
    }

    /// Hebrew reaches the terminal grid through `FontFamily::Monospace`, and
    /// the tab bar through the UI families — so the fallback has to be behind
    /// *every* family, not just the one it was added for.
    ///
    /// The emoji face has the same "behind our own faces, in every family"
    /// requirement plus one more of its own — it must be *ahead* of epaint's
    /// subset — so it is checked here rather than in a parallel test that would
    /// have to rebuild the same font definitions.
    #[test]
    #[cfg(target_os = "macos")]
    fn script_fallbacks_land_behind_every_family() {
        let mut fonts = egui::FontDefinitions::default();
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .insert(0, "JetBrains Mono".to_owned());
        install_emoji(&mut fonts);
        let script = install_script_fallbacks(&mut fonts);
        assert!(!script.is_empty(), "no script fallback loaded");
        for family in [egui::FontFamily::Monospace, egui::FontFamily::Proportional] {
            fonts
                .families
                .entry(family)
                .or_default()
                .extend(script.iter().cloned());
        }
        install_ui_family(&mut fonts);

        for family in [
            egui::FontFamily::Monospace,
            egui::FontFamily::Proportional,
            egui::FontFamily::Name(UI_FAMILY.into()),
            egui::FontFamily::Name(UI_MEDIUM_FAMILY.into()),
        ] {
            let list = &fonts.families[&family];
            let at = |name: &String| list.iter().position(|f| f == name);
            for name in &script {
                assert!(at(name).is_some(), "{family:?} is missing {name}");
            }
            // Behind, never in front: our own faces must still win for Latin.
            // Menlo covers ASCII, so were it ahead of JetBrains Mono the whole
            // grid would silently change typeface — assert against every
            // fallback, not just the first, and against the *leading* face
            // rather than only position 0.
            assert!(
                !script.contains(&list[0]),
                "{family:?} leads with fallback {}",
                list[0]
            );
            assert!(
                script.iter().all(|name| at(name) > Some(0)),
                "{family:?} puts a fallback in front of its own faces"
            );

            // Same rule for the emoji face — Menlo covers ASCII, but so does
            // nothing in Noto Emoji, so this guards the subtler case: any
            // stray non-emoji codepoint it does carry must not outrank
            // JetBrains Mono or SF Pro.
            let of = |name: &str| list.iter().position(|f| f == name);
            let emoji = of(EMOJI_FACE);
            assert!(emoji.is_some(), "{family:?} is missing {EMOJI_FACE}");
            assert!(
                emoji > Some(0),
                "{family:?} leads with {EMOJI_FACE} instead of its own face"
            );
            // ...and, uniquely, it must beat epaint's subset, or U+1F642 is
            // still tofu however well the file is registered.
            assert!(
                emoji < of(EPAINT_EMOJI_FACE),
                "{family:?} puts epaint's subset emoji ahead of {EMOJI_FACE}"
            );
        }
    }

    /// The emoji this bundle exists for. U+1F642 is the one that was tofu;
    /// the rest are ordinary emoji that must not have regressed with it.
    const EMOJI: &[(char, u32)] = &[
        ('🙂', 0x1F642),
        ('😀', 0x1F600),
        ('👍', 0x1F44D),
        ('❤', 0x2764),
        ('🎉', 0x1F389),
    ];

    /// The bundled face has to cover strictly more than the one egui already
    /// ships, or embedding two megabytes of font buys nothing.
    ///
    /// The negative control is the half that makes this a real test: epaint's
    /// subset must be shown *not* to have U+1F642, otherwise a passing
    /// assertion above would prove only that emoji fonts contain emoji.
    #[test]
    fn the_bundled_emoji_font_covers_what_the_egui_subset_misses() {
        for &(glyph, cp) in EMOJI {
            assert!(
                covers(NOTO_EMOJI, 0, cp),
                "{glyph} (U+{cp:04X}) is not in the bundled emoji font"
            );
        }

        // Reached through `FontDefinitions::default()` rather than the
        // `epaint_default_fonts` crate, which terra does not depend on
        // directly; `FontData::font` is the same static bytes either way.
        let defaults = egui::FontDefinitions::default();
        let subset: &[u8] = &defaults.font_data[EPAINT_EMOJI_FACE].font;
        assert!(
            covers(subset, 0, 0x1F600),
            "the coverage helper cannot read epaint's emoji font at all"
        );
        assert!(
            !covers(subset, 0, 0x1F642),
            "epaint now ships U+1F642 — this bundle may no longer be needed"
        );
    }

    /// Emoji are drawn well outside the em square, so an untweaked face would
    /// overflow the two cells the grid gives a wide char. The exact factor is
    /// judgement, but it has to shrink, and it has to keep the face's ascender
    /// under JetBrains Mono's 1.02 em so the emoji never inflate row height.
    #[test]
    fn the_emoji_face_is_scaled_down_to_fit_the_cell_grid() {
        let mut fonts = egui::FontDefinitions::default();
        install_emoji(&mut fonts);

        // Read back off the registered face, not off the constant, so this
        // tests what `install_emoji` actually applied.
        let data = &fonts.font_data[EMOJI_FACE];
        let scale = data.tweak.scale;
        assert_eq!(scale, EMOJI_SCALE);
        assert!((0.5..1.0).contains(&scale), "{scale} is no shrink at all");
        // 2600/2048 em advance must land inside two 0.6 em cells.
        assert!(scale * 2600.0 / 2048.0 <= 1.2, "wide chars would overlap");
        // 1900/2048 em ascender must stay under JetBrains Mono's 1020/1000.
        assert!(scale * 1900.0 / 2048.0 <= 1.02, "rows would grow taller");

        let coords = data.tweak.coords.as_ref();
        assert_eq!(coords.len(), 1, "expected exactly the wght axis pinned");
        assert!(coords[0].0 == egui::epaint::text::Tag::new(b"wght"));
        assert_eq!(coords[0].1, EMOJI_WGHT_REGULAR);
    }

    /// epaint reads glyph *outlines* only (see the module docs), so a colour
    /// emoji font would register fine, claim every emoji codepoint and then
    /// draw nothing. Assert the bundled file is outline-only.
    #[test]
    fn the_bundled_emoji_font_has_no_colour_tables() {
        assert_eq!(sfnt_face_count(NOTO_EMOJI), Some(1));
        assert!(table(NOTO_EMOJI, 0, b"glyf").is_some(), "no outlines");
        for colour in [b"sbix", b"CBDT", b"COLR", b"SVG "] {
            assert!(
                table(NOTO_EMOJI, 0, colour).is_none(),
                "bundled emoji font carries a {} table — epaint cannot draw it",
                String::from_utf8_lossy(colour)
            );
        }
    }

    /// The point of the Menlo/Apple Symbols/STIX entries: every symbol Claude
    /// Code paints has to be reachable from *some* registered fallback, or it
    /// comes out as a tofu box. This is the test that would have caught the
    /// original bug, so it asserts coverage of the real codepoints rather than
    /// merely that the files load.
    #[test]
    #[cfg(target_os = "macos")]
    fn every_tui_symbol_is_covered_by_some_fallback() {
        let loaded: Vec<(&ScriptFallback, Vec<u8>)> = SCRIPT_FALLBACKS
            .iter()
            .filter_map(|f| Some((f, read_fallback(f)?)))
            .collect();
        assert_eq!(
            loaded.len(),
            SCRIPT_FALLBACKS.len(),
            "a fallback file is missing or unparsable on this machine"
        );

        // U+23FA is excluded: it comes from the bundled Noto Emoji on every
        // platform (see `REQUIRED_COVERAGE`), and STIX Two Math happens to
        // carry it here as well. Everything else has to come off the disk.
        for &cp in TUI_SYMBOLS.iter().filter(|&&cp| cp != 0x23FA) {
            assert!(
                loaded.iter().any(|(f, bytes)| covers(bytes, f.index, cp)),
                "U+{cp:04X} is not in any fallback — it will render as tofu"
            );
        }
    }

    /// The coverage helper is only trustworthy if it says *no* where it should:
    /// JetBrains Mono is exactly the face these symbols are missing from, and
    /// it also pins down that a plain `.ttf` has no face 1.
    #[test]
    fn cmap_coverage_distinguishes_present_from_absent() {
        assert!(covers(JETBRAINS_MONO_REGULAR, 0, 'A' as u32));
        assert!(!covers(JETBRAINS_MONO_REGULAR, 0, 0x23FA));
        assert!(!covers(JETBRAINS_MONO_REGULAR, 1, 'A' as u32));
        assert!(!covers(b"#!/bin/sh", 0, 'A' as u32));
    }

    /// Widest a centred glyph may hang out of its cell, per side, in em.
    ///
    /// Not a taste threshold: JetBrains Mono's own `?` has a 0.115 em right
    /// side bearing and its `m` 0.060 em, so anything under this cannot reach
    /// the ink of the character next to it — it only eats white space.
    #[cfg(target_os = "macos")]
    const MAX_SPILL_EM: f32 = 0.05;

    /// The grid gives every character exactly one cell and centres the glyph
    /// in it, so a proportional fallback is only safe while its advance stays
    /// within that cell. This is the test that pins the Hebrew face's `scale`:
    /// both the cell and the advances come out of the real font files, so
    /// changing the tweak, the fallback face or the `m`-based cell derivation
    /// all move the numbers it checks.
    #[test]
    #[cfg(target_os = "macos")]
    fn hebrew_letters_stay_inside_the_cell_the_grid_derives_from_jetbrains_mono() {
        let cell = cell_width_em();
        let (bytes, tweak) = registered_hebrew();

        let spill = |cp| {
            let advance = advance_em(&bytes, 0, cp).expect("Hebrew letter missing") * tweak.scale;
            // Centred, so half of any excess hangs off each side.
            (advance - cell) / 2.0
        };

        let worst = HEBREW_LETTERS
            .iter()
            .map(|&cp| spill(cp))
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            worst <= MAX_SPILL_EM,
            "a Hebrew letter hangs {worst:.3} em out of its {cell:.3} em cell"
        );

        // Shin and tav are the two that overflow by anything a pixel grid can
        // show; he and final mem exceed the cell by 0.001 em, which is why the
        // bar is a hairline rather than zero. More than two means the face or
        // the cell derivation changed and the trade-off wants re-deciding.
        let hairline = 0.005; // ≈ 0.07 px at the default 14 pt.
        let over = HEBREW_LETTERS
            .iter()
            .filter(|&&cp| spill(cp) > hairline)
            .count();
        assert!(over <= 2, "{over} Hebrew letters visibly overflow the cell");
    }

    /// Why the Hebrew face is *not* scaled down even though two letters
    /// overflow: the scale that would fit them shrinks every letter well below
    /// JetBrains Mono's x-height, which is the more visible defect of the two.
    ///
    /// Executable so the judgement is re-checked against the fonts actually
    /// installed rather than trusted from a comment.
    #[test]
    #[cfg(target_os = "macos")]
    fn hebrew_is_left_unscaled_because_shrinking_it_to_fit_costs_more_than_it_saves() {
        let cell = cell_width_em();
        let (bytes, tweak) = registered_hebrew();
        assert_eq!(tweak.scale, 1.0, "the Hebrew face grew a scale tweak");

        let widest = HEBREW_LETTERS
            .iter()
            .map(|&cp| advance_em(&bytes, 0, cp).expect("Hebrew letter missing"))
            .fold(f32::NEG_INFINITY, f32::max);
        let to_fit = cell / widest;
        assert!(to_fit < 1.0, "nothing overflows — the trade-off is gone");

        let latin = x_height_em(JETBRAINS_MONO_REGULAR, 0).expect("no JetBrains Mono x-height");
        let hebrew = x_height_em(&bytes, 0).expect("no Arial Hebrew x-height");
        assert!(
            hebrew / latin > 0.9,
            "unscaled Hebrew is already the wrong size ({hebrew:.3} vs {latin:.3} em)"
        );
        assert!(
            to_fit * hebrew < 0.85 * latin,
            "shrinking to {to_fit:.3} now costs little — reconsider scaling"
        );
    }

    /// epaint centres each fallback against the family's *leading* face
    /// instead of aligning baselines, so an untweaked Arial Hebrew floats a
    /// sixth of an em above the Latin it sits beside. See
    /// [`baseline_offset_factor`].
    ///
    /// Checked for every fallback, and read back off the registered face so it
    /// is the shipped tweak under test. The `install_script_fallbacks` call
    /// there is what proves the correction is applied at all rather than
    /// merely computable.
    #[test]
    #[cfg(target_os = "macos")]
    fn script_fallbacks_are_pulled_onto_the_jetbrains_mono_baseline() {
        let (grid_ascent, grid_row_height) =
            vertical_metrics(JETBRAINS_MONO_REGULAR, 0).expect("unreadable JetBrains Mono");

        let mut fonts = egui::FontDefinitions::default();
        install_script_fallbacks(&mut fonts);

        for f in SCRIPT_FALLBACKS {
            let bytes = read_fallback(f).expect("fallback missing");
            let (ascent, row_height) = vertical_metrics(&bytes, f.index).expect("unreadable face");
            let shift = fonts.font_data[f.name].tweak.y_offset_factor;

            // Where epaint will now put the face's baseline, shift included.
            let baseline = ascent + 0.5 * (grid_row_height - row_height) + shift;
            assert!(
                (baseline - grid_ascent).abs() < 1e-4,
                "{} lands at {baseline:.4} em, not {grid_ascent:.4}",
                f.name
            );
        }

        // Negative control: without the tweak Arial Hebrew is off by enough to
        // see (0.163 em = 2.3 pt at the default 14 pt), so a regression that
        // dropped the correction could not pass the loop above by luck.
        let hebrew = read_fallback(&SCRIPT_FALLBACKS[0]).expect("Arial Hebrew missing");
        assert!(
            baseline_offset_factor(&hebrew, 0, 1.0) > 0.1,
            "Arial Hebrew no longer needs a correction — this test proves nothing"
        );
    }

    /// The correction has to come out of each face's own metrics, not a shared
    /// fudge factor: Menlo is already within a rounding error of the grid
    /// baseline and must be left alone, while scaling a face changes what it
    /// needs (epaint multiplies the factor by `scale`).
    #[test]
    #[cfg(target_os = "macos")]
    fn the_baseline_correction_is_per_face_and_scale_aware() {
        let menlo = read_fallback(&SCRIPT_FALLBACKS[1]).expect("Menlo missing");
        assert_eq!(SCRIPT_FALLBACKS[1].name, "Menlo");
        assert!(
            baseline_offset_factor(&menlo, 0, 1.0).abs() < 0.03,
            "Menlo should barely need moving"
        );

        // Halving the size halves the misalignment, so the factor — which
        // epaint multiplies back up by `scale` — has to roughly double.
        let hebrew = read_fallback(&SCRIPT_FALLBACKS[0]).expect("Arial Hebrew missing");
        let full = baseline_offset_factor(&hebrew, 0, 1.0);
        let half = baseline_offset_factor(&hebrew, 0, 0.5);
        assert!(half > full, "the factor ignores scale ({half} vs {full})");

        // Nonsense inputs must degrade to "no shift", never to a NaN offset
        // that would send every glyph of a family off-screen.
        assert_eq!(baseline_offset_factor(&hebrew, 0, 0.0), 0.0);
        assert_eq!(baseline_offset_factor(b"#!/bin/sh", 0, 1.0), 0.0);
        assert_eq!(baseline_offset_factor(&hebrew, u32::MAX, 1.0), 0.0);
    }

    /// The UI families must exist after `install` whether or not the system
    /// font was there, so `ui.rs` can name them unconditionally.
    #[test]
    fn ui_families_are_always_registered() {
        let mut fonts = egui::FontDefinitions::default();
        install_ui_family(&mut fonts);

        for family in [UI_FAMILY, UI_MEDIUM_FAMILY] {
            let list = fonts
                .families
                .get(&egui::FontFamily::Name(family.into()))
                .unwrap_or_else(|| panic!("{family} not registered"));
            assert!(!list.is_empty(), "{family} has no faces to fall back on");
        }
    }

    /// On macOS the system face should be the *first* choice in both families,
    /// at two different weights. This is what makes the tab bar look native, so
    /// a regression here should fail loudly rather than silently fall back.
    #[test]
    #[cfg(target_os = "macos")]
    fn system_ui_face_loads_at_two_weights() {
        let mut fonts = egui::FontDefinitions::default();
        install_ui_family(&mut fonts);

        assert!(has_real_ui_medium(), "{SF_PRO_FILE} did not load");
        assert_eq!(
            fonts.families[&egui::FontFamily::Name(UI_FAMILY.into())][0],
            "SF Pro"
        );
        assert_eq!(
            fonts.families[&egui::FontFamily::Name(UI_MEDIUM_FAMILY.into())][0],
            "SF Pro Medium"
        );

        let weight = |name: &str| fonts.font_data[name].tweak.coords.as_ref()[0].1;
        assert_eq!(weight("SF Pro"), SF_WGHT_REGULAR);
        assert_eq!(weight("SF Pro Medium"), SF_WGHT_MEDIUM);
        assert!(weight("SF Pro Medium") > weight("SF Pro"));
    }

    // ------------------------------------------------------------------
    // The coverage warning
    // ------------------------------------------------------------------

    /// The warning has to be computed from the *family*, not from the table,
    /// and it has to name something the user can act on. Runs everywhere: the
    /// inputs are the bundled faces, so it needs no system font at all — which
    /// also means the Windows and Linux CI runners exercise it.
    #[test]
    fn the_coverage_warning_names_what_the_family_cannot_draw() {
        use std::sync::Arc;

        let mut fonts = egui::FontDefinitions::empty();
        fonts.font_data.insert(
            "JetBrains Mono".to_owned(),
            Arc::new(egui::FontData::from_static(JETBRAINS_MONO_REGULAR)),
        );
        fonts.families.insert(
            egui::FontFamily::Monospace,
            vec!["JetBrains Mono".to_owned()],
        );

        let missing = |fonts: &egui::FontDefinitions, what| -> Vec<u32> {
            missing_coverage(fonts, &egui::FontFamily::Monospace)
                .into_iter()
                .find(|(req, _)| req.what == what)
                .map(|(_, cps)| cps)
                .unwrap_or_default()
        };

        assert_eq!(missing(&fonts, "Hebrew"), HEBREW_LETTERS, "all 27 are gone");
        // The negative control: JetBrains Mono *does* have `❯`, so a report
        // that listed it would be reading the table rather than the family.
        let symbols = missing(&fonts, "the TUI symbol set");
        assert!(!symbols.contains(&0x276F), "{symbols:04X?}");
        assert!(symbols.contains(&0x23BF), "{symbols:04X?}");
        assert_eq!(symbols.len(), TUI_SYMBOLS.len() - 1);

        // The wording: a script the user recognises, some codepoints, what
        // goes wrong and what to do about it.
        let all = uncovered(&fonts, &egui::FontFamily::Monospace).join("\n");
        assert!(all.contains("Hebrew"), "{all}");
        assert!(all.contains("U+05D0"), "{all}");
        assert!(all.contains("empty boxes"), "{all}");
        assert!(all.contains(HEBREW_FIX), "the warning says nothing to do");
        assert!(all.contains(SYMBOL_FIX), "the warning says nothing to do");
        // 27 Hebrew letters, of which only MAX_LISTED are spelled out.
        assert!(
            all.contains(&format!("and {} more", 27 - MAX_LISTED)),
            "{all}"
        );

        // Adding a face that covers part of the gap must shrink the report:
        // the bundled emoji is where U+23FA comes from on every platform.
        install_emoji(&mut fonts);
        fonts
            .families
            .get_mut(&egui::FontFamily::Monospace)
            .expect("no monospace family")
            .push(EMOJI_FACE.to_owned());
        let symbols = missing(&fonts, "the TUI symbol set");
        assert!(
            !symbols.contains(&0x23FA),
            "the emoji face was not credited"
        );
        assert!(
            !symbols.contains(&0x2733),
            "the emoji face was not credited"
        );
        assert!(symbols.contains(&0x23BF), "{symbols:04X?}");
        assert_eq!(missing(&fonts, "Hebrew"), HEBREW_LETTERS, "still no Hebrew");

        // A family with nothing in it, and a family that does not exist at
        // all, must both degrade rather than panic during startup.
        fonts
            .families
            .insert(egui::FontFamily::Monospace, Vec::new());
        assert_eq!(
            uncovered(&fonts, &egui::FontFamily::Monospace).len(),
            REQUIRED_COVERAGE.len()
        );
        assert!(uncovered(&fonts, &egui::FontFamily::Name("nope".into())).is_empty());
    }

    /// Every codepoint the warning is about has to be one the terminal can
    /// actually be asked to draw, and the two lists must not drift apart from
    /// the tables they justify.
    #[test]
    fn the_required_coverage_lists_are_the_ones_the_tables_are_for() {
        assert_eq!(HEBREW_LETTERS.len(), 27, "the Hebrew alphabet plus finals");
        assert_eq!(*HEBREW_LETTERS.first().expect("empty"), 0x05D0);
        assert_eq!(*HEBREW_LETTERS.last().expect("empty"), 0x05EA);
        for cp in HEBREW_LETTERS.iter().chain(TUI_SYMBOLS) {
            assert!(char::from_u32(*cp).is_some(), "U+{cp:04X} is not a char");
        }
        for req in REQUIRED_COVERAGE {
            assert!(!req.codepoints.is_empty(), "{} checks nothing", req.what);
            assert!(!req.fix.is_empty(), "{} suggests nothing", req.what);
        }
    }

    // ------------------------------------------------------------------
    // macOS
    // ------------------------------------------------------------------

    /// The whole point, on the platform whose table is measured: after
    /// `install` has assembled the families, nothing terra needs is missing.
    #[test]
    #[cfg(target_os = "macos")]
    fn macos_families_can_draw_everything_required() {
        let warnings = uncovered(&assembled_families(), &egui::FontFamily::Monospace);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    // ------------------------------------------------------------------
    // Windows
    // ------------------------------------------------------------------

    /// `%SystemRoot%\Fonts` has to resolve to a real directory, or every
    /// Windows entry in the table is unreachable and the rest of these tests
    /// would "pass" by finding nothing.
    ///
    /// This is also the check that the C: assumption is gone: it asserts the
    /// directory the code will actually search, not a literal path.
    #[test]
    #[cfg(windows)]
    fn windows_resolves_the_system_font_directory_without_assuming_a_drive() {
        let system = expand(r"${SystemRoot}\Fonts").expect("%SystemRoot% is not set");
        assert!(
            std::path::Path::new(&system).is_dir(),
            "{system} is not a directory"
        );
        assert!(
            FONT_DIRS.iter().all(|dir| !dir.contains(':')),
            "a Windows font directory still hardcodes a drive letter"
        );
        // The per-user directory need not exist (it is created on first
        // per-user font install), but the variable behind it always is.
        assert!(
            std::env::var("LOCALAPPDATA").is_ok(),
            "%LOCALAPPDATA% is not set — per-user fonts are unreachable"
        );
    }

    /// The faces Microsoft documents as always installed on Windows 10 and 11
    /// must actually be found. If this fails on a real Windows machine the
    /// table is wrong, which is precisely what could not be checked before.
    #[test]
    #[cfg(windows)]
    fn windows_ships_the_faces_the_table_claims_are_always_installed() {
        let installed = installed_fallbacks();
        for name in ["Segoe UI", "Segoe UI Symbol", "Tahoma", "Arial"] {
            assert!(
                installed.contains(&name),
                "{name} is documented as shipping with Windows but was not found; \
                 searched {FONT_DIRS:?}"
            );
        }
        // Cascadia Mono is deliberately not asserted: it is delivered inside
        // the Windows Terminal package rather than into the fonts directory,
        // so its absence is expected and only its presence is a bonus.
    }

    /// Hebrew and the TUI symbols must both be reachable from the assembled
    /// families. The per-codepoint coverage of Segoe UI Symbol is the one
    /// claim in the Windows table that rests on third-party data; this is
    /// where it is finally checked against the installed files.
    #[test]
    #[cfg(windows)]
    fn windows_fallbacks_cover_the_scripts_the_grid_needs() {
        let warnings = uncovered(&assembled_families(), &egui::FontFamily::Monospace);
        assert!(warnings.is_empty(), "{warnings:?}");

        // And name the source of each, so a failure says which face to go and
        // look at rather than only that something is missing.
        let loaded: Vec<(&ScriptFallback, Vec<u8>)> = SCRIPT_FALLBACKS
            .iter()
            .filter_map(|f| Some((f, read_fallback(f)?)))
            .collect();
        for &cp in HEBREW_LETTERS {
            assert!(
                loaded.iter().any(|(f, bytes)| covers(bytes, f.index, cp)),
                "U+{cp:04X} is in no Windows fallback — Segoe UI should have it"
            );
        }
    }

    /// Same baseline correction the macOS faces get, checked against whatever
    /// this machine actually has: a Windows fallback is just as proportional
    /// as Arial Hebrew and just as badly centred without it.
    #[test]
    #[cfg(windows)]
    fn windows_script_fallbacks_are_pulled_onto_the_jetbrains_mono_baseline() {
        let (grid_ascent, grid_row_height) =
            vertical_metrics(JETBRAINS_MONO_REGULAR, 0).expect("unreadable JetBrains Mono");

        let mut fonts = egui::FontDefinitions::default();
        let installed = install_script_fallbacks(&mut fonts);
        assert!(!installed.is_empty(), "no Windows fallback loaded at all");

        for f in SCRIPT_FALLBACKS {
            let Some(bytes) = read_fallback(f) else {
                continue; // not installed here; that is the silent-skip path
            };
            let (ascent, row_height) = vertical_metrics(&bytes, f.index).expect("unreadable face");
            let shift = fonts.font_data[f.name].tweak.y_offset_factor;
            let baseline = ascent + 0.5 * (grid_row_height - row_height) + shift;
            assert!(
                (baseline - grid_ascent).abs() < 1e-4,
                "{} lands at {baseline:.4} em, not {grid_ascent:.4}",
                f.name
            );
        }
    }

    // ------------------------------------------------------------------
    // Linux
    // ------------------------------------------------------------------

    /// Nothing on Linux is guaranteed installed — a plain `ubuntu:24.04` image
    /// and the GitHub Actions runners both ship essentially no fonts — so this
    /// asserts one of two things depending on what is there:
    ///
    /// * with the Noto packages installed, the families cover everything;
    /// * without them, the *warning* covers everything: it must fire, and it
    ///   must name the packages to install rather than leaving the user with
    ///   unexplained boxes.
    ///
    /// Both branches assert; neither is a silent skip.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn linux_either_covers_the_scripts_or_says_how_to() {
        let installed = installed_fallbacks();
        let warnings = uncovered(&assembled_families(), &egui::FontFamily::Monospace);

        // `fonts-noto-core` (Debian/Ubuntu), `noto-fonts` (Arch) or the three
        // `google-noto-*` packages (Fedora) supply exactly these.
        let noto = [
            "Noto Sans Hebrew",
            "Noto Sans Symbols",
            "Noto Sans Symbols 2",
        ];
        if noto.iter().all(|n| installed.contains(n)) {
            assert!(
                warnings.is_empty(),
                "the Noto faces are installed but something is still uncovered: {warnings:?}"
            );
            return;
        }

        if warnings.is_empty() {
            // Some other combination (DejaVu plus Unifont, say) covered it.
            // Nothing to complain about, but prove it was really checked.
            assert!(
                !installed.is_empty(),
                "nothing is installed yet nothing is missing — the check is inert"
            );
            return;
        }

        let all = warnings.join("\n");
        for package in ["fonts-noto-core", "google-noto", "noto-fonts"] {
            assert!(
                all.contains(package),
                "the warning does not tell the user to install {package}: {all}"
            );
        }
        assert!(
            all.contains("empty boxes"),
            "the warning does not say what will go wrong: {all}"
        );
    }

    /// Whatever *is* installed has to be usable: parsed at the claimed face
    /// index and pulled onto the grid baseline. On a machine with no fonts
    /// this asserts nothing about faces, which is why the coverage test above
    /// asserts about the warning instead.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn linux_script_fallbacks_are_pulled_onto_the_jetbrains_mono_baseline() {
        let (grid_ascent, grid_row_height) =
            vertical_metrics(JETBRAINS_MONO_REGULAR, 0).expect("unreadable JetBrains Mono");

        let mut fonts = egui::FontDefinitions::default();
        install_script_fallbacks(&mut fonts);

        for f in SCRIPT_FALLBACKS {
            let Some(bytes) = read_fallback(f) else {
                continue; // not installed on this distribution
            };
            let (ascent, row_height) = vertical_metrics(&bytes, f.index).expect("unreadable face");
            let shift = fonts.font_data[f.name].tweak.y_offset_factor;
            let baseline = ascent + 0.5 * (grid_row_height - row_height) + shift;
            assert!(
                (baseline - grid_ascent).abs() < 1e-4,
                "{} lands at {baseline:.4} em, not {grid_ascent:.4}",
                f.name
            );
        }
    }

    /// The Hebrew face must never be one with no Hebrew in it. DejaVu Sans
    /// Mono is in the table for its dingbats and has **zero** Hebrew
    /// codepoints, so if it ever moved ahead of the Hebrew faces the script
    /// would keep rendering — as tofu from a face that claimed nothing.
    ///
    /// Checked as an ordering property of the table, so it holds on a runner
    /// with no fonts installed too.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn linux_leads_with_a_face_that_actually_has_hebrew() {
        let at = |name| SCRIPT_FALLBACKS.iter().position(|f| f.name == name);
        assert!(
            at("Noto Sans Hebrew") < at("DejaVu Sans Mono"),
            "the Hebrew face must precede the monospace symbol face"
        );
        assert!(
            at("Noto Sans Hebrew") < at("DejaVu Sans"),
            "the dedicated Hebrew face must win over the generalist"
        );
        assert_eq!(
            SCRIPT_FALLBACKS.last().map(|f| f.name),
            Some("Unifont"),
            "Unifont is the last resort and must stay last"
        );
    }
}
