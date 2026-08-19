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
//! window after the columns are laid out. Dragged *outside* the window
//! altogether, the same ghost tears the tab out into an OS window of its own —
//! Chrome's gesture, and the reason the ghost puts on a window frame once it
//! is past the edge. The torn window appears the instant the pointer clears
//! the edge and then follows it (see [`TabDrag::torn`]); dropping it over
//! another terra window moves the tab in there instead, whichever window it
//! came from. [`release_tears_out`] is the release-time fallback for the
//! shapes of "outside" the mid-drag test cannot see (a pointer that left
//! without a farewell position).

use std::collections::HashMap;
use std::sync::Arc;

use egui::{
    Color32, CornerRadius, FontId, Galley, Id, Key, KeyboardShortcut, Modifiers, PointerButton,
    Rect, Sense, Stroke, Ui, Vec2,
};

use crate::config::Profile;
use crate::edit_tools::EditTool;
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
/// Tabs never grow past this, however empty the bar is. Chrome and VS Code both
/// cap the pill and leave the rest of the strip bare; without a cap a lone tab
/// spans the whole window and reads as an address bar rather than as a tab.
/// 240 is wide enough for a full `~/src/terra — zsh` style title at 13px.
const MAX_TAB_WIDTH: f32 = 240.0;
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
/// The wash behind a bar button (`+`, `⌄`) under the pointer. At rest they draw
/// no chrome at all, Windows Terminal style — see [`bar_button_chrome`].
const BAR_BUTTON_HOVER_BG: Color32 = Color32::from_rgb(0x2e, 0x2e, 0x33);
/// The same wash a shade stronger, so a press reads as landing.
const BAR_BUTTON_ACTIVE_BG: Color32 = Color32::from_rgb(0x3a, 0x3a, 0x40);
/// Height of that wash — a little short of the tab height, so it reads as a
/// small button inside the bar rather than as another tab.
const BAR_BUTTON_HEIGHT: f32 = 21.0;
const BAR_BUTTON_CORNER: u8 = 6;
/// The hairline between `+` and `⌄` (Windows Terminal's `+ | ⌄`). Shorter than
/// the hover wash so it reads as a divider, not a border.
const BAR_BUTTON_SEPARATOR_HEIGHT: f32 = 14.0;
// Premultiplied white at ~8% alpha (from_white_alpha is not const).
const BAR_BUTTON_SEPARATOR: Color32 = Color32::from_rgba_premultiplied(0x14, 0x14, 0x14, 0x14);
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
/// How far past the window's own edge the pointer must be for a release to
/// read as "outside" (see [`release_tears_out`]). The rect and the pointer are
/// both in points and the pointer may legitimately sit *on* the last row of
/// pixels inside the window, so a bare `!contains` would tear a drag that
/// merely brushed the frame. A few points of slack costs nothing: a gesture
/// meant to tear ends well clear of the window.
const TEAR_MARGIN: f32 = 6.0;
/// The gap between the ghost pill and the window outline drawn around it once
/// the drag is beyond the window edge.
const TEAR_FRAME_INSET: u8 = 5;
/// The tear-out outline: the same blue that marks a split drop zone, so the
/// two drop affordances read as one family.
const TEAR_FRAME_EDGE: Color32 = DROP_ZONE_EDGE;
/// How far below its steered position a docked drag parks the window it is
/// carrying: far enough to be off any desktop, near enough to be an ordinary
/// window move. The window stays alive and keeps the mouse capture — it is
/// only out of sight while its tab rides another window's bar.
const DOCK_PARK: Vec2 = Vec2::new(0.0, 6000.0);
/// Extra slack on the strip a drag is *already* docked to, so undocking takes
/// a deliberate move rather than a point of jitter — and undocking is not
/// free: it moves the tab back out of the bar it is sitting in. It earns its
/// keep in the one-tab case, where the parked window is also the window
/// reporting the pointer: the frame its move lands, the origin and the pointer
/// it is added to change together, and a hair of disagreement between them
/// must not bounce the tab in and out of its new home.
const DOCK_HYSTERESIS: f32 = 12.0;

/// Title bar height to assume while the window server has not told us what a
/// window's decoration actually costs. macOS's is 28 points.
const DECOR_FALLBACK: f32 = 28.0;

/// How far a torn-out window's outer top-left sits from the pointer that tore
/// it out, so the pointer holds the tab *by its pill* in the new window's bar:
/// back off by the pill's left inset plus the grab offset inside it, and
/// vertically by the window decoration plus half a bar — the pill's middle.
///
/// `grab` is [`TabDrag::grab`], clamped the way the ghost clamps it so the
/// pill lands under exactly the point of the ghost it replaces. `decor_h` is
/// the tearing window's own decoration height ([`decor_height`]).
///
/// It lives here rather than in `main.rs` because the drag mints the position
/// itself (mid-drag, in this viewport's points); `main.rs` uses the same
/// arithmetic for the routes that have no drag to ask (the palette, IPC).
pub fn tear_anchor(grab: f32, decor_h: f32) -> Vec2 {
    Vec2::new(
        f32::from(PAD_X) + grab.clamp(0.0, GHOST_WIDTH),
        decor_h + TAB_BAR_HEIGHT / 2.0,
    )
}

/// What this viewport's decoration costs above its content: the gap between
/// its inner and outer rects, or [`DECOR_FALLBACK`] before the window server
/// has answered.
pub fn decor_height(ctx: &egui::Context) -> f32 {
    ctx.input(|i| {
        let viewport = i.viewport();
        match (viewport.inner_rect, viewport.outer_rect) {
            (Some(inner), Some(outer)) => (inner.min.y - outer.min.y).max(0.0),
            _ => DECOR_FALLBACK,
        }
    })
}

