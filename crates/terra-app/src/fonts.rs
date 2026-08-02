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

/// macOS ships the system UI face as a single *variable* TrueType file (not a
/// `.ttc` collection), so it needs no face index — just variation coordinates.
#[cfg(target_os = "macos")]
const SF_PRO_PATH: &str = "/System/Library/Fonts/SFNS.ttf";

/// Candidate paths for the system UI face, best first; the first that exists
/// and parses wins. See [`install_ui_family`].
///
/// One entry on macOS (the path is fixed and the face is variable), several
/// elsewhere: Windows 11 ships a variable Segoe UI but Windows 10 only the
/// static one, and on Linux there is no single system UI font at all.
#[cfg(target_os = "macos")]
const UI_FONT_PATHS: &[&str] = &[SF_PRO_PATH];

#[cfg(windows)]
const UI_FONT_PATHS: &[&str] = &[
    // Windows 11's variable Segoe UI — the only one of these with a `wght`
    // axis, and therefore the only one that yields a real medium weight.
    r"C:\Windows\Fonts\SegUIVar.ttf",
    r"C:\Windows\Fonts\segoeui.ttf",
    r"C:\Windows\Fonts\arial.ttf",
];

#[cfg(all(unix, not(target_os = "macos")))]
const UI_FONT_PATHS: &[&str] = &[
    // Fontconfig would be the right answer here; a path list is the honest
    // approximation until this crate takes that dependency. Distro layouts
    // differ, so each face is listed everywhere it is commonly installed.
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/liberation-sans/LiberationSans-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/cantarell/Cantarell-VF.otf",
];

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
    /// Where the face may live, best first; [`read_fallback`] takes the first
    /// that exists and parses. macOS has one fixed path per entry; Linux
    /// genuinely needs the list, because the same font sits under
    /// `truetype/dejavu`, `TTF` or `dejavu` depending on the distribution.
    paths: &'static [&'static str],
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
/// Every platform has its own table below; the machinery around them
/// ([`read_fallback`], [`baseline_offset_factor`], [`install_script_fallbacks`])
/// is shared and unchanged, including the silent skip when a file is missing.
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
        paths: &["/System/Library/Fonts/ArialHB.ttc"],
        index: 0,
    },
    // Face 0 of the collection is Menlo-Regular (0/1/2/3 are Regular, Bold,
    // Italic, Bold Italic); the italics don't even carry the dingbats.
    ScriptFallback {
        name: "Menlo",
        paths: &["/System/Library/Fonts/Menlo.ttc"],
        index: 0,
    },
    ScriptFallback {
        name: "Apple Symbols",
        paths: &["/System/Library/Fonts/Apple Symbols.ttf"],
        index: 0,
    },
    // Note the file name: macOS ships this as `STIXTwoMath.otf`, without the
    // `-Regular` suffix the family's other members use.
    ScriptFallback {
        name: "STIX Two Math",
        paths: &["/System/Library/Fonts/Supplemental/STIXTwoMath.otf"],
        index: 0,
    },
];

/// Linux fallbacks. Same job as the macOS table, chosen from what distributions
/// actually ship rather than from one fixed system font set.
///
/// There is no `/System/Library/Fonts` here and no guarantee any given face is
/// installed, so each entry lists several paths and [`read_fallback`] takes the
/// first that exists — Debian/Ubuntu put fonts under
/// `/usr/share/fonts/truetype/<pkg>/`, Arch under `/usr/share/fonts/TTF/`,
/// Fedora under `/usr/share/fonts/<pkg>/`. Every entry may legitimately be
/// absent, which is the pre-existing silent-skip path, not a new failure mode.
///
/// Ordering mirrors the macOS one: the script face first, then the broad
/// symbol faces.
///
/// * **Noto Sans Hebrew** is the Hebrew equivalent of Arial Hebrew and is what
///   fontconfig's own cascade picks on a modern desktop.
/// * **DejaVu Sans** is the closest thing Linux has to a universal fallback —
///   it also carries Hebrew, so it doubles as the Hebrew backstop on a machine
///   with no Noto, plus a good deal of the dingbat/geometric-shape range
///   (`✳ ✻ ✽ ◒ ☒`).
/// * **Noto Sans Symbols 2** is where the technical symbols live (`⎿` U+23BF,
///   `⏺` U+23FA) — DejaVu has neither.
/// * **Unifont** is the last resort: a bitmap-derived outline face with
///   near-complete BMP coverage, ugly but never tofu. Last in the list, so it
///   only ever serves codepoints nothing else has.
#[cfg(all(unix, not(target_os = "macos")))]
const SCRIPT_FALLBACKS: &[ScriptFallback] = &[
    ScriptFallback {
        name: "Noto Sans Hebrew",
        paths: &[
            "/usr/share/fonts/truetype/noto/NotoSansHebrew-Regular.ttf",
            "/usr/share/fonts/noto/NotoSansHebrew-Regular.ttf",
            "/usr/share/fonts/TTF/NotoSansHebrew-Regular.ttf",
            "/usr/share/fonts/google-noto/NotoSansHebrew-Regular.ttf",
        ],
        index: 0,
    },
    ScriptFallback {
        name: "DejaVu Sans",
        paths: &[
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
        ],
        index: 0,
    },
    ScriptFallback {
        name: "Noto Sans Symbols 2",
        paths: &[
            "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
            "/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf",
            "/usr/share/fonts/TTF/NotoSansSymbols2-Regular.ttf",
            "/usr/share/fonts/google-noto/NotoSansSymbols2-Regular.ttf",
        ],
        index: 0,
    },
    ScriptFallback {
        name: "Unifont",
        paths: &[
            "/usr/share/fonts/truetype/unifont/unifont.ttf",
            "/usr/share/fonts/misc/unifont.ttf",
            "/usr/share/fonts/unifont/unifont.ttf",
        ],
        index: 0,
    },
];

