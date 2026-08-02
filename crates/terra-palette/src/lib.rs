//! Spotlight-style command palette widget for egui.
//!
//! CONTRACT (terra-app depends on exactly this API — keep it stable):
//!
//! - `Palette::default()` creates a closed palette.
//! - `palette.open(actions)` opens it in command mode with the given actions.
//! - `palette.open_prompt(prompt, prefill, id)` opens a free-text input mode
//!   (used e.g. for "Rename Tab"); on submit it returns
//!   `PaletteEvent::PromptSubmitted { id, text }`.
//! - `palette.is_open()` reports visibility.
//! - `palette.show(ctx)` renders the overlay (centered, dimmed background),
//!   handles keys (type-to-filter fuzzy matching, Up/Down to move selection,
//!   Enter to confirm, Esc to close) and returns `Some(PaletteEvent)` when
//!   something happened this frame.
//!
//! The widget owns no application logic: it just filters and returns the
//! chosen `action_id` string.

use egui::{
    Align2, Area, Color32, Context, CornerRadius, FontId, Frame, Id, Key, LayerId, Margin,
    Modifiers, Order, Pos2, Rect, Sense, Shadow, Stroke, TextEdit, Vec2,
};

/// Glyph drawn in an action's leading tile. Painted from primitives rather
/// than typeset, so it never depends on a font shipping the codepoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteIcon {
    Plus,
    Cross,
    Pencil,
    ArrowRight,
    ArrowLeft,
    /// A `>_` prompt — used for "go to tab".
    Terminal,
    Power,
    Dot,
}

#[derive(Debug, Clone)]
pub struct PaletteAction {
    /// Stable identifier returned on selection, e.g. "tab.new".
    pub id: String,
    /// Human label shown in the list, e.g. "New Tab".
    pub label: String,
    /// Optional right-aligned keybinding hint, e.g. "⌘T".
    pub shortcut: Option<String>,
    /// Optional group heading, e.g. "Tabs". Actions sharing a section are
    /// rendered together under one header and share an accent colour.
    pub section: Option<String>,
    /// Optional leading icon.
    pub icon: Option<PaletteIcon>,
}

impl PaletteAction {
    pub fn new(id: impl Into<String>, label: impl Into<String>, shortcut: Option<&str>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            shortcut: shortcut.map(|s| s.to_string()),
            section: None,
            icon: None,
        }
    }

    /// Place this action under a group heading.
    pub fn in_section(mut self, section: impl Into<String>) -> Self {
        self.section = Some(section.into());
        self
    }

    /// Give this action a leading icon tile.
    pub fn with_icon(mut self, icon: PaletteIcon) -> Self {
        self.icon = Some(icon);
        self
    }
}

#[derive(Debug, Clone)]
pub enum PaletteEvent {
    /// User picked an action in command mode.
    ActionChosen { action_id: String },
    /// User submitted text in prompt mode.
    PromptSubmitted { id: String, text: String },
    /// Palette was dismissed (Esc / click outside).
    Dismissed,
}

#[derive(Default)]
pub struct Palette {
    state: State,
}

#[derive(Default)]
enum State {
    #[default]
    Closed,
    Command {
        actions: Vec<PaletteAction>,
        query: String,
        selected: usize,
    },
    Prompt {
        id: String,
        prompt: String,
        text: String,
    },
}

// ---------------------------------------------------------------------------
// Styling (floating "Spotlight" layer: big borderless input, soft rows)
// ---------------------------------------------------------------------------

const PANEL_WIDTH: f32 = 640.0;
const PANEL_RADIUS: u8 = 14;

const ROW_HEIGHT: f32 = 38.0;
const MAX_VISIBLE_ROWS: usize = 9;
/// Height of a group heading row.
const SECTION_HEIGHT: f32 = 26.0;
/// Extra breathing room above a heading that is not the first thing in the list.
const SECTION_GAP_TOP: f32 = 6.0;
/// Vertical breathing room above and below the result list.
const LIST_PAD_Y: f32 = 6.0;
/// Rows are inset from the panel edge so their fill reads as a floating pill.
const ROW_INSET_X: f32 = 6.0;
/// Text padding inside an inset row. `ROW_INSET_X + ROW_PAD_X` deliberately
/// equals `INPUT_PAD_X`, so labels line up under the query text.
const ROW_PAD_X: f32 = 10.0;

/// Horizontal breathing room inside the input.
const INPUT_PAD_X: i8 = 16;

const FONT_INPUT: f32 = 19.0;
const FONT_ROW: f32 = 14.5;
const FONT_SHORTCUT: f32 = 11.0;
const FONT_SECTION: f32 = 10.5;
const FONT_PROMPT: f32 = 12.5;
const FONT_EMPTY: f32 = 13.5;

/// Leading icon tile.
const ICON_TILE: f32 = 18.0;
const ICON_TILE_RADIUS: u8 = 5;
const ICON_STROKE: f32 = 1.5;
/// Space between the icon tile and the label.
const ICON_GAP: f32 = 10.0;