/// Something the user asked for via keyboard, tab bar or palette.
///
/// Not `Eq`: the window-dragging arms carry screen positions, which are
/// floats. Nothing compares actions for hashing, so `PartialEq` is all the
/// tests want.
#[derive(Debug, Clone, PartialEq)]
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
    /// Tear a tab out of this window into an OS window of its own: the pill
    /// was dragged *outside* the window's content area, or the palette's
    /// `tab.move-to-new-window` asked for it on the active tab. The model
    /// decides what that costs — a tab that is the sole tab of its window has
    /// nowhere to go and the action is a no-op there (the drag steers the
    /// whole window instead, so it never asks).
    ///
    /// `pos` is where the new window's top-left goes, in screen points. The
    /// drag knows it (it is minting the window under the pointer); the routes
    /// that have no pointer pass `None` and let `main.rs` cascade.
    MoveTabToNewWindow {
        id: u64,
        pos: Option<egui::Pos2>,
    },
    /// Hand a dragged tab to another *window*: the pill was carried into window
    /// `win`'s tab bar, at slot `index` of its group `group`. Works in every
    /// direction — root into torn, torn into root, torn into torn — because the
    /// hit test is done against every live window's bar in screen points rather
    /// than against this viewport's.
    ///
    /// This is a *docking*, not a drop: it happens while the button is still
    /// down, so the tab is really in `win` from that moment (Chrome's rule —
    /// mouse-up changes nothing), and the drag stays live in `host`, the window
    /// the tab came from. `host` is held ([`TabManager::hold_window`]) so that
    /// emptying it does not close the window the OS is delivering the drag to;
    /// [`Self::ReleaseDragHold`] settles that up when the gesture ends.
    DockTab {
        id: u64,
        host: u64,
        win: u64,
        group: usize,
        index: usize,
    },
    /// The docked tab slid to another slot of the bar it is docked in: the
    /// pointer moved along the strip. Emitted only when the slot actually
    /// changes, so a still pointer costs nothing.
    ReorderDocked {
        id: u64,
        win: u64,
        group: usize,
        index: usize,
    },
    /// The pill was pulled back out of the bar it was docked in: the tab goes
    /// home to `host`, which is still there because the drag has been holding
    /// it, and the window resumes following the pointer.
    UndockTab {
        id: u64,
        host: u64,
    },
    /// The drag is over: drop the hold on `host` ([`Self::DockTab`]). If the
    /// tab ended up somewhere else, `host` is empty and collapses now; if it
    /// came home, this changes nothing.
    ReleaseDragHold {
        host: u64,
    },
    /// [`Self::SplitTab`] across a window boundary: a torn drag was released
    /// over a half of window `win`'s group `group`, so the tab crosses into
    /// that window *and* becomes a pane there. Two steps rather than one
    /// because group indices are scoped to the focused window — the tab has to
    /// arrive before `group` names anything.
    SplitTabInWindow {
        id: u64,
        win: u64,
        group: usize,
        dir: SplitDir,
    },
    /// Steer window `win`'s OS window to `pos` (its outer top-left, in screen
    /// points). Emitted every frame of a torn drag: the real window follows
    /// the pointer, Chrome style, instead of a ghost pretending to.
    DragWindowTo {
        win: u64,
        pos: egui::Pos2,
    },
    /// Drop every session override, returning to what the file says.
    ResetSession,
    /// Re-read `~/.terra/config.toml`, keeping session overrides on top.
    ReloadConfig,
    /// Open `~/.terra/config.toml` in the system's editor (seeding it with
    /// the documented example first if it does not exist yet) — Windows
    /// Terminal's "Settings" menu item.
    OpenConfig,
    /// Hand `~/.terra/config.toml` to one detected tool: an agent gets a new
    /// tab and a prompt, an editor just gets the file. See
    /// [`crate::edit_tools`].
    EditConfigWith(EditTool),
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
        // The macOS-universal Settings shortcut; terra's settings are a file.
        if i.consume_shortcut(&cmd(Key::Comma)) {
            actions.push(AppAction::OpenConfig);
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
/// which case the row overflows and is clipped instead of collapsing to slivers,
/// and to [`MAX_TAB_WIDTH`], in which case the tabs sit at the left of the bar
/// and the space they declined stays empty.
fn tab_width(bar_width: f32, n: usize) -> f32 {
    if n == 0 {
        return 0.0;
    }
    let n_f = n as f32;
    let usable = bar_width - PLUS_WIDTH - CHEVRON_WIDTH - PLUS_GAP;
    let per = (usable - TAB_GAP * (n_f - 1.0)) / n_f;
    per.clamp(MIN_TAB_WIDTH, MAX_TAB_WIDTH)
}

/// Left edge of the `+` button: docked immediately after the last tab, Chrome
/// style, rather than pinned to the right of the bar. The `⌄` rides along
/// directly off its right edge — see [`chevron_left`] — so the pair reads as one
/// control that follows the row instead of two things stranded across the bar.
///
/// `tabs_end` is the right edge of the last tab *as drawn this frame* — the
/// animated one — so the cluster slides along with a tab growing in or shrinking
/// away instead of jumping a whole slot. Once the tabs fill their share it stops
/// where both buttons still fit before `bar_right`, which is exactly the row's
/// own limit ([`tabs_limit`]) plus the gap, so crossing that threshold moves it
/// by nothing at all.
fn plus_left(tabs_end: f32, bar_right: f32) -> f32 {
    (tabs_end + PLUS_GAP).min(bar_right - PLUS_WIDTH - CHEVRON_WIDTH)
}

/// Left edge of the `⌄`, which hangs off the `+` wherever that ended up.
fn chevron_left(plus_left: f32) -> f32 {
    plus_left + PLUS_WIDTH
}

/// How far right the tabs themselves may reach: the bar minus the space the
/// `+` and `⌄` need once the row is dense enough to push them to the end.
/// Bounds both the row's clip and the drag clamp, so a tab can never slide
/// under either button.
fn tabs_limit(bar_right: f32) -> f32 {
    bar_right - PLUS_WIDTH - CHEVRON_WIDTH - PLUS_GAP
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
    /// The window that owns the drag, and the only one whose render pass may
    /// drive or end it. Every window's pass runs [`tab_drag_overlay`] over the
    /// *same* (global) drag state, and a window that is not the owner cannot
    /// even tell whether the tab still exists — the group APIs are scoped to
    /// the focused window, which during a render is whichever window is being
    /// drawn. Without this an innocent bystander's pass saw
    /// `group_of(id) == None` and cleared the drag out from under the owner.
    ///
    /// [`u64::MAX`] until claimed: the bar starts the drag and does not know
    /// its own window id, so the first overlay pass that *can* see the tab
    /// claims it (see [`tab_drag_overlay`]).
    win: u64,
    /// The drag has left the window and a real OS window is now following the
    /// pointer — either the one just torn out (`moved`) or this window itself.
    /// No ghost is painted from here on: the window *is* the feedback.
    torn: bool,
    /// Which flavour of `torn`: `true` after a [`AppAction::MoveTabToNewWindow`]
    /// minted a window for the tab, `false` when the tab was the last one in
    /// its window and the whole window is being steered instead.
    moved: bool,
    /// Screen pointer minus the steered window's outer top-left, taken at the
    /// moment following started. Subtracting it every frame keeps the window
    /// under the same part of the cursor instead of snapping its corner there.
    ///
    /// For a freshly torn window that part is its *tab pill*
    /// ([`tear_anchor`]): the pointer holds the tab it is carrying, exactly
    /// where inside the pill it grabbed it. For a whole window being steered
    /// it is wherever the cursor already was, so nothing jumps.
    wgrab: Vec2,
    /// Where the tab is docked, while the pointer is inside a foreign tab bar.
    ///
    /// Docking is Chrome's rule taken literally: the tab is *really* in that
    /// window from the moment the pill enters its bar — pill in the bar,
    /// terminal on screen, at the slot under the pointer — so letting go
    /// changes nothing. The window the tab came from parks far offscreen
    /// ([`DOCK_PARK`]) and is held alive ([`TabDrag::host`]) so the drag it is
    /// pumping survives being emptied. Pulling the pill back out undoes all of
    /// it. Recomputed every frame from what is under the pointer, so leaving
    /// the bar — or the target window dying under it — undocks by itself.
    docked: Option<DockSlot>,
    /// The window the carried tab belongs to while it is not docked: the one
    /// torn out for it, or the whole window being steered. Remembered from the
    /// first dock onwards because that window is held from then on, and the
    /// hold has to be dropped when the gesture ends whatever ended it.
    host: Option<u64>,
}

/// Where a docked tab currently sits: window, group, and slot in that group's
/// bar. Compared frame to frame, so the tab only actually moves when the
/// pointer has moved it somewhere new.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DockSlot {
    win: u64,
    group: usize,
    index: usize,
}

/// Where the windows are, for the frame [`tab_drag_overlay`] is running in.
///
/// A drag that crosses windows needs two coordinate systems: this viewport's
/// (what the pointer is reported in) and the desktop's (what a window position
/// and a cross-window hit test are in). `origin` is the bridge between them.
pub struct DragWindows {
    /// The window this render pass belongs to.
    pub win: u64,
    /// This viewport's content rect, in its own points — what the drag is
    /// "inside" of. `ctx.viewport_rect()` is the root's own and means nothing
    /// inside a deferred viewport, so the renderer passes the real one.
    pub local: Rect,
    /// This viewport's inner-rect origin in screen points, `None` while the OS
    /// has not told us yet. Local pointer + `origin` = screen pointer.
    pub origin: Option<egui::Pos2>,
    /// Every live window's tab-bar strips in screen points, one entry per
    /// group. A torn drag docks the moment the pointer enters one of them, so
    /// this is what that hit test runs against.
    pub bars: Vec<BarStrip>,
    /// Every live window's terminal areas in screen points, as
    /// `(window, group, rect)` — the group being its index in that window's
    /// DFS order, which is what [`AppAction::SplitTabInWindow`] names. A torn
    /// drag released over one of these splits that group.
    pub terms: Vec<(u64, usize, Rect)>,
    /// The torn tab in flight, as the window driving it published it last
    /// frame. A window that is not driving the drag has no other way to know
    /// the pointer is over it: the pointer is reported to the dragging
    /// viewport, not to this one.
    pub carry: Option<CarryState>,
}

/// One group's tab-bar strip on the desktop, as the drag sees it.
///
/// The tab count travels with the rect because the slot a dropped pill lands
/// in is worked out from the two together — the same `tab_width` /
/// [`insertion_index`] arithmetic the bar itself lays tabs out with, run from
/// another window entirely.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarStrip {
    pub win: u64,
    /// The group's index in its window's DFS order.
    pub group: usize,
    /// The strip in screen points ([`attach_strip`]).
    pub rect: Rect,
    /// How many tabs that group holds right now.
    pub tabs: usize,
}

/// A torn tab in flight, published once a frame by the window that owns the
/// drag so a window it is carried over can wash the half a drop would split
/// into. Not published while the drag is docked: a docked tab is really in its
/// new bar, and that bar draws it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CarryState {
    pub id: u64,
    /// Where the pointer is, in screen points — the only frame of reference
    /// two viewports share.
    pub screen: egui::Pos2,
    /// The window riding under the pointer, which is never a drop target: it
    /// is the thing being carried.
    pub steer: Option<u64>,
}

/// What one window's render pass has to say about the carried tab.
///
/// Only the window that owns the drag knows anything, so a bystander's pass
/// says [`CarryReport::NotMine`] and leaves the published state alone —
/// without that, the first other window to render would clear the drag's
/// state out from under it every frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CarryReport {
    /// Someone else's drag (or nobody's): this pass knows nothing.
    NotMine,
    /// This pass owns a torn drag, and here is where it has got to.
    Carrying(CarryState),
    /// Nothing is in flight, and this pass can say so: it owns the drag and
    /// the drag is not torn (or has just ended), or there is no drag at all —
    /// the drag state is global, so that much any window can see.
    Idle,
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

/// The chrome shared by the bar's two buttons, the `+` and the `⌄`: nothing at
/// all at rest, a rounded wash under the pointer, a stronger one while held.
/// Windows Terminal's treatment — the buttons read as bar furniture until you
/// reach for one, instead of as two permanently outlined controls competing
/// with the tabs. Returns the colour to draw the glyph in.
///
/// Only the *fill* is inset from `rect`; the rect itself is still the hit
/// target, so shedding the resting outline costs the button no clickable area.
/// `lit` forces the hot look on for a button whose menu is open.
fn bar_button_chrome(ui: &Ui, rect: Rect, response: &egui::Response, lit: bool) -> Color32 {
    let held = response.is_pointer_button_down_on();
    let hot = lit || response.hovered();
    if hot {
        ui.painter().rect_filled(
            Rect::from_center_size(rect.center(), Vec2::new(rect.width(), BAR_BUTTON_HEIGHT)),
            CornerRadius::same(BAR_BUTTON_CORNER),
            if held {
                BAR_BUTTON_ACTIVE_BG
            } else {
                BAR_BUTTON_HOVER_BG
            },
        );
    }
    match (hot, held) {
        (_, true) => TITLE_ACTIVE,
        (true, false) => TEXT_ACTIVE,
        // The inactive tab title's grey: at rest these are furniture.
        (false, false) => TITLE_IDLE,
    }
}

