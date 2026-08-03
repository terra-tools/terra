//! "Close Window?" — the question terra asks before a close takes a running
//! session down with it.
//!
//! Three pieces, deliberately separable:
//!
//! - [`should_confirm`] — the *decision*. Pure: per-tab foreground process
//!   names plus the config key in, ask/don't-ask out. Nothing to mock.
//! - [`ConfirmClose`] — the *state machine*. Idle → Asking → Approved, driven
//!   from the frame loop, so `main.rs` holds one close request back, keeps the
//!   window alive, and then lets the *next* one through untouched.
//! - [`show`] — the *dialog*, drawn in egui and styled like the command
//!   palette (see `terra-palette`): the same glass panel over the same scrim,
//!   scaled down to a question and two buttons.
//!
//! The decision is Ghostty's, not "always ask": a window whose tabs are all
//! sitting at a shell prompt protects nothing, and a modal there is nagging
//! that trains people to hit Return without reading. See
//! [`crate::config::DEFAULT_WINDOW_CONFIRM_CLOSE`].

use egui::{
    Align2, Area, Color32, Context, CornerRadius, FontId, Frame, Id, Key, LayerId, Margin,
    Modifiers, Order, Pos2, Rect, Sense, Shadow, Stroke, Vec2,
};

// ---------------------------------------------------------------------------
// The decision
// ---------------------------------------------------------------------------

/// Programs that are the *absence* of a program: a tab sitting at one of these
/// is an idle prompt, and closing it loses nothing.
///
/// Matched against the lowercased basename [`crate::procinfo`] reports, which
/// is the same vocabulary `tabs::login_args` and the icon table already speak.
/// A shell running *under* the tab's shell (`zsh -c`, a nested `bash`) is
/// still just a prompt, so nesting needs no special case.
const SHELLS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "dash",
    "ksh",
    "fish",
    "csh",
    "tcsh",
    "nu",
    "xonsh",
    "elvish",
    "pwsh",
    "powershell",
    "cmd",
    "command",
    "login",
];

/// Whether `command` is a bare shell prompt rather than a running program.
fn is_shell(command: &str) -> bool {
    SHELLS.contains(&command)
}

/// Should closing this window ask first?
///
/// `foreground` is one entry per tab: the lowercased basename of whatever is
/// running in it, or `None` when terra could not tell (no process table on
/// this platform, a tab whose shell has not spawned yet, a process that exited
/// between the walk and the read).
///
/// `None` counts as *not* protecting anything. It is the same "no opinion"
/// that [`crate::procinfo::foreground_command`] documents, and the safe
/// reading of it here is the quiet one: an answer terra could not obtain is
/// not evidence that a build is running, and a dialog raised on no evidence at
/// all is one the user cannot act on.
pub fn should_confirm(enabled: bool, foreground: &[Option<&str>]) -> bool {
    enabled
        && foreground
            .iter()
            .any(|fg| fg.is_some_and(|command| !is_shell(command)))
}

/// Should closing *this tab* ask first?
///
/// A tab close is a window close in disguise when it is the last tab in the
/// window (the last tab of the last group — `tab_count` counts every group's
/// tabs, not the focused group's): the tabs path tears the window down without
/// ever raising `close_requested`, so the red traffic light's dialog would
/// never get a say. Same switch, same shell list, same `None` reading as
/// [`should_confirm`] — this only adds the "is it the last one" question.
///
/// Closing a tab that is *not* the last is deliberately never held back. Some
/// terminals ask there too; terra does not, because a window that survives the
/// close still shows every other session and the cost of a mistake is one tab,
/// not the workspace. This is scope, not an oversight — widening it means
/// giving the dialog per-tab wording ("Close Tab?"), not just relaxing this
/// condition.
pub fn should_confirm_tab_close(enabled: bool, tab_count: usize, foreground: Option<&str>) -> bool {
    tab_count == 1 && should_confirm(enabled, &[foreground])
}

// ---------------------------------------------------------------------------
// The state machine
// ---------------------------------------------------------------------------

