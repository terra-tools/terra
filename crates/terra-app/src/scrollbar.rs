//! Ghostty/macOS-style overlay scrollbar for the terminal view.
//!
//! No track, no gutter: a thin translucent thumb is painted on top of the
//! terminal, flush to its right edge. It is invisible while the viewport sits
//! at the bottom and idle, appears on scroll activity, and fades out shortly
//! after the terminal returns to the bottom.
//!
//! Sign convention: `BackendCommand::Scroll(n)` maps to alacritty's
//! `Scroll::Delta(n)`, so a **positive** delta moves the viewport *up* into the
//! history (it increases `grid.display_offset()`); negative moves back down
//! toward the live prompt. Same convention egui_term uses for wheel events.

use alacritty_terminal::grid::Dimensions;
use egui::{pos2, Color32, CornerRadius, Rect, Sense, Ui, Vec2};
use egui_term::{BackendCommand, TerminalBackend};

/// Width of the thumb itself.
const THUMB_WIDTH: f32 = 7.0;
/// Gap between the thumb and the right edge of the terminal.
const EDGE_INSET: f32 = 2.0;
/// Shortest the thumb is allowed to get on a very long scrollback.
const MIN_THUMB_HEIGHT: f32 = 40.0;
/// How long the thumb stays fully visible after the last activity.
const HOLD_SECONDS: f64 = 0.8;
/// Fade-out duration once the hold expires.
const FADE_SECONDS: f64 = 0.2;

const IDLE_ALPHA: f32 = 0.35;
const ACTIVE_ALPHA: f32 = 0.55;

/// Everything the geometry math needs: scrollback length, viewport height and
/// the current distance from the bottom (all in lines).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Metrics {
    /// Lines of scrollback above the viewport (`grid.history_size()`).
    pub history: usize,
    /// Visible lines (`grid.screen_lines()`).
    pub screen: usize,
    /// Lines scrolled up from the bottom (`grid.display_offset()`), 0 = bottom.
    pub offset: usize,
}

impl Metrics {
    fn read(backend: &TerminalBackend) -> Self {
        let grid = &backend.last_content().grid;
        Self {
            history: grid.history_size(),
            screen: grid.screen_lines(),
            offset: grid.display_offset(),
        }
    }

    /// A scrollbar only means anything once something has scrolled off the top.
    pub fn scrollable(&self) -> bool {
        self.history > 0 && self.screen > 0
    }

    fn offset(&self) -> usize {
        self.offset.min(self.history)
    }

    /// Thumb rectangle inside `bar` (the full-height column the thumb lives in).
    ///
    /// `offset == 0` (bottom) puts the thumb at the bottom of the bar,
    /// `offset == history` (top of scrollback) puts it at the top.
    pub fn thumb_rect(&self, bar: Rect) -> Option<Rect> {
        if !self.scrollable() || bar.height() <= 0.0 {
            return None;
        }
        let total = (self.history + self.screen) as f32;
        let ideal = bar.height() * self.screen as f32 / total;
        let height = ideal.clamp(MIN_THUMB_HEIGHT.min(bar.height()), bar.height());
        let travel = bar.height() - height;
        // Fraction of the travel measured from the top of the bar.
        let from_top = (self.history - self.offset()) as f32 / self.history as f32;
        let top = bar.top() + travel * from_top;
        Some(Rect::from_min_size(
            pos2(bar.left(), top),
            Vec2::new(bar.width(), height),
        ))
    }

    /// Inverse of [`Metrics::thumb_rect`]: the display offset that would place
    /// the thumb's top edge at `thumb_top`.
    pub fn offset_at(&self, bar: Rect, thumb_top: f32) -> usize {
        if !self.scrollable() {
            return 0;
        }
        let Some(thumb) = self.thumb_rect(bar) else {
            return 0;
        };
        let travel = bar.height() - thumb.height();
        if travel <= 0.0 {
            return self.offset();
        }
        let from_top = ((thumb_top - bar.top()) / travel).clamp(0.0, 1.0);
        let scrolled = (from_top * self.history as f32).round() as usize;
        self.history.saturating_sub(scrolled.min(self.history))
    }
}