/// Translucent white, premultiplied (the `const` form of `from_white_alpha`).
const fn white(alpha: u8) -> Color32 {
    Color32::from_rgba_premultiplied(alpha, alpha, alpha, alpha)
}

/// The panel is *semi-transparent*: the terminal behind it stays faintly
/// visible through the fill, which (over the scrim) is what reads as glass.
/// egui has no backdrop blur, so the illusion is carried entirely by this
/// alpha plus the highlight/border layering in `show_panel`.
///
/// Premultiplied form of `rgb(0x20,0x21,0x26)` at 86% opacity.
const BG_PANEL: Color32 = Color32::from_rgba_premultiplied(0x1b, 0x1c, 0x20, 0xdc);
/// 12% white hairline around the floating layer — brighter than an opaque
/// panel would need, because a glass edge is what sells the material.
const PANEL_BORDER: Color32 = white(30);
/// A 1px inner highlight along the top edge, as if lit from above.
const PANEL_HIGHLIGHT: Color32 = white(22);
/// 6% white rule under the input.
const DIVIDER: Color32 = white(15);
/// 7% / 4% white — soft, Raycast-style, never a saturated bar.
const BG_SELECTED: Color32 = white(20);
const BG_HOVER: Color32 = white(11);
/// Hairline inside the selected row, so it reads as a raised pill.
const SELECTED_STROKE: Color32 = white(12);
/// 5% white kbd chip.
const CHIP_FILL: Color32 = white(13);

/// Accent colours cycled across sections, in declaration order. Keying off
/// the section (not the action) means every command in a group shares a
/// colour, so the eye can chunk the list without reading it.
const SECTION_TINTS: [Color32; 4] = [
    Color32::from_rgb(0x7d, 0xd3, 0xa0), // green
    Color32::from_rgb(0x8a, 0xb4, 0xf8), // blue
    Color32::from_rgb(0xf0, 0xa8, 0x68), // orange
    Color32::from_rgb(0xc4, 0xa2, 0xf5), // purple
];
/// Tint for actions that belong to no section.
const TINT_NEUTRAL: Color32 = Color32::from_rgb(0x9a, 0x9f, 0xa8);

const FG_INPUT: Color32 = Color32::from_rgb(0xf2, 0xf2, 0xf4);
const FG_ROW_SELECTED: Color32 = Color32::from_rgb(0xe8, 0xe8, 0xec);
const FG_ROW: Color32 = Color32::from_rgb(0xc9, 0xc9, 0xcf);
const FG_MUTED: Color32 = Color32::from_rgb(0x7a, 0x7f, 0x87);
const FG_SECTION: Color32 = Color32::from_rgb(0x82, 0x88, 0x91);
const FG_PROMPT: Color32 = Color32::from_rgb(0x8a, 0x8f, 0x97);
const FG_SHORTCUT: Color32 = Color32::from_rgb(0xa0, 0xa5, 0xad);
const CARET: Color32 = Color32::from_rgb(0x4a, 0x90, 0xd9);
/// Lighter than an opaque panel would want: the terminal has to stay legible
/// *through* the glass for the material to register at all.
const SCRIM: Color32 = Color32::from_black_alpha(70);

/// Shortcut chip metrics.
const CHIP_HEIGHT: f32 = 19.0;
const CHIP_PAD_X: f32 = 6.5;
const CHIP_RADIUS: u8 = 6;
/// Extra space inserted between the glyphs of a shortcut, so `⌘T` does not
/// set the letter flush against the symbol.
const CHIP_TRACKING: f32 = 1.5;

fn prop(size: f32) -> FontId {
    FontId::proportional(size)
}

/// Make the caret and the placeholder legible inside the palette's own UI.
/// There is no focus ring: the palette itself *is* the focus.
fn style_input(ui: &mut egui::Ui) {
    let v = ui.visuals_mut();
    v.text_cursor.stroke = Stroke::new(2.0, CARET);
    v.weak_text_color = Some(FG_MUTED);
    v.selection.stroke = Stroke::NONE;
}

/// Full-width hairline under the input.
fn divider(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, DIVIDER);
}

/// Points along a circle from `start_deg` to `end_deg` (screen coords, so
/// angles run clockwise and -90° is straight up).
fn arc_points(center: Pos2, radius: f32, start_deg: f32, end_deg: f32, steps: usize) -> Vec<Pos2> {
    (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            let a = (start_deg + (end_deg - start_deg) * t).to_radians();
            Pos2::new(center.x + radius * a.cos(), center.y + radius * a.sin())
        })
        .collect()
}

/// Shaft plus chevron. `dir` is +1 for right, -1 for left.
fn paint_arrow(p: &egui::Painter, c: Pos2, r: f32, s: Stroke, dir: f32) {
    let tip = Pos2::new(c.x + r * dir, c.y);
    p.line_segment([Pos2::new(c.x - r * dir, c.y), tip], s);
    p.line_segment([tip, Pos2::new(c.x + r * 0.4 * dir, c.y - r * 0.55)], s);
    p.line_segment([tip, Pos2::new(c.x + r * 0.4 * dir, c.y + r * 0.55)], s);
}