/// What the user chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Keep the window. The close is fully abandoned; the next one asks again.
    Cancel,
    /// Go through with it.
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    /// No question outstanding; the next close request may raise one.
    #[default]
    Idle,
    /// The dialog is up and a close request is being held back.
    Asking,
    /// The user said yes. Every close request from here on goes straight
    /// through — the close path re-issues its request once, after the fade,
    /// and asking a second time would deadlock the window shut.
    Approved,
}

/// Holds one close request back while the dialog is up. See the module docs.
#[derive(Debug, Default)]
pub struct ConfirmClose {
    state: State,
}

impl ConfirmClose {
    /// A close request arrived this frame. `needed` is consulted only when
    /// there is no answer already on record, so the process table is read once
    /// per question rather than once per frame.
    ///
    /// Returns `true` when the caller must cancel the close and keep the
    /// window alive; `false` lets it proceed exactly as it did before this
    /// feature existed.
    pub fn requested(&mut self, needed: impl FnOnce() -> bool) -> bool {
        match self.state {
            State::Approved => false,
            // A second request while the dialog is up (clicking the red button
            // again) is not consent — it is the same question, unanswered.
            State::Asking => true,
            State::Idle => {
                if needed() {
                    self.state = State::Asking;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Whether the dialog should be drawn — and, for the caller, whether the
    /// terminal underneath must go unfocused.
    pub fn is_open(&self) -> bool {
        self.state == State::Asking
    }

    /// Record the user's answer. Cancelling returns to [`State::Idle`], which
    /// is what makes a later close attempt ask again.
    pub fn answer(&mut self, choice: Choice) {
        self.state = match choice {
            Choice::Cancel => State::Idle,
            Choice::Close => State::Approved,
        };
    }
}

// ---------------------------------------------------------------------------
// The dialog
// ---------------------------------------------------------------------------

pub const TITLE: &str = "Close Window?";
pub const BODY: &str = "All terminal sessions in this window will be terminated.";
pub const CANCEL_LABEL: &str = "Cancel";
pub const CLOSE_LABEL: &str = "Close";

/// The egui id of one of the two buttons — a fixed tuple rather than one
/// derived from the enclosing `Ui`, so a harness test can find the button's
/// rect with `Context::read_response` and click it where it actually is.
pub fn button_id(label: &str) -> Id {
    Id::new(("terra_confirm_close_button", label))
}

const PANEL_WIDTH: f32 = 380.0;
const PANEL_RADIUS: u8 = 14;
const PANEL_PAD_X: i8 = 22;
const PANEL_PAD_Y: i8 = 20;
const TITLE_GAP: f32 = 10.0;
const BODY_GAP: f32 = 18.0;
const BUTTON_HEIGHT: f32 = 30.0;
const BUTTON_MIN_WIDTH: f32 = 92.0;
const BUTTON_PAD_X: f32 = 18.0;
const BUTTON_GAP: f32 = 10.0;
const BUTTON_RADIUS: u8 = 8;

const FONT_TITLE: f32 = 16.0;
const FONT_BODY: f32 = 13.0;
const FONT_BUTTON: f32 = 13.0;

const fn white(alpha: u8) -> Color32 {
    Color32::from_rgba_premultiplied(alpha, alpha, alpha, alpha)
}

/// The palette's glass, to the byte — this is the same floating layer, and two
/// nearly-alike panels would read as a bug.
const BG_PANEL: Color32 = Color32::from_rgba_premultiplied(0x1b, 0x1c, 0x20, 0xdc);
const PANEL_BORDER: Color32 = white(30);
const PANEL_HIGHLIGHT: Color32 = white(22);
const SCRIM: Color32 = Color32::from_black_alpha(70);

const FG_TITLE: Color32 = Color32::from_rgb(0xf2, 0xf2, 0xf4);
const FG_BODY: Color32 = Color32::from_rgb(0xa8, 0xac, 0xb4);

/// The secondary button: the palette's row fills, so Cancel reads as chrome
/// rather than as a second thing to decide.
const CANCEL_FILL: Color32 = white(20);
const CANCEL_HOVER: Color32 = white(30);
const CANCEL_STROKE: Color32 = white(18);
const FG_CANCEL: Color32 = Color32::from_rgb(0xe8, 0xe8, 0xec);

/// terra's steel accent — the selection blue of the shipped theme
/// (`ghostty_theme::palette().selection_background`, `#3f638b`). Ghostty's
/// dialog puts a saturated system blue here; terra's own blue is this one.
const ACCENT: Color32 = Color32::from_rgb(0x3f, 0x63, 0x8b);
const ACCENT_HOVER: Color32 = Color32::from_rgb(0x4d, 0x76, 0xa4);
const FG_ACCENT: Color32 = Color32::WHITE;

fn ui_font(size: f32, medium: bool) -> FontId {
    let family = if medium {
        crate::fonts::UI_MEDIUM_FAMILY
    } else {
        crate::fonts::UI_FAMILY
    };
    FontId::new(size, egui::FontFamily::Name(family.into()))
}

/// Draw the dialog and answer with the user's choice, if they made one.
///
/// Keys are read with `consume_key` *before* anything else, exactly as the
/// palette does: Return and Escape are answers here, and must not also reach
/// the app's shortcut table or the terminal below.
pub fn show(ctx: &Context) -> Option<Choice> {
    let (escape, enter) = ctx.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::Escape),
            i.consume_key(Modifiers::NONE, Key::Enter),
        )
    });

