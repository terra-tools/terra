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
//!
//! A drag that leaves its own bar goes cross-group, VS Code style: the pill
//! turns into a floating ghost under the pointer, another group's bar accepts
//! it as a move, and any of the four halves of a terminal (left/right for a
//! side-by-side split, top/bottom for a stacked one) accepts it as a split —
//! see
//! [`tab_drag_overlay`], which `main.rs` runs once per frame over the whole
//! window after the columns are laid out.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{
    Color32, CornerRadius, FontId, Galley, Id, Key, KeyboardShortcut, Modifiers, PointerButton,
    Rect, Sense, Stroke, Ui, Vec2,
};

use crate::config::Profile;
use crate::tab_icon::{IconCache, TabIcon};
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
/// Fixed width reserved to the right of `+` for the `⌄` profile menu.
/// Narrower than `+`: it is a disclosure affordance hanging off the button
/// next to it, the way Windows Terminal's is, not a peer of it.
const CHEVRON_WIDTH: f32 = 20.0;
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

/// Side of the square a tab icon is drawn in — the title's own size, so the
/// logo reads as part of the label rather than as a bullet next to it.
const ICON_SIZE: f32 = 13.0;
/// Gap between the icon and the first letter of the title.
const ICON_GAP: f32 = 5.0;
/// How far back the generic `>_` glyph is faded relative to the title.
///
/// It carries no information — it is what terra draws when it has nothing to
/// say — so it holds the title's position without competing with the tabs that
/// do say something.
const GENERIC_ICON_ALPHA: f32 = 0.5;
/// How far back a brand icon is faded on an *inactive* pill. Colour already
/// makes these loud; the active tab keeps them at full strength.
const IDLE_ICON_ALPHA: f32 = 0.8;

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

/// How far above/below its bar a drag may stray and still count as an in-bar
/// reorder. Past this the pill detaches into a floating ghost and the drop
/// targets (other bars, terminal halves) take over.
const BAR_DRAG_SLACK: f32 = 14.0;
/// Width of the floating ghost pill that follows the pointer mid-drag.
const GHOST_WIDTH: f32 = 150.0;
/// VS Code's `editorGroup.dropBackground`: a translucent blue wash over the
/// half of the terminal the drop would split into.
const DROP_ZONE_FILL: Color32 = Color32::from_rgba_premultiplied(0x14, 0x24, 0x3c, 0x50);
const DROP_ZONE_EDGE: Color32 = Color32::from_rgb(0x3d, 0x6e, 0xc7);

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
    /// Focus a group (column); its active tab becomes the globally active tab.
    /// Pushed by any click that lands inside the group's column.
    FocusGroup(usize),
    /// Split the active tab into a new group to the right of its own.
    SplitRight,
    /// Split the active tab into a new group to the left of its own.
    SplitLeft,
    /// Split the active tab into a new group below its own.
    SplitDown,
    /// Split the active tab into a new group above its own.
    SplitUp,
    /// Focus the next group in DFS order (wrapping).
    NextGroup,
    /// Focus the previous group in DFS order (wrapping).
    PrevGroup,
    OpenPalette,
    RenameActive,
    /// Flip UAX #9 right-to-left reordering for the session.
    ToggleBidi,
    /// Cycle the BiDi paragraph direction: auto -> ltr -> rtl.
    CycleBidiBase,
    /// Open a tab from a named `[profile.<name>]` in the config.
    NewTabProfile(String),
    /// Nudge the terminal font size for the session. `+1.0` / `-1.0`.
    NudgeFontSize(i8),
    /// Drop a dragged tab onto another group's bar: move it there, at `index`.
    MoveTab {
        id: u64,
        group: usize,
        index: usize,
    },
    /// Drop a dragged tab onto a half of group `group`'s terminal: split that
    /// group towards `dir`, with the tab as the new leaf.
    SplitTab {
        id: u64,
        group: usize,
        dir: SplitDir,
    },
    /// Drop every session override, returning to what the file says.
    ResetSession,
    /// Re-read `~/.terra/config.toml`, keeping session overrides on top.
    ReloadConfig,
    ShowConfigWarnings,
    Quit,
}

/// Which side of a leaf a drop (or a split action) targets. Left/Right make
/// side-by-side columns (a `Horizontal` split in the model), Up/Down stack
/// rows (`Vertical`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDir {
    Left,
    Right,
    Up,
    Down,
}

const fn cmd(key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(Modifiers::COMMAND, key)
}

fn cmd_shift(key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), key)
}

