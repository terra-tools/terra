//! Tab bar chrome and global keybindings.
//!
//! The bar mimics Ghostty on macOS: tabs share the full width of the bar evenly
//! (no content-sized pills) and sit flush with it like a segmented control —
//! inactive tabs are all but invisible, separated by a hairline, while the
//! active one is a light, large-radius capsule. The hovered tab reveals a `×` on
//! its left inside edge, the first nine tabs carry a dimmed `⌘n` hint on the
//! right, and tabs can be dragged along the bar to reorder them.
//!
//! Everything that moves is animated: tab widths grow in and shrink out, and
//! tabs slide to their slot instead of jumping there (see [`BarState`]), while
//! the hover fill and the `×` fade with the pointer rather than snapping.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{
    Color32, CornerRadius, FontId, Galley, Id, Key, KeyboardShortcut, Modifiers, PointerButton,
    Rect, Sense, Stroke, Ui, Vec2,
};

use crate::tabs::TabManager;

/// Outer height of the tab bar panel.
pub const TAB_BAR_HEIGHT: f32 = 32.0;

/// Vertical padding inside the bar; `TAB_BAR_HEIGHT - 2 * PAD_Y` is the tab height.
const PAD_Y: i8 = 4;
/// Horizontal padding inside the bar.
const PAD_X: i8 = 6;
/// Space between two neighbouring tabs. Zero on purpose: like a segmented
/// control, tabs touch and are told apart by a hairline instead of a gap.
const TAB_GAP: f32 = 0.0;
/// Breathing room between the last tab and the `+` zone.
const PLUS_GAP: f32 = 6.0;
/// Fixed width reserved at the far right for the `+` button.
const PLUS_WIDTH: f32 = 28.0;
/// Tabs never shrink below this, even if that means the row overflows (clipped).
const MIN_TAB_WIDTH: f32 = 44.0;
/// A tab only shows its `⌘n` hint when it is at least this wide.
const HINT_MIN_TAB_WIDTH: f32 = 120.0;
/// Horizontal space kept free on both sides of the centred title, so the title
/// never collides with the `×` or the `⌘n` hint (and never jumps on hover).
const TITLE_RESERVE: f32 = 24.0;

/// Large enough to read as a capsule at a 28px tab height.
const CORNER: u8 = 12;
/// macOS sets window and tab titles at 13px; the system face is small on the
/// body relative to egui's default, so this reads no larger than the old 12px.
const FONT_SIZE: f32 = 13.0;
const HINT_FONT_SIZE: f32 = 10.0;

/// The title font, in one place. The active tab is set a weight heavier, as
/// macOS does — `medium` picks [`fonts::UI_MEDIUM_FAMILY`], which only really
/// differs when the system face loaded (see [`fonts::has_real_ui_medium`]).
fn title_font(medium: bool) -> FontId {
    let family = if medium {
        crate::fonts::UI_MEDIUM_FAMILY
    } else {
        crate::fonts::UI_FAMILY
    };
    FontId::new(FONT_SIZE, egui::FontFamily::Name(family.into()))
}

/// The `⌘n` hint font: same family as the title, smaller.
fn hint_font() -> FontId {
    FontId::new(
        HINT_FONT_SIZE,
        egui::FontFamily::Name(crate::fonts::UI_FAMILY.into()),
    )
}

const CLOSE_RADIUS: f32 = 8.0;
const CLOSE_INSET: f32 = 4.0;
const CLOSE_ARM: f32 = 3.0;
const HINT_PAD: f32 = 8.0;

/// How much of the tab height the hairline between two inactive tabs spans.
const SEPARATOR_HEIGHT: f32 = 0.5;