    let screen = ctx.content_rect();

    // Scrim: an interactable full-screen area *below* the panel, so a click
    // meant for the terminal lands here instead.
    Area::new(Id::new("terra_confirm_close_scrim"))
        .order(Order::Middle)
        .fixed_pos(screen.min)
        .interactable(true)
        .show(ctx, |ui| {
            ui.allocate_exact_size(screen.size(), Sense::click());
        });
    ctx.layer_painter(LayerId::new(
        Order::Middle,
        Id::new("terra_confirm_close_scrim"),
    ))
    .rect_filled(screen, CornerRadius::ZERO, SCRIM);

    let width = PANEL_WIDTH.min(screen.width() - 32.0);
    let mut clicked: Option<Choice> = None;

    let panel = Area::new(Id::new("terra_confirm_close_panel"))
        .order(Order::Foreground)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .constrain_to(screen)
        .show(ctx, |ui| {
            ui.set_width(width);
            Frame::NONE
                .fill(BG_PANEL)
                .stroke(Stroke::new(1.0, PANEL_BORDER))
                .corner_radius(CornerRadius::same(PANEL_RADIUS))
                .inner_margin(Margin::symmetric(PANEL_PAD_X, PANEL_PAD_Y))
                .shadow(Shadow {
                    offset: [0, 18],
                    blur: 48,
                    spread: 0,
                    color: Color32::from_black_alpha(140),
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    clicked = contents(ui);
                })
                .response
                .rect
        });

    // The lit top edge, painted over the fill — the palette's trick for making
    // a translucent panel read as glass rather than as a flat rectangle.
    let rect = panel.inner;
    ctx.layer_painter(LayerId::new(
        Order::Foreground,
        Id::new("terra_confirm_close_panel"),
    ))
    .rect_filled(
        Rect::from_min_max(
            Pos2::new(rect.left() + f32::from(PANEL_RADIUS), rect.top() + 1.0),
            Pos2::new(rect.right() - f32::from(PANEL_RADIUS), rect.top() + 2.0),
        ),
        CornerRadius::ZERO,
        PANEL_HIGHLIGHT,
    );

    // Repaint while the dialog is up: hover states are animation-free, but a
    // parked terra would not notice the pointer crossing a button.
    ctx.request_repaint();

    if escape {
        return Some(Choice::Cancel);
    }
    if enter {
        return Some(Choice::Close);
    }
    if clicked.is_some() {
        return clicked;
    }
    // Outside the panel is the same answer Escape gives: a modal dismissed
    // without a decision must never be read as one.
    clicked_outside(ctx, rect).then_some(Choice::Cancel)
}