fn cmd_alt(key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::ALT), key)
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
        if i.consume_shortcut(&cmd_shift(Key::B)) {
            actions.push(AppAction::ToggleBidi);
        }
        // VS Code's group keys: ⌘\ splits, ⌥⌘ arrows move focus between
        // groups — ←/↑ to the previous leaf in DFS order, →/↓ to the next
        // (order-based, not spatial). Checked before the plain-Cmd bindings
        // for the same reason as Cmd+Shift: an extra held modifier must not
        // fall through to them.
        for key in [Key::ArrowLeft, Key::ArrowUp] {
            if i.consume_shortcut(&cmd_alt(key)) {
                actions.push(AppAction::PrevGroup);
            }
        }
        for key in [Key::ArrowRight, Key::ArrowDown] {
            if i.consume_shortcut(&cmd_alt(key)) {
                actions.push(AppAction::NextGroup);
            }
        }
        if i.consume_shortcut(&cmd(Key::Backslash)) {
            actions.push(AppAction::SplitRight);
        }
        // Both `=` and `+` so the shortcut works without reaching for Shift.
        for key in [Key::Plus, Key::Equals] {
            if i.consume_shortcut(&cmd(key)) {
                actions.push(AppAction::NudgeFontSize(1));
            }
        }
        if i.consume_shortcut(&cmd(Key::Minus)) {
            actions.push(AppAction::NudgeFontSize(-1));
        }
        if i.consume_shortcut(&cmd(Key::Num0)) {
            actions.push(AppAction::ResetSession);
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

/// Width of a single tab: the bar minus the `+` and `⌄` zone, split evenly.
///
/// `n` is the number of tabs. The result is clamped to [`MIN_TAB_WIDTH`], in
/// which case the row overflows and is clipped instead of collapsing to slivers.
fn tab_width(bar_width: f32, n: usize) -> f32 {
    if n == 0 {
        return 0.0;
    }
    let n_f = n as f32;
    let usable = bar_width - PLUS_WIDTH - CHEVRON_WIDTH - PLUS_GAP;
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
    /// The icon it had when it closed. Carried so a tab shrinking away keeps
    /// its face for the two frames it is still visible.
    icon: Option<TabIcon>,
    /// Slot it occupied when it disappeared; where it keeps shrinking.
    index: usize,
}

/// The tab currently held by the pointer. One per window, not per bar — a drag
/// crosses group boundaries, so every bar (and the drop overlay) reads the same
/// state, kept in egui's temp data under [`drag_state_id`].
#[derive(Clone, Copy)]
struct TabDrag {
    id: u64,
    /// Pointer offset inside the tab when the drag started, so the tab does not
    /// jump to centre itself under the cursor.
    grab: f32,
}

fn drag_state_id() -> Id {
    Id::new("terra_tab_drag")
}

fn current_drag(ctx: &egui::Context) -> Option<TabDrag> {
    ctx.data(|d| d.get_temp(drag_state_id()))
}

fn set_drag(ctx: &egui::Context, drag: Option<TabDrag>) {
    ctx.data_mut(|d| match drag {
        Some(drag) => {
            d.insert_temp(drag_state_id(), drag);
        }
        None => d.remove::<TabDrag>(drag_state_id()),
    });
}

/// Everything the bar remembers between frames, kept in egui's temporary data.
#[derive(Clone, Default)]
struct BarState {
    anims: HashMap<(Track, u64), Anim>,
    /// Tabs drawn last frame, in order, with everything a [`Ghost`] would need
    /// to keep drawing them — the baseline for spotting opens (grow in) and
    /// closes (leave a ghost behind).
    live: Vec<Slot>,
    ghosts: Vec<Ghost>,
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
fn middle_truncated(
    painter: &egui::Painter,
    text: &str,
    font: FontId,
    max_width: f32,
) -> (Arc<Galley>, bool) {
    let layout = |s: String| painter.layout_no_wrap(s, font.clone(), Color32::PLACEHOLDER);

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
    /// Logo for whatever is running here, or `None` with icons switched off.
    icon: Option<TabIcon>,
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

    // Centred, middle-truncated title, with the icon and its gap treated as
    // part of it: the pair is centred together, and the icon eats into the
    // width the title may use rather than overhanging it.
    let lead = if v.icon.is_some() {
        ICON_SIZE + ICON_GAP
    } else {
        0.0
    };
    let max_text = (v.rect.width() - TITLE_RESERVE * 2.0 - lead).max(8.0);
    let shown = crate::tab_icon::display_title(v.title, v.icon);
    let (galley, truncated) = middle_truncated(painter, shown, title_font(v.active), max_text);
    let left = v.rect.center().x - (galley.size().x + lead) / 2.0;
    let pos = egui::pos2(left + lead, v.rect.center().y - galley.size().y / 2.0);
    if let Some(icon) = v.icon {
        let alpha = if icon.is_generic() {
            GENERIC_ICON_ALPHA
        } else {
            // Brand icons only fade *out* to the idle level, and follow the
            // same crossfade as the title so nothing pops on tab switch.
            IDLE_ICON_ALPHA + (1.0 - IDLE_ICON_ALPHA) * active_t
        };
        let rect = Rect::from_min_size(
            egui::pos2(left, v.rect.center().y - ICON_SIZE / 2.0),
            Vec2::splat(ICON_SIZE),
        );
        crate::tab_icon::paint(ui, icon, rect, fg.gamma_multiply(alpha));
    }
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

/// Whether a group's bar is worth showing at all: like Ghostty, a lone tab in
/// the only group gets no chrome and the terminal owns the full column height.
/// The moment there is a second group, every group shows its bar — otherwise
/// a single-tab column would be indistinguishable from its neighbour.
pub fn bar_visible(tab_count: usize, group_count: usize) -> bool {
    group_count >= 2 || tab_count >= 2
}

// ---------------------------------------------------------------------------
// The `⌄` dropdown
// ---------------------------------------------------------------------------

/// One row of a dropdown: what it says, what it wears, and what choosing it
/// does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    pub label: String,
    /// The logo drawn to the left of the label, from the same set the pills
    /// use — so the row for an `htop` profile and the tab it opens carry the
    /// same mark.
    pub icon: TabIcon,
    pub action: AppAction,
}

impl MenuEntry {
    /// A row wearing the generic `>_`. Rows that can name a program build on
    /// this with [`Self::with_icon`].
    pub fn new(label: impl Into<String>, action: AppAction) -> Self {
        Self {
            label: label.into(),
            icon: TabIcon::Terminal,
            action,
        }
    }

    pub fn with_icon(mut self, icon: TabIcon) -> Self {
        self.icon = icon;
        self
    }
}

/// The entries the `⌄` next to a `+` offers: the default shell, then one row
/// per profile in name order.
///
/// Takes whole [`Profile`]s rather than names because the row wants an icon,
/// and what a profile *runs* is the only honest source for one — a profile
/// called `work` running `htop` is an htop row. The tab it opens will resolve
/// its own icon from the live process table a moment later; this is the same
/// guess made from the only thing known before the tab exists.
///
/// Split out from the drawing so the list is testable without a `Ui`, and so
/// whoever re-anchors the button only has to decide *where* it goes.
pub fn new_tab_entries<'a>(profiles: impl IntoIterator<Item = &'a Profile>) -> Vec<MenuEntry> {
    let mut entries = vec![MenuEntry::new("New Tab", AppAction::NewTab)];
    entries.extend(profiles.into_iter().map(|profile| {
        let mut text = profile.command.join(" ");
        if let Some(title) = &profile.title {
            text.push(' ');
            text.push_str(title);
        }
        MenuEntry::new(
            &profile.name,
            AppAction::NewTabProfile(profile.name.clone()),
        )
        .with_icon(crate::tab_icon::from_text(&text).unwrap_or(TabIcon::Terminal))
    }));
    entries
}

/// A `⌄` disclosure button and the menu it opens: `ui` + a rect + a list of
/// actions in, the chosen action out.
///
/// Deliberately knows nothing about the tab bar. Everything positional arrives
/// as `rect`, and everything offered arrives as `entries`, so re-anchoring the
/// same menu next to a per-group `+` is a matter of passing a different rect
/// and a different `salt` — no state of its own crosses frames beyond the
/// popup's own open flag, which egui keys on the button's id.
///
/// Escape and a click outside close the popup: that is egui's default
/// `PopupCloseBehavior` for a menu, and choosing a row closes it explicitly.
pub fn chevron_menu(
    ui: &mut Ui,
    rect: Rect,
    salt: impl std::hash::Hash + std::fmt::Debug,
    entries: &[MenuEntry],
) -> Option<AppAction> {
    let id = ui.id().with(("terra_new_tab_menu", salt));
    let response = ui
        .interact(rect, id, Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("New tab from a profile");

    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        // Same visual language as the `+`: a dark rounded fill on hover, the
        // glyph itself drawn rather than typeset so no font has to have it.
        if response.hovered() || open {
            painter.rect_filled(
                Rect::from_center_size(rect.center(), Vec2::new(rect.width(), CHEVRON_HEIGHT)),
                CornerRadius::same(CHEVRON_CORNER),
                PLUS_HOVER_BG,
            );
        }
        let c = rect.center();
        let color = if response.hovered() || open {
            TEXT_ACTIVE
        } else {
            TEXT_IDLE
        };
        let stroke = Stroke::new(1.3, color);
        // A `⌄`: two strokes meeting below centre, so it reads as pointing at
        // the menu that drops out of it.
        let (w, h) = (CHEVRON_ARM, CHEVRON_ARM * 0.6);
        painter.line_segment(
            [egui::pos2(c.x - w, c.y - h), egui::pos2(c.x, c.y + h)],
            stroke,
        );
        painter.line_segment(
            [egui::pos2(c.x, c.y + h), egui::pos2(c.x + w, c.y - h)],
            stroke,
        );
    }

    let width = menu_width(ui, entries);
    let mut chosen = None;
    egui::Popup::menu(&response)
        // Right-aligned under the chevron, which is itself the rightmost thing
        // in the bar: growing left is the only direction that cannot run off
        // the window.
        .align(egui::RectAlign::BOTTOM_END)
        .gap(MENU_GAP)
        .frame(menu_frame())
        .show(|ui| {
            // Fixed, not `set_min_width`: a row is a full-width shape, and
            // `available_width` inside a free-floating popup is the rest of
            // the screen — asking for it would stretch the panel to the
            // window edge.
            ui.set_width(width);
            // Rows own their own height and the frame owns the padding, so
            // egui's default rhythm has nothing left to add.
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            for (i, entry) in entries.iter().enumerate() {
                // The default shell is not one of the profiles; a hairline
                // says so without a heading.
                if i == 1 {
                    menu_separator(ui);
                }
                if menu_row(ui, entry).clicked() {
                    chosen = Some(entry.action.clone());
                    ui.close();
                }
            }
        });
    chosen
}

/// How wide the panel's *content* has to be for the longest label to fit,
/// floored at [`MENU_MIN_WIDTH`] so a menu of short names is still a menu and
/// not a chip.
fn menu_width(ui: &Ui, entries: &[MenuEntry]) -> f32 {
    let font = title_font(false);
    let widest = entries
        .iter()
        .map(|entry| {
            ui.painter()
                .layout_no_wrap(entry.label.clone(), font.clone(), MENU_TEXT)
                .size()
                .x
        })
        .fold(0.0_f32, f32::max);
    let content = MENU_ROW_PAD_X * 2.0 + ICON_SIZE + MENU_ICON_GAP + widest;
    content.max(MENU_MIN_WIDTH - 2.0 * f32::from(MENU_PAD))
}

/// The dropdown's panel: a dark card a shade above the bar, hairlined and
/// floated off the window with a soft shadow — the same material the command
/// palette is made of (see `terra-palette`), scaled down to a menu.
fn menu_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(MENU_BG)
        .stroke(Stroke::new(1.0, MENU_BORDER))
        .corner_radius(CornerRadius::same(MENU_CORNER))
        .inner_margin(egui::Margin::same(MENU_PAD))
        .shadow(egui::Shadow {
            offset: [0, 8],
            blur: 28,
            spread: 0,
            color: Color32::from_black_alpha(130),
        })
}

