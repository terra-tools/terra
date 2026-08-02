//! terra patch: UAX #9 BiDi Level 1 — per-row visual reordering.
//!
//! A terminal grid stores cells in *logical* order (the order bytes arrived
//! in) and paints column `n` at `x = n * cell_width`. For right-to-left
//! scripts that renders every word mirrored. This module computes, for one
//! row, the permutation that turns logical columns into visual ones, per the
//! [Unicode Bidirectional Algorithm](https://unicode.org/reports/tr9/).
//!
//! Scope is deliberately **Level 1** of the
//! [terminal BiDi spec](https://terminal-wg.pages.freedesktop.org/bidi/):
//! rules L1 (whitespace reset), L2 (reordering) and L4 (mirroring). No Arabic
//! joining/shaping — see the module's limitations note in `PATCHES.md`.
//!
//! # Why the algorithm runs over the row's non-blank prefix
//!
//! The base direction is selectable ([`BidiBase`]), and the interesting mode
//! is [`BidiBase::Auto`], which lets UAX #9's P2/P3 derive the direction from
//! the row's first strong character. Autodetection is what a pure-RTL line
//! needs: rule N2 resolves a trailing neutral that is followed only by
//! whitespace to the *paragraph* level, so under a forced LTR base the `?`
//! of `היי מה קורה?` resolves to level 0, refuses to join the reversed run,
//! and is stranded on the visual right — the opposite end from where the
//! sentence ends.
//!
//! Naive autodetection, though, is exactly the thing that breaks a terminal,
//! and for one specific reason: **a row is not a paragraph, it is a
//! fixed-width window into an addressable cell array**, padded out to the
//! full column count with blanks. Hand the padding to the algorithm under an
//! RTL base and rule L1's whitespace reset no longer saves it — the blanks
//! belong to the paragraph, they take the paragraph level, they join the
//! reversed run, and a four-letter Hebrew word on an 80-column row renders at
//! column 76 instead of column 0. Applications position by column (`ESC [ n
//! G`, ncurses, tmux borders); a row whose visual origin drifts with its
//! padding makes column addressing meaningless.
//!
//! So [`map_row`] runs the algorithm over the row's **non-blank prefix
//! only** — everything up to and including the last non-`' '` cell — and
//! extends the resulting maps with identity entries, level 0, for the
//! padding. The padding is then inert by construction rather than by
//! whatever L1 happens to do, and the content is laid out as if the row ended
//! where its text ends. That is what makes `Auto` safe enough to be the
//! default: it defends against the failure mode that made forcing LTR
//! attractive in the first place.
//!
//! The residual instability is inherent to autodetection and cannot be
//! trimmed away: the same row is re-evaluated on every keystroke, so a row
//! that begins with a Hebrew letter is an RTL paragraph and a row that begins
//! with `a` is not, and typing at the front of a line can flip it. Rows that
//! start with a shell prompt are unaffected, since P2 stops at the prompt's
//! first strong Latin character. [`BidiBase::Ltr`] remains available for
//! callers that want the old, wholly content-independent behaviour, and
//! applications that genuinely want an RTL paragraph can always say so
//! explicitly with U+200F RLM / U+202B RLE / U+2067 RLI, which are honoured
//! under every mode.
//!
//! Everything here is pure: no egui, no terminal, no I/O. [`map_row`] is a
//! total function of its input and every accessor clamps rather than panics,
//! so a map left stale by a resize degrades to a slightly wrong layout rather
//! than a crash inside the paint loop.

use unicode_bidi::{BidiClass, Level, ParagraphBidiInfo};

/// Which paragraph direction [`map_row`] resolves a row against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BidiBase {
    /// Force LTR. Stable for prompts and TUIs; strands RTL sentence
    /// punctuation on the wrong side.
    Ltr,
    /// Detect per row with UAX #9 rules P2/P3, over the row's non-blank
    /// prefix only.
    #[default]
    Auto,
    /// Force RTL.
    Rtl,
}

/// The visual order of one terminal row, plus its per-column directionality.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowMap {
    /// Nothing in the row can perturb the order: visual == logical.
    /// Carries no allocation, which matters because this is the case for
    /// essentially every row of ordinary terminal output.
    Identity(usize),
    Reordered {
        /// `v2l[visual] = logical`. Authoritative — `l2v` is its inverse.
        v2l: Vec<u16>,
        /// `l2v[logical] = visual`.
        l2v: Vec<u16>,
        /// Indexed by *logical* column: is this column's resolved level odd?
        rtl: Vec<bool>,
    },
}