/// Paint a tinted rounded tile with `icon` stroked inside it.
fn paint_icon(p: &egui::Painter, tile: Rect, icon: PaletteIcon, tint: Color32) {
    p.rect_filled(
        tile,
        CornerRadius::same(ICON_TILE_RADIUS),
        tint.gamma_multiply(0.18),
    );
    let c = tile.center();
    let r = tile.width() * 0.26;
    let s = Stroke::new(ICON_STROKE, tint);
    match icon {
        PaletteIcon::Plus => {
            p.line_segment([Pos2::new(c.x - r, c.y), Pos2::new(c.x + r, c.y)], s);
            p.line_segment([Pos2::new(c.x, c.y - r), Pos2::new(c.x, c.y + r)], s);
        }
        PaletteIcon::Cross => {
            let d = r * 0.85;
            p.line_segment(
                [Pos2::new(c.x - d, c.y - d), Pos2::new(c.x + d, c.y + d)],
                s,
            );
            p.line_segment(
                [Pos2::new(c.x + d, c.y - d), Pos2::new(c.x - d, c.y + d)],
                s,
            );
        }
        PaletteIcon::Pencil => {
            // A bare diagonal reads as a slash, not a pencil. What makes it
            // legible at 18px is the pairing: a solid nib at the tip plus the
            // rule it writes on.
            let tip = Pos2::new(c.x - r * 0.75, c.y + r * 0.35);
            let butt = Pos2::new(c.x + r * 0.95, c.y - r * 1.05);
            p.line_segment([tip, butt], Stroke::new(ICON_STROKE * 1.15, tint));
            // Nib: a small filled wedge pointing down-left past the shaft.
            let nib = r * 0.42;
            p.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(tip.x - nib, tip.y + nib),
                    Pos2::new(tip.x + nib * 0.55, tip.y - nib * 0.15),
                    Pos2::new(tip.x + nib * 0.15, tip.y + nib * 0.55),
                ],
                tint,
                Stroke::NONE,
            ));
            // The rule being written on.
            p.line_segment(
                [
                    Pos2::new(c.x - r * 0.95, c.y + r * 1.05),
                    Pos2::new(c.x + r * 0.95, c.y + r * 1.05),
                ],
                Stroke::new(ICON_STROKE, tint.gamma_multiply(0.75)),
            );
        }
        PaletteIcon::ArrowRight => paint_arrow(p, c, r, s, 1.0),
        PaletteIcon::ArrowLeft => paint_arrow(p, c, r, s, -1.0),
        PaletteIcon::Terminal => {
            // A `>_` prompt.
            let elbow = Pos2::new(c.x - r * 0.1, c.y - r * 0.15);
            p.line_segment([Pos2::new(c.x - r, c.y - r * 0.85), elbow], s);
            p.line_segment([elbow, Pos2::new(c.x - r, c.y + r * 0.55)], s);
            p.line_segment(
                [
                    Pos2::new(c.x + r * 0.15, c.y + r * 0.8),
                    Pos2::new(c.x + r, c.y + r * 0.8),
                ],
                s,
            );
        }
        PaletteIcon::Power => {
            // Ring with a gap at the top, plus the stem through the gap.
            p.add(egui::Shape::line(arc_points(c, r, -60.0, 240.0, 18), s));
            p.line_segment(
                [
                    Pos2::new(c.x, c.y - r * 1.1),
                    Pos2::new(c.x, c.y - r * 0.15),
                ],
                s,
            );
        }
        PaletteIcon::Dot => {
            p.circle_filled(c, r * 0.45, tint);
        }
    }
}

/// Paint a macOS-style kbd chip whose right edge sits at `right_center`.
///
/// Two details this does not get for free from a single `layout_no_wrap`:
///
/// * **Font.** The modifier glyphs live in a narrow slice of Unicode
///   (`⇧` U+21E7, `⌘` U+2318, `⌥` U+2325, `⌃` U+2303). egui's default
///   *proportional* stack does not cover all of them and renders the misses
///   as tofu, while the monospace face the terminal already ships does.
/// * **Tracking.** `⌘T` typeset normally sets the letter hard against the
///   glyph, because the symbol has no side bearing to speak of. Laying each
///   character out separately lets us open the tracking back up.
fn shortcut_chip(painter: &egui::Painter, right_center: Pos2, text: &str) {
    let font = FontId::monospace(FONT_SHORTCUT);
    let glyphs: Vec<_> = text
        .chars()
        .map(|ch| painter.layout_no_wrap(ch.to_string(), font.clone(), FG_SHORTCUT))
        .collect();
    if glyphs.is_empty() {
        return;
    }

    let text_w: f32 =
        glyphs.iter().map(|g| g.size().x).sum::<f32>() + CHIP_TRACKING * (glyphs.len() - 1) as f32;
    let size = Vec2::new(text_w + CHIP_PAD_X * 2.0, CHIP_HEIGHT);
    let rect = Rect::from_min_size(
        Pos2::new(right_center.x - size.x, right_center.y - size.y * 0.5),
        size,
    );
    painter.rect_filled(rect, CornerRadius::same(CHIP_RADIUS), CHIP_FILL);

    let mut x = rect.left() + CHIP_PAD_X;
    for g in glyphs {
        let w = g.size().x;
        let h = g.size().y;
        painter.galley(Pos2::new(x, rect.center().y - h * 0.5), g, FG_SHORTCUT);
        x += w + CHIP_TRACKING;
    }
}