/// Windows fallbacks, from the set that ships with the OS.
///
/// Paths are literal rather than resolved through `%SystemRoot%`: the fonts
/// directory has been `C:\Windows\Fonts` since NT, and a machine that moved it
/// simply gets the existing silent skip. (Worth revisiting together with
/// per-user fonts under `%LOCALAPPDATA%\Microsoft\Windows\Fonts`, which this
/// does not look at either.)
///
/// * **Segoe UI** is the system UI face and covers Hebrew.
/// * **Segoe UI Symbol** is Windows' own technical-symbol face — the
///   counterpart to Apple Symbols, and where `⎿`/`⏺` come from.
/// * **Cascadia Mono** ships with Windows Terminal and, being monospaced,
///   draws the dingbats at the cell width the way Menlo does on macOS.
/// * **Arial** is the backstop.
///
/// Unverified on hardware: this table is chosen from documented Windows font
/// coverage, not measured the way the macOS one was (see the coverage tests,
/// which only run on macOS because they read the installed files).
#[cfg(windows)]
const SCRIPT_FALLBACKS: &[ScriptFallback] = &[
    ScriptFallback {
        name: "Segoe UI",
        paths: &[r"C:\Windows\Fonts\segoeui.ttf"],
        index: 0,
    },
    ScriptFallback {
        name: "Segoe UI Symbol",
        paths: &[r"C:\Windows\Fonts\seguisym.ttf"],
        index: 0,
    },
    ScriptFallback {
        name: "Cascadia Mono",
        paths: &[
            r"C:\Windows\Fonts\CascadiaMono.ttf",
            r"C:\Windows\Fonts\CascadiaCode.ttf",
        ],
        index: 0,
    },
    ScriptFallback {
        name: "Arial",
        paths: &[r"C:\Windows\Fonts\arial.ttf"],
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

/// Read the first system UI font in [`UI_FONT_PATHS`] that exists and carries
/// font magic. Any failure is silent: the caller falls back to the default
/// font.
///
/// The parse check is [`sfnt_face_count`] rather than "does it have a `wght`
/// axis", which is what it used to be. On macOS that is the same answer — SFNS
/// is variable — but elsewhere most system UI faces are static, and rejecting
/// them would mean *no* UI font at all rather than one without a real medium
/// weight. Which of the two we got is [`has_weight_axis`]'s job.
fn read_system_ui_font() -> Option<Vec<u8>> {
    UI_FONT_PATHS.iter().find_map(|path| {
        let bytes = std::fs::read(path).ok()?;
        sfnt_face_count(&bytes).map(|_| bytes)
    })
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
/// ask for. Any failure is silent — the family just keeps what it had, which
/// is the same tofu as before rather than a crash on startup.
///
/// [`ScriptFallback::paths`] is tried in order and the first file that passes
/// *both* checks wins; a path that exists but is not a font (or is a
/// collection without our face index) is skipped rather than ending the search,
/// so one bad file cannot mask a good one further down the list.
fn read_fallback(f: &ScriptFallback) -> Option<Vec<u8>> {
    f.paths.iter().find_map(|path| {
        let bytes = std::fs::read(path).ok()?;
        (f.index < sfnt_face_count(&bytes)?).then_some(bytes)
    })
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
    /// is the only format that reaches past the BMP, but macOS system fonts
    /// use it for BMP codepoints too, so it has to be checked either way.
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
    fn covers(bytes: &[u8], face: u32, cp: u32) -> bool {
        glyph_of(bytes, face, cp).is_some()
    }

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

    /// Every Hebrew letter, including the five final forms — the whole set the
    /// grid has to place, not a sample of it.
    #[cfg(target_os = "macos")]
    const HEBREW_LETTERS: std::ops::RangeInclusive<u32> = 0x05D0..=0x05EA;

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

    /// The symbols TUIs paint that no monospace face on this machine covers —
    /// the tofu this table exists to kill. See [`SCRIPT_FALLBACKS`].
    #[cfg(target_os = "macos")]
    const TUI_SYMBOLS: &[(char, u32)] = &[
        ('✳', 0x2733),
        ('✻', 0x273B),
        ('✽', 0x273D),
        ('◒', 0x25D2),
        ('⚒', 0x2692),
        ('☒', 0x2612),
        ('⎿', 0x23BF),
        ('⏺', 0x23FA),
    ];

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
    /// *normal* case is that most paths in an entry do not exist.
    #[test]
    fn missing_fallback_file_is_skipped() {
        assert!(read_fallback(&ScriptFallback {
            name: "nope",
            paths: &["/definitely/not/a/font.ttc"],
            index: 0,
        })
        .is_none());
        // Every path missing is still just "not available", never a panic.
        assert!(read_fallback(&ScriptFallback {
            name: "nope",
            paths: &["/nope/one.ttf", "/nope/two.ttf"],
            index: 0,
        })
        .is_none());
        assert!(read_fallback(&ScriptFallback {
            name: "nope",
            paths: &[],
            index: 0,
        })
        .is_none());
    }

    /// The point of the path list: a face is found wherever the distribution
    /// happens to put it, and entries ahead of it that are absent — or present
    /// but not fonts — do not stop the search.
    #[test]
    fn a_fallback_takes_the_first_readable_path_and_skips_the_rest() {
        let dir = std::env::temp_dir().join("terra-font-fallback-probe");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let decoy = dir.join("not-a-font.ttf");
        let real = dir.join("real.ttf");
        std::fs::write(&decoy, b"#!/bin/sh\n").expect("write decoy");
        std::fs::write(&real, JETBRAINS_MONO_REGULAR).expect("write font");

        let paths: Vec<&str> = vec![
            "/definitely/not/a/font.ttc",
            decoy.to_str().unwrap(),
            real.to_str().unwrap(),
        ];
        // `paths` is `&'static` in the table; leaking here is a test-only way
        // to build one from runtime paths.
        let paths: &'static [&'static str] = Box::leak(
            paths
                .into_iter()
                .map(|p| &*Box::leak(p.to_owned().into_boxed_str()))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );

        let found = read_fallback(&ScriptFallback {
            name: "probe",
            paths,
            index: 0,
        })
        .expect("the readable font should have been found");
        assert_eq!(found, JETBRAINS_MONO_REGULAR);

        let _ = std::fs::remove_dir_all(&dir);
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

        for &(glyph, cp) in TUI_SYMBOLS {
            assert!(
                loaded.iter().any(|(f, bytes)| covers(bytes, f.index, cp)),
                "{glyph} (U+{cp:04X}) is not in any fallback — it will render as tofu"
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

        let worst = HEBREW_LETTERS.map(&spill).fold(f32::NEG_INFINITY, f32::max);
        assert!(
            worst <= MAX_SPILL_EM,
            "a Hebrew letter hangs {worst:.3} em out of its {cell:.3} em cell"
        );

        // Shin and tav are the two that overflow by anything a pixel grid can
        // show; he and final mem exceed the cell by 0.001 em, which is why the
        // bar is a hairline rather than zero. More than two means the face or
        // the cell derivation changed and the trade-off wants re-deciding.
        let hairline = 0.005; // ≈ 0.07 px at the default 14 pt.
        let over = HEBREW_LETTERS.filter(|&cp| spill(cp) > hairline).count();
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
            .map(|cp| advance_em(&bytes, 0, cp).expect("Hebrew letter missing"))
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

        assert!(has_real_ui_medium(), "{SF_PRO_PATH} did not load");
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
}