const BAR_BG: Color32 = Color32::from_rgb(0x1c, 0x1c, 0x1e);
const BAR_LINE: Color32 = Color32::from_rgb(0x2a, 0x2a, 0x2e);
/// Sampled from Ghostty's active tab: a light grey capsule with a slightly
/// lighter edge, both far above the bar.
const TAB_ACTIVE_BG: Color32 = Color32::from_rgb(0x4a, 0x4a, 0x4f);
const TAB_ACTIVE_EDGE: Color32 = Color32::from_rgb(0x5c, 0x5c, 0x62);
/// Barely off the bar — inactive tabs read as one continuous strip.
const TAB_IDLE_BG: Color32 = Color32::from_rgb(0x1f, 0x1f, 0x21);
const TAB_HOVER_BG: Color32 = Color32::from_rgb(0x27, 0x27, 0x2a);
/// Darker than the bar: the seam between two inactive tabs.
const TAB_SEPARATOR: Color32 = Color32::from_rgb(0x14, 0x14, 0x16);
const CLOSE_HOVER_BG: Color32 = Color32::from_rgb(0x63, 0x63, 0x6b);
const PLUS_HOVER_BG: Color32 = Color32::from_rgb(0x2e, 0x2e, 0x33);
/// The + button's enclosing circle (Ghostty-style).
const PLUS_CIRCLE_RADIUS: f32 = 10.5;
const PLUS_CIRCLE_EDGE: Color32 = Color32::from_rgb(0x45, 0x45, 0x4a);
const TEXT_ACTIVE: Color32 = Color32::from_rgb(0xef, 0xef, 0xf4);
const TEXT_IDLE: Color32 = Color32::from_rgb(0xb6, 0xb6, 0xbe);
/// Title colours, sampled off Ghostty's native tab bar: the active title is
/// literally white (its glyph cores hit `#ffffff`), the inactive ones a mid
/// grey. Kept apart from `TEXT_*`, which also tint the `×` and `+` glyphs.
const TITLE_ACTIVE: Color32 = Color32::WHITE;
const TITLE_IDLE: Color32 = Color32::from_rgb(0xb0, 0xb0, 0xb6);
const TEXT_HINT: Color32 = Color32::from_rgb(0x6a, 0x6a, 0x74);

/// A newly opened tab widens from nothing to its slot in this long, and a
/// closing one shrinks back to nothing just as fast.
const GROW_TIME: f32 = 0.14;
/// Becoming active is near-instant (the click/keypress must feel answered);
/// releasing the active state eases out, which carries the smoothness.
const ACTIVE_IN_TIME: f32 = 0.03;
const ACTIVE_OUT_TIME: f32 = 0.14;
/// How long a tab takes to slide to a new slot (reorder, or a neighbour making
/// room). Long enough to read as motion, short enough to feel direct.
const SLIDE_TIME: f32 = 0.15;
/// The hover `×` fades in over this long when the pointer enters a tab, and
/// back out when it leaves, instead of popping in and out.
const CLOSE_FADE_TIME: f32 = 0.12;
/// How long an inactive tab takes to reach its hover fill, and to leave it.
const HOVER_FADE_TIME: f32 = 0.11;
/// Below this the `×` is too faint to be worth hit-testing on its own.
const CLOSE_HIT_ALPHA: f32 = 0.5;

/// Something the user asked for via keyboard, tab bar or palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppAction {
    NewTab,
    CloseActive,
    CloseTab(u64),
    SelectTab(u64),
    SelectNth(usize),
    NextTab,
    PrevTab,
    OpenPalette,
    RenameActive,
    Quit,
}

const fn cmd(key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(Modifiers::COMMAND, key)
}

fn cmd_shift(key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), key)
}

const DIGITS: [Key; 9] = [
    Key::Num1,
    Key::Num2,
    Key::Num3,
    Key::Num4,
    Key::Num5,
    Key::Num6,
    Key::Num7,
    Key::Num8,
    Key::Num9,
];

/// Consume terra's global shortcuts so the terminal widget never sees them.
///
/// More specific (Cmd+Shift+…) bindings are checked first, because
/// `consume_shortcut` matches modifiers logically and ignores an extra Shift.
pub fn consume_shortcuts(ui: &mut Ui) -> Vec<AppAction> {
    let mut actions = Vec::new();
    ui.input_mut(|i| {
        if i.consume_shortcut(&cmd_shift(Key::P)) {
            actions.push(AppAction::OpenPalette);
        }
        if i.consume_shortcut(&cmd_shift(Key::OpenBracket)) {
            actions.push(AppAction::PrevTab);
        }
        if i.consume_shortcut(&cmd_shift(Key::CloseBracket)) {
            actions.push(AppAction::NextTab);
        }
        if i.consume_shortcut(&cmd(Key::T)) {
            actions.push(AppAction::NewTab);
        }
        if i.consume_shortcut(&cmd(Key::W)) {
            actions.push(AppAction::CloseActive);
        }
        for (idx, key) in DIGITS.iter().enumerate() {
            if i.consume_shortcut(&cmd(*key)) {
                actions.push(AppAction::SelectNth(idx));
            }
        }
    });
    actions
}

/// Width of a single tab: the bar minus the `+` zone, split evenly.
///
/// `n` is the number of tabs. The result is clamped to [`MIN_TAB_WIDTH`], in
/// which case the row overflows and is clipped instead of collapsing to slivers.
fn tab_width(bar_width: f32, n: usize) -> f32 {
    if n == 0 {
        return 0.0;
    }
    let n_f = n as f32;
    let usable = bar_width - PLUS_WIDTH - PLUS_GAP;
    let per = (usable - TAB_GAP * (n_f - 1.0)) / n_f;
    per.max(MIN_TAB_WIDTH)
}