// ---------------------------------------------------------------------------
// Fuzzy matching (pure, unit-tested)
// ---------------------------------------------------------------------------

/// How well a candidate matched the query. Lower sorts first.
///
/// * `class` — 0 for an exact (case-insensitive) substring hit, 1 for a
///   subsequence-only hit. Substring always beats subsequence.
/// * `pos` — character index where the match starts. Earlier beats later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct MatchScore {
    class: u8,
    pos: usize,
}

/// Case-insensitive fuzzy match of `query` against `text`.
///
/// An empty query matches everything with the best possible score. A non-empty
/// query matches if it is a substring (best) or a subsequence (worse) of the
/// text. Returns `None` when there is no match at all.
fn fuzzy_match(text: &str, query: &str) -> Option<MatchScore> {
    if query.is_empty() {
        return Some(MatchScore { class: 0, pos: 0 });
    }

    let hay: Vec<char> = text.chars().flat_map(|c| c.to_lowercase()).collect();
    let needle: Vec<char> = query.chars().flat_map(|c| c.to_lowercase()).collect();

    if needle.len() > hay.len() {
        return None;
    }

    // Exact substring first.
    for start in 0..=(hay.len() - needle.len()) {
        if hay[start..start + needle.len()] == needle[..] {
            return Some(MatchScore {
                class: 0,
                pos: start,
            });
        }
    }

    // Subsequence fallback.
    let mut first = None;
    let mut ni = 0usize;
    for (hi, hc) in hay.iter().enumerate() {
        if *hc == needle[ni] {
            if first.is_none() {
                first = Some(hi);
            }
            ni += 1;
            if ni == needle.len() {
                return Some(MatchScore {
                    class: 1,
                    pos: first.unwrap_or(hi),
                });
            }
        }
    }

    None
}

/// Filter + rank `actions` by `query`, returning indices into `actions`.
///
/// Ranking: exact substring beats subsequence, earlier match position beats
/// later, ties broken by the original order of `actions`.
fn filter_actions(actions: &[PaletteAction], query: &str) -> Vec<usize> {
    let mut scored: Vec<(MatchScore, usize)> = actions
        .iter()
        .enumerate()
        .filter_map(|(i, a)| fuzzy_match(&a.label, query).map(|s| (s, i)))
        .collect();
    scored.sort_by_key(|(score, idx)| (*score, *idx));
    scored.into_iter().map(|(_, i)| i).collect()
}

// ---------------------------------------------------------------------------
// Sectioning (pure, unit-tested)
// ---------------------------------------------------------------------------

/// Section names in declaration order, deduplicated. The position of a name
/// in here is what picks its accent colour, so colours stay put while the
/// user types instead of shuffling with the filtered results.
fn section_order(actions: &[PaletteAction]) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for a in actions {
        if let Some(s) = a.section.as_deref() {
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out
}

/// The filtered list, reordered so each section's members sit together.
struct Grouped {
    /// Indices into `actions`, in display order.
    order: Vec<usize>,
    /// `headers[i]` is the section index whose heading is drawn *above*
    /// `order[i]`, or `None` when no heading breaks there.
    headers: Vec<Option<usize>>,
}

/// Group `matches` by section while preserving relevance.
///
/// Sections appear in the order their best-scoring member appears in
/// `matches`, so the top hit is always in the first group (and therefore is
/// the initially selected row); within a group, `matches` order is kept.
/// Sectionless actions form one leading group with no heading.
fn group_matches(actions: &[PaletteAction], matches: &[usize]) -> Grouped {
    let names = section_order(actions);
    // `None` = the sectionless bucket, which always leads.
    let mut buckets: Vec<(Option<usize>, Vec<usize>)> = Vec::new();
    for &m in matches {
        let key = actions[m]
            .section
            .as_deref()
            .and_then(|s| names.iter().position(|n| *n == s));
        match buckets.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => v.push(m),
            None => buckets.push((key, vec![m])),
        }
    }
    buckets.sort_by_key(|(k, _)| k.is_some()); // sectionless first, else stable

    let mut order = Vec::with_capacity(matches.len());
    let mut headers = Vec::with_capacity(matches.len());
    for (key, items) in buckets {
        for (i, m) in items.into_iter().enumerate() {
            headers.push(if i == 0 { key } else { None });
            order.push(m);
        }
    }
    Grouped { order, headers }
}

// ---------------------------------------------------------------------------
// Widget
// ---------------------------------------------------------------------------

impl Palette {
    pub fn is_open(&self) -> bool {
        !matches!(self.state, State::Closed)
    }

    pub fn open(&mut self, actions: Vec<PaletteAction>) {
        self.state = State::Command {
            actions,
            query: String::new(),
            selected: 0,
        };
    }