/// Title, body and the button row.
fn contents(ui: &mut egui::Ui) -> Option<Choice> {
    let full = ui.available_width();

    let (rect, _) = ui.allocate_exact_size(Vec2::new(full, FONT_TITLE * 1.3), Sense::hover());
    ui.painter().text(
        rect.left_center(),
        Align2::LEFT_CENTER,
        TITLE,
        ui_font(FONT_TITLE, true),
        FG_TITLE,
    );
    ui.add_space(TITLE_GAP);

    // Wrapped, so a narrow window folds the sentence instead of clipping it.
    let galley = ui.painter().layout(
        BODY.to_owned(),
        ui_font(FONT_BODY, false),
        FG_BODY,
        full.max(1.0),
    );
    let (rect, _) = ui.allocate_exact_size(galley.size(), Sense::hover());
    ui.painter().galley(rect.min, galley, FG_BODY);
    ui.add_space(BODY_GAP);

    // Right-aligned, primary last: macOS's order, and the one the reference
    // screenshot uses.
    let (row, _) = ui.allocate_exact_size(Vec2::new(full, BUTTON_HEIGHT), Sense::hover());
    let close_width = button_width(ui, CLOSE_LABEL);
    let cancel_width = button_width(ui, CANCEL_LABEL);
    let close_rect = Rect::from_min_size(
        Pos2::new(row.right() - close_width, row.top()),
        Vec2::new(close_width, BUTTON_HEIGHT),
    );
    let cancel_rect = Rect::from_min_size(
        Pos2::new(close_rect.left() - BUTTON_GAP - cancel_width, row.top()),
        Vec2::new(cancel_width, BUTTON_HEIGHT),
    );

    let cancel = button(ui, cancel_rect, CANCEL_LABEL, false);
    let close = button(ui, close_rect, CLOSE_LABEL, true);
    match (cancel, close) {
        (_, true) => Some(Choice::Close),
        (true, _) => Some(Choice::Cancel),
        _ => None,
    }
}

fn button_width(ui: &egui::Ui, label: &str) -> f32 {
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_owned(), ui_font(FONT_BUTTON, true), Color32::WHITE);
    (galley.size().x + BUTTON_PAD_X * 2.0).max(BUTTON_MIN_WIDTH)
}

/// One button. `primary` is the accent-filled default.
fn button(ui: &mut egui::Ui, rect: Rect, label: &str, primary: bool) -> bool {
    let response = ui.interact(rect, button_id(label), Sense::click());
    let hovered = response.hovered();

    let (fill, stroke, text) = if primary {
        let fill = if hovered { ACCENT_HOVER } else { ACCENT };
        (fill, Stroke::NONE, FG_ACCENT)
    } else {
        let fill = if hovered { CANCEL_HOVER } else { CANCEL_FILL };
        (fill, Stroke::new(1.0, CANCEL_STROKE), FG_CANCEL)
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(BUTTON_RADIUS), fill);
    if stroke != Stroke::NONE {
        painter.rect_stroke(
            rect,
            CornerRadius::same(BUTTON_RADIUS),
            stroke,
            egui::StrokeKind::Inside,
        );
    }
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        ui_font(FONT_BUTTON, true),
        text,
    );
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.clicked()
}