/// One row: hover pill, icon, label. Drawn rather than composed out of
/// `ui.button`, because a menu row is a shape (a full-width rounded highlight
/// with a leading logo) and not a button with the padding filed off.
fn menu_row(ui: &mut Ui, entry: &MenuEntry) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), MENU_ROW_HEIGHT),
        Sense::click(),
    );
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let hovered = response.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(MENU_ROW_CORNER), MENU_ROW_HOVER);
    }
    let text_color = if hovered { TITLE_ACTIVE } else { MENU_TEXT };

    let icon = Rect::from_center_size(
        egui::pos2(
            rect.left() + MENU_ROW_PAD_X + ICON_SIZE * 0.5,
            rect.center().y,
        ),
        Vec2::splat(ICON_SIZE),
    );
    // The generic `>_` is chrome and fades back like it does on a pill; a
    // brand mark is the point of the row and stays at full strength.
    let tint = if entry.icon.is_generic() {
        text_color.gamma_multiply(GENERIC_ICON_ALPHA)
    } else {
        text_color
    };
    crate::tab_icon::paint(ui, entry.icon, icon, tint);

    let galley = ui
        .painter()
        .layout_no_wrap(entry.label.clone(), title_font(false), text_color);
    let baseline = egui::pos2(
        icon.right() + MENU_ICON_GAP,
        rect.center().y - galley.size().y * 0.5,
    );
    ui.painter().galley(baseline, galley, text_color);
    response
}