    pub fn open_prompt(
        &mut self,
        prompt: impl Into<String>,
        prefill: impl Into<String>,
        id: impl Into<String>,
    ) {
        self.state = State::Prompt {
            id: id.into(),
            prompt: prompt.into(),
            text: prefill.into(),
        };
    }

    pub fn close(&mut self) {
        self.state = State::Closed;
    }

    /// Render. Returns an event if the user acted this frame.
    pub fn show(&mut self, ctx: &Context) -> Option<PaletteEvent> {
        match self.state {
            State::Closed => None,
            State::Command { .. } => self.show_command(ctx),
            State::Prompt { .. } => self.show_prompt(ctx),
        }
    }

    // -- command mode -------------------------------------------------------

    fn show_command(&mut self, ctx: &Context) -> Option<PaletteEvent> {
        // Read keys *before* building the UI so the TextEdit below never sees
        // them and they never leak to the app underneath.
        let keys = consume_keys(ctx);

        let State::Command {
            actions,
            query,
            selected,
        } = &mut self.state
        else {
            return None;
        };

        let tints = section_order(actions)
            .iter()
            .enumerate()
            .map(|(i, _)| SECTION_TINTS[i % SECTION_TINTS.len()])
            .collect::<Vec<_>>();
        let names: Vec<String> = section_order(actions)
            .into_iter()
            .map(str::to_owned)
            .collect();

        let mut grouped = group_matches(actions, &filter_actions(actions, query));

        if keys.escape {
            self.close();
            return Some(PaletteEvent::Dismissed);
        }

        let mut scroll_to_selected = false;
        if !grouped.order.is_empty() {
            let n = grouped.order.len();
            if *selected >= n {
                *selected = 0;
            }
            if keys.up {
                *selected = (*selected + n - 1) % n;
                scroll_to_selected = true;
            }
            if keys.down {
                *selected = (*selected + 1) % n;
                scroll_to_selected = true;
            }
            if keys.enter {
                let action_id = actions[grouped.order[*selected]].id.clone();
                self.close();
                return Some(PaletteEvent::ActionChosen { action_id });
            }
        }
        // (With no matches, Enter is simply swallowed by `consume_keys`.)

        let mut event = None;
        {
            let edit_id = Id::new("terra_palette_query");
            let mut chosen: Option<String> = None;

            let rect = show_panel(ctx, |ui| {
                let before = query.clone();
                ui.memory_mut(|m| m.request_focus(edit_id));
                style_input(ui);
                // TextEdit ignores .margin() with Frame::NONE, so all padding
                // is explicit: 13px above/below, 16px left indent.
                ui.add_space(13.0);
                ui.horizontal(|ui| {
                    ui.add_space(f32::from(INPUT_PAD_X));
                    ui.add(
                        TextEdit::singleline(query)
                            .id(edit_id)
                            .hint_text(
                                egui::RichText::new("Type a command…").font(prop(FONT_INPUT)),
                            )
                            .font(prop(FONT_INPUT))
                            .text_color(FG_INPUT)
                            .desired_width(ui.available_width() - f32::from(INPUT_PAD_X))
                            .margin(Margin::ZERO)
                            .frame(Frame::NONE),
                    );
                });
                ui.add_space(13.0);

                if *query != before {
                    *selected = 0;
                    grouped = group_matches(actions, &filter_actions(actions, query));
                }

                divider(ui);
                ui.add_space(LIST_PAD_Y);

                if grouped.order.is_empty() {
                    let (rect, _) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), ROW_HEIGHT + 16.0),
                        Sense::hover(),
                    );
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        "No matching commands",
                        prop(FONT_EMPTY),
                        FG_MUTED,
                    );
                } else {
                    // Budget for headings too, so a sectioned list still shows
                    // MAX_VISIBLE_ROWS worth of actual commands.
                    let headings = grouped.headers.iter().filter(|h| h.is_some()).count() as f32;
                    let max_height = ROW_HEIGHT * MAX_VISIBLE_ROWS as f32
                        + (SECTION_HEIGHT + SECTION_GAP_TOP) * headings.min(4.0);
                    egui::ScrollArea::vertical()
                        .max_height(max_height)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            for (row, &action_idx) in grouped.order.iter().enumerate() {
                                // Group heading, when this row starts a section.
                                if let Some(sec) = grouped.headers[row] {
                                    if row > 0 {
                                        ui.add_space(SECTION_GAP_TOP);
                                    }
                                    let (hrect, _) = ui.allocate_exact_size(
                                        Vec2::new(ui.available_width(), SECTION_HEIGHT),
                                        Sense::hover(),
                                    );
                                    ui.painter().text(
                                        hrect.left_center()
                                            + Vec2::new(ROW_INSET_X + ROW_PAD_X, 0.0),
                                        Align2::LEFT_CENTER,
                                        names[sec].to_uppercase(),
                                        prop(FONT_SECTION),
                                        FG_SECTION,
                                    );
                                }

                                let action = &actions[action_idx];
                                let is_sel = row == *selected;
                                let (rect, resp) = ui.allocate_exact_size(
                                    Vec2::new(ui.available_width(), ROW_HEIGHT),
                                    Sense::click(),
                                );
                                if is_sel && scroll_to_selected {
                                    resp.scroll_to_me(None);
                                }
                                let inset = Rect::from_min_max(
                                    Pos2::new(rect.left() + ROW_INSET_X, rect.top()),
                                    Pos2::new(rect.right() - ROW_INSET_X, rect.bottom()),
                                );
                                let bg = if is_sel {
                                    Some(BG_SELECTED)
                                } else if resp.hovered() {
                                    Some(BG_HOVER)
                                } else {
                                    None
                                };
                                if let Some(bg) = bg {
                                    ui.painter().rect_filled(inset, CornerRadius::same(8), bg);
                                }
                                if is_sel {
                                    ui.painter().rect_stroke(
                                        inset,
                                        CornerRadius::same(8),
                                        Stroke::new(1.0, SELECTED_STROKE),
                                        egui::StrokeKind::Inside,
                                    );
                                }

                                let p = ui.painter();
                                let mut text_x = ROW_PAD_X;
                                if let Some(icon) = action.icon {
                                    let tint = grouped.headers[..=row]
                                        .iter()
                                        .rev()
                                        .find_map(|h| *h)
                                        .and_then(|s| tints.get(s).copied())
                                        .unwrap_or(TINT_NEUTRAL);
                                    let tile = Rect::from_center_size(
                                        inset.left_center()
                                            + Vec2::new(ROW_PAD_X + ICON_TILE * 0.5, 0.0),
                                        Vec2::splat(ICON_TILE),
                                    );
                                    paint_icon(p, tile, icon, tint);
                                    text_x += ICON_TILE + ICON_GAP;
                                }
                                p.text(
                                    inset.left_center() + Vec2::new(text_x, 0.0),
                                    Align2::LEFT_CENTER,
                                    &action.label,
                                    prop(FONT_ROW),
                                    if is_sel { FG_ROW_SELECTED } else { FG_ROW },
                                );
                                if let Some(sc) = &action.shortcut {
                                    shortcut_chip(
                                        p,
                                        inset.right_center() - Vec2::new(ROW_PAD_X, 0.0),
                                        sc,
                                    );
                                }
                                if resp.clicked() {
                                    chosen = Some(action.id.clone());
                                }
                            }
                        });
                }
                ui.add_space(LIST_PAD_Y);
            });

            if let Some(action_id) = chosen {
                event = Some(PaletteEvent::ActionChosen { action_id });
            } else if scrim_clicked(ctx, rect) {
                event = Some(PaletteEvent::Dismissed);
            }
        }

        if event.is_some() {
            self.close();
        }
        event
    }

    // -- prompt mode --------------------------------------------------------

    fn show_prompt(&mut self, ctx: &Context) -> Option<PaletteEvent> {
        let keys = consume_keys(ctx);

        let State::Prompt { id, prompt, text } = &mut self.state else {
            return None;
        };

        if keys.escape {
            self.close();
            return Some(PaletteEvent::Dismissed);
        }
        if keys.enter {
            let ev = PaletteEvent::PromptSubmitted {
                id: id.clone(),
                text: text.clone(),
            };
            self.close();
            return Some(ev);
        }

        let edit_id = Id::new("terra_palette_prompt");
        let prompt_label = prompt.clone();
        let rect = show_panel(ctx, |ui| {
            ui.add_space(14.0);
            let (label_rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 16.0), Sense::hover());
            ui.painter().text(
                label_rect.left_center() + Vec2::new(INPUT_PAD_X as f32, 0.0),
                Align2::LEFT_CENTER,
                &prompt_label,
                prop(FONT_PROMPT),
                FG_PROMPT,
            );
            ui.memory_mut(|m| m.request_focus(edit_id));
            style_input(ui);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.add_space(f32::from(INPUT_PAD_X));
                ui.add(
                    TextEdit::singleline(text)
                        .id(edit_id)
                        .font(prop(FONT_INPUT))
                        .text_color(FG_INPUT)
                        .cursor_at_end(true)
                        .desired_width(ui.available_width() - f32::from(INPUT_PAD_X))
                        .margin(Margin::ZERO)
                        .frame(Frame::NONE),
                );
            });
            ui.add_space(12.0);
        });

        if scrim_clicked(ctx, rect) {
            self.close();
            return Some(PaletteEvent::Dismissed);
        }

        None
    }
}