impl RowMap {
    pub fn len(&self) -> usize {
        match self {
            Self::Identity(n) => *n,
            Self::Reordered { v2l, .. } => v2l.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// True when the row needs no reordering at all.
    pub fn is_identity(&self) -> bool {
        matches!(self, Self::Identity(_))
    }

    /// Logical column -> visual column. Out of range maps to itself.
    pub fn visual_of(&self, logical: usize) -> usize {
        match self {
            Self::Identity(_) => logical,
            Self::Reordered { l2v, .. } => {
                l2v.get(logical).map_or(logical, |v| *v as usize)
            }
        }
    }

    /// Visual column -> logical column. Out of range maps to itself.
    pub fn logical_of(&self, visual: usize) -> usize {
        match self {
            Self::Identity(_) => visual,
            Self::Reordered { v2l, .. } => {
                v2l.get(visual).map_or(visual, |l| *l as usize)
            }
        }
    }

    /// Whether the resolved embedding level of `logical` is odd, i.e. the
    /// character reads right-to-left.
    pub fn is_rtl(&self, logical: usize) -> bool {
        match self {
            Self::Identity(_) => false,
            Self::Reordered { rtl, .. } => {
                rtl.get(logical).copied().unwrap_or(false)
            }
        }
    }

    /// Leftmost visual column of the `width`-column atomic unit starting at
    /// logical column `logical`.
    ///
    /// A double-width character and its spacer are one unit: they always
    /// resolve to the same embedding level (see [`map_row`]'s contract), so
    /// their visual columns are adjacent and the leftmost is the minimum.
    /// Painting at `visual_of(logical)` instead would put the glyph one cell
    /// too far right inside an RTL run, where L2 has swapped the pair.
    pub fn visual_span_start(&self, logical: usize, width: usize) -> usize {
        (0..width.max(1))
            .map(|k| self.visual_of(logical + k))
            .min()
            .unwrap_or(logical)
    }

    /// The glyph to paint for logical column `logical`, applying rule L4.
    ///
    /// Mirroring is a *rendering-time* substitution: the cell, the clipboard
    /// and any URL match keep the original character.
    pub fn display_char(&self, logical: usize, c: char) -> char {
        if self.is_rtl(logical) {
            mirror(c)
        } else {
            c
        }
    }
}

/// UAX #9 rule L4: the mirrored form of a Bidi_Mirrored character.
///
/// Restricted to the pairs a terminal actually emits. `unicode-bidi` does not
/// implement L4 (it documents the omission), and pulling in the full
/// Bidi_Mirroring_Glyph table would be a whole extra dataset for mathematical
/// operators and CJK brackets that will essentially never appear here.
fn mirror(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '«' => '»',
        '»' => '«',
        '‹' => '›',
        '›' => '‹',
        _ => c,
    }
}

/// Whether `c` could possibly perturb the visual order under an LTR-resolved
/// base — the fast-path test.
///
/// Tier 1 is a bare range compare. Every RTL-capable script and every
/// explicit directional formatting character in Unicode sits at or above
/// U+0590 (the start of the Hebrew block), so nothing below it can matter.
/// That single comparison rejects all of ASCII, Latin-1, Greek and Cyrillic,
/// which is the overwhelming majority of terminal output.
///
/// Tier 2 only runs for the few characters that clear the range check — most
/// often CJK, which is class `L` and must *not* be allowed to force the slow
/// path — and costs one table lookup.
fn perturbs_order(c: char) -> bool {
    if (c as u32) < 0x0590 {
        return false;
    }
    matches!(
        unicode_bidi::bidi_class(c),
        BidiClass::R
            | BidiClass::AL
            | BidiClass::AN
            | BidiClass::RLE
            | BidiClass::RLO
            | BidiClass::RLI
            | BidiClass::LRE
            | BidiClass::LRO
            | BidiClass::LRI
            | BidiClass::FSI
            | BidiClass::PDF
            | BidiClass::PDI
    )
}

/// How many characters at the start of `chars` carry no direction of their
/// own — the run before the first strong left-to-right or right-to-left
/// letter.
///
/// This is a terminal's stand-in for the isolate a well-behaved application
/// would emit around its text; see [`map_row`] for why it exists.
fn chrome_prefix(chars: &[char]) -> usize {
    chars
        .iter()
        .position(|&c| {
            matches!(
                unicode_bidi::bidi_class(c),
                // Strong characters are content by definition.
                BidiClass::L | BidiClass::R | BidiClass::AL
                    // So are digits, even though they are not strong. A
                    // leading `03:34`, `2026-08-02` or `[12:04]` is the
                    // line's subject, not decoration around it, and
                    // stopping only at strong characters swallowed the
                    // whole run — pinning a timestamp to the left margin
                    // of an otherwise right-to-left line, with the
                    // sentence-final period landing against it.
                    | BidiClass::EN
                    | BidiClass::AN
            )
        })
        .unwrap_or(chars.len())
}

/// Compute the visual order of one row.
///
/// `chars` holds exactly one character per *column*, trailing blanks
/// included, so a char index is a logical column with no offset arithmetic.
/// A double-width character must be pushed **twice** — once for each column
/// it occupies — which guarantees both columns share a `BidiClass` and
/// therefore an embedding level, keeping the pair atomic under L2.
///
/// Only the row's non-blank prefix is handed to the algorithm; the trailing
/// padding is appended to the maps as identity, level 0. See the module doc
/// for why.
pub fn map_row(chars: &[char], base: BidiBase) -> RowMap {
    let content_len = chars
        .iter()
        .rposition(|&c| c != ' ')
        .map_or(0, |last| last + 1);

    // A row with nothing but padding has no content to order, under any base.
    if content_len == 0 {
        return RowMap::Identity(chars.len());
    }
    let content = &chars[..content_len];

    // Under `Auto`, hold the row's leading chrome out of the paragraph.
    //
    // A TUI prefixes lines with characters that carry no direction of their
    // own — `⏺` bullets, `❯` prompts, `│` and `⎿` box drawing, indentation.
    // Rule P2 skips straight past them to the first strong character, so a
    // line of Hebrew turns the whole row into an RTL paragraph and rule L2
    // sweeps that chrome to the right margin. It is correct for a paragraph
    // and wrong for a terminal, where those glyphs are structure, not text:
    // a bulleted list would have its bullets flip side depending on the
    // language of each item.
    //
    // The Unicode answer is for the application to wrap its text in an
    // isolate (U+2068 FSI). Terminal applications do not, so terra does it
    // for them: detection runs on the text *after* the chrome, and the
    // chrome itself stays pinned at level 0 on the left.
    //
    // This costs nothing when there is no chrome (`heb: שלום` starts with a
    // strong `L`, so the prefix is empty) and nothing under an explicit base.
    let chrome_len = match base {
        BidiBase::Auto => chrome_prefix(content),
        BidiBase::Ltr | BidiBase::Rtl => 0,
    };
    let content = &content[chrome_len..];

    // The fast path asserts that a row of purely left-to-right characters
    // lays out as itself. That holds when the base resolves to LTR — `Ltr` by
    // construction, `Auto` because P2 can only find a strong `L` here — but
    // not under `Rtl`, where even an all-ASCII row can be reordered: rule N2
    // resolves the trailing `.` of `total 48.` to the paragraph level and L2
    // moves it to the visual left.
    if base != BidiBase::Rtl && !content.iter().copied().any(perturbs_order) {
        return RowMap::Identity(chars.len());
    }

    let text: String = content.iter().collect();
    let level = match base {
        BidiBase::Ltr => Some(Level::ltr()),
        // `None` is what asks `unicode-bidi` to apply P2/P3 itself.
        BidiBase::Auto => None,
        BidiBase::Rtl => Some(Level::rtl()),
    };
    // `ParagraphBidiInfo` treats the input as a single paragraph. `BidiInfo`
    // would split on paragraph separators, which a terminal row must never
    // do — a row is one paragraph by definition.
    let info = ParagraphBidiInfo::new(&text, level);
    // The `levels` field is indexed by *byte*; this is the only accessor that
    // yields one level per char, and it applies rule L1 (trailing-whitespace
    // reset) on the way out.
    let levels = info.reordered_levels_per_char(0..text.len());

    // `reorder_visual` is rule L2, and returns `index_map[visual] = logical`.
    // The chrome sits ahead of the reordered run, mapping to itself at
    // level 0, so `reorder_visual`'s indices need shifting past it.
    let mut v2l: Vec<u16> = (0..chrome_len as u16).collect();
    v2l.extend(
        ParagraphBidiInfo::reorder_visual(&levels)
            .into_iter()
            .map(|l| (l + chrome_len) as u16),
    );
    let mut rtl: Vec<bool> = vec![false; chrome_len];
    rtl.extend(levels.iter().map(Level::is_rtl));

    // The padding never entered the algorithm, so it maps to itself and sits
    // at level 0. Appending it here — after L2 rather than before it — is
    // what keeps an RTL row anchored at column 0 instead of the right margin.
    for column in content_len..chars.len() {
        v2l.push(column as u16);
        rtl.push(false);
    }

    // Derived by inversion, never computed separately: two independent
    // permutations would be free to disagree, and the disagreement would only
    // show up as mouse hits landing one cell off on RTL rows.
    let mut l2v = vec![0u16; v2l.len()];
    for (visual, &logical) in v2l.iter().enumerate() {
        l2v[logical as usize] = visual as u16;
    }

    RowMap::Reordered { v2l, l2v, rtl }
}

/// Which side of a visual column boundary the cursor beam marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeamSide {
    /// The boundary is the left edge of the cell the beam belongs to, so the
    /// beam grows rightwards from it — the ordinary left-to-right beam.
    Left,
    /// The boundary is the right edge, so the painter pulls the beam back by
    /// its own width to keep it inside the cell it marks.
    Right,
}