/// The hairline between the default shell and the profiles, inset from the
/// panel's edges the way a macOS menu separator is.
fn menu_separator(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), MENU_SEPARATOR_HEIGHT),
        Sense::hover(),
    );
    let y = rect.center().y.round() + 0.5;
    ui.painter().hline(
        (rect.left() + MENU_SEPARATOR_INSET)..=(rect.right() - MENU_SEPARATOR_INSET),
        y,
        Stroke::new(1.0, MENU_SEPARATOR),
    );
}

/// Height of the chevron's hover fill — a little short of the tab height, so
/// it reads as a small button inside the bar rather than a tab.
const CHEVRON_HEIGHT: f32 = 21.0;
const CHEVRON_CORNER: u8 = 6;
/// Half-width of the `⌄` glyph.
const CHEVRON_ARM: f32 = 4.0;
/// Keeps the menu from collapsing to the width of "New Tab".
const MENU_MIN_WIDTH: f32 = 220.0;
/// How far below the *chevron* the panel floats. The chevron is inset from the
/// bar's bottom edge by [`PAD_Y`], so the visible gap under the bar is smaller
/// than this — 8 here lands the panel ~5px clear of the bar, which is where it
/// reads as a separate surface without detaching from the button.
const MENU_GAP: f32 = 8.0;
/// One notch above the bar (`BAR_BG`), so the panel reads as sitting *over*
/// the chrome rather than being cut out of it.
const MENU_BG: Color32 = Color32::from_rgb(0x26, 0x26, 0x2b);
/// ~9% white: the lit edge that lifts the card off whatever is behind it.
const MENU_BORDER: Color32 = Color32::from_rgba_premultiplied(0x17, 0x17, 0x17, 0x17);
const MENU_CORNER: u8 = 10;
const MENU_PAD: i8 = 6;
/// Roomy enough to click without aiming, from the same family as macOS's own
/// menu rows.
const MENU_ROW_HEIGHT: f32 = 29.0;
const MENU_ROW_CORNER: u8 = 8;
const MENU_ROW_PAD_X: f32 = 10.0;
/// Wider than the pills' [`ICON_GAP`]: a menu has the room, and the extra air
/// lets the labels line up as a column instead of crowding their logos.
const MENU_ICON_GAP: f32 = 8.0;
/// The hover fill, from the pills' grey family — between [`TAB_HOVER_BG`] and
/// [`TAB_ACTIVE_BG`], because it has to read against the panel and not the bar.
const MENU_ROW_HOVER: Color32 = Color32::from_rgb(0x3a, 0x3a, 0x40);
/// Vertical space the separator row takes, and the 1px rule inside it.
const MENU_SEPARATOR_HEIGHT: f32 = 7.0;
const MENU_SEPARATOR_INSET: f32 = 4.0;
/// ~7% white — present, never a bar across the menu.
const MENU_SEPARATOR: Color32 = Color32::from_rgba_premultiplied(0x12, 0x12, 0x12, 0x12);
/// Menu labels sit a touch brighter than an inactive pill's: nothing in a
/// dropdown is "inactive".
const MENU_TEXT: Color32 = Color32::from_rgb(0xe2, 0xe2, 0xe8);

/// Which slot index a tab dragged to `x` wants, given the pitch of the row.
fn drop_index(x: f32, bar_left: f32, pitch: f32, count: usize) -> usize {
    if pitch <= 0.0 || count == 0 {
        return 0;
    }
    let raw = ((x - bar_left) / pitch).round();
    raw.clamp(0.0, (count - 1) as f32) as usize
}