// ---------------------------------------------------------------------------
// Chrome helpers
// ---------------------------------------------------------------------------

#[derive(Default, Clone, Copy)]
struct Keys {
    escape: bool,
    enter: bool,
    up: bool,
    down: bool,
}

/// Consume the keys the palette cares about so they don't reach the app below.
fn consume_keys(ctx: &Context) -> Keys {
    ctx.input_mut(|i| {
        let none = Modifiers::NONE;
        let escape = i.consume_key(none, Key::Escape);
        let enter = i.consume_key(none, Key::Enter);
        let mut up = i.consume_key(none, Key::ArrowUp);
        let mut down = i.consume_key(none, Key::ArrowDown);
        // Emacs-style alternatives.
        if i.consume_key(Modifiers::CTRL, Key::P) {
            up = true;
        }
        if i.consume_key(Modifiers::CTRL, Key::N) {
            down = true;
        }
        Keys {
            escape,
            enter,
            up,
            down,
        }
    })
}

/// Paint the scrim and the panel; returns the panel's screen rect.
fn show_panel(ctx: &Context, add_contents: impl FnOnce(&mut egui::Ui)) -> Rect {
    let screen = ctx.content_rect();

    // Scrim: an interactable full-screen area *below* the panel that swallows
    // clicks meant for the app underneath.
    Area::new(Id::new("terra_palette_scrim"))
        .order(Order::Middle)
        .fixed_pos(screen.min)
        .interactable(true)
        .show(ctx, |ui| {
            ui.allocate_exact_size(screen.size(), Sense::click());
        });
    ctx.layer_painter(LayerId::new(Order::Middle, Id::new("terra_palette_scrim")))
        .rect_filled(screen, CornerRadius::ZERO, SCRIM);

    let top = screen.top() + (screen.height() * 0.12).clamp(24.0, 160.0);
    let left = screen.center().x - PANEL_WIDTH * 0.5;

    let inner = Area::new(Id::new("terra_palette_panel"))
        .order(Order::Foreground)
        .fixed_pos(Pos2::new(left.max(screen.left() + 8.0), top))
        .constrain_to(screen)
        .show(ctx, |ui| {
            ui.set_width(PANEL_WIDTH.min(screen.width() - 16.0));
            // No inner margin: the input owns its own padding and the divider
            // runs edge to edge.
            Frame::NONE
                .fill(BG_PANEL)
                .stroke(Stroke::new(1.0, PANEL_BORDER))
                .corner_radius(CornerRadius::same(PANEL_RADIUS))
                .inner_margin(Margin::ZERO)
                .shadow(Shadow {
                    offset: [0, 18],
                    blur: 48,
                    spread: 0,
                    color: Color32::from_black_alpha(140),
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    // All vertical rhythm is explicit below.
                    ui.spacing_mut().item_spacing.y = 0.0;
                    add_contents(ui);
                })
                .response
                .rect
        });

    // Inner top highlight, painted after the frame so it sits over the fill.
    // A translucent panel with only an outer border reads as flat; the lit
    // top edge is what makes it read as a pane of glass.
    let panel = inner.inner;
    let highlight = Rect::from_min_max(
        Pos2::new(panel.left() + f32::from(PANEL_RADIUS), panel.top() + 1.0),
        Pos2::new(panel.right() - f32::from(PANEL_RADIUS), panel.top() + 2.0),
    );
    ctx.layer_painter(LayerId::new(
        Order::Foreground,
        Id::new("terra_palette_panel"),
    ))
    .rect_filled(highlight, CornerRadius::ZERO, PANEL_HIGHLIGHT);

    panel
}