// ---------------------------------------------------------------------------
// Animation
// ---------------------------------------------------------------------------

/// One eased scalar in flight, from `from` to `to` starting at `start`.
///
/// egui's own `animate_value_with_time` interpolates linearly and cannot be
/// seeded, so the bar keeps its own tween: it needs an ease-out curve, an
/// explicit "start at zero width" for tabs that did not exist last frame, and
/// the ability to snap a value to the pointer mid-drag.
#[derive(Clone, Copy)]
struct Anim {
    from: f32,
    to: f32,
    start: f64,
}

impl Anim {
    fn value(&self, now: f64, duration: f32) -> f32 {
        if duration <= 0.0 {
            return self.to;
        }
        let t = (((now - self.start) as f32) / duration).clamp(0.0, 1.0);
        // Ease-out cubic: quick off the mark, gentle at the destination.
        let eased = 1.0 - (1.0 - t).powi(3);
        self.from + (self.to - self.from) * eased
    }
}

/// What an [`Anim`] describes, so widths and positions of the same tab do not
/// collide in [`BarState::anims`].
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Track {
    Width,
    X,
}

/// A tab that is gone from the model but still shrinking away on screen.
#[derive(Clone)]
struct Ghost {
    id: u64,
    title: String,
    /// Slot it occupied when it disappeared; where it keeps shrinking.
    index: usize,
}

/// The tab currently held by the pointer.
#[derive(Clone, Copy)]
struct Drag {
    id: u64,
    /// Pointer offset inside the tab when the drag started, so the tab does not
    /// jump to centre itself under the cursor.
    grab: f32,
}

/// Everything the bar remembers between frames, kept in egui's temporary data.
#[derive(Clone, Default)]
struct BarState {
    anims: HashMap<(Track, u64), Anim>,
    /// Tabs drawn last frame, in order, with their titles — the baseline for
    /// spotting opens (grow in) and closes (leave a [`Ghost`] behind).
    live: Vec<(u64, String)>,
    ghosts: Vec<Ghost>,
    drag: Option<Drag>,
}

impl BarState {
    /// Move `key` towards `target` and read it back. Retargets mid-flight from
    /// wherever the value currently is, so nothing ever snaps.
    fn animate(&mut self, key: (Track, u64), target: f32, duration: f32, now: f64) -> f32 {
        let anim = self.anims.entry(key).or_insert(Anim {
            from: target,
            to: target,
            start: now,
        });
        if (anim.to - target).abs() > 0.01 {
            let current = anim.value(now, duration);
            *anim = Anim {
                from: current,
                to: target,
                start: now,
            };
        }
        anim.value(now, duration)
    }

    /// Force `key` to `value` with no animation — for a tab that has just
    /// appeared, or one being dragged (which follows the pointer exactly, and
    /// must carry on from there once released).
    fn seed(&mut self, key: (Track, u64), value: f32, now: f64) {
        self.anims.insert(
            key,
            Anim {
                from: value,
                to: value,
                start: now,
            },
        );
    }

    fn forget(&mut self, id: u64) {
        self.anims.remove(&(Track::Width, id));
        self.anims.remove(&(Track::X, id));
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// Lay out `text`, shortening it in the middle with `…` until it fits `max_width`.
fn middle_truncated(ui: &Ui, text: &str, font: FontId, max_width: f32) -> (Arc<Galley>, bool) {
    let layout = |s: String| {
        ui.painter()
            .layout_no_wrap(s, font.clone(), Color32::PLACEHOLDER)
    };

    let full = layout(text.to_owned());
    if full.size().x <= max_width {
        return (full, false);
    }

    let chars: Vec<char> = text.chars().collect();
    for keep in (1..chars.len()).rev() {
        let head = keep.div_ceil(2);
        let tail = keep - head;
        let mut candidate: String = chars[..head].iter().collect();
        candidate.push('…');
        candidate.extend(chars[chars.len() - tail..].iter());
        let galley = layout(candidate);
        if galley.size().x <= max_width {
            return (galley, true);
        }
    }
    (layout("…".to_owned()), true)
}

/// Mix two opaque colours channel-wise; `t` of 0 is `a`, 1 is `b`.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let chan = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(chan(a.r(), b.r()), chan(a.g(), b.g()), chan(a.b(), b.b()))
}

/// The `×` hit area on the left inside edge of a tab.
fn close_rect(tab: Rect) -> Rect {
    let center = egui::pos2(tab.left() + CLOSE_INSET + CLOSE_RADIUS, tab.center().y);
    Rect::from_center_size(center, Vec2::splat(CLOSE_RADIUS * 2.0))
}

/// Fake a medium weight by painting `galley` a second time, a third of a pixel
/// to the right, so every stem picks up a sliver of extra coverage.
///
/// Only for the *active* title, and only when no real medium face is available
/// ([`fonts::has_real_ui_medium`]) — doubling a face that is already heavier
/// would smear it. The offset is sub-pixel on purpose: it thickens without
/// widening, so the caller's centring and truncation still hold.
fn paint_faux_medium(
    painter: &egui::Painter,
    pos: egui::Pos2,
    galley: &Arc<Galley>,
    color: Color32,
) {
    painter.galley(pos + Vec2::new(0.3, 0.0), galley.clone(), color);
}

/// Everything drawing one tab needs to know about it.
struct TabVisual<'a> {
    rect: Rect,
    id: u64,
    /// Visual position, for the `⌘n` hint.
    index: usize,
    title: &'a str,
    active: bool,
    dragged: bool,
    /// Draw the segmented-control hairline on this tab's right edge.
    separator: bool,
    /// Suppress the hover `×` while any tab is being dragged.
    closable: bool,
}