/// One row entry: a live tab, or a ghost of one that just closed.
#[derive(Clone)]
struct Slot {
    id: u64,
    title: String,
    icon: Option<TabIcon>,
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

/// Draw one group's tab bar across the top of `ui`'s available rect (the
/// group's column), allocating [`TAB_BAR_HEIGHT`]. Appends any user
/// interaction to `actions`.
///
/// With a single group holding fewer than two tabs nothing is drawn and no
/// space is taken, so the terminal below simply grows into the whole column.
/// Keyboard shortcuts are handled by [`consume_shortcuts`] and keep working
/// with the bar hidden. `focused` gates the `⌘n` hints, which act on the
/// focused group only.
///
/// `icons` is read, never refreshed: deciding what is running in a tab is a
/// syscall on a clock, which belongs with the app's other per-frame work and
/// not inside a paint routine. One cache serves every group. An empty cache —
/// which is what the `[tabs] icons = false` kill-switch produces — simply
/// draws the bar terra drew before icons existed.
pub fn tab_bar(
    ui: &mut Ui,
    tabs: &TabManager,
    group: usize,
    focused: bool,
    icons: &IconCache,
    actions: &mut Vec<AppAction>,
) {
    let state_id = Id::new(("terra_tab_bar_state", group));
    if !bar_visible(tabs.group_tabs(group).len(), tabs.group_count()) {
        // Nothing on screen to continue from: drop the animations so the bar
        // comes back settled rather than mid-flight from minutes ago.
        ui.ctx().data_mut(|d| d.remove::<BarState>(state_id));
        return;
    }

    let mut state: BarState = ui
        .ctx()
        .data_mut(|d| d.get_temp(state_id))
        .unwrap_or_default();

    let column = ui.available_rect_before_wrap();
    let panel = Rect::from_min_size(column.min, Vec2::new(column.width(), TAB_BAR_HEIGHT));
    ui.painter().rect_filled(panel, 0.0, BAR_BG);
    {
        // Salted per group: every interact id below hangs off `ui.id()`, and
        // two groups' bars must not collide on ids like `terra_new_tab`.
        let ui = &mut ui.new_child(
            egui::UiBuilder::new()
                .max_rect(panel)
                .id_salt(("terra_tab_bar", group)),
        );
        ui.set_clip_rect(panel);
        {
            let bar = panel.shrink2(Vec2::new(f32::from(PAD_X), f32::from(PAD_Y)));
            let now = ui.input(|i| i.time);
            let chevron_left = bar.right() - CHEVRON_WIDTH;
            let plus_left = chevron_left - PLUS_WIDTH;
            let tabs_right = plus_left - PLUS_GAP;
            let width = tab_width(bar.width(), tabs.group_tabs(group).len());

            // 1. Carry an in-progress drag first, so the rest of the frame lays
            //    out the order the pointer is asking for, with no lag.
            let drag_x = drive_drag(ui, tabs, group, panel, bar, tabs_right, width);

            // 2. Tabs that vanished since last frame linger as shrinking ghosts.
            let ids = tabs.group_tabs(group);
            let live: Vec<Slot> = ids
                .iter()
                .map(|id| Slot {
                    id: *id,
                    title: slot_title(tabs, *id),
                    icon: icons.get(*id),
                    ghost: false,
                })
                .collect();
            for (index, slot) in state.live.iter().enumerate() {
                if !ids.contains(&slot.id) && !state.ghosts.iter().any(|g| g.id == slot.id) {
                    state.ghosts.push(Ghost {
                        id: slot.id,
                        title: slot.title.clone(),
                        icon: slot.icon,
                        index,
                    });
                }
            }

            let mut slots: Vec<Slot> = live.clone();
            let mut ghosts = state.ghosts.clone();
            ghosts.sort_by_key(|g| g.index);
            for ghost in &ghosts {
                let at = ghost.index.min(slots.len());
                slots.insert(
                    at,
                    Slot {
                        id: ghost.id,
                        title: ghost.title.clone(),
                        icon: ghost.icon,
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
                let is_new = !slot.ghost && !state.live.iter().any(|live| live.id == slot.id);
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
            //    "Active" is the *group's* active tab: every group's bar
            //    highlights the tab whose terminal it shows.
            let active = tabs.group_active(group);
            let dragged = current_drag(ui.ctx()).map(|d| d.id);
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
                    // ⌘n selects within the *focused* group, so only its bar
                    // shows the hints.
                    index: if focused {
                        live_index.get(&slot.id).copied().unwrap_or(usize::MAX)
                    } else {
                        usize::MAX
                    },
                    title: &slot.title,
                    icon: slot.icon,
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
                        started_drag = Some(TabDrag {
                            id: slot.id,
                            grab: pos.x - rect.left(),
                        });
                    }
                }
            }
            if let Some(drag) = started_drag {
                actions.push(AppAction::SelectTab(drag.id));
                set_drag(ui.ctx(), Some(drag));
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

            // The `⌄` hangs off this group's `+` right edge, Windows-Terminal
            // style. The bar is the only thing deciding *where*; the button
            // itself is anchor-agnostic (see [`chevron_menu`]), so every group
            // gets its own, salted by leaf index so two bars' popups and
            // interact ids never collide.
            let chevron = Rect::from_min_size(
                egui::pos2(chevron_left, bar.top()),
                Vec2::new(CHEVRON_WIDTH, bar.height()),
            );
            let entries = new_tab_entries(tabs.profiles().values());
            if let Some(action) = chevron_menu(ui, chevron, group, &entries) {
                // The menu's rows open a tab in *this* group. `open` targets
                // the focused group, so say which one that is first rather
                // than leaning on the click having landed inside the column
                // (the popup is a separate layer and may not).
                if !focused {
                    actions.push(AppAction::FocusGroup(group));
                }
                actions.push(action);
            }
        }
    }

    // Advance the column's cursor past the bar, so the caller lays the
    // terminal out below it.
    ui.allocate_rect(panel, Sense::hover());
    ui.ctx().data_mut(|d| d.insert_temp(state_id, state));

    // Hairline under the bar, drawn on top of the terminal's own background.
    ui.painter()
        .hline(panel.x_range(), panel.bottom(), Stroke::new(1.0, BAR_LINE));
}

/// Advance a drag started on an earlier frame *while it stays in its own bar*:
/// follow the pointer and reorder the tabs it crosses within their group.
/// Returns where the dragged tab should be painted this frame.
///
/// Only the position is owned here. The drag's life ends in
/// [`tab_drag_overlay`], which also takes over the moment the pointer strays
/// past [`BAR_DRAG_SLACK`] — from there the pill is a floating ghost and this
/// returns `None`, so the in-bar pill animates back to its slot.
fn drive_drag(
    ui: &Ui,
    tabs: &TabManager,
    group: usize,
    panel: Rect,
    bar: Rect,
    tabs_right: f32,
    width: f32,
) -> Option<(u64, f32)> {
    let drag = current_drag(ui.ctx())?;
    if tabs.group_of(drag.id) != Some(group) {
        // Some other group's bar owns this drag.
        return None;
    }
    let down = ui.input(|i| i.pointer.primary_down());
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let (true, Some(pointer)) = (down, pointer) else {
        // Released: the overlay pass performs the drop and clears the state;
        // the tab animates from wherever it is to its slot.
        return None;
    };
    if !panel
        .expand2(Vec2::new(0.0, BAR_DRAG_SLACK))
        .contains(pointer)
    {
        // Left the bar: the cross-group ghost has it now.
        return None;
    }

    let max_x = (tabs_right - width).max(bar.left());
    let x = (pointer.x - drag.grab).clamp(bar.left(), max_x);
    let count = tabs.group_tabs(group).len();
    tabs.move_tab(
        drag.id,
        group,
        drop_index(x, bar.left(), width + TAB_GAP, count),
    );
    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    Some((drag.id, x))
}

// ---------------------------------------------------------------------------
// Cross-group drag & drop
// ---------------------------------------------------------------------------

/// Where one group's column sits this frame, for routing a cross-group drag.
/// `main.rs` collects one per group as it lays the columns out.
pub struct GroupGeometry {
    /// The bar strip across the top of the column ([`Rect::NOTHING`] while the
    /// bar is hidden, i.e. a lone tab in a lone group).
    pub bar: Rect,
    /// The terminal area below the bar, whose four halves (nearest edge
    /// wins) are the split drop zones.
    pub terminal: Rect,
}

/// What the pointer is over mid-drag.
enum DropTarget {
    /// A group's tab bar: drop moves the tab there, at `index`.
    Bar { group: usize, index: usize },
    /// A half of a group's terminal: drop splits that group towards `dir`.
    /// `zone` is the half itself, for the hover overlay.
    Split {
        group: usize,
        dir: SplitDir,
        zone: Rect,
    },
}

/// Which of the four halves of `rect` the pointer is in: whichever edge it
/// is proportionally closest to wins (VS Code's quadrant rule), so the rect
/// is cut along its diagonals. Ties go to the horizontal sides.
fn split_zone(rect: Rect, pointer: egui::Pos2) -> (SplitDir, Rect) {
    let dx = (pointer.x - rect.center().x) / rect.width().max(1.0);
    let dy = (pointer.y - rect.center().y) / rect.height().max(1.0);
    let dir = if dx.abs() >= dy.abs() {
        if dx >= 0.0 {
            SplitDir::Right
        } else {
            SplitDir::Left
        }
    } else if dy >= 0.0 {
        SplitDir::Down
    } else {
        SplitDir::Up
    };
    let zone = match dir {
        SplitDir::Left => rect.split_left_right_at_fraction(0.5).0,
        SplitDir::Right => rect.split_left_right_at_fraction(0.5).1,
        SplitDir::Up => rect.split_top_bottom_at_fraction(0.5).0,
        SplitDir::Down => rect.split_top_bottom_at_fraction(0.5).1,
    };
    (dir, zone)
}

/// Insertion slot for a tab dropped at `x` on a *foreign* bar: unlike
/// [`drop_index`] (which reorders `count` existing tabs), a foreign drop may
/// also land *after* the last tab, so this clamps to `count`, not `count - 1`.
fn insertion_index(x: f32, bar_left: f32, pitch: f32, count: usize) -> usize {
    if pitch <= 0.0 {
        return count;
    }
    let raw = ((x - bar_left) / pitch).round().max(0.0) as usize;
    raw.min(count)
}

/// The drop target under `pointer`, if it is a *valid* one for `drag`:
/// splitting a group towards itself when the tab is alone in it would be pure
/// churn (the model refuses it too), so that half reads as no target at all.
fn drop_target(
    pointer: egui::Pos2,
    tabs: &TabManager,
    drag: TabDrag,
    geoms: &[GroupGeometry],
) -> Option<DropTarget> {
    let src_group = tabs.group_of(drag.id)?;
    for (group, geom) in geoms.iter().enumerate() {
        if geom.bar.contains(pointer) {
            let inner = geom
                .bar
                .shrink2(Vec2::new(f32::from(PAD_X), f32::from(PAD_Y)));
            let count = tabs.group_tabs(group).len();
            let width = tab_width(inner.width(), count);
            return Some(DropTarget::Bar {
                group,
                index: insertion_index(pointer.x, inner.left(), width + TAB_GAP, count),
            });
        }
        if geom.terminal.contains(pointer) {
            if group == src_group && tabs.group_tabs(src_group).len() < 2 {
                return None;
            }
            let (dir, zone) = split_zone(geom.terminal, pointer);
            return Some(DropTarget::Split { group, dir, zone });
        }
    }
    None
}

/// The floating pill that follows the pointer once a drag has left its bar.
/// Painted on the tooltip layer, so it rides above every column.
fn paint_ghost(
    ctx: &egui::Context,
    tabs: &TabManager,
    icons: &IconCache,
    drag: TabDrag,
    pointer: egui::Pos2,
) {
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        Id::new("terra_tab_drag_ghost"),
    ));
    let height = TAB_BAR_HEIGHT - 2.0 * f32::from(PAD_Y);
    let rect = Rect::from_min_size(
        egui::pos2(
            pointer.x - drag.grab.clamp(0.0, GHOST_WIDTH),
            pointer.y - height / 2.0,
        ),
        Vec2::new(GHOST_WIDTH, height),
    );
    let radius = CornerRadius::same(CORNER);
    painter.rect_filled(rect, radius, TAB_ACTIVE_BG.gamma_multiply(0.9));
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(1.0, TAB_ACTIVE_EDGE),
        egui::StrokeKind::Inside,
    );
    let title = slot_title(tabs, drag.id);
    // The ghost is the pill, so it carries the pill's icon too — the tab keeps
    // its face all the way across the window.
    let icon = icons.get(drag.id);
    let lead = if icon.is_some() {
        ICON_SIZE + ICON_GAP
    } else {
        0.0
    };
    let (galley, _) = middle_truncated(
        &painter,
        crate::tab_icon::display_title(&title, icon),
        title_font(true),
        rect.width() - TITLE_RESERVE - lead,
    );
    let left = rect.center().x - (galley.size().x + lead) / 2.0;
    let pos = egui::pos2(left + lead, rect.center().y - galley.size().y / 2.0);
    if let Some(icon) = icon {
        crate::tab_icon::paint_on(
            ctx,
            &painter,
            icon,
            Rect::from_min_size(
                egui::pos2(left, rect.center().y - ICON_SIZE / 2.0),
                Vec2::splat(ICON_SIZE),
            ),
            TITLE_ACTIVE,
        );
    }
    painter.galley(pos, galley, TITLE_ACTIVE);
}