/// Where the cursor beam goes on one row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Beam {
    /// Distance from the row's left edge, in cell widths.
    pub offset: f32,
    /// Which edge `offset` names.
    pub side: BeamSide,
}

/// Decide where to draw the insertion-point beam for a cursor parked on
/// logical column `logical_col`.
///
/// Pure and total: out-of-range columns, a stale map and `None` (BiDi off)
/// all degrade to the left-to-right answer rather than panicking.
///
/// * `map` — the row's visual order, or `None` when BiDi is off.
/// * `logical_col` — the **first** column of the cursor's cell-unit. A cursor
///   parked on a double-width character's spacer must be resolved back to the
///   character itself by the caller, so that the unit is addressed as a whole.
/// * `is_wide` — the unit is a double-width character, i.e. two columns.
/// * `content_end` — one past the row's last occupied column, counting a
///   double-width character's spacer as occupied. This is [`map_row`]'s own
///   notion of where the row's content stops, and it is what separates "the
///   cursor is sitting on a blank *inside* the line" from "the cursor is past
///   the end of the line".
///
/// Three cases; see `PATCHES.md` for the prose version.
///
/// 1. The cursor's own cell reads right-to-left. Text grows leftwards, so the
///    insertion point is the unit's visual **right** edge.
/// 2. The cursor is past the row's content and the row ends in a right-to-left
///    run. The next character joins that run, and a run grows from its visual
///    **left** end — which is the run's leftmost visual column, *not*
///    necessarily the cell holding the last character. This is the case that
///    fires on every keystroke while typing Hebrew or Arabic.
/// 3. Anything else — the cursor's own visual left edge, which is what every
///    terminal does and exactly what this returns when `map` is `None`.
pub fn beam_position(
    map: Option<&RowMap>,
    logical_col: usize,
    is_wide: bool,
    content_end: usize,
) -> Beam {
    let width = if is_wide { 2 } else { 1 };
    let Some(map) = map else {
        return Beam {
            offset: logical_col as f32,
            side: BeamSide::Left,
        };
    };

    // 1. Inside a right-to-left run: insert to the unit's right.
    if map.is_rtl(logical_col) {
        return Beam {
            offset: (map.visual_span_start(logical_col, width) + width) as f32,
            side: BeamSide::Right,
        };
    }

    // 2. Past the end of a row that ends in a right-to-left run.
    //
    // Judging by the cursor's own cell is what puts the beam in the wrong
    // place here: the cell is blank padding, which rule L1 resets to the base
    // level, so it claims to be left-to-right and the beam lands to the right
    // of the word the user is typing. Judging by the single cell logically
    // before the cursor is not enough either — that is the run's leftmost
    // column only when the run is one plain reversal. Take the minimum over
    // the whole trailing run instead, which is right by construction whatever
    // L2 did inside it (nested digits, a swapped double-width pair) and
    // whatever blanks separate the cursor from the text.
    if logical_col >= content_end
        && content_end > 0
        && map.is_rtl(content_end - 1)
    {
        let end = content_end - 1;
        let mut start = end;
        while start > 0 && map.is_rtl(start - 1) {
            start -= 1;
        }
        let offset = (start..=end)
            .map(|logical| map.visual_of(logical))
            .min()
            .unwrap_or(end);
        return Beam {
            offset: offset as f32,
            side: BeamSide::Left,
        };
    }

    // 3. The cell's own left edge.
    Beam {
        offset: map.visual_span_start(logical_col, width) as f32,
        side: BeamSide::Left,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Render a string the way the draw loop would: walk visual columns, pull
    /// the logical cell that belongs there, and apply L4.
    fn visual(s: &str, base: BidiBase) -> String {
        let chars: Vec<char> = s.chars().collect();
        let map = map_row(&chars, base);
        (0..chars.len())
            .map(|v| {
                let l = map.logical_of(v);
                map.display_char(l, chars[l])
            })
            .collect()
    }

    fn map_of(s: &str, base: BidiBase) -> RowMap {
        map_row(&s.chars().collect::<Vec<_>>(), base)
    }

    #[test]
    fn a_pure_ascii_row_takes_the_identity_path() {
        let map = map_of("total 48 drwxr-xr-x", BidiBase::Ltr);
        assert!(map.is_identity(), "ASCII must not allocate a permutation");
        assert_eq!(map.len(), 19);
        assert_eq!(map.visual_of(7), 7);
        assert_eq!(map.logical_of(7), 7);
        assert!(!map.is_rtl(7));
    }

    /// CJK clears the U+0590 range check but is Bidi class `L`, so the second
    /// tier has to catch it — otherwise every CJK row would pay for a full
    /// BiDi pass that cannot change anything.
    #[test]
    fn a_cjk_row_takes_the_identity_path_because_han_is_bidi_class_l() {
        assert!(map_of("日本語のテスト", BidiBase::Ltr).is_identity());
        assert!(map_of("日本語のテスト", BidiBase::Auto).is_identity());
        assert!(map_of("▛▀▜ box drawing ▙▄▟", BidiBase::Ltr).is_identity());
    }

    #[test]
    fn a_mixed_latin_hebrew_row_reverses_only_the_hebrew_run() {
        let s = "heb: שלום עולם | abc 123";
        assert_eq!(visual(s, BidiBase::Ltr), "heb: םלוע םולש | abc 123");

        let map = map_of(s, BidiBase::Ltr);
        // The Latin prefix and everything past the run stay put.
        for i in [0, 1, 2, 3, 4, 14, 20, 23] {
            assert_eq!(map.visual_of(i), i, "column {i} should not move");
        }
        // The run 5..=13 reverses onto itself.
        assert_eq!(map.visual_of(5), 13);
        assert_eq!(map.visual_of(13), 5);
        // A single contiguous reversal is an involution.
        for i in 0..s.chars().count() {
            assert_eq!(map.logical_of(i), map.visual_of(i));
        }
        // The *interior* space (logical 9) is swept into the reversed run by
        // rule N1; the spaces bounding the run are not. That asymmetry is the
        // whole point of running a real BiDi implementation instead of
        // reversing spans by hand.
        for i in 5..=13 {
            assert!(map.is_rtl(i), "column {i} should be RTL");
        }
        assert!(!map.is_rtl(4));
        assert!(!map.is_rtl(14));
    }

    /// The trailing blanks of a padded row must never join a reversed run,
    /// whatever the base. Under a naive autodetecting implementation — one
    /// that hands the padding to the algorithm — `v2l` here would be a full
    /// reversal, `[9,8,..,0]`, and the word would render at the right margin.
    #[test]
    fn trailing_blanks_after_an_rtl_run_do_not_push_the_text_right() {
        for base in [BidiBase::Ltr, BidiBase::Auto, BidiBase::Rtl] {
            let map = map_of("שלום      ", base);
            assert_eq!(visual("שלום      ", base), "םולש      ");
            for (logical, expected) in [(0, 3), (1, 2), (2, 1), (3, 0)] {
                assert_eq!(map.visual_of(logical), expected, "{base:?}");
            }
            // Stated as the margin question it really is: the word occupies
            // visual columns 0..=3 and so is flush left, not pushed out to
            // 6..=9 by six columns of padding.
            let last = (0..4).map(|l| map.visual_of(l)).max().unwrap();
            assert_eq!(last, 3, "{base:?}: the word left the left margin");
            for blank in 4..10 {
                assert_eq!(map.visual_of(blank), blank, "blank {blank} moved");
                assert!(!map.is_rtl(blank), "blank {blank} joined the run");
            }
        }
    }

    /// A terminal row is padded to the full column count, so the map is
    /// always computed over trailing blanks. That padding must be inert.
    #[test]
    fn padding_a_row_with_blanks_does_not_change_the_prefix_mapping() {
        for (base, s) in [
            (BidiBase::Ltr, "heb: שלום עולם | abc 123"),
            (BidiBase::Auto, "heb: שלום עולם | abc 123"),
            (BidiBase::Auto, "שלום עולם."),
            (BidiBase::Rtl, "שלום עולם."),
        ] {
            let short = map_of(s, base);
            let padded = map_of(&format!("{s}{}", " ".repeat(56)), base);
            for i in 0..s.chars().count() {
                assert_eq!(
                    short.visual_of(i),
                    padded.visual_of(i),
                    "{base:?}: column {i} shifted when the row was padded"
                );
            }
        }
    }

    /// The stability guarantee for the default mode: P2 stops at the `t` of
    /// `terra`, the first strong character in the row, so `Auto` resolves the
    /// paragraph to LTR and produces exactly what `Ltr` does. A prompt with
    /// Hebrew typed after it does not jump to the right margin.
    #[test]
    fn a_shell_prompt_followed_by_hebrew_keeps_the_prompt_in_place() {
        let s = "→ terra git:(main) ✗ שלום";
        for base in [BidiBase::Ltr, BidiBase::Auto] {
            assert_eq!(visual(s, base), "→ terra git:(main) ✗ םולש");

            let map = map_of(s, base);
            for i in 0..=20 {
                assert_eq!(map.visual_of(i), i, "{base:?}: the prompt moved");
            }
            assert_eq!(map.visual_of(21), 24);
            assert_eq!(map.visual_of(24), 21);
            // The parens in `(main)` are level 0, so L4 leaves them alone.
            assert!(!map.is_rtl(13));
        }
    }

    #[test]
    fn brackets_are_mirrored_only_when_their_own_level_is_odd() {
        // Parens inside the Hebrew run resolve to level 1 and flip.
        assert_eq!(visual("שלום (עולם) abc", BidiBase::Ltr), "(םלוע) םולש abc");
        // The same parens outside it stay level 0 and do not.
        assert_eq!(visual("abc (שלום) def", BidiBase::Ltr), "abc (םולש) def");
        assert_eq!(visual("abc (שלום) def", BidiBase::Auto), "abc (םולש) def");
    }

    /// European digits inside an RTL run sit at level 2 and keep reading
    /// left-to-right *within* the reversed run. This is the case a hand-rolled
    /// "just reverse the RTL span" implementation gets wrong.
    #[test]
    fn european_digits_inside_an_rtl_run_still_read_left_to_right() {
        assert_eq!(visual("שלום 123 עולם", BidiBase::Ltr), "םלוע 123 םולש");
        let map = map_of("שלום 123 עולם", BidiBase::Ltr);
        // 123 occupies logical 5,6,7 and lands on consecutive ascending
        // visual columns, not reversed ones.
        assert_eq!(map.visual_of(6), map.visual_of(5) + 1);
        assert_eq!(map.visual_of(7), map.visual_of(6) + 1);
    }

    /// A double-width char is pushed once per column it occupies, so the pair
    /// shares a level and stays adjacent. The glyph anchors to the visual
    /// left of the pair even when L2 has swapped the two columns.
    #[test]
    fn a_wide_char_and_its_spacer_stay_one_atomic_left_anchored_unit() {
        // `日` occupies two columns inside an RTL run.
        let chars: Vec<char> = "שלום 日日 עולם".chars().collect();
        let map = map_row(&chars, BidiBase::Ltr);
        let (a, b) = (5usize, 6usize);
        assert_eq!(
            map.visual_of(a).abs_diff(map.visual_of(b)),
            1,
            "the pair must stay adjacent"
        );
        assert_eq!(
            map.visual_span_start(a, 2),
            map.visual_of(a).min(map.visual_of(b)),
            "the glyph anchors to the leftmost visual column of the pair"
        );
    }

    /// A map can outlive its row by a frame across a resize. Every accessor
    /// has to survive that, because it is called from inside the paint loop.
    #[test]
    fn out_of_range_lookups_clamp_instead_of_panicking() {
        for base in [BidiBase::Ltr, BidiBase::Auto, BidiBase::Rtl] {
            for s in ["plain ascii", "שלום", "שלום עולם.", "   "] {
                let map = map_of(s, base);
                assert_eq!(map.visual_of(9999), 9999);
                assert_eq!(map.logical_of(9999), 9999);
                assert!(!map.is_rtl(9999));
                assert_eq!(map.display_char(9999, '('), '(');
                assert_eq!(map.visual_span_start(9999, 2), 9999);
            }
        }
    }

    #[test]
    fn an_empty_row_maps_to_an_empty_identity() {
        for base in [BidiBase::Ltr, BidiBase::Auto, BidiBase::Rtl] {
            let map = map_row(&[], base);
            assert!(map.is_identity());
            assert!(map.is_empty());
        }
    }

    /// A row of nothing but padding has no content to order, so there is
    /// nothing for even a forced RTL base to reverse. Bailing out early also
    /// keeps the overwhelmingly common blank row allocation-free.
    #[test]
    fn an_all_blank_row_is_identity_under_every_base() {
        for base in [BidiBase::Ltr, BidiBase::Auto, BidiBase::Rtl] {
            let map = map_of("        ", base);
            assert!(map.is_identity(), "{base:?} allocated for a blank row");
            assert_eq!(map.len(), 8);
            assert_eq!(map.visual_of(5), 5);
            assert!(!map.is_rtl(5));
        }
    }

    /// The bug `Auto` exists to fix. Rule N2 resolves a sentence-final
    /// neutral followed only by whitespace to the *paragraph* level, so under
    /// a forced LTR base it stays at level 0, never joins the reversed run,
    /// and is stranded at the visual right — the wrong end of an RTL
    /// sentence. Autodetection makes the paragraph RTL and the punctuation
    /// lands on the visual left, where a reader expects it.
    #[test]
    fn sentence_final_punctuation_lands_on_the_visual_left_under_auto() {
        for (logical, expected) in [
            ("היי מה קורה?", "?הרוק המ ייה"),
            ("שלום עולם.", ".םלוע םולש"),
            ("אני רואה (משהו) פה.", ".הפ (והשמ) האור ינא"),
        ] {
            assert_eq!(visual(logical, BidiBase::Auto), expected);
            // Stated directly: the final character of the logical row is the
            // first character of the visual row.
            let map = map_of(logical, BidiBase::Auto);
            assert_eq!(map.visual_of(logical.chars().count() - 1), 0);
        }
    }

    /// The modes have to genuinely differ, otherwise the config key that
    /// selects between them is decoration. This is the stranded punctuation
    /// the forced-LTR base produced before `Auto` existed.
    #[test]
    fn forcing_an_ltr_base_still_strands_the_punctuation_on_the_right() {
        assert_eq!(visual("היי מה קורה?", BidiBase::Ltr), "הרוק המ ייה?");
        assert_eq!(visual("שלום עולם.", BidiBase::Ltr), "םלוע םולש.");
        assert_eq!(
            visual("אני רואה (משהו) פה.", BidiBase::Ltr),
            "הפ (והשמ) האור ינא."
        );
    }

    /// What keeps `Auto` safe as the default for ordinary output: P2 finds a
    /// strong `L` and stops, so a Latin row resolves LTR and nothing moves —
    /// and it still takes the allocation-free fast path.
    #[test]
    fn a_latin_row_under_auto_is_still_identity() {
        for s in ["total 48 drwxr-xr-x", "$ cargo test -p egui_term", "ok."] {
            let map = map_of(s, BidiBase::Auto);
            assert!(map.is_identity(), "{s:?} left the fast path");
            assert_eq!(visual(s, BidiBase::Auto), s);
        }
    }

    /// Under a forced RTL base the fast path is unsound, so it must be
    /// skipped: N2 resolves the trailing `.` to the paragraph level and L2
    /// moves it to the visual left, even though the row is pure ASCII.
    #[test]
    fn a_forced_rtl_base_skips_the_identity_fast_path() {
        let map = map_of("total 48.", BidiBase::Rtl);
        assert!(!map.is_identity(), "the fast path swallowed an RTL base");
        assert_eq!(visual("total 48.", BidiBase::Rtl), ".total 48");
    }

    #[test]
    fn mirroring_only_touches_characters_that_have_a_mirror() {
        assert_eq!(mirror('('), ')');
        assert_eq!(mirror('»'), '«');
        assert_eq!(mirror('a'), 'a');
        assert_eq!(mirror('ש'), 'ש');
    }
    /// A TUI bullet must not change sides because the line happens to be in
    /// Hebrew — otherwise a mixed-language list has its bullets on both
    /// margins. This is the Claude Code case that motivated the prefix.
    #[test]
    fn a_leading_tui_bullet_stays_on_the_left_under_auto() {
        for (row, chrome) in [
            ("⏺ היי מה קורה?", 2),
            ("❯ שלום עולם", 2),
            ("  ⎿ שלום", 4),
            ("│ │ שלום", 4),
        ] {
            let chars: Vec<char> = row.chars().collect();
            let map = map_row(&chars, BidiBase::Auto);
            for column in 0..chrome {
                assert_eq!(
                    map.visual_of(column),
                    column,
                    "chrome column {column} of {row:?} moved"
                );
                assert!(
                    !map.is_rtl(column),
                    "chrome {column} of {row:?} is RTL"
                );
            }
            // The text after it still reads right-to-left.
            assert!(
                map.is_rtl(chrome),
                "the text after the chrome of {row:?} should be RTL"
            );
        }
    }

    /// The prefix is the run before the first strong letter, so a line that
    /// starts with ordinary text has none and is completely unaffected.
    #[test]
    fn a_row_starting_with_a_strong_letter_has_no_chrome_prefix() {
        assert_eq!(chrome_prefix(&"heb: שלום".chars().collect::<Vec<_>>()), 0);
        assert_eq!(chrome_prefix(&"שלום".chars().collect::<Vec<_>>()), 0);
        // All-neutral: the whole row, which then has nothing to reorder.
        assert_eq!(chrome_prefix(&"── ─┤".chars().collect::<Vec<_>>()), 5);
    }

    /// Sentence punctuation must still resolve with the text, not be mistaken
    /// for chrome — the prefix only looks at the *start* of the row.
    #[test]
    fn holding_the_chrome_out_does_not_strand_sentence_punctuation() {
        assert_eq!(visual("⏺ היי מה קורה?", BidiBase::Auto), "⏺ ?הרוק המ ייה");
        assert_eq!(visual("שלום עולם.", BidiBase::Auto), ".םלוע םולש");
    }

    /// An explicit base is the user overriding us; do not second-guess it.
    #[test]
    fn an_explicit_base_ignores_the_chrome_prefix() {
        let chars: Vec<char> = "⏺ שלום".chars().collect();
        let forced = map_row(&chars, BidiBase::Rtl);
        assert_ne!(forced.visual_of(0), 0, "forced rtl should move the bullet");
        assert_eq!(map_row(&chars, BidiBase::Ltr).visual_of(0), 0);
    }

    // -----------------------------------------------------------------
    // Cursor beam
    // -----------------------------------------------------------------

    /// One past the row's last occupied column — what the view derives from
    /// the grid and hands to [`beam_position`].
    fn content_end(chars: &[char]) -> usize {
        chars
            .iter()
            .rposition(|&c| c != ' ')
            .map_or(0, |last| last + 1)
    }

    /// The beam for a cursor parked on logical column `col` of row `s`,
    /// padded out to 20 columns the way a real terminal row is.
    fn beam_of(s: &str, base: BidiBase, col: usize) -> Beam {
        beam_wide(s, base, col, false)
    }

    fn beam_wide(s: &str, base: BidiBase, col: usize, is_wide: bool) -> Beam {
        let mut chars: Vec<char> = s.chars().collect();
        let end = content_end(&chars);
        chars.resize(20.max(chars.len()), ' ');
        let map = map_row(&chars, base);
        beam_position(Some(&map), col, is_wide, end)
    }

    fn left(offset: f32) -> Beam {
        Beam {
            offset,
            side: BeamSide::Left,
        }
    }

    fn right(offset: f32) -> Beam {
        Beam {
            offset,
            side: BeamSide::Right,
        }
    }

    /// The default configuration, and the one that must never move: with no
    /// right-to-left text the beam is the cell's own left edge, in every
    /// column, under every base.
    #[test]
    fn an_ltr_row_puts_the_beam_at_the_cells_left_edge() {
        for base in [BidiBase::Ltr, BidiBase::Auto] {
            for col in 0..20 {
                assert_eq!(
                    beam_of("total 48 drwxr-xr-x", base, col),
                    left(col as f32),
                    "{base:?}: column {col}"
                );
            }
        }
    }

    /// BiDi off is `None`, and `None` must be indistinguishable from plain
    /// left-to-right for every column — including on rows that *would*
    /// reorder, since turning the feature off has to turn all of it off.
    #[test]
    fn bidi_off_answers_exactly_what_plain_ltr_answers() {
        let rows = ["total 48", "שלום עולם", "heb: שלום abc", "היי מה קורה?"];
        for s in rows {
            let chars: Vec<char> = s.chars().collect();
            let end = content_end(&chars);
            for col in 0..24 {
                for is_wide in [false, true] {
                    assert_eq!(
                        beam_position(None, col, is_wide, end),
                        left(col as f32),
                        "{s:?}: column {col}, wide {is_wide}"
                    );
                }
            }
        }
    }

    /// Inside a right-to-left run the cursor cell's *right* edge is the
    /// insertion point, because the run grows leftwards. `abc שלום` lays out
    /// as `abc םולש`, so the run owns visual columns 4..=7 and logical 5
    /// (`ל`) sits at visual 6 — its right edge is 7.
    #[test]
    fn a_cursor_inside_an_rtl_run_marks_the_cells_right_edge() {
        assert_eq!(beam_of("abc שלום", BidiBase::Auto, 5), right(7.0));
        // The run's first character is at its visual right end, so the beam
        // is the run's own right edge.
        assert_eq!(beam_of("abc שלום", BidiBase::Auto, 4), right(8.0));
        // ...and its last character is at the visual left end.
        assert_eq!(beam_of("abc שלום", BidiBase::Auto, 7), right(5.0));
    }

    /// The reported bug. After the last letter of a right-to-left word the
    /// cursor sits on a blank column, which rule L1 resets to the base level;
    /// judging by that cell alone puts the beam to the *right* of the word,
    /// the side it started from. The next character joins the run and appears
    /// at the run's visual left end, so that is where the beam belongs.
    #[test]
    fn a_cursor_past_an_rtl_run_marks_the_runs_visual_left_edge() {
        // `שלום` is anchored flush left, occupying visual 0..=3, so the beam
        // is at 0 — not at 4, where the cursor's own blank cell lives.
        assert_eq!(beam_of("שלום", BidiBase::Auto, 4), left(0.0));
        // Behind a prompt the run starts at visual 2 (`❯` and its space are
        // held out of the paragraph), so the beam is at 2, not 6.
        assert_eq!(beam_of("❯ שלום", BidiBase::Auto, 6), left(2.0));
        // Mixed row: `abc שלום` renders as `abc םולש`, the run owns visual
        // 4..=7, and the beam is at 4 rather than the cursor's own 8.
        assert_eq!(beam_of("abc שלום", BidiBase::Auto, 8), left(4.0));
        // Same answer under a forced base.
        assert_eq!(beam_of("abc שלום", BidiBase::Ltr, 8), left(4.0));
        assert_eq!(beam_of("שלום", BidiBase::Rtl, 4), left(0.0));
    }

    /// Space typed after a right-to-left word leaves the cursor two columns
    /// past the text, and the answer must not drift with the gap: the next
    /// character still joins the run at its visual left end. Taking the
    /// minimum over the whole trailing run, rather than reading the single
    /// cell before the cursor, is what makes this hold.
    #[test]
    fn blanks_between_the_run_and_the_cursor_do_not_move_the_beam() {
        for col in 4..12 {
            assert_eq!(
                beam_of("שלום", BidiBase::Auto, col),
                left(0.0),
                "column {col}"
            );
        }
        // A word with an interior space is one run — rule N1 sweeps the space
        // in — and the run is still anchored at visual 0.
        assert_eq!(beam_of("שלום עולם", BidiBase::Auto, 9), left(0.0));
        assert_eq!(beam_of("שלום עולם", BidiBase::Auto, 12), left(0.0));
    }

    /// The other half of the reported bug: a blank *inside* the row is a real
    /// cell with a real position, and the cursor on it must stay there.
    /// Deciding by "the character before the cursor is right-to-left" alone
    /// drags the beam back to the run's left edge — visual 5 here — which is
    /// what put it in the middle of the Hebrew.
    #[test]
    fn a_cursor_on_a_blank_inside_the_row_stays_on_that_blank() {
        // `heb: שלום abc`: logical 9 is the space between the run and `abc`.
        assert_eq!(beam_of("heb: שלום abc", BidiBase::Auto, 9), left(9.0));
        // The columns of `abc` are not dragged either.
        assert_eq!(beam_of("heb: שלום abc", BidiBase::Auto, 10), left(10.0));
        // A left-to-right letter wedged between two right-to-left ones keeps
        // its own column: the run before it must not claim the beam.
        assert_eq!(beam_of("שaל", BidiBase::Ltr, 1), left(1.0));
    }

    /// Column 0 has no character before it, so the rule that consults the row
    /// content must not underflow — and an all-left-to-right row still
    /// answers 0.
    #[test]
    fn a_cursor_at_column_zero_is_total_under_every_base() {
        for base in [BidiBase::Ltr, BidiBase::Auto, BidiBase::Rtl] {
            for s in ["", " ", "abc", "שלום", "❯ שלום"] {
                let beam = beam_of(s, base, 0);
                assert!(beam.offset >= 0.0, "{base:?} {s:?}: {beam:?}");
            }
            assert_eq!(beam_of("abc", base, 0), left(0.0), "{base:?}");
            assert_eq!(beam_of("", base, 0), left(0.0), "{base:?}");
        }
        // On a pure right-to-left row, logical 0 is the run's *first*
        // character, which sits at the visual right end: `שלום` occupies
        // visual 0..=3, so the beam is that run's right edge, 4.
        assert_eq!(beam_of("שלום", BidiBase::Auto, 0), right(4.0));
    }

    /// A double-width character is one two-column unit, so the beam marks an
    /// edge of the *pair*, never the seam down the middle of the glyph.
    #[test]
    fn a_cursor_on_a_wide_char_uses_both_columns_of_the_pair() {
        // A wide *neutral* between Hebrew letters is the only way a pair
        // reaches an odd level: rule N1 sweeps it into the run. CJK is Bidi
        // class `L` and would sit at level 2 instead.
        let mut chars = vec!['ש', 'ל', '（', '（', 'ע', 'ם'];
        let end = chars.len();
        chars.resize(20, ' ');
        let map = map_row(&chars, BidiBase::Auto);
        // L2 reverses the whole run, so the pair occupies visual 3 and 2 —
        // swapped — and starts at 2. Its right edge is therefore 4.
        assert_eq!(map.visual_of(2), 3);
        assert_eq!(map.visual_of(3), 2);
        assert_eq!(map.visual_span_start(2, 2), 2);
        assert_eq!(beam_position(Some(&map), 2, true, end), right(4.0));
        // Judging the same cell as single-width happens to agree, and that
        // is an invariant rather than a coincidence: a pair at an odd level
        // is always swapped by L2, so `visual_of + 1 == span_start + 2`.
        // Spelling the width out keeps the rule true of the pair itself
        // instead of resting on that identity.
        assert_eq!(beam_position(Some(&map), 2, false, end), right(4.0));

        // A wide char at level 2 inside a right-to-left paragraph is *not*
        // swapped, and it is not right-to-left either, so the beam is the
        // pair's left edge. `שלום日日` lays the CJK out at the paragraph's
        // visual left, columns 0 and 1, so the answer is 0.
        let mut chars: Vec<char> = "שלום日日".chars().collect();
        let end = chars.len();
        chars.resize(20, ' ');
        let map = map_row(&chars, BidiBase::Auto);
        assert_eq!((map.visual_of(4), map.visual_of(5)), (0, 1));
        assert_eq!(beam_position(Some(&map), 4, true, end), left(0.0));

        // And on an ordinary left-to-right row it is simply the cell's own
        // column, wide or not — the default configuration must not move.
        let ltr: Vec<char> = "ab日日cd".chars().collect();
        for base in [BidiBase::Ltr, BidiBase::Auto] {
            let map = map_row(&ltr, base);
            assert_eq!(beam_position(Some(&map), 2, true, 6), left(2.0));
            assert_eq!(beam_position(Some(&map), 4, false, 6), left(4.0));
        }
    }

    /// The map can be a frame stale across a resize and the view can hand in
    /// a column past its end; every path clamps, exactly like the accessors.
    #[test]
    fn beam_positions_out_of_range_clamp_instead_of_panicking() {
        for base in [BidiBase::Ltr, BidiBase::Auto, BidiBase::Rtl] {
            let rows = ["plain ascii", "שלום", "שלום עולם.", "   ", ""];
            for s in rows {
                let chars: Vec<char> = s.chars().collect();
                let map = map_row(&chars, base);
                for (col, end) in
                    [(9999, 0), (9999, 9999), (0, 9999), (3, 9999)]
                {
                    let beam = beam_position(Some(&map), col, true, end);
                    assert!(
                        beam.offset.is_finite(),
                        "{base:?} {s:?} {col}/{end}: {beam:?}"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod chrome_prefix_tests {
    use super::*;

    fn prefix(s: &str) -> usize {
        chrome_prefix(&s.chars().collect::<Vec<_>>())
    }

    /// The hold-out exists for glyphs a TUI draws *around* its text. A
    /// number is the text.
    #[test]
    fn a_leading_timestamp_is_content_not_chrome() {
        // `⏺ ` is chrome; `03:34` is where the paragraph starts.
        assert_eq!(prefix("⏺ 03:34 בלילה"), 2);
        assert_eq!(prefix("[12:04] שלום"), 1);
        assert_eq!(prefix("2026-08-02 שלום"), 0);
    }

    /// The cases the hold-out was built for must still work.
    #[test]
    fn structural_glyphs_are_still_held_out() {
        assert_eq!(prefix("⏺ שלום"), 2);
        assert_eq!(prefix("❯ שלום"), 2);
        assert_eq!(prefix("  ⎿ שלום"), 4);
        assert_eq!(prefix("│ │ שלום"), 4);
    }

    /// A leading number makes the row's own direction detectable again, so
    /// the timestamp lands on the right with the Hebrew rather than stranded
    /// at the left margin.
    #[test]
    fn a_timestamp_before_hebrew_moves_to_the_visual_right() {
        let row = "⏺ 03:34 בלילה";
        let chars: Vec<char> = row.chars().collect();
        let map = map_row(&chars, BidiBase::Auto);
        // The bullet stays pinned left...
        assert_eq!(map.visual_of(0), 0);
        // ...and the digits are no longer pinned with it.
        let digit = row.chars().position(|c| c == '0').unwrap();
        assert!(
            map.visual_of(digit) > digit,
            "the timestamp should move right, not stay at column {digit}"
        );
    }
}