/// Opacity of the thumb given the time since the last scroll activity.
/// Fully opaque for [`HOLD_SECONDS`], then a linear fade over [`FADE_SECONDS`].
pub fn fade(now: f64, last_activity: f64) -> f32 {
    let idle = now - last_activity;
    if idle <= HOLD_SECONDS {
        1.0
    } else if idle >= HOLD_SECONDS + FADE_SECONDS {
        0.0
    } else {
        (1.0 - (idle - HOLD_SECONDS) / FADE_SECONDS) as f32
    }
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    /// Pointer offset from the thumb's top edge when the drag started.
    grab_dy: f32,
    /// Offset we last asked the backend for (the grid lags by a frame).
    offset: usize,
}

/// Visibility + drag bookkeeping. One per app is enough.
#[derive(Clone, Debug, Default)]
pub struct ScrollbarState {
    last_offset: usize,
    last_activity: f64,
    seen: bool,
    drag: Option<Drag>,
}

impl ScrollbarState {
    fn note_activity(&mut self, now: f64) {
        self.last_activity = now;
    }
}

/// Paints and drives the overlay scrollbar over `rect` (the terminal's rect).
///
/// Call this *after* the `TerminalView` has been added so the thumb wins the
/// hit test. Never takes keyboard focus.
pub fn show(ui: &mut Ui, rect: Rect, backend: &mut TerminalBackend, state: &mut ScrollbarState) {
    let now = ui.input(|i| i.time);
    let mut metrics = Metrics::read(backend);

    // First frame for this state: adopt the offset without flashing the thumb.
    if !state.seen {
        state.seen = true;
        state.last_offset = metrics.offset;
        state.last_activity = now - HOLD_SECONDS - FADE_SECONDS;
    }

    let bar = Rect::from_min_max(
        pos2(rect.right() - EDGE_INSET - THUMB_WIDTH, rect.top()),
        pos2(rect.right() - EDGE_INSET, rect.bottom()),
    );
    // A slightly wider strip is easier to grab than 7 logical pixels.
    let hit_area = Rect::from_min_max(pos2(bar.left() - EDGE_INSET, bar.top()), rect.max);

    // Activity: the viewport moved, the wheel turned over the terminal, or the
    // pointer came to rest over the bar (which reveals a hidden thumb).
    if metrics.offset != state.last_offset {
        state.note_activity(now);
        state.last_offset = metrics.offset;
    }
    let wheel_over_terminal =
        ui.rect_contains_pointer(rect) && ui.input(|i| i.smooth_scroll_delta.y != 0.0);
    if wheel_over_terminal || ui.rect_contains_pointer(hit_area) {
        state.note_activity(now);
    }

    if !metrics.scrollable() {
        state.drag = None;
        return;
    }

    // Scrolled away from the bottom -> pinned visible; at the bottom -> fade.
    let dragging = state.drag.is_some();
    let alpha_scale = if metrics.offset > 0 || dragging {
        state.note_activity(now);
        1.0
    } else {
        fade(now, state.last_activity)
    };

    if alpha_scale <= 0.0 {
        state.drag = None;
        return;
    }

    // Interaction (registered after the terminal, so it wins the hit test).
    let response = ui.allocate_rect(hit_area, Sense::click_and_drag());
    let pointer = response.interact_pointer_pos();
    let thumb = metrics.thumb_rect(bar).expect("scrollable");
    let mut command: Option<i32> = None;

    if response.drag_started() || response.clicked() {
        match pointer {
            Some(pos) if thumb.y_range().contains(pos.y) => {
                state.drag = Some(Drag {
                    grab_dy: pos.y - thumb.top(),
                    offset: metrics.offset,
                });
            }
            // Click above/below the thumb pages through the scrollback.
            Some(pos) => {
                let page = metrics.screen as i32;
                command = Some(if pos.y < thumb.top() { page } else { -page });
                state.drag = None;
            }
            None => state.drag = None,
        }
    }

    if let (Some(drag), Some(pos)) = (state.drag, pointer) {
        if response.dragged() {
            // The grid lags a frame behind our own commands, so measure the
            // delta against what we last asked for.
            let from = Metrics {
                offset: drag.offset,
                ..metrics
            };
            let target = from.offset_at(bar, pos.y - drag.grab_dy);
            if target != drag.offset {
                command = Some(target as i32 - drag.offset as i32);
                state.drag = Some(Drag {
                    offset: target,
                    ..drag
                });
                metrics.offset = target;
            }
        }
    }

    if response.drag_stopped() {
        state.drag = None;
    }

    if let Some(delta) = command {
        // Positive = toward history (see module docs).
        backend.process_command(BackendCommand::Scroll(delta));
        state.note_activity(now);
        ui.ctx().request_repaint();
    }

    let hot = response.hovered() || response.dragged() || state.drag.is_some();
    let base = if hot { ACTIVE_ALPHA } else { IDLE_ALPHA };
    let alpha = (base * alpha_scale * 255.0).round().clamp(0.0, 255.0) as u8;
    let thumb = metrics.thumb_rect(bar).expect("scrollable");
    ui.painter().rect_filled(
        thumb,
        CornerRadius::same((THUMB_WIDTH / 2.0) as u8),
        Color32::from_white_alpha(alpha),
    );

    // Keep the fade animating while it still has something to draw.
    if metrics.offset == 0 && state.drag.is_none() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(16));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar() -> Rect {
        Rect::from_min_size(pos2(100.0, 0.0), Vec2::new(7.0, 400.0))
    }

    #[test]
    fn no_thumb_without_scrollback() {
        let m = Metrics {
            history: 0,
            screen: 40,
            offset: 0,
        };
        assert!(!m.scrollable());
        assert!(m.thumb_rect(bar()).is_none());
    }

    #[test]
    fn thumb_height_is_proportional_and_clamped() {
        let m = Metrics {
            history: 60,
            screen: 40,
            offset: 0,
        };
        // 40 / 100 of 400px.
        assert_eq!(m.thumb_rect(bar()).unwrap().height(), 160.0);

        let long = Metrics {
            history: 100_000,
            screen: 40,
            offset: 0,
        };
        assert_eq!(long.thumb_rect(bar()).unwrap().height(), MIN_THUMB_HEIGHT);
    }

    #[test]
    fn bottom_and_top_anchor_the_thumb() {
        let bottom = Metrics {
            history: 60,
            screen: 40,
            offset: 0,
        };
        let r = bottom.thumb_rect(bar()).unwrap();
        assert_eq!(r.bottom(), bar().bottom());

        let top = Metrics {
            offset: 60,
            ..bottom
        };
        assert_eq!(top.thumb_rect(bar()).unwrap().top(), bar().top());
    }

    #[test]
    fn thumb_stays_inside_the_bar_and_tracks_the_offset() {
        let m = Metrics {
            history: 500,
            screen: 40,
            offset: 250,
        };
        let r = m.thumb_rect(bar()).unwrap();
        assert!(r.top() >= bar().top() && r.bottom() <= bar().bottom() + 0.001);
        assert_eq!(r.width(), bar().width());
        assert_eq!(r.left(), bar().left());

        // Scrolling further up moves the thumb up.
        let higher = Metrics { offset: 400, ..m };
        assert!(higher.thumb_rect(bar()).unwrap().top() < r.top());
    }

    #[test]
    fn offset_at_inverts_thumb_rect() {
        for &history in &[1usize, 7, 60, 500, 10_000] {
            for &screen in &[10usize, 40] {
                for step in 0..=10 {
                    let offset = history * step / 10;
                    let m = Metrics {
                        history,
                        screen,
                        offset,
                    };
                    let thumb = m.thumb_rect(bar()).unwrap();
                    let back = m.offset_at(bar(), thumb.top());
                    // Rounding to whole lines can cost a line on huge histories.
                    let slack = (history as f32 / (bar().height() - thumb.height()).max(1.0)).ceil()
                        as usize;
                    assert!(
                        back.abs_diff(offset) <= slack,
                        "history={history} screen={screen} offset={offset} back={back}"
                    );
                }
            }
        }
    }

    #[test]
    fn offset_at_clamps_outside_the_bar() {
        let m = Metrics {
            history: 500,
            screen: 40,
            offset: 100,
        };
        assert_eq!(m.offset_at(bar(), -1000.0), 500);
        assert_eq!(m.offset_at(bar(), 1000.0), 0);
    }

    #[test]
    fn fade_holds_then_ramps_to_zero() {
        assert_eq!(fade(10.0, 10.0), 1.0);
        assert_eq!(fade(10.0 + HOLD_SECONDS, 10.0), 1.0);
        let mid = fade(10.0 + HOLD_SECONDS + FADE_SECONDS / 2.0, 10.0);
        assert!((mid - 0.5).abs() < 0.01, "mid = {mid}");
        assert_eq!(fade(10.0 + HOLD_SECONDS + FADE_SECONDS, 10.0), 0.0);
        assert_eq!(fade(100.0, 10.0), 0.0);
    }
}