/// Paint the body of a tab: fill, title, `⌘n` hint, separator. Returns whether
/// the title had to be shortened, so the caller can offer the full one on hover.
///
/// `hover_t` eases the inactive fill between idle and hover; the active capsule
/// keeps its own colour throughout.
fn paint_tab(ui: &Ui, v: &TabVisual<'_>, hover_t: f32, active_t: f32) -> bool {
    let painter = ui.painter();
    // The active state crossfades in/out (Ghostty-smooth) instead of snapping.
    let bg = mix(
        mix(TAB_IDLE_BG, TAB_HOVER_BG, hover_t),
        TAB_ACTIVE_BG,
        active_t,
    );
    let radius = CornerRadius::same(CORNER);
    painter.rect_filled(v.rect, radius, bg);
    if active_t > 0.02 {
        // A hair of light along the capsule's edge, as Ghostty has.
        painter.rect_stroke(
            v.rect,
            radius,
            Stroke::new(1.0, TAB_ACTIVE_EDGE.gamma_multiply(active_t)),
            egui::StrokeKind::Inside,
        );
    }

    // Seam between two inactive neighbours. Drawn just inside this tab's right
    // edge so the next tab's fill (painted after) cannot swallow it.
    if v.separator {
        let x = v.rect.right() - 0.5;
        let half = v.rect.height() * SEPARATOR_HEIGHT / 2.0;
        painter.vline(
            x,
            (v.rect.center().y - half)..=(v.rect.center().y + half),
            Stroke::new(1.0, TAB_SEPARATOR),
        );
    }

    let fg = mix(TITLE_IDLE, TITLE_ACTIVE, active_t);

    // Centred, middle-truncated title.
    let max_text = (v.rect.width() - TITLE_RESERVE * 2.0).max(8.0);
    let (galley, truncated) = middle_truncated(ui, v.title, title_font(v.active), max_text);
    let pos = egui::pos2(
        v.rect.center().x - galley.size().x / 2.0,
        v.rect.center().y - galley.size().y / 2.0,
    );
    if v.active && !crate::fonts::has_real_ui_medium() {
        paint_faux_medium(painter, pos, &galley, fg);
    }
    painter.galley(pos, galley, fg);

    // Dimmed ⌘n hint for the first nine tabs, when there is room for it.
    if v.index < 9 && v.rect.width() >= HINT_MIN_TAB_WIDTH {
        let hint = format!("⌘{}", v.index + 1);
        let galley = painter.layout_no_wrap(hint, hint_font(), Color32::PLACEHOLDER);
        let pos = egui::pos2(
            v.rect.right() - HINT_PAD - galley.size().x,
            v.rect.center().y - galley.size().y / 2.0,
        );
        painter.galley(pos, galley, TEXT_HINT);
    }

    truncated
}

/// Paint the hover-only `×` at `alpha`, which fades it with the pointer.
fn paint_close(ui: &Ui, rect: Rect, hovered: bool, fg: Color32, alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let painter = ui.painter();
    let c = close_rect(rect).center();
    if hovered {
        painter.circle_filled(c, CLOSE_RADIUS, CLOSE_HOVER_BG.gamma_multiply(alpha));
    }
    let color = if hovered { TEXT_ACTIVE } else { fg };
    let stroke = Stroke::new(1.2, color.gamma_multiply(alpha));
    painter.line_segment(
        [
            egui::pos2(c.x - CLOSE_ARM, c.y - CLOSE_ARM),
            egui::pos2(c.x + CLOSE_ARM, c.y + CLOSE_ARM),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(c.x - CLOSE_ARM, c.y + CLOSE_ARM),
            egui::pos2(c.x + CLOSE_ARM, c.y - CLOSE_ARM),
        ],
        stroke,
    );
}