/// True when the user clicked outside the palette panel this frame.
fn scrim_clicked(ctx: &Context, panel_rect: Rect) -> bool {
    ctx.input(|i| {
        if !i.pointer.any_click() {
            return false;
        }
        match i.pointer.interact_pos() {
            Some(pos) => !panel_rect.contains(pos),
            None => false,
        }
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn acts(labels: &[&str]) -> Vec<PaletteAction> {
        labels
            .iter()
            .enumerate()
            .map(|(i, l)| PaletteAction::new(format!("id.{i}"), *l, None))
            .collect()
    }

    fn labels(actions: &[PaletteAction], idxs: &[usize]) -> Vec<String> {
        idxs.iter().map(|&i| actions[i].label.clone()).collect()
    }

    #[test]
    fn empty_query_matches_everything_in_order() {
        let a = acts(&["New Tab", "Close Tab", "Rename Tab"]);
        assert_eq!(filter_actions(&a, ""), vec![0, 1, 2]);
    }

    #[test]
    fn substring_match_is_case_insensitive() {
        assert!(fuzzy_match("New Tab", "NEW").is_some());
        assert!(fuzzy_match("New Tab", "tab").is_some());
        assert!(fuzzy_match("New Tab", "w t").is_some());
    }

    #[test]
    fn non_match_returns_none() {
        assert!(fuzzy_match("New Tab", "xyz").is_none());
        // Right letters, wrong order -> not a subsequence.
        assert!(fuzzy_match("New Tab", "bat").is_none());
        // Longer than the haystack.
        assert!(fuzzy_match("ab", "abc").is_none());
    }

    #[test]
    fn subsequence_matches_when_substring_does_not() {
        let s = fuzzy_match("Rename Tab", "rt").expect("should match as subsequence");
        assert_eq!(s.class, 1);
        assert_eq!(s.pos, 0);
        assert!(fuzzy_match("Rename Tab", "rt")
            .zip(fuzzy_match("Rename Tab", "ren"))
            .map(|(sub, exact)| exact < sub)
            .unwrap());
    }

    #[test]
    fn substring_beats_subsequence() {
        // "Next Tab" contains "nt" only as a subsequence (n..t), while
        // "Not There" contains the literal "not" -- test with a shared query.
        let a = acts(&["Select Next Tab", "Nt Direct"]);
        // "nt" is a substring of "Nt Direct" (pos 0) and a subsequence of
        // "Select Next Tab".
        assert_eq!(
            labels(&a, &filter_actions(&a, "nt")),
            vec!["Nt Direct", "Select Next Tab"]
        );
    }

    #[test]
    fn earlier_match_beats_later() {
        let a = acts(&["Zoom Tab", "Tab Bar"]);
        // "tab" at pos 5 vs pos 0 -> the pos-0 one wins even though it is
        // second in the original order.
        assert_eq!(
            labels(&a, &filter_actions(&a, "tab")),
            vec!["Tab Bar", "Zoom Tab"]
        );
    }

    #[test]
    fn ties_keep_original_order() {
        let a = acts(&["Tab One", "Tab Two", "Tab Three"]);
        assert_eq!(filter_actions(&a, "tab"), vec![0, 1, 2]);
    }

    #[test]
    fn filters_out_non_matches() {
        let a = acts(&["New Tab", "Close Tab", "Quit"]);
        assert_eq!(
            labels(&a, &filter_actions(&a, "tab")),
            vec!["New Tab", "Close Tab"]
        );
        assert!(filter_actions(&a, "zzz").is_empty());
    }

    #[test]
    fn score_ordering_is_total_and_sane() {
        assert!(MatchScore { class: 0, pos: 9 } < MatchScore { class: 1, pos: 0 });
        assert!(MatchScore { class: 0, pos: 1 } < MatchScore { class: 0, pos: 2 });
    }

    /// `[(label, section)]` -> actions.
    fn sectioned(rows: &[(&str, Option<&str>)]) -> Vec<PaletteAction> {
        rows.iter()
            .enumerate()
            .map(|(i, (l, s))| {
                let a = PaletteAction::new(format!("id.{i}"), *l, None);
                match s {
                    Some(s) => a.in_section(*s),
                    None => a,
                }
            })
            .collect()
    }

    fn grouped_labels(actions: &[PaletteAction], query: &str) -> Vec<String> {
        let g = group_matches(actions, &filter_actions(actions, query));
        g.order
            .iter()
            .zip(&g.headers)
            .map(|(&i, h)| match h {
                Some(s) => format!("[{}] {}", section_order(actions)[*s], actions[i].label),
                None => actions[i].label.clone(),
            })
            .collect()
    }

    #[test]
    fn section_order_dedupes_and_keeps_declaration_order() {
        let a = sectioned(&[
            ("New", Some("Tabs")),
            ("Next", Some("Navigate")),
            ("Close", Some("Tabs")),
            ("Quit", None),
        ]);
        assert_eq!(section_order(&a), vec!["Tabs", "Navigate"]);
    }

    #[test]
    fn grouping_collects_each_section_together() {
        let a = sectioned(&[
            ("New Tab", Some("Tabs")),
            ("Next Tab", Some("Navigate")),
            ("Close Tab", Some("Tabs")),
            ("Quit terra", Some("Application")),
        ]);
        // Declaration order interleaves Tabs/Navigate; display must not.
        assert_eq!(
            grouped_labels(&a, ""),
            vec![
                "[Tabs] New Tab",
                "Close Tab",
                "[Navigate] Next Tab",
                "[Application] Quit terra",
            ]
        );
    }

    #[test]
    fn sectionless_actions_lead_without_a_heading() {
        let a = sectioned(&[("Alpha", Some("Group")), ("Loose", None)]);
        assert_eq!(grouped_labels(&a, ""), vec!["Loose", "[Group] Alpha"]);
    }

    #[test]
    fn best_match_stays_first_so_it_is_the_default_selection() {
        let a = sectioned(&[
            ("New Tab", Some("Tabs")),
            ("Quit terra", Some("Application")),
        ]);
        // "quit" only hits the second section -- that section must lead, or
        // Enter would fire the wrong command.
        let g = group_matches(&a, &filter_actions(&a, "quit"));
        assert_eq!(a[g.order[0]].label, "Quit terra");
        assert_eq!(grouped_labels(&a, "quit"), vec!["[Application] Quit terra"]);
    }

    #[test]
    fn grouping_preserves_relevance_within_a_section() {
        let a = sectioned(&[
            ("Zoom Tab", Some("Tabs")),
            ("Tab Bar", Some("Tabs")),
            ("Unrelated", Some("Other")),
        ]);
        // "tab" at pos 0 beats pos 5, same as the ungrouped ranking.
        assert_eq!(
            grouped_labels(&a, "tab"),
            vec!["[Tabs] Tab Bar", "Zoom Tab"]
        );
    }

    #[test]
    fn every_row_has_a_header_slot() {
        let a = sectioned(&[("A", Some("X")), ("B", Some("Y"))]);
        let g = group_matches(&a, &filter_actions(&a, ""));
        assert_eq!(g.order.len(), g.headers.len());
    }

    #[test]
    fn palette_open_close_state() {
        let mut p = Palette::default();
        assert!(!p.is_open());
        p.open(acts(&["New Tab"]));
        assert!(p.is_open());
        p.close();
        assert!(!p.is_open());
        p.open_prompt("Rename tab", "old", "rename");
        assert!(p.is_open());
    }
}