/// True when the user clicked anywhere but the panel this frame.
fn clicked_outside(ctx: &Context, panel: Rect) -> bool {
    ctx.input(|i| {
        if !i.pointer.any_click() {
            return false;
        }
        match i.pointer.interact_pos() {
            Some(pos) => !panel.contains(pos),
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

    /// A window of idle prompts protects nothing; asking there is nagging.
    #[test]
    fn a_window_of_bare_shells_never_asks() {
        assert!(!should_confirm(true, &[]));
        assert!(!should_confirm(true, &[Some("zsh")]));
        assert!(!should_confirm(
            true,
            &[Some("zsh"), Some("bash"), Some("fish"), Some("pwsh")]
        ));
    }

    /// One running program in one tab is the whole reason for the feature.
    #[test]
    fn one_running_program_is_enough_to_ask() {
        assert!(should_confirm(true, &[Some("ssh")]));
        assert!(should_confirm(true, &[Some("zsh"), Some("cargo")]));
        assert!(should_confirm(true, &[Some("claude"), Some("zsh")]));
    }

    /// `false` means never, however busy the window is.
    #[test]
    fn the_config_switch_wins_over_everything() {
        assert!(!should_confirm(false, &[Some("ssh"), Some("cargo")]));
        assert!(!should_confirm(false, &[]));
    }

    /// An answer terra could not obtain is not evidence of a running program.
    #[test]
    fn an_unknown_foreground_process_is_not_a_reason_to_ask() {
        assert!(!should_confirm(true, &[None, None]));
        assert!(!should_confirm(true, &[None, Some("zsh")]));
        // …but a known one alongside it still is.
        assert!(should_confirm(true, &[None, Some("vim")]));
    }

    /// The bug this exists for: the lone tab running `claude` is the window,
    /// and closing it must ask.
    #[test]
    fn closing_the_last_busy_tab_asks() {
        assert!(should_confirm_tab_close(true, 1, Some("claude")));
        assert!(should_confirm_tab_close(true, 1, Some("sleep")));
    }

    /// Any other tab close is out of scope: the window survives it.
    #[test]
    fn closing_a_tab_that_is_not_the_last_never_asks() {
        assert!(!should_confirm_tab_close(true, 2, Some("claude")));
        assert!(!should_confirm_tab_close(true, 7, Some("cargo")));
    }

    /// A lone idle prompt closes instantly, exactly as it always did.
    #[test]
    fn closing_the_last_idle_tab_never_asks() {
        assert!(!should_confirm_tab_close(true, 1, Some("zsh")));
        assert!(!should_confirm_tab_close(true, 1, Some("fish")));
        // No answer from the process table is not evidence of work.
        assert!(!should_confirm_tab_close(true, 1, None));
    }

    /// The same switch governs both doors.
    #[test]
    fn the_config_switch_also_wins_over_a_last_tab_close() {
        assert!(!should_confirm_tab_close(false, 1, Some("claude")));
    }

    /// A tab close routed through the dialog and approved runs the close
    /// itself, and the window close that follows from the empty window is not
    /// questioned again.
    #[test]
    fn approving_a_last_tab_close_lets_the_window_close_follow() {
        let mut confirm = ConfirmClose::default();
        assert!(confirm.requested(|| true), "the tab close is held");
        confirm.answer(Choice::Close);
        // The close itself, then the empty-window close, then the fade's retry.
        assert!(!confirm.requested(|| panic!("must not re-decide")));
        assert!(!confirm.requested(|| panic!("must not re-decide")));
        assert!(!confirm.requested(|| panic!("must not re-decide")));
    }

    /// The happy path: ask, say yes, and the close that follows is not
    /// questioned again — including the second request the fade re-issues.
    #[test]
    fn approving_lets_this_close_and_its_retry_through() {
        let mut confirm = ConfirmClose::default();
        assert!(confirm.requested(|| true), "the first request is held");
        assert!(confirm.is_open());

        confirm.answer(Choice::Close);
        assert!(!confirm.is_open());
        assert!(!confirm.requested(|| panic!("must not re-decide")));
        assert!(!confirm.requested(|| panic!("must not re-decide")));
    }

    /// Cancelling leaves no half-state: the window is not closing, and the
    /// next attempt asks from scratch.
    #[test]
    fn cancelling_aborts_the_close_and_the_next_one_asks_again() {
        let mut confirm = ConfirmClose::default();
        assert!(confirm.requested(|| true));
        confirm.answer(Choice::Cancel);
        assert!(!confirm.is_open());

        let mut asked = 0;
        assert!(confirm.requested(|| {
            asked += 1;
            true
        }));
        assert_eq!(asked, 1, "the second attempt re-reads the world");
        assert!(confirm.is_open());
    }

    /// Nothing worth protecting: the close goes through untouched and no
    /// dialog is ever drawn.
    #[test]
    fn a_close_that_protects_nothing_is_never_held_back() {
        let mut confirm = ConfirmClose::default();
        assert!(!confirm.requested(|| false));
        assert!(!confirm.is_open());
    }

    /// Clicking the red button twice is the same question asked twice, not an
    /// answer — unlike the fade's re-issued request, which follows approval.
    #[test]
    fn a_second_request_while_asking_keeps_asking() {
        let mut confirm = ConfirmClose::default();
        assert!(confirm.requested(|| true));
        assert!(confirm.requested(|| panic!("must not re-decide")));
        assert!(confirm.is_open());
    }
}