/// Draw one live tab and turn its clicks into actions. Returns its response so
/// the caller can start a drag from it.
fn tab(ui: &mut Ui, v: &TabVisual<'_>, actions: &mut Vec<AppAction>) -> egui::Response {
    let response = ui
        .interact(
            v.rect,
            ui.id().with(("terra_tab", v.id)),
            Sense::click_and_drag(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);

    // Both fades are driven by egui's own animation manager, keyed per tab, so
    // they survive the tab moving slot and are dropped once the tab is gone.
    let pointer_in = response.contains_pointer();
    let close_alpha = ui.ctx().animate_bool_with_time(
        ui.id().with(("terra_tab_close_fade", v.id)),
        v.closable && pointer_in,
        CLOSE_FADE_TIME,
    );
    let hover_t = ui.ctx().animate_bool_with_time(
        ui.id().with(("terra_tab_hover_fade", v.id)),
        pointer_in || v.dragged,
        HOVER_FADE_TIME,
    );

    // The close button is registered *after* the tab, so it sits on top of it
    // and swallows the click instead of selecting the tab. A `×` on its way out
    // stays visible but stops taking clicks once the pointer has left the tab.
    let close_hits = v.closable && (pointer_in || close_alpha > CLOSE_HIT_ALPHA);
    let close = close_hits.then(|| {
        ui.interact(
            close_rect(v.rect),
            ui.id().with(("terra_tab_close", v.id)),
            Sense::click(),
        )
    });
    let close_hovered = close.as_ref().is_some_and(|r| r.hovered());

    if ui.is_rect_visible(v.rect) {
        let active_t = ui.ctx().animate_bool_with_time(
            ui.id().with(("terra_tab_active", v.id)),
            v.active,
            if v.active {
                ACTIVE_IN_TIME
            } else {
                ACTIVE_OUT_TIME
            },
        );
        let truncated = paint_tab(ui, v, hover_t, active_t);
        let fg = mix(TEXT_IDLE, TEXT_ACTIVE, active_t);
        paint_close(ui, v.rect, close_hovered, fg, close_alpha);
        if truncated && !v.dragged {
            response.clone().on_hover_text(v.title);
        }
    }

    // Closing must not select the tab first.
    if let Some(close) = close {
        if close.clicked() {
            actions.push(AppAction::CloseTab(v.id));
            return response;
        }
    }
    // Select on mouse DOWN (like native macOS tabs) so switching feels
    // instant; never on a press that lands on the close button.
    let pressed_here = response.is_pointer_button_down_on()
        && ui.input(|i| i.pointer.primary_pressed())
        && !close_hovered;
    if pressed_here || response.clicked() {
        actions.push(AppAction::SelectTab(v.id));
    }
    if response.clicked_by(PointerButton::Middle) {
        actions.push(AppAction::CloseTab(v.id));
    }
    response
}

/// Draw the `+` button in its fixed zone at the far right of the bar.
fn plus_button(ui: &mut Ui, rect: Rect, actions: &mut Vec<AppAction>) {
    let response = ui
        .interact(rect, ui.id().with("terra_new_tab"), Sense::click())
        .on_hover_text("New tab  ⌘T");

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let c = rect.center();
        // Ghostty draws the + inside its own small circle.
        let r = PLUS_CIRCLE_RADIUS;
        if response.hovered() {
            painter.circle_filled(c, r, PLUS_HOVER_BG);
        }
        painter.circle_stroke(c, r, Stroke::new(1.0, PLUS_CIRCLE_EDGE));
        let arm = 4.5;
        let color = if response.hovered() {
            TEXT_ACTIVE
        } else {
            TEXT_IDLE
        };
        let stroke = Stroke::new(1.3, color);
        painter.line_segment(
            [egui::pos2(c.x - arm, c.y), egui::pos2(c.x + arm, c.y)],
            stroke,
        );
        painter.line_segment(
            [egui::pos2(c.x, c.y - arm), egui::pos2(c.x, c.y + arm)],
            stroke,
        );
    }

    if response.clicked() {
        actions.push(AppAction::NewTab);
    }
}

/// Whether the bar is worth showing at all: like Ghostty, a lone tab gets no
/// chrome and the terminal owns the full window height.
pub fn bar_visible(tab_count: usize) -> bool {
    tab_count >= 2
}