/// Draw the `+` button, in whatever zone the bar has docked it into.
fn plus_button(ui: &mut Ui, rect: Rect, actions: &mut Vec<AppAction>) {
    let response = ui
        .interact(rect, ui.id().with("terra_new_tab"), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("New tab  ⌘T");

    if ui.is_rect_visible(rect) {
        let color = bar_button_chrome(ui, rect, &response, false);
        let painter = ui.painter();
        let c = rect.center();
        let arm = 4.5;
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

/// Whether a group's bar is worth showing at all.
///
/// Two tabs, or two groups, always show every bar — otherwise a single-tab
/// column would be indistinguishable from its neighbour. The one open question
/// is a lone tab in the only group, and `with_one_tab` (the `[tabs]
/// bar_with_one_tab` config key) answers it: on by default, so the `+` button
/// and the tab's title are there from the first tab; off gives the older,
/// Ghostty-like bare window, where the terminal owns the full column height.
///
/// The lone *empty* group — a transient between the last close and the app
/// exiting — is chrome over nothing, so it stays bare under either setting.
pub fn bar_visible(tab_count: usize, group_count: usize, with_one_tab: bool) -> bool {
    group_count >= 2 || tab_count >= 2 || (tab_count == 1 && with_one_tab)
}

// ---------------------------------------------------------------------------
// The `⌄` dropdown
// ---------------------------------------------------------------------------

/// One row of a dropdown: what it says, what it wears, and what choosing it
/// does.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuEntry {
    pub label: String,
    /// The logo drawn to the left of the label, from the same set the pills
    /// use — so the row for an `htop` profile and the tab it opens carry the
    /// same mark.
    pub icon: TabIcon,
    /// Right-aligned keybinding hint, macOS-menu style (`⇧⌘P`). Rows that are
    /// only reachable through the menu carry none.
    pub shortcut: Option<String>,
    pub action: AppAction,
}

impl MenuEntry {
    /// A row wearing the generic `>_`. Rows that can name a program build on
    /// this with [`Self::with_icon`].
    pub fn new(label: impl Into<String>, action: AppAction) -> Self {
        Self {
            label: label.into(),
            icon: TabIcon::Terminal,
            shortcut: None,
            action,
        }
    }

    pub fn with_icon(mut self, icon: TabIcon) -> Self {
        self.icon = icon;
        self
    }

    pub fn with_shortcut(mut self, shortcut: impl Into<String>) -> Self {
        self.shortcut = Some(shortcut.into());
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
    let mut entries = vec![
        MenuEntry::new("New Tab", AppAction::NewTab).with_shortcut("⌘T"),
        MenuEntry::new("Command Palette", AppAction::OpenPalette).with_shortcut("⇧⌘P"),
    ];
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

/// How many rows at the head of [`new_tab_entries`] are terra's own commands
/// rather than profiles — i.e. where the menu's hairline goes.
const MENU_COMMAND_ROWS: usize = 2;

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
        // Same chrome as the `+` next to it, and an open menu keeps it lit.
        // The glyph is drawn rather than typeset, so no font has to have it.
        let color = bar_button_chrome(ui, rect, &response, open);
        let painter = ui.painter();
        let c = rect.center();
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
                // terra's own commands are not profiles; a hairline says so
                // without a heading. Never trailing: the loop only reaches
                // this index when there is a profile row after it.
                if i == MENU_COMMAND_ROWS {
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
    let measure =
        |text: String, font: FontId| ui.painter().layout_no_wrap(text, font, MENU_TEXT).size().x;
    let widest = entries
        .iter()
        .map(|entry| {
            let label = measure(entry.label.clone(), font.clone());
            // The hint is part of the row's width, not an overlay: a menu that
            // sized itself on labels alone would have "⇧⌘P" sitting on top of
            // "Command Palette".
            let hint = entry.shortcut.as_ref().map_or(0.0, |sc| {
                MENU_SHORTCUT_GAP + measure(sc.clone(), hint_font())
            });
            label + hint
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

    // The hint, dimmed and hard right — where macOS puts a menu key equivalent.
    if let Some(shortcut) = &entry.shortcut {
        let galley = ui
            .painter()
            .layout_no_wrap(shortcut.clone(), hint_font(), TEXT_HINT);
        let pos = egui::pos2(
            rect.right() - MENU_ROW_PAD_X - galley.size().x,
            rect.center().y - galley.size().y * 0.5,
        );
        ui.painter().galley(pos, galley, TEXT_HINT);
    }
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
/// Least space between the longest label and its keybinding hint, so the two
/// columns never touch however narrow the menu gets.
const MENU_SHORTCUT_GAP: f32 = 24.0;
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
/// Nothing is drawn and no space is taken when [`bar_visible`] says so — a
/// single group holding no tabs, or holding one tab with `bar_with_one_tab`
/// off — and the terminal below simply grows into the whole column.
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
    with_one_tab: bool,
    actions: &mut Vec<AppAction>,
) {
    // Identity by stable leaf id, not DFS index: a split renumbers every
    // group after it, and index-keyed state would hand each of those bars a
    // neighbour's animations — one drop visibly nudging every other bar.
    let leaf = tabs.group_leaf_id(group).unwrap_or(u64::MAX);
    let state_id = Id::new(("terra_tab_bar_state", leaf));
    if !bar_visible(
        tabs.group_tabs(group).len(),
        tabs.group_count(),
        with_one_tab,
    ) {
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
        // Salted per leaf: every interact id below hangs off `ui.id()`, and
        // two groups' bars must not collide on ids like `terra_new_tab`.
        // The leaf id (not the DFS index) keeps hover/active fades and the
        // open popup attached to *this* bar when a split elsewhere
        // renumbers the groups.
        let ui = &mut ui.new_child(
            egui::UiBuilder::new()
                .max_rect(panel)
                .id_salt(("terra_tab_bar", leaf)),
        );
        ui.set_clip_rect(panel);
        {
            let bar = panel.shrink2(Vec2::new(f32::from(PAD_X), f32::from(PAD_Y)));
            let now = ui.input(|i| i.time);
            // The furthest right the tabs may reach. The `+` and the `⌄` dock
            // after the last tab (see [`plus_left`]) rather than sitting here,
            // but a dense row still has to stop short of where they end up.
            let tabs_right = tabs_limit(bar.right());
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
                            // The bar draws a column, not a window, and has no
                            // window id to hand over; the overlay claims the
                            // drag for whichever window can see the tab.
                            win: u64::MAX,
                            torn: false,
                            moved: false,
                            wgrab: Vec2::ZERO,
                            docked: None,
                            host: None,
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

            // `cursor` left the layout loop just past the last slot, so it is
            // the animated right edge of the row — ghosts included, which is
            // what keeps the `+` gliding back as a closing tab shrinks away.
            let tabs_end = if slots.is_empty() {
                bar.left()
            } else {
                cursor - TAB_GAP
            };
            let plus_x = plus_left(tabs_end, bar.right());
            let plus = Rect::from_min_size(
                egui::pos2(plus_x, bar.top()),
                Vec2::new(PLUS_WIDTH, bar.height()),
            );
            plus_button(ui, plus, actions);

            // Windows Terminal draws a hairline between `+` and `⌄` so the
            // pair reads as two controls rather than one wide button. Faint on
            // purpose — quieter than the glyphs, shorter than the hover wash.
            let sep_x = chevron_left(plus_x);
            ui.painter().vline(
                sep_x,
                egui::Rangef::new(
                    bar.center().y - BAR_BUTTON_SEPARATOR_HEIGHT / 2.0,
                    bar.center().y + BAR_BUTTON_SEPARATOR_HEIGHT / 2.0,
                ),
                egui::Stroke::new(1.0, BAR_BUTTON_SEPARATOR),
            );

            // The `⌄` hangs off this group's `+` right edge, Windows-Terminal
            // style — so it travels with the `+` as the row grows rather than
            // being stranded at the far end of an empty bar. The bar is the
            // only thing deciding *where*; the button itself is anchor-agnostic
            // (see [`chevron_menu`]), so every group gets its own, salted by
            // leaf id so two bars' popups and interact ids never collide (and
            // an open popup stays this bar's when a split renumbers the
            // groups).
            let chevron = Rect::from_min_size(
                egui::pos2(chevron_left(plus_x), bar.top()),
                Vec2::new(CHEVRON_WIDTH, bar.height()),
            );
            let entries = new_tab_entries(tabs.profiles().values());
            if let Some(action) = chevron_menu(ui, chevron, leaf, &entries) {
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
    if drag.torn {
        // A torn drag docked into this bar: the tab really is here, but the
        // pointer is not — the gesture belongs to another window's viewport,
        // which is the only one the OS is telling about it. The pill animates
        // to the slot the dock put it in, like any other tab that just
        // arrived, and [`torn_drag`] does the driving.
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

/// The strip of a column that catches a torn drag ([`attach_into_bar`]): its
/// tab bar, or — when the bar is hidden, which is a lone tab under `[tabs]
/// bar_with_one_tab = false` — the band where the bar *would* be. A bare
/// window has to be able to receive a tab too, and that band is where the
/// pointer holding a pill expects the bar to be.
pub fn attach_strip(geom: &GroupGeometry) -> Rect {
    if geom.bar.is_positive() {
        geom.bar
    } else {
        Rect::from_min_size(
            geom.terminal.min,
            Vec2::new(geom.terminal.width(), TAB_BAR_HEIGHT),
        )
    }
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

/// What crossing the window edge mid-drag means for the window this drag
/// belongs to. See [`mid_drag_tear`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MidDragTear {
    /// Mint a window for the tab and let it follow the pointer.
    NewWindow,
    /// The tab is the last one here, so there is nothing to tear it *out* of:
    /// steer this whole window instead, the way Chrome drags a one-tab window
    /// bodily. It also spares the sole-tab-of-the-sole-window case a refusal —
    /// the gesture always does something.
    SteerWindow,
    /// Still inside (or nothing to act on): carry on dragging the ghost.
    None,
}

/// Should this frame turn the drag into a window? Pure, so the three cases can
/// be tested without a window server.
///
/// `local` is the dragging window's own content rect and `pointer` is in the
/// same points; `tabs_in_window` counts the tabs of the window the drag
/// started in.
fn mid_drag_tear(
    down: bool,
    pointer: Option<egui::Pos2>,
    local: Rect,
    tabs_in_window: usize,
) -> MidDragTear {
    // A pointer with no position cannot steer a window to anywhere, so that
    // shape of "outside" stays with the release-time path
    // ([`release_tears_out`]).
    let (true, Some(pointer)) = (down, pointer) else {
        return MidDragTear::None;
    };
    if local.expand(TEAR_MARGIN).contains(pointer) {
        return MidDragTear::None;
    }
    match tabs_in_window {
        0 => MidDragTear::None,
        1 => MidDragTear::SteerWindow,
        _ => MidDragTear::NewWindow,
    }
}

/// Which window's tab bar the torn drag is over, if any — the bar it docks to
/// while the button is down, and hands the tab to when it comes up.
///
/// The first live window whose bar strip is under `screen` wins, minus the
/// window being steered: the carried window's own bar rides under the pointer
/// the whole time (that is what [`tear_anchor`] arranges), so excluding it is
/// what keeps the drag from docking to itself.
///
/// The strip is given the same [`BAR_DRAG_SLACK`] band an in-bar drag gets, so
/// grazing the bar's top edge on the way in counts; the strip already docked
/// to gets [`DOCK_HYSTERESIS`] more.
fn attach_into_bar(
    screen: egui::Pos2,
    steered: u64,
    docked: Option<DockSlot>,
    bars: &[BarStrip],
) -> Option<(u64, usize, usize)> {
    let strip = bars.iter().find(|strip| {
        let slack = if docked.map(|d| d.win) == Some(strip.win) {
            BAR_DRAG_SLACK + DOCK_HYSTERESIS
        } else {
            BAR_DRAG_SLACK
        };
        strip.win != steered && strip.rect.expand2(Vec2::new(0.0, slack)).contains(screen)
    })?;
    let here = docked.is_some_and(|d| d.win == strip.win && d.group == strip.group);
    Some((strip.win, strip.group, bar_slot(strip, screen.x, here)))
}

/// Which slot of `strip` the point `x` names, run from outside the window the
/// bar belongs to: the bar's own layout ([`tab_width`], [`insertion_index`])
/// replayed from the strip rect and the tab count that travelled with it.
///
/// `already_here` says the dragged tab is one of those `tabs` — it has already
/// docked into this group — so the answer is a slot to *reorder* into
/// (`0..tabs`) rather than one to insert at (`0..=tabs`).
fn bar_slot(strip: &BarStrip, x: f32, already_here: bool) -> usize {
    let inner = strip
        .rect
        .shrink2(Vec2::new(f32::from(PAD_X), f32::from(PAD_Y)));
    let width = tab_width(inner.width(), strip.tabs.max(1));
    let index = insertion_index(x, inner.left(), width + TAB_GAP, strip.tabs);
    if already_here {
        index.min(strip.tabs.saturating_sub(1))
    } else {
        index
    }
}

/// What a torn drag is over on the desktop. Neither target is a promise: a bar
/// docks the drag (the tab really moves there, but the gesture is still on and
/// can take it back), a terminal half only shows its wash until the release.
#[derive(Debug, Clone, Copy, PartialEq)]
enum TornTarget {
    /// Slot `index` of group `group` of window `win`'s tab bar.
    Bar {
        win: u64,
        group: usize,
        index: usize,
    },
    /// A half of window `win`'s group `group`. `zone` is the half itself, for
    /// the preview wash.
    Split {
        win: u64,
        group: usize,
        dir: SplitDir,
        zone: Rect,
    },
}

/// Where a torn drag at `screen` is pointing, skipping the window it is
/// steering (that one is the thing being carried, in every direction).
///
/// The bar wins where the two overlap: a bar strip reaches [`BAR_DRAG_SLACK`]
/// down into the terminal below it, and there the gesture aimed at the row of
/// pills has to be the one that happens. The strip a drag is already docked to
/// reaches [`DOCK_HYSTERESIS`] further still, so a docked tab does not flicker
/// in and out of the bar it is sitting in.
fn torn_target(
    screen: egui::Pos2,
    steered: u64,
    docked: Option<DockSlot>,
    bars: &[BarStrip],
    terms: &[(u64, usize, Rect)],
) -> Option<TornTarget> {
    if let Some((win, group, index)) = attach_into_bar(screen, steered, docked, bars) {
        return Some(TornTarget::Bar { win, group, index });
    }
    let (win, group, terminal) = terms
        .iter()
        .find(|(win, _, rect)| *win != steered && rect.contains(screen))?;
    let (dir, zone) = split_zone(*terminal, screen);
    Some(TornTarget::Split {
        win: *win,
        group: *group,
        dir,
        zone,
    })
}

/// What one frame of a torn drag does with the tab and the window carrying it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum TornStep {
    /// Keep the window under the pointer, as it has been since the tear.
    Steer,
    /// Put the tab into window `win`'s bar at `index` — for real, now — and
    /// park the window it came from.
    Dock {
        win: u64,
        group: usize,
        index: usize,
    },
    /// Already docked there, and the pointer has slid to a different slot.
    Slide {
        win: u64,
        group: usize,
        index: usize,
    },
    /// Docked, and nothing has changed: the tab is where it should be.
    Settled,
    /// Pulled back out of the bar with the button still down: the tab goes
    /// home and its window comes back under the pointer.
    Undock,
    /// Let go while docked. The tab is already where it belongs, so this only
    /// ends the gesture.
    Attach,
    /// Let go over a half of window `win`'s group `group`: the tab crosses and
    /// becomes a pane there.
    Split {
        win: u64,
        group: usize,
        dir: SplitDir,
    },
    /// Let go anywhere else: the window simply stays where it was carried to.
    Drop,
}

/// The whole gesture in one place: what the pointer is over ([`torn_target`],
/// which already knows what the drag is docked to) plus whether the button is
/// still down decides what this frame does.
///
/// Docking is a hover state, so it is remade from the target every frame and
/// never has to be torn down explicitly: leaving the bar undocks, and a target
/// window that disappears takes its strip out of the hit test with it. The
/// release commits nothing that docking has not already done — which is the
/// point of docking for real rather than painting a promise.
fn torn_step(down: bool, target: Option<TornTarget>, docked: Option<DockSlot>) -> TornStep {
    match (down, target) {
        (_, Some(TornTarget::Bar { win, group, index })) => {
            let slot = DockSlot { win, group, index };
            match docked {
                Some(now) if now == slot => {
                    if down {
                        TornStep::Settled
                    } else {
                        TornStep::Attach
                    }
                }
                Some(now) if now.win == win => TornStep::Slide { win, group, index },
                // Not docked at all, or docked in another window: either way
                // the tab has to travel, and one action does both.
                _ => TornStep::Dock { win, group, index },
            }
        }
        (true, _) => {
            if docked.is_some() {
                TornStep::Undock
            } else {
                TornStep::Steer
            }
        }
        (
            false,
            Some(TornTarget::Split {
                win, group, dir, ..
            }),
        ) => TornStep::Split { win, group, dir },
        (false, None) => TornStep::Drop,
    }
}

/// VS Code's drop shade: a translucent blue wash over the half of a terminal
/// the drop would split into. One drawing for both the in-window drag and the
/// torn one, so a tab dropped across windows promises what it promises at
/// home.
fn paint_drop_zone(painter: &egui::Painter, zone: Rect) {
    painter.rect_filled(zone, 0.0, DROP_ZONE_FILL);
    painter.rect_stroke(
        zone,
        0.0,
        Stroke::new(1.0, DROP_ZONE_EDGE),
        egui::StrokeKind::Inside,
    );
}

/// The wash a window paints for a tab being carried over it: the pointer
/// belongs to whichever viewport is driving the drag, so it arrives in screen
/// points and is converted here against this window's own origin.
///
/// Whichever of this window's terminals holds it gets the same quadrant
/// treatment an in-window drag would draw. Repaints while it is hovered, so
/// the wash follows the pointer even though nothing else is happening here.
///
/// A bar needs no such preview: a tab carried into one is really in it, and
/// the bar draws its own pill.
fn paint_carried_zone(ui: &Ui, geoms: &[GroupGeometry], screen: egui::Pos2, origin: egui::Pos2) {
    let local = screen - origin.to_vec2();
    let Some(geom) = geoms.iter().find(|geom| geom.terminal.contains(local)) else {
        return;
    };
    let (_, zone) = split_zone(geom.terminal, local);
    paint_drop_zone(ui.painter(), zone);
    ui.ctx().request_repaint();
}

/// Does releasing the drag here tear the tab out into a window of its own?
///
/// The fallback half of the gesture. Crossing the edge with a live pointer
/// already tears mid-drag ([`mid_drag_tear`]), so what reaches here is the
/// release whose position the window never saw, plus the ordinary in-window
/// releases this has to answer "no" to.
///
/// Two shapes of "outside", because winit reports both on macOS while a button
/// is held:
///
/// * a **position beyond the viewport**. The OS keeps delivering moves to the
///   window that owns the drag even once the pointer has left it, so the usual
///   report is a perfectly good `Pos2` that simply is not inside the window's
///   own content rect ([`DragWindows::local`]) — negative coordinates above or
///   left of the window,
///   or coordinates past its width/height. It must clear the edge by
///   [`TEAR_MARGIN`] to count, so a pill let go on the window's own frame
///   stays where it is.
/// * **no position at all**. The pointer can also leave without a farewell
///   position — the window loses the cursor (`PointerGone`, a release whose
///   coordinates the window never sees), and egui then has no `interact_pos`
///   to hand out. A drag that ends with the pointer nowhere ended off the
///   window, so that tears too.
///
/// Pure on purpose: the caller passes the two facts and the tests fabricate
/// them (`main.rs::hover_focus` is the same shape). It also answers the
/// *present tense* mid-drag — "would a release here tear?" — which is what
/// dresses the ghost in a window frame before the button comes up.
fn release_tears_out(pointer: Option<egui::Pos2>, viewport: Rect) -> bool {
    match pointer {
        Some(pos) => !viewport.expand(TEAR_MARGIN).contains(pos),
        None => true,
    }
}

/// The floating pill that follows the pointer once a drag has left its bar.
/// Painted on the tooltip layer, so it rides above every column.
///
/// `tearing` says the pointer is already past the window edge, where a release
/// would tear the tab out ([`release_tears_out`]): the pill then gains a thin
/// outline standing off it — the outline of the window it is about to become —
/// so the outcome is legible before the button comes up. Deliberately quiet:
/// the pill is still the pill, wearing a frame, not a second widget.
fn paint_ghost(
    ctx: &egui::Context,
    tabs: &TabManager,
    icons: &IconCache,
    drag: TabDrag,
    pointer: egui::Pos2,
    tearing: bool,
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
    if tearing {
        // The window-to-be: one hairline rectangle standing off the pill on
        // every side, so the pill reads as content inside a frame rather than
        // as a pill that changed colour.
        let frame = rect.expand(f32::from(TEAR_FRAME_INSET));
        painter.rect_stroke(
            frame,
            CornerRadius::same(CORNER + TEAR_FRAME_INSET),
            Stroke::new(1.0, TEAR_FRAME_EDGE),
            egui::StrokeKind::Inside,
        );
    }
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

/// The follow half of a torn drag, run from [`tab_drag_overlay`] in the
/// *owning* window's pass only.
///
/// The window being carried is not always the owner: after a
/// [`AppAction::MoveTabToNewWindow`] the tab lives in a window of its own and
/// that is the one to move, so it is looked up from the tab each frame rather
/// than remembered. The action is applied by the root frame, so for a frame or
/// two the tab is still here and there is nothing to carry yet — that frame is
/// simply skipped.
///
/// While the drag is docked the tab is somewhere else entirely, so the carried
/// window is [`TabDrag::host`] instead: the one parked offscreen, held alive,
/// and waiting to be handed the tab back if the pill leaves the bar again.
///
/// Returns what to publish about the tab in flight, so a window it is being
/// carried over can wash the half a drop would split into ([`CarryReport`]).
fn torn_drag(
    ui: &Ui,
    tabs: &TabManager,
    mut drag: TabDrag,
    geoms: &[GroupGeometry],
    windows: &DragWindows,
    actions: &mut Vec<AppAction>,
) -> CarryReport {
    let ctx = ui.ctx();
    let pointer = ctx.input(|i| i.pointer.interact_pos());
    let down = ctx.input(|i| i.pointer.primary_down());
    let Some(home) = tabs.window_of_tab(drag.id) else {
        // The tab died mid-flight (shell exit, `terra kill`) — with the hold
        // and the parked window still outstanding, both of which this settles.
        end_torn_drag(ctx, drag, None, None, actions);
        return CarryReport::Idle;
    };
    // Docked, the tab is in the target window and says nothing about what is
    // being carried: that is the host, parked below the desktop.
    let carried = match (drag.docked, drag.host) {
        (Some(_), Some(host)) => Some(host),
        _ if drag.moved => (home != drag.win).then_some(home),
        _ => Some(drag.win),
    };
    let screen = match (pointer, windows.origin) {
        (Some(pointer), Some(origin)) => Some(origin + pointer.to_vec2()),
        _ => None,
    };
    // What is under the pointer. Only consulted once there is a window to
    // carry: until then the tab is still at home and the owner's own bar is
    // right under the pointer.
    let target = match (screen, carried) {
        (Some(screen), Some(carried)) => {
            torn_target(screen, carried, drag.docked, &windows.bars, &windows.terms)
        }
        _ => None,
    };
    // Where the carried window goes if it is following the pointer this frame.
    let follow = |screen: egui::Pos2| screen - drag.wgrab;

    match torn_step(down, target, drag.docked) {
        TornStep::Dock { win, group, index } => {
            let (Some(screen), Some(carried)) = (screen, carried) else {
                return CarryReport::Idle;
            };
            // The tab really moves, now, into the bar under the pointer. The
            // window it came from is held first (emptying it would otherwise
            // close the very window the OS is delivering this drag to) and
            // parked far below the desktop, so what the user sees is the tab
            // sitting in its new bar — which is exactly what has happened.
            actions.push(AppAction::DockTab {
                id: drag.id,
                host: carried,
                win,
                group,
                index,
            });
            if drag.docked.is_none() {
                actions.push(AppAction::DragWindowTo {
                    win: carried,
                    pos: follow(screen) + DOCK_PARK,
                });
            }
            drag.docked = Some(DockSlot { win, group, index });
            drag.host = Some(carried);
            set_drag(ctx, Some(drag));
            if !down {
                // Docked and released on the same frame: the tab has arrived,
                // there is nothing left to do but tidy up.
                end_torn_drag(ctx, drag, Some(carried), Some(screen), actions);
                return CarryReport::Idle;
            }
        }
        TornStep::Slide { win, group, index } => {
            // Sliding along the bar it is docked in, Chrome style: one move
            // per slot crossed, nothing at all while the pointer sits still.
            actions.push(AppAction::ReorderDocked {
                id: drag.id,
                win,
                group,
                index,
            });
            drag.docked = Some(DockSlot { win, group, index });
            set_drag(ctx, Some(drag));
        }
        TornStep::Settled => {}
        TornStep::Undock => {
            // Pulled back out of the bar: the tab goes home to the window that
            // has been waiting for it, which comes back under the pointer on
            // this very frame, from the same `wgrab` it was parked with.
            if let Some(host) = drag.host {
                actions.push(AppAction::UndockTab { id: drag.id, host });
                if let Some(screen) = screen {
                    actions.push(AppAction::DragWindowTo {
                        win: host,
                        pos: follow(screen),
                    });
                }
            }
            drag.docked = None;
            set_drag(ctx, Some(drag));
        }
        TornStep::Steer => {
            if let Some(TornTarget::Split { win, .. }) = target {
                // Over a terminal half: nothing happens until the button comes
                // up, but the half is washed so the split is legible first. A
                // foreign window paints its own ([`paint_carried_zone`], off
                // `carry`); this window paints it here, where the pointer
                // needs no round trip.
                if win == windows.win {
                    if let (Some(screen), Some(origin)) = (screen, windows.origin) {
                        paint_carried_zone(ui, geoms, screen, origin);
                    }
                }
            }
            if let (Some(screen), Some(carried)) = (screen, carried) {
                actions.push(AppAction::DragWindowTo {
                    win: carried,
                    pos: follow(screen),
                });
            }
        }
        TornStep::Attach => {
            // Let go while docked. The tab has been in that window since the
            // pill entered its bar, so the release moves nothing — it only
            // ends the gesture and lets the emptied host go.
            end_torn_drag(ctx, drag, carried, screen, actions);
            return CarryReport::Idle;
        }
        TornStep::Split { win, group, dir } => {
            // Let go over a foreign terminal half: the one the wash has been
            // promising.
            actions.push(AppAction::SplitTabInWindow {
                id: drag.id,
                win,
                group,
                dir,
            });
            end_torn_drag(ctx, drag, carried, screen, actions);
            return CarryReport::Idle;
        }
        TornStep::Drop => {
            // Let go in the open: the window stays where it was carried to,
            // which is the whole point of having carried it there.
            end_torn_drag(ctx, drag, carried, screen, actions);
            return CarryReport::Idle;
        }
    }

    // No ghost: the window under the pointer — or, docked, the pill the target
    // window is now drawing itself — is the drag feedback.
    ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
    ctx.request_repaint();
    match screen {
        // Docked, there is nothing for another window to preview: the tab is
        // in a bar, not hovering over a terminal, and the bar it is in draws
        // it. Publishing nothing also keeps a bar's slack band, which hangs a
        // little into the terminal below it, from washing that terminal blue
        // under the docked pill.
        Some(screen) if drag.docked.is_none() => CarryReport::Carrying(CarryState {
            id: drag.id,
            screen,
            steer: carried,
        }),
        _ => CarryReport::Idle,
    }
}

/// End a torn drag, whatever ended it: clear the drag state, drop the hold on
/// the window the tab came from, and — the part that is easy to forget — bring
/// that window back from the parking spot below the desktop if it was still
/// docked. A window left there is gone as far as the user is concerned, and a
/// hold left standing is an empty window that never closes, so every exit goes
/// through here.
fn end_torn_drag(
    ctx: &egui::Context,
    drag: TabDrag,
    carried: Option<u64>,
    screen: Option<egui::Pos2>,
    actions: &mut Vec<AppAction>,
) {
    if let (Some(_), Some(host), Some(screen)) = (drag.docked, drag.host, screen) {
        // Docked at the end: the host is empty and about to collapse, but a
        // move that did not take must not cost the user a window.
        actions.push(AppAction::DragWindowTo {
            win: host,
            pos: screen - drag.wgrab,
        });
    }
    if let Some(host) = drag.host.or(carried) {
        actions.push(AppAction::ReleaseDragHold { host });
    }
    set_drag(ctx, None);
    ctx.request_repaint();
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
///
/// Once the pointer leaves the window the drag goes *torn*: a real OS window
/// (freshly minted, or this whole window when the tab is its last) follows the
/// pointer, held by its own tab pill, until the button comes up. From there
/// the desktop offers the same two targets a window does. Carrying the tab
/// into another terra window's *tab bar* docks it there — and docking is not a
/// promise: the tab really moves, into that bar, at the slot under the pointer,
/// with its terminal on screen, while the window it came from parks out of
/// sight and is held alive so the drag it is pumping survives. Pull the pill
/// back out and the tab goes home again; let go and nothing further happens,
/// because everything already has. Over a half of another window's *terminal*
/// the half is washed blue as it is at home, and that one does wait for the
/// release; anywhere else the release just leaves the window where it was.
///
/// Every window's render pass calls this with the same global drag state, so
/// the first job is deciding whose drag it is — only the owner may touch it.
/// A bystander's pass is not idle though: the pointer belongs to the owner's
/// viewport, so a window being carried over paints its own preview from what
/// the owner published ([`DragWindows::carry`]).
pub fn tab_drag_overlay(
    ui: &Ui,
    tabs: &TabManager,
    icons: &IconCache,
    geoms: &[GroupGeometry],
    windows: &DragWindows,
    actions: &mut Vec<AppAction>,
) -> CarryReport {
    let ctx = ui.ctx().clone();
    let Some(mut drag) = current_drag(&ctx) else {
        // No drag anywhere — the state is global, so any pass may say so, and
        // one of them has to or a finished carry would never be cleared.
        return CarryReport::Idle;
    };
    let pointer = ctx.input(|i| i.pointer.interact_pos());
    let down = ctx.input(|i| i.pointer.primary_down());

    // Ownership, claimed by the focus loan: during a render the model is
    // focused on the window being drawn, so `group_of` answers `Some` in
    // exactly one window's pass — the one the dragged tab lives in. That makes
    // the first pass that can see the tab the owner.
    if drag.win == u64::MAX {
        if tabs.group_of(drag.id).is_some() {
            drag.win = windows.win;
            set_drag(&ctx, Some(drag));
        } else if down {
            // Someone else's window, or nobody's yet. Crucially it does *not*
            // clear: a bystander deciding a drag is dead is how a second
            // torn-out window used to kill the drag on its first frame.
            return CarryReport::NotMine;
        } else {
            // Released and still unclaimed — no window ever saw the tab, so
            // there is nothing to drop and no owner to clean up after it.
            set_drag(&ctx, None);
            return CarryReport::Idle;
        }
    }
    if drag.win != windows.win {
        // Not this window's drag to drive — but the tab may be over one of its
        // terminals, and the wash showing which half a drop would take is this
        // window's to paint. (A tab over a *bar* needs no preview: it is really
        // in that bar, drawn by the bar itself.)
        if let (Some(carry), Some(origin)) = (windows.carry, windows.origin) {
            if carry.steer != Some(windows.win) {
                paint_carried_zone(ui, geoms, carry.screen, origin);
            }
        }
        return CarryReport::NotMine;
    }

    if drag.torn {
        return torn_drag(ui, tabs, drag, geoms, windows, actions);
    }

    let Some(src_group) = tabs.group_of(drag.id) else {
        // Closed under the pointer (⌘W, `terra kill`, shell exit).
        set_drag(&ctx, None);
        return CarryReport::Idle;
    };
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
        // Outside the window first, before any drop target is consulted: a
        // release out there belongs to no group, and the tear-out is what the
        // gesture meant. The model may still decide it is a no-op (the sole
        // tab of the sole window has nowhere to go).
        if release_tears_out(pointer, windows.local) {
            actions.push(AppAction::MoveTabToNewWindow {
                id: drag.id,
                pos: None,
            });
            set_drag(&ctx, None);
            return CarryReport::Idle;
        }
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
        return CarryReport::Idle;
    }

    let Some(pointer) = pointer else {
        return CarryReport::Idle;
    };

    // Past the edge, with the button still down: the window is born now and
    // follows the pointer from here (Chrome's gesture), instead of appearing
    // out of nowhere when the button comes up.
    let here = tabs
        .ids()
        .into_iter()
        .filter(|id| tabs.window_of_tab(*id) == Some(drag.win))
        .count();
    match mid_drag_tear(down, Some(pointer), windows.local, here) {
        MidDragTear::NewWindow => {
            // The window opens with its first pill under the pointer, at the
            // same spot inside the pill the drag was started at, so the ghost
            // becoming a window is not a jump. `wgrab` is that same offset,
            // which is what the follow keeps holding every frame after.
            let anchor = tear_anchor(drag.grab, decor_height(&ctx));
            actions.push(AppAction::MoveTabToNewWindow {
                id: drag.id,
                pos: windows
                    .origin
                    .map(|origin| origin + pointer.to_vec2() - anchor),
            });
            drag.torn = true;
            drag.moved = true;
            drag.wgrab = anchor;
            set_drag(&ctx, Some(drag));
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
            ctx.request_repaint();
            return CarryReport::Idle;
        }
        MidDragTear::SteerWindow => {
            // Nothing to tear out of a one-tab window: carry the window
            // itself, held wherever the cursor currently is on it.
            let outer = ctx.input(|i| i.viewport().outer_rect);
            if let (Some(origin), Some(outer)) = (windows.origin, outer) {
                drag.torn = true;
                drag.moved = false;
                drag.wgrab = (origin + pointer.to_vec2()) - outer.min;
                set_drag(&ctx, Some(drag));
                ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
                ctx.request_repaint();
                return CarryReport::Idle;
            }
            // No screen geometry yet: fall through and keep dragging the
            // ghost, which still tears on release.
        }
        MidDragTear::None => {}
    }

    if let Some(DropTarget::Split { zone, .. }) = target {
        paint_drop_zone(ui.painter(), zone);
    }
    if !in_own_bar(pointer) {
        // Inside its own bar the live pill *is* the drag feedback. Past the
        // window edge the ghost puts on a window frame, so the tear-out reads
        // before the button comes up rather than surprising the user after.
        let tearing = release_tears_out(Some(pointer), windows.local);
        paint_ghost(&ctx, tabs, icons, drag, pointer, tearing);
    }
    ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
    ctx.request_repaint();
    CarryReport::Idle
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

    /// A lone tab is a tab, not an address bar: it takes its cap and leaves the
    /// rest of the bar empty. Only a bar too narrow to grant even that goes back
    /// to sharing.
    #[test]
    fn a_single_tab_stops_at_the_cap() {
        assert_eq!(tab_width(400.0, 1), MAX_TAB_WIDTH);
        assert_eq!(tab_width(2000.0, 1), MAX_TAB_WIDTH);
        let narrow = tab_width(200.0, 1);
        assert!(narrow < MAX_TAB_WIDTH);
        assert!((narrow + PLUS_GAP + PLUS_WIDTH + CHEVRON_WIDTH - 200.0).abs() < 0.01);
    }

    /// The cap is per bar, so it is the *share* that decides: the same window
    /// hands three tabs the cap and four tabs less than it.
    #[test]
    fn crowded_tabs_keep_sharing_the_bar_below_the_cap() {
        // 800 wide: 746 usable, so three tabs want 248.7 and are capped.
        assert_eq!(tab_width(800.0, 3), MAX_TAB_WIDTH);
        let four = tab_width(800.0, 4);
        assert!(four < MAX_TAB_WIDTH);
        assert!(
            (4.0 * four + 3.0 * TAB_GAP + PLUS_GAP + PLUS_WIDTH + CHEVRON_WIDTH - 800.0).abs()
                < 0.01
        );
        // A narrow bar's tabs are untouched by the cap, at every count.
        for n in 1..40 {
            let w = tab_width(300.0, n);
            assert!(w <= MAX_TAB_WIDTH, "{n} tabs at {w}");
        }
    }

    /// The `+` follows the last tab while there is room, and parks at the end
    /// of the bar the moment the tabs claim the whole row — the two agree
    /// exactly at the crossover, so adding tabs never makes it jump.
    #[test]
    fn the_plus_docks_after_the_last_tab_until_the_row_is_full() {
        let (left, right) = (10.0, 810.0);
        let parked = right - PLUS_WIDTH - CHEVRON_WIDTH;
        let tabs_right = tabs_limit(right);

        // One capped tab: the button sits a gap past its right edge.
        let one = left + tab_width(800.0, 1);
        assert_eq!(plus_left(one, right), one + PLUS_GAP);
        // Three capped tabs: further right, still short of the parked slot.
        let three = left + 3.0 * tab_width(800.0, 3) + 2.0 * TAB_GAP;
        assert_eq!(plus_left(three, right), three + PLUS_GAP);
        assert!(plus_left(three, right) < parked);
        // Exactly full, and overflowing: parked at the end, both times.
        assert_eq!(plus_left(tabs_right, right), parked);
        assert_eq!(plus_left(tabs_right + 500.0, right), parked);
        // Never leaves the bar, however long the row claims to be.
        for end in [0.0, 100.0, 500.0, 5000.0] {
            assert!(chevron_left(plus_left(end, right)) + CHEVRON_WIDTH <= right);
        }
    }

    /// Render one frame of a bar button at `rect` with the pointer somewhere,
    /// and report the glyph colour it chose plus every rect fill that reached
    /// the paint list. Two frames because egui hit-tests against the widget
    /// rects it registered *last* frame.
    fn bar_button_frame(rect: Rect, pointer: egui::Pos2) -> (Color32, Vec<Color32>) {
        let ctx = egui::Context::default();
        let mut glyph = Color32::PLACEHOLDER;
        let mut fills = Vec::new();
        let screen = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(800.0, 600.0));
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events: vec![egui::Event::PointerMoved(pointer)],
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let response = ui.interact(rect, Id::new("probe"), Sense::click());
                    glyph = bar_button_chrome(ui, rect, &response, false);
                });
            });
            fills = output
                .shapes
                .iter()
                .filter_map(|clipped| match &clipped.shape {
                    egui::Shape::Rect(r) => Some(r.fill),
                    _ => None,
                })
                .collect();
        }
        (glyph, fills)
    }

    /// Windows Terminal's treatment: at rest the `+` and `⌄` are bare glyphs in
    /// the inactive title's grey, with no ring and no fill of their own; the
    /// wash and the brighter glyph only arrive with the pointer.
    #[test]
    fn a_bar_button_is_bare_until_the_pointer_reaches_it() {
        let rect = Rect::from_min_size(egui::pos2(100.0, 4.0), Vec2::new(PLUS_WIDTH, 24.0));

        let (cold, cold_fills) = bar_button_frame(rect, egui::pos2(5.0, 400.0));
        assert_eq!(cold, TITLE_IDLE);
        assert!(!cold_fills.contains(&BAR_BUTTON_HOVER_BG));
        assert!(!cold_fills.contains(&BAR_BUTTON_ACTIVE_BG));

        let (hot, hot_fills) = bar_button_frame(rect, rect.center());
        assert_eq!(hot, TEXT_ACTIVE);
        assert!(hot_fills.contains(&BAR_BUTTON_HOVER_BG));
    }

    /// The `⌄` is welded to the `+`, so the pair travels as one cluster —
    /// including into the parked slot, where together they exactly fill the
    /// space the row gave up.
    #[test]
    fn the_chevron_rides_along_on_the_plus() {
        let right = 810.0;
        for end in [0.0, 100.0, 500.0, 5000.0] {
            let plus = plus_left(end, right);
            assert_eq!(chevron_left(plus), plus + PLUS_WIDTH);
        }
        // At the crossover the tabs stop one gap short of the cluster, and the
        // cluster reaches the bar's right edge exactly.
        let plus = plus_left(tabs_limit(right), right);
        assert_eq!(plus - tabs_limit(right), PLUS_GAP);
        assert_eq!(chevron_left(plus) + CHEVRON_WIDTH, right);
    }

    /// A tab wide enough to be capped is wide enough for its `⌘n`, so the hint
    /// never disappears just because the cap kicked in.
    #[test]
    fn a_capped_tab_still_has_room_for_its_hint() {
        // However wide the window, the widest tab it can hand out clears both
        // the hint's threshold and the title's own reserved margins.
        for bar in [400.0, 1200.0, 4000.0] {
            let w = tab_width(bar, 1);
            assert!(w >= HINT_MIN_TAB_WIDTH, "{bar} wide bar gave {w}");
            assert!(w > 2.0 * TITLE_RESERVE);
        }
    }

    /// The menu offers terra's own two commands first — a plain new tab and
    /// the palette, each with its key hint — then the profiles in the order
    /// they arrive, which is the config's `BTreeMap` order, i.e. alphabetical.
    #[test]
    fn the_chevron_menu_lists_the_commands_then_every_profile() {
        let bare = new_tab_entries(std::iter::empty());
        assert_eq!(bare.len(), MENU_COMMAND_ROWS);
        assert_eq!(
            bare[0],
            MenuEntry::new("New Tab", AppAction::NewTab).with_shortcut("⌘T")
        );
        assert_eq!(
            bare[1],
            MenuEntry::new("Command Palette", AppAction::OpenPalette).with_shortcut("⇧⌘P")
        );

        let profiles = [profile("build", "cargo build"), profile("htop", "htop")];
        let entries = new_tab_entries(&profiles);
        let labels: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["New Tab", "Command Palette", "build", "htop"]);
        assert_eq!(
            entries[2].action,
            AppAction::NewTabProfile("build".to_owned())
        );
        assert_eq!(
            entries[3].action,
            AppAction::NewTabProfile("htop".to_owned())
        );
        // A profile row is reachable only from the menu, so it carries no hint.
        assert!(entries[2].shortcut.is_none());
    }

    /// Settings left this menu when "Edit Settings With …" arrived: the app
    /// menu and the palette both carry it (⌘, still opens the file), and a
    /// dropdown hanging off `+` is about *opening tabs*.
    #[test]
    fn the_chevron_menu_no_longer_offers_settings() {
        let profiles = [profile("build", "cargo build")];
        for entry in new_tab_entries(&profiles) {
            assert_ne!(entry.action, AppAction::OpenConfig);
            assert_ne!(entry.icon, TabIcon::Gear);
        }
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
                // "New Tab" and "Command Palette" — terra's own commands.
                TabIcon::Terminal,
                TabIcon::Terminal,
                TabIcon::Htop,
                TabIcon::OpenAi,
                TabIcon::Terminal,
            ]
        );
    }

    #[test]
    fn the_bar_shows_for_a_lone_tab_by_default() {
        // Chrome over nothing stays hidden; one tab is the default-on case.
        assert!(!bar_visible(0, 1, true));
        assert!(bar_visible(1, 1, true));
        assert!(bar_visible(2, 1, true));
        // With a second group every column shows its bar, tabs or not.
        assert!(bar_visible(1, 2, true));
        assert!(bar_visible(0, 2, true));
    }

    /// `[tabs] bar_with_one_tab = false` buys back the bare single-tab window,
    /// and nothing else: two tabs, or two groups, still show every bar.
    #[test]
    fn bar_with_one_tab_off_hides_the_bar_for_a_lone_tab_only() {
        assert!(!bar_visible(0, 1, false));
        assert!(!bar_visible(1, 1, false));
        assert!(bar_visible(2, 1, false));
        assert!(bar_visible(1, 2, false));
        assert!(bar_visible(2, 2, false));
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

    /// A 900×600 window, the shape [`release_tears_out`] is asked about.
    fn viewport() -> Rect {
        Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(900.0, 600.0))
    }

    /// A release anywhere the window can see keeps the drag ordinary: the drop
    /// targets (own bar, foreign bar, terminal half) decide it, not this.
    #[test]
    fn a_release_inside_the_window_never_tears() {
        let v = viewport();
        for pos in [
            v.center(),
            v.min,
            v.max,
            egui::pos2(0.0, 300.0),
            egui::pos2(899.0, 1.0),
        ] {
            assert!(!release_tears_out(Some(pos), v), "{pos:?}");
        }
    }

    /// Past every edge, on all four sides — the window's own frame is not
    /// enough, but a hand that carried the pill off the window is.
    #[test]
    fn a_release_beyond_any_edge_tears() {
        let v = viewport();
        for pos in [
            egui::pos2(-40.0, 300.0),
            egui::pos2(940.0, 300.0),
            egui::pos2(450.0, -30.0),
            egui::pos2(450.0, 660.0),
            // Diagonally off a corner counts too.
            egui::pos2(-20.0, -20.0),
        ] {
            assert!(release_tears_out(Some(pos), v), "{pos:?}");
        }
    }

    /// [`TEAR_MARGIN`] is the whole point of the slack: a pointer sitting on —
    /// or a hair past — the window edge is inside a window whose last row of
    /// pixels it is touching, not a tear-out.
    #[test]
    fn grazing_the_edge_is_not_a_tear() {
        let v = viewport();
        assert!(!release_tears_out(
            Some(egui::pos2(900.0 + TEAR_MARGIN - 1.0, 300.0)),
            v
        ));
        assert!(!release_tears_out(Some(egui::pos2(-TEAR_MARGIN, 300.0)), v));
        assert!(release_tears_out(
            Some(egui::pos2(900.0 + TEAR_MARGIN + 1.0, 300.0)),
            v
        ));
    }

    /// The other shape of "outside": the pointer left without a farewell
    /// position, so there is no `interact_pos` at all. That release happened
    /// off the window.
    #[test]
    fn a_release_with_no_pointer_position_tears() {
        assert!(release_tears_out(None, viewport()));
    }

    /// The mid-drag half: inside the window (edge slack included) the gesture
    /// is still an ordinary drag, whatever the tab count.
    #[test]
    fn a_pointer_inside_the_window_tears_nothing() {
        let v = viewport();
        for pos in [v.center(), v.min, v.max, egui::pos2(900.0, 300.0)] {
            assert_eq!(
                mid_drag_tear(true, Some(pos), v, 3),
                MidDragTear::None,
                "{pos:?}"
            );
        }
    }

    /// Past the edge with company left behind: the tab becomes a window of its
    /// own, mid-drag, without waiting for the button.
    #[test]
    fn crossing_the_edge_with_siblings_mints_a_window() {
        let v = viewport();
        assert_eq!(
            mid_drag_tear(true, Some(egui::pos2(-40.0, 300.0)), v, 2),
            MidDragTear::NewWindow
        );
        assert_eq!(
            mid_drag_tear(true, Some(egui::pos2(450.0, 700.0)), v, 9),
            MidDragTear::NewWindow
        );
    }

    /// The last tab of a window has nothing to be torn out of, so the window
    /// itself goes along for the ride — Chrome's behaviour, and the reason the
    /// gesture never has to refuse.
    #[test]
    fn crossing_the_edge_with_the_last_tab_steers_the_window() {
        let v = viewport();
        assert_eq!(
            mid_drag_tear(true, Some(egui::pos2(-40.0, 300.0)), v, 1),
            MidDragTear::SteerWindow
        );
    }

    /// Nothing to decide without a pointer down and a position: a release is
    /// the old path's business ([`release_tears_out`]), and a pointer that
    /// vanished cannot steer a window anywhere.
    #[test]
    fn no_button_or_no_position_decides_nothing() {
        let v = viewport();
        assert_eq!(
            mid_drag_tear(false, Some(egui::pos2(-40.0, 300.0)), v, 3),
            MidDragTear::None
        );
        assert_eq!(mid_drag_tear(true, None, v, 3), MidDragTear::None);
        assert_eq!(
            mid_drag_tear(true, Some(egui::pos2(-40.0, 300.0)), v, 0),
            MidDragTear::None
        );
    }

    /// Two windows' bars, side by side: the root's across the top of a
    /// 900-wide window at the origin holding three tabs, and torn-out window
    /// 7's at (1000, 200) holding one.
    fn bars() -> [BarStrip; 2] {
        [
            BarStrip {
                win: 0,
                group: 0,
                rect: Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(900.0, TAB_BAR_HEIGHT)),
                tabs: 3,
            },
            BarStrip {
                win: 7,
                group: 0,
                rect: Rect::from_min_size(
                    egui::pos2(1000.0, 200.0),
                    Vec2::new(400.0, TAB_BAR_HEIGHT),
                ),
                tabs: 1,
            },
        ]
    }

    /// Docked in the root window's only group, at slot `index`.
    fn docked(index: usize) -> Option<DockSlot> {
        Some(DockSlot {
            win: 0,
            group: 0,
            index,
        })
    }

    /// The dock hit test works in screen points and skips the window being
    /// carried — a torn window's own bar is under its own pointer the whole
    /// time, which is exactly where the pointer holds it.
    #[test]
    fn a_torn_drag_finds_the_bar_it_is_over() {
        let bars = bars();
        // Into the root's bar, carrying window 7: a move back home, at the
        // slot the pointer is over rather than always at the end.
        assert_eq!(
            attach_into_bar(egui::pos2(60.0, 16.0), 7, None, &bars),
            Some((0, 0, 0))
        );
        assert_eq!(
            attach_into_bar(egui::pos2(880.0, 16.0), 7, None, &bars),
            Some((0, 0, 3))
        );
        // Into window 7's bar, carrying the root: the other direction too.
        assert_eq!(
            attach_into_bar(egui::pos2(1010.0, 216.0), 0, None, &bars),
            Some((7, 0, 0))
        );
        // Over the carried window's own bar: nothing, or the drag would dock
        // into the window it is carrying.
        assert_eq!(
            attach_into_bar(egui::pos2(1100.0, 216.0), 7, None, &bars),
            None
        );
    }

    /// Only the bar docks: the body of a window below it is somewhere to
    /// leave a window, not somewhere to hand a tab over.
    #[test]
    fn a_torn_drag_over_a_window_body_or_the_desktop_docks_to_nothing() {
        let bars = bars();
        assert_eq!(
            attach_into_bar(egui::pos2(450.0, 300.0), 7, None, &bars),
            None
        );
        assert_eq!(
            attach_into_bar(egui::pos2(1500.0, 900.0), 7, None, &bars),
            None
        );
    }

    /// The slot is the bar's own arithmetic replayed from outside: the same
    /// tab widths, and one more slot to insert at than there are tabs — except
    /// for a tab already docked in that bar, which can only trade places with
    /// the ones beside it.
    #[test]
    fn the_slot_follows_the_pointer_along_the_strip() {
        let strip = bars()[0];
        assert_eq!(bar_slot(&strip, 0.0, false), 0);
        assert_eq!(bar_slot(&strip, 10_000.0, false), 3);
        assert_eq!(bar_slot(&strip, 10_000.0, true), 2);
        // Monotonic across the row: the slot never goes backwards as the
        // pointer moves right.
        let mut last = 0;
        for step in 0..90 {
            let slot = bar_slot(&strip, step as f32 * 10.0, false);
            assert!(slot >= last, "slot {slot} after {last}");
            last = slot;
        }
    }

    /// Entering a foreign bar docks the tab — really, into that window — and
    /// releasing there has nothing left to do.
    #[test]
    fn entering_a_bar_docks_and_releasing_there_only_ends_the_gesture() {
        let bar = TornTarget::Bar {
            win: 0,
            group: 0,
            index: 2,
        };
        assert_eq!(
            torn_step(true, Some(bar), None),
            TornStep::Dock {
                win: 0,
                group: 0,
                index: 2
            }
        );
        assert_eq!(torn_step(true, Some(bar), docked(2)), TornStep::Settled);
        assert_eq!(torn_step(false, Some(bar), docked(2)), TornStep::Attach);
        // Released the same frame it arrives: it still has to get there.
        assert_eq!(
            torn_step(false, Some(bar), None),
            TornStep::Dock {
                win: 0,
                group: 0,
                index: 2
            }
        );
    }

    /// Moving along the bar it is docked in slides the tab; moving to another
    /// window's bar is a fresh dock, which travels in one step.
    #[test]
    fn sliding_along_a_bar_reorders_and_a_foreign_bar_redocks() {
        let here = TornTarget::Bar {
            win: 0,
            group: 0,
            index: 1,
        };
        assert_eq!(
            torn_step(true, Some(here), docked(2)),
            TornStep::Slide {
                win: 0,
                group: 0,
                index: 1
            }
        );
        let elsewhere = TornTarget::Bar {
            win: 7,
            group: 0,
            index: 0,
        };
        assert_eq!(
            torn_step(true, Some(elsewhere), docked(2)),
            TornStep::Dock {
                win: 7,
                group: 0,
                index: 0
            }
        );
    }

    /// Leaving the bar with the button still down takes the tab back out:
    /// undocked, the window it came from follows the pointer again.
    #[test]
    fn leaving_the_bar_undocks_and_steers_again() {
        assert_eq!(torn_step(true, None, docked(0)), TornStep::Undock);
        assert_eq!(torn_step(true, None, None), TornStep::Steer);
        let split = TornTarget::Split {
            win: 0,
            group: 1,
            dir: SplitDir::Right,
            zone: Rect::NOTHING,
        };
        assert_eq!(torn_step(true, Some(split), docked(0)), TornStep::Undock);
        assert_eq!(torn_step(true, Some(split), None), TornStep::Steer);
    }

    /// A release away from every bar never docks: over a foreign terminal it
    /// splits that group, over bare desktop it leaves the window where it was.
    #[test]
    fn a_release_outside_a_bar_never_docks() {
        assert_eq!(
            torn_step(
                false,
                Some(TornTarget::Split {
                    win: 4,
                    group: 2,
                    dir: SplitDir::Down,
                    zone: Rect::NOTHING,
                }),
                None
            ),
            TornStep::Split {
                win: 4,
                group: 2,
                dir: SplitDir::Down,
            }
        );
        assert_eq!(torn_step(false, None, None), TornStep::Drop);
    }

    /// The strip a drag is already docked to is the more forgiving one: a
    /// point that would not have docked keeps a docked drag docked, so the
    /// tab does not hop in and out of the bar it is sitting in.
    #[test]
    fn a_docked_strip_holds_on_a_little_longer() {
        let bars = bars();
        let outside = egui::pos2(450.0, TAB_BAR_HEIGHT + BAR_DRAG_SLACK + 1.0);
        assert_eq!(attach_into_bar(outside, 7, None, &bars), None);
        assert!(attach_into_bar(outside, 7, docked(1), &bars).is_some());
        // Only for the strip it is docked to, and only so far.
        let elsewhere = Some(DockSlot {
            win: 9,
            group: 0,
            index: 0,
        });
        assert_eq!(attach_into_bar(outside, 7, elsewhere, &bars), None);
        assert_eq!(
            attach_into_bar(
                egui::pos2(
                    450.0,
                    TAB_BAR_HEIGHT + BAR_DRAG_SLACK + DOCK_HYSTERESIS + 1.0
                ),
                7,
                docked(1),
                &bars
            ),
            None
        );
    }

    /// The root window's two terminals under its bar, and window 7's one.
    fn terms() -> [(u64, usize, Rect); 3] {
        [
            (
                0u64,
                0usize,
                Rect::from_min_size(egui::pos2(0.0, TAB_BAR_HEIGHT), Vec2::new(450.0, 568.0)),
            ),
            (
                0u64,
                1usize,
                Rect::from_min_size(egui::pos2(450.0, TAB_BAR_HEIGHT), Vec2::new(450.0, 568.0)),
            ),
            (
                7u64,
                0usize,
                Rect::from_min_size(
                    egui::pos2(1000.0, 200.0 + TAB_BAR_HEIGHT),
                    Vec2::new(400.0, 268.0),
                ),
            ),
        ]
    }

    /// A torn drag over a foreign terminal picks that window's group and the
    /// quadrant under the pointer — the same rule an in-window drag follows,
    /// only across a window boundary.
    #[test]
    fn a_torn_drag_over_a_foreign_terminal_picks_a_half_of_it() {
        let (bars, terms) = (bars(), terms());
        // Well right of centre in the root's second group, carrying window 7.
        assert_eq!(
            torn_target(egui::pos2(880.0, 300.0), 7, None, &bars, &terms),
            Some(TornTarget::Split {
                win: 0,
                group: 1,
                dir: SplitDir::Right,
                zone: Rect::from_min_size(
                    egui::pos2(675.0, TAB_BAR_HEIGHT),
                    Vec2::new(225.0, 568.0)
                ),
            })
        );
        // Near the top of the first group: the upper half.
        assert!(matches!(
            torn_target(egui::pos2(200.0, 80.0), 7, None, &bars, &terms),
            Some(TornTarget::Split {
                win: 0,
                group: 0,
                dir: SplitDir::Up,
                ..
            })
        ));
    }

    /// The window under the pointer is the window being carried, and a tab
    /// cannot be dropped into the thing carrying it.
    #[test]
    fn the_steered_window_is_no_split_target_either() {
        let (bars, terms) = (bars(), terms());
        assert_eq!(
            torn_target(egui::pos2(1100.0, 300.0), 7, None, &bars, &terms),
            None
        );
        // Someone else carrying the root: window 7's terminal is fair game.
        assert!(matches!(
            torn_target(egui::pos2(1100.0, 300.0), 0, None, &bars, &terms),
            Some(TornTarget::Split { win: 7, .. })
        ));
    }

    /// The bar's slack band hangs a few points into the terminal below it.
    /// There the gesture aimed at the row of pills wins: docking, not
    /// splitting.
    #[test]
    fn the_bar_wins_where_its_slack_overlaps_a_terminal() {
        let (bars, terms) = (bars(), terms());
        let overlap = egui::pos2(200.0, TAB_BAR_HEIGHT + BAR_DRAG_SLACK / 2.0);
        assert!(terms[0].2.contains(overlap));
        assert!(matches!(
            torn_target(overlap, 7, None, &bars, &terms),
            Some(TornTarget::Bar {
                win: 0,
                group: 0,
                ..
            })
        ));
        // A hair further down and the terminal has it.
        assert!(matches!(
            torn_target(
                egui::pos2(200.0, TAB_BAR_HEIGHT + BAR_DRAG_SLACK + 1.0),
                7,
                None,
                &bars,
                &terms
            ),
            Some(TornTarget::Split {
                win: 0,
                group: 0,
                ..
            })
        ));
    }

    /// Bare desktop is neither.
    #[test]
    fn a_torn_drag_over_nothing_targets_nothing() {
        assert_eq!(
            torn_target(egui::pos2(1500.0, 900.0), 7, None, &bars(), &terms()),
            None
        );
    }

    /// The torn window is held by its pill: back the outer corner off by the
    /// bar's left padding plus where inside the pill the drag began, and by
    /// the decoration plus half a bar, and the pointer lands on that spot of
    /// the pill. A grab past the ghost's own width clamps, so a pointer can
    /// never end up beyond the pill it is carrying.
    #[test]
    fn the_tear_anchor_puts_the_pointer_on_the_pill() {
        let anchor = tear_anchor(40.0, 28.0);
        assert!((anchor.x - (f32::from(PAD_X) + 40.0)).abs() < 0.01);
        assert!((anchor.y - (28.0 + TAB_BAR_HEIGHT / 2.0)).abs() < 0.01);
        // Grabbed at the very left of the pill: only the bar padding is left.
        assert!((tear_anchor(0.0, 0.0).x - f32::from(PAD_X)).abs() < 0.01);
        // Absurd grabs clamp to the ghost the anchor is taking over from.
        assert_eq!(tear_anchor(9000.0, 28.0), tear_anchor(GHOST_WIDTH, 28.0));
        assert_eq!(tear_anchor(-50.0, 28.0), tear_anchor(0.0, 28.0));
    }
}