/// The cross-group half of a tab drag, run once per frame after every column
/// (bar + terminal) has been laid out — it needs the whole window's geometry,
/// which no single group's bar has.
///
/// While the button is down: paints the translucent split zone under the
/// pointer and, once the drag has left its own bar, the floating ghost. On
/// release: turns the drop target into an action —
/// [`AppAction::MoveTab`] for a foreign bar, [`AppAction::SplitTab`] for a
/// terminal half — and ends the drag. Anywhere else the release is a no-op and
/// the pill simply animates back to its slot.
pub fn tab_drag_overlay(
    ui: &Ui,
    tabs: &TabManager,
    icons: &IconCache,
    geoms: &[GroupGeometry],
    actions: &mut Vec<AppAction>,
) {
    let ctx = ui.ctx().clone();
    let Some(drag) = current_drag(&ctx) else {
        return;
    };
    let Some(src_group) = tabs.group_of(drag.id) else {
        // Closed under the pointer (⌘W, `terra kill`, shell exit).
        set_drag(&ctx, None);
        return;
    };
    let pointer = ctx.input(|i| i.pointer.interact_pos());
    let down = ctx.input(|i| i.pointer.primary_down());
    // While the pointer stays in its own bar's band the drag is an in-bar
    // reorder ([`drive_drag`]) and nothing here may compete with it — the
    // band's overhang into the terminal must not read as a split target.
    let in_own_bar = |p: egui::Pos2| {
        geoms
            .get(src_group)
            .is_some_and(|g| g.bar.expand2(Vec2::new(0.0, BAR_DRAG_SLACK)).contains(p))
    };
    let target = pointer
        .filter(|p| !in_own_bar(*p))
        .and_then(|p| drop_target(p, tabs, drag, geoms));

    if !down {
        match target {
            Some(DropTarget::Bar { group, index }) if group != src_group => {
                actions.push(AppAction::MoveTab {
                    id: drag.id,
                    group,
                    index,
                });
            }
            Some(DropTarget::Split { group, dir, .. }) => {
                actions.push(AppAction::SplitTab {
                    id: drag.id,
                    group,
                    dir,
                });
            }
            // Own bar (the in-bar reorder already happened live) or thin air:
            // nothing to do, the pill snaps back on its own.
            _ => {}
        }
        set_drag(&ctx, None);
        return;
    }

    let Some(pointer) = pointer else { return };
    if let Some(DropTarget::Split { zone, .. }) = target {
        // VS Code's drop shade: a blue wash over the half the split would take.
        ui.painter().rect_filled(zone, 0.0, DROP_ZONE_FILL);
        ui.painter().rect_stroke(
            zone,
            0.0,
            Stroke::new(1.0, DROP_ZONE_EDGE),
            egui::StrokeKind::Inside,
        );
    }
    if !in_own_bar(pointer) {
        // Inside its own bar the live pill *is* the drag feedback.
        paint_ghost(&ctx, tabs, icons, drag, pointer);
    }
    ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `+` and the `⌄` next to it both come out of the tabs' share, or the
    /// last tab would slide under them.
    #[test]
    fn tabs_share_the_bar_minus_the_plus_and_chevron_zone() {
        // 800 wide bar, 4 tabs: 800 - 28 - 20 - 6 = 746 usable, split evenly.
        let w = tab_width(800.0, 4);
        assert!((w - (746.0 - 3.0 * TAB_GAP) / 4.0).abs() < 0.01);
        // Tabs plus gaps plus both buttons exactly fill the bar.
        assert!(
            (4.0 * w + 3.0 * TAB_GAP + PLUS_GAP + PLUS_WIDTH + CHEVRON_WIDTH - 800.0).abs() < 0.01
        );
    }

    #[test]
    fn a_single_tab_takes_the_whole_bar() {
        let w = tab_width(400.0, 1);
        assert!((w + PLUS_GAP + PLUS_WIDTH + CHEVRON_WIDTH - 400.0).abs() < 0.01);
    }

    /// The menu always offers a plain new tab first, then the profiles in the
    /// order they arrive — which is the config's `BTreeMap` order, i.e.
    /// alphabetical.
    #[test]
    fn the_chevron_menu_lists_the_default_shell_then_every_profile() {
        let bare = new_tab_entries(std::iter::empty());
        assert_eq!(bare.len(), 1);
        assert_eq!(bare[0], MenuEntry::new("New Tab", AppAction::NewTab));

        let profiles = [profile("build", "cargo build"), profile("htop", "htop")];
        let entries = new_tab_entries(&profiles);
        let labels: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["New Tab", "build", "htop"]);
        assert_eq!(
            entries[1].action,
            AppAction::NewTabProfile("build".to_owned())
        );
        assert_eq!(
            entries[2].action,
            AppAction::NewTabProfile("htop".to_owned())
        );
    }

    fn profile(name: &str, command: &str) -> Profile {
        Profile {
            name: name.to_owned(),
            command: command.split(' ').map(str::to_owned).collect(),
            ..Profile::default()
        }
    }

    /// A row wears the mark of whatever the profile runs, not of its name: a
    /// profile called `top` that runs htop is an htop row. Anything terra does
    /// not recognise falls back to the generic `>_` rather than to nothing, so
    /// the labels stay in one column.
    #[test]
    fn a_profile_row_takes_its_icon_from_the_command_it_runs() {
        let profiles = [
            profile("top", "htop"),
            profile("ai", "codex"),
            profile("plain", "/bin/zsh -l"),
        ];
        let icons: Vec<TabIcon> = new_tab_entries(&profiles)
            .iter()
            .map(|entry| entry.icon)
            .collect();
        assert_eq!(
            icons,
            [
                // "New Tab" itself.
                TabIcon::Terminal,
                TabIcon::Htop,
                TabIcon::OpenAi,
                TabIcon::Terminal,
            ]
        );
    }

    #[test]
    fn the_bar_hides_for_a_lone_tab_in_a_lone_group() {
        assert!(!bar_visible(0, 1));
        assert!(!bar_visible(1, 1));
        assert!(bar_visible(2, 1));
        // With a second group every column shows its bar, tabs or not.
        assert!(bar_visible(1, 2));
        assert!(bar_visible(0, 2));
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

    /// A tab dropped on a *foreign* bar may land after the last tab, so the
    /// insertion slot clamps to `count`, one past what [`drop_index`] allows.
    #[test]
    fn a_foreign_drop_can_land_after_the_last_tab() {
        let pitch = 100.0;
        assert_eq!(insertion_index(0.0, 0.0, pitch, 3), 0);
        assert_eq!(insertion_index(151.0, 0.0, pitch, 3), 2);
        assert_eq!(insertion_index(260.0, 0.0, pitch, 3), 3);
        assert_eq!(insertion_index(9000.0, 0.0, pitch, 3), 3);
        assert_eq!(insertion_index(-9000.0, 0.0, pitch, 3), 0);
        // A degenerate pitch appends rather than dividing by zero.
        assert_eq!(insertion_index(50.0, 0.0, 0.0, 3), 3);
    }

    /// A drop splits towards whichever edge the pointer is proportionally
    /// closest to — the rect is cut along its diagonals into four zones, and
    /// the highlighted half is the one the new leaf would take.
    #[test]
    fn the_four_drop_zones_are_cut_along_the_diagonals() {
        let rect = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(400.0, 200.0));
        let at = |x: f32, y: f32| split_zone(rect, egui::pos2(x, y));

        let (dir, zone) = at(40.0, 100.0); // deep in the left wedge
        assert_eq!(dir, SplitDir::Left);
        assert_eq!(
            zone,
            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(200.0, 200.0))
        );
        let (dir, zone) = at(360.0, 100.0);
        assert_eq!(dir, SplitDir::Right);
        assert_eq!(zone.min.x, 200.0);
        let (dir, zone) = at(200.0, 20.0); // top wedge, centred horizontally
        assert_eq!(dir, SplitDir::Up);
        assert_eq!(
            zone,
            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(400.0, 100.0))
        );
        let (dir, zone) = at(200.0, 180.0);
        assert_eq!(dir, SplitDir::Down);
        assert_eq!(zone.min.y, 100.0);

        // Proportional, not absolute: in a wide rect a point 30% across but
        // 40% down is *relatively* nearer the left edge than the top one.
        let (dir, _) = at(120.0, 80.0);
        assert_eq!(dir, SplitDir::Left);
        // The exact centre ties; the horizontal sides win ties.
        let (dir, _) = at(200.0, 100.0);
        assert_eq!(dir, SplitDir::Right);
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