/// Which slot index a tab dragged to `x` wants, given the pitch of the row.
fn drop_index(x: f32, bar_left: f32, pitch: f32, count: usize) -> usize {
    if pitch <= 0.0 || count == 0 {
        return 0;
    }
    let raw = ((x - bar_left) / pitch).round();
    raw.clamp(0.0, (count - 1) as f32) as usize
}

/// One row entry: a live tab, or a ghost of one that just closed.
struct Slot {
    id: u64,
    title: String,
    ghost: bool,
}

fn slot_title(tabs: &TabManager, id: u64) -> String {
    let title = tabs.title(id).unwrap_or("");
    if title.trim().is_empty() {
        "shell".to_string()
    } else {
        title.to_string()
    }
}

/// Draw the top tab bar. Appends any user interaction to `actions`.
///
/// With fewer than two tabs nothing is drawn and no space is taken, so the
/// terminal below simply grows into the whole viewport. Keyboard shortcuts are
/// handled by [`consume_shortcuts`] and keep working with the bar hidden.
pub fn tab_bar(ui: &mut Ui, tabs: &TabManager, actions: &mut Vec<AppAction>) {
    let state_id = Id::new("terra_tab_bar_state");
    if !bar_visible(tabs.ids().len()) {
        // Nothing on screen to continue from: drop the animations so the bar
        // comes back settled rather than mid-flight from minutes ago.
        ui.ctx().data_mut(|d| d.remove::<BarState>(state_id));
        return;
    }

    let mut state: BarState = ui
        .ctx()
        .data_mut(|d| d.get_temp(state_id))
        .unwrap_or_default();

    let frame = egui::Frame::NONE
        .fill(BAR_BG)
        .inner_margin(egui::Margin {
            left: PAD_X,
            right: PAD_X,
            top: PAD_Y,
            bottom: PAD_Y,
        })
        .stroke(Stroke::NONE);

    egui::Panel::top("terra_tab_bar")
        .exact_size(TAB_BAR_HEIGHT)
        .show_separator_line(false)
        .frame(frame)
        .show(ui, |ui| {
            let bar = ui.available_rect_before_wrap();
            ui.allocate_rect(bar, Sense::hover());
            ui.set_clip_rect(bar);
            let now = ui.input(|i| i.time);
            let plus_left = bar.right() - PLUS_WIDTH;
            let tabs_right = plus_left - PLUS_GAP;
            let width = tab_width(bar.width(), tabs.ids().len());

            // 1. Carry an in-progress drag first, so the rest of the frame lays
            //    out the order the pointer is asking for, with no lag.
            let drag_x = drive_drag(ui, tabs, &mut state, bar, tabs_right, width);

            // 2. Tabs that vanished since last frame linger as shrinking ghosts.
            let ids = tabs.ids();
            let live: Vec<(u64, String)> =
                ids.iter().map(|id| (*id, slot_title(tabs, *id))).collect();
            for (index, (id, title)) in state.live.iter().enumerate() {
                if !ids.contains(id) && !state.ghosts.iter().any(|g| g.id == *id) {
                    state.ghosts.push(Ghost {
                        id: *id,
                        title: title.clone(),
                        index,
                    });
                }
            }

            let mut slots: Vec<Slot> = live
                .iter()
                .map(|(id, title)| Slot {
                    id: *id,
                    title: title.clone(),
                    ghost: false,
                })
                .collect();
            let mut ghosts = state.ghosts.clone();
            ghosts.sort_by_key(|g| g.index);
            for ghost in &ghosts {
                let at = ghost.index.min(slots.len());
                slots.insert(
                    at,
                    Slot {
                        id: ghost.id,
                        title: ghost.title.clone(),
                        ghost: true,
                    },
                );
            }

            // 3. Widths first: every slot's width is animated, and positions
            //    fall out of the running sum, so opening a tab pushes its
            //    neighbours aside instead of teleporting them.
            let mut rects: Vec<Rect> = Vec::with_capacity(slots.len());
            let mut settled = true;
            let mut cursor = bar.left();
            for slot in &slots {
                let target_w = if slot.ghost { 0.0 } else { width };
                let is_new = !slot.ghost && !state.live.iter().any(|(id, _)| *id == slot.id);
                if is_new {
                    // Born at its slot with no width at all, then grows.
                    state.seed((Track::Width, slot.id), 0.0, now);
                    state.seed((Track::X, slot.id), cursor, now);
                }
                let w = state.animate((Track::Width, slot.id), target_w, GROW_TIME, now);
                let target_x = cursor;
                cursor += w + TAB_GAP;

                let x = match drag_x {
                    Some((id, x)) if id == slot.id => {
                        // The dragged tab is pinned to the pointer; seeding
                        // means it carries on from here when released.
                        state.seed((Track::X, slot.id), x, now);
                        x
                    }
                    _ => state.animate((Track::X, slot.id), target_x, SLIDE_TIME, now),
                };
                if (w - target_w).abs() > 0.05 || (x - target_x).abs() > 0.05 {
                    settled = false;
                }
                rects.push(Rect::from_min_size(
                    egui::pos2(x, bar.top()),
                    Vec2::new(w, bar.height()),
                ));
            }

            // 4. Paint. Inactive tabs first so the active capsule and the
            //    dragged tab overlap them rather than the other way round.
            let active = tabs.active_id();
            let dragged = state.drag.map(|d| d.id);
            let is_plain = |i: usize| {
                !slots[i].ghost && Some(slots[i].id) != active && Some(slots[i].id) != dragged
            };
            let mut draw_order: Vec<usize> = (0..slots.len()).collect();
            draw_order.sort_by_key(|&i| {
                if Some(slots[i].id) == dragged {
                    2
                } else if Some(slots[i].id) == active {
                    1
                } else {
                    0
                }
            });

            let mut index = 0usize; // ⌘n counts live tabs only.
            let mut live_index: HashMap<u64, usize> = HashMap::new();
            for slot in &slots {
                if !slot.ghost {
                    live_index.insert(slot.id, index);
                    index += 1;
                }
            }

            let mut started_drag = None;
            for i in draw_order {
                let slot = &slots[i];
                let rect = rects[i];
                if rect.right() <= bar.left() || rect.left() >= tabs_right || rect.width() < 0.5 {
                    continue;
                }
                let visual = TabVisual {
                    rect,
                    id: slot.id,
                    index: live_index.get(&slot.id).copied().unwrap_or(usize::MAX),
                    title: &slot.title,
                    active: Some(slot.id) == active,
                    dragged: Some(slot.id) == dragged,
                    separator: is_plain(i) && slots.get(i + 1).is_some_and(|_| is_plain(i + 1)),
                    closable: dragged.is_none(),
                };
                if slot.ghost {
                    // No hit target for something that is on its way out.
                    paint_tab(ui, &visual, 0.0, 0.0);
                    continue;
                }
                let response = tab(ui, &visual, actions);
                if response.drag_started_by(PointerButton::Primary) {
                    if let Some(pos) = response.interact_pointer_pos() {
                        started_drag = Some(Drag {
                            id: slot.id,
                            grab: pos.x - rect.left(),
                        });
                    }
                }
            }
            if let Some(drag) = started_drag {
                actions.push(AppAction::SelectTab(drag.id));
                state.drag = Some(drag);
            }

            // 5. Retire ghosts that have shrunk away, and remember this frame.
            for (i, slot) in slots.iter().enumerate() {
                if slot.ghost && rects[i].width() < 0.5 {
                    state.ghosts.retain(|g| g.id != slot.id);
                    state.forget(slot.id);
                }
            }
            state.live = live;
            if !settled {
                ui.ctx().request_repaint();
            }

            let plus = Rect::from_min_size(
                egui::pos2(plus_left, bar.top()),
                Vec2::new(PLUS_WIDTH, bar.height()),
            );
            plus_button(ui, plus, actions);
        });

    ui.ctx().data_mut(|d| d.insert_temp(state_id, state));

    // Hairline under the bar, drawn on top of the terminal's own background.
    let bar_bottom = ui.max_rect().top();
    ui.painter().hline(
        ui.max_rect().x_range(),
        bar_bottom,
        Stroke::new(1.0, BAR_LINE),
    );
}

/// Advance a drag started on an earlier frame: follow the pointer, reorder the
/// tabs it crosses, and end when the button comes up. Returns where the dragged
/// tab should be painted this frame.
fn drive_drag(
    ui: &Ui,
    tabs: &TabManager,
    state: &mut BarState,
    bar: Rect,
    tabs_right: f32,
    width: f32,
) -> Option<(u64, f32)> {
    let drag = state.drag?;
    if tabs.index_of(drag.id).is_none() {
        // The tab was closed under the pointer.
        state.drag = None;
        return None;
    }
    let down = ui.input(|i| i.pointer.primary_down());
    let pointer = ui.input(|i| i.pointer.interact_pos()).map(|p| p.x);
    let (true, Some(pointer_x)) = (down, pointer) else {
        // Released: the tab animates from wherever it is to its slot.
        state.drag = None;
        return None;
    };

    let max_x = (tabs_right - width).max(bar.left());
    let x = (pointer_x - drag.grab).clamp(bar.left(), max_x);
    let count = tabs.ids().len();
    tabs.move_tab(drag.id, drop_index(x, bar.left(), width + TAB_GAP, count));
    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    Some((drag.id, x))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_share_the_bar_minus_the_plus_zone() {
        // 800 wide bar, 4 tabs: 800 - 28 - 6 = 766 usable, split evenly.
        let w = tab_width(800.0, 4);
        assert!((w - (766.0 - 3.0 * TAB_GAP) / 4.0).abs() < 0.01);
        // Tabs plus gaps plus the `+` zone exactly fill the bar.
        assert!((4.0 * w + 3.0 * TAB_GAP + PLUS_GAP + PLUS_WIDTH - 800.0).abs() < 0.01);
    }

    #[test]
    fn a_single_tab_takes_the_whole_bar() {
        let w = tab_width(400.0, 1);
        assert!((w + PLUS_GAP + PLUS_WIDTH - 400.0).abs() < 0.01);
    }

    #[test]
    fn the_bar_hides_for_a_lone_tab() {
        assert!(!bar_visible(0));
        assert!(!bar_visible(1));
        assert!(bar_visible(2));
    }

    #[test]
    fn very_many_tabs_stop_shrinking() {
        assert_eq!(tab_width(300.0, 40), MIN_TAB_WIDTH);
        assert_eq!(tab_width(0.0, 0), 0.0);
    }

    /// A dragged tab lands in the slot it covers most, and never outside the row.
    #[test]
    fn a_dragged_tab_snaps_to_the_nearest_slot() {
        let pitch = 100.0;
        assert_eq!(drop_index(0.0, 0.0, pitch, 4), 0);
        assert_eq!(drop_index(49.0, 0.0, pitch, 4), 0);
        assert_eq!(drop_index(51.0, 0.0, pitch, 4), 1);
        assert_eq!(drop_index(220.0, 0.0, pitch, 4), 2);
        // Clamped to the row, both ends, and offset by the bar's own left edge.
        assert_eq!(drop_index(9000.0, 0.0, pitch, 4), 3);
        assert_eq!(drop_index(-9000.0, 0.0, pitch, 4), 0);
        assert_eq!(drop_index(160.0, 10.0, pitch, 4), 2);
        assert_eq!(drop_index(10.0, 0.0, 0.0, 4), 0);
    }

    /// The hover fill travels from idle to hover and stops at both ends, so a
    /// tab at rest is exactly `TAB_IDLE_BG` and a fully hovered one exactly
    /// `TAB_HOVER_BG`.
    #[test]
    fn the_hover_fill_eases_between_the_two_tab_colours() {
        assert_eq!(mix(TAB_IDLE_BG, TAB_HOVER_BG, 0.0), TAB_IDLE_BG);
        assert_eq!(mix(TAB_IDLE_BG, TAB_HOVER_BG, 1.0), TAB_HOVER_BG);
        // Out-of-range values clamp rather than overshoot past either colour.
        assert_eq!(mix(TAB_IDLE_BG, TAB_HOVER_BG, -1.0), TAB_IDLE_BG);
        assert_eq!(mix(TAB_IDLE_BG, TAB_HOVER_BG, 2.0), TAB_HOVER_BG);

        let half = mix(TAB_IDLE_BG, TAB_HOVER_BG, 0.5);
        assert!(half.r() > TAB_IDLE_BG.r() && half.r() < TAB_HOVER_BG.r());
        assert_eq!(half.a(), 255);
    }

    /// Animated values ease out: past the halfway point in time, more than half
    /// the distance is done, and they land exactly on the target.
    #[test]
    fn animations_ease_out_and_settle() {
        let mut state = BarState::default();
        let key = (Track::Width, 7);
        // First sight of a value never animates.
        assert_eq!(state.animate(key, 100.0, GROW_TIME, 0.0), 100.0);

        state.seed(key, 0.0, 0.0);
        assert_eq!(state.animate(key, 100.0, GROW_TIME, 0.0), 0.0);
        let half = state.animate(key, 100.0, GROW_TIME, (GROW_TIME / 2.0) as f64);
        assert!(half > 50.0 && half < 100.0, "eased out, not linear: {half}");
        assert_eq!(state.animate(key, 100.0, GROW_TIME, 10.0), 100.0);

        // Retargeting mid-flight starts from the current value, not the origin.
        state.seed(key, 0.0, 0.0);
        let mid = state.animate(key, 100.0, GROW_TIME, (GROW_TIME / 4.0) as f64);
        let retargeted = state.animate(key, 0.0, GROW_TIME, (GROW_TIME / 4.0) as f64);
        assert!((retargeted - mid).abs() < 0.01);
    }
}
