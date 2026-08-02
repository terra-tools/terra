//! The AppKit corner of terra: the titlebar proxy icon, app activation, and
//! the window's open/close transition.
//!
//! *Proxy icon* — the little folder next to the window title that Finder,
//! TextEdit and Ghostty show, and that can be dragged or ⌘-clicked to reveal
//! the enclosing folders. AppKit draws it for any window with a *represented
//! filename*, so all we do is keep `NSWindow.representedFilename` pointing at
//! the active tab's working directory. The tab title already carries that
//! directory (zsh reports the cwd via OSC, see `tabs.rs`), so `title_path` just
//! has to recognise it.
//!
//! *Transition* — the window fades and grows in on launch and fades and
//! shrinks out on quit, instead of snapping in and out. See
//! [`animate_open`]/[`animate_close`] and [`CloseAnimation`].
//!
//! Everything AppKit-flavoured is `cfg`-gated; other platforms get a no-op —
//! and, for the close animation, a state machine that closes immediately.

use std::path::PathBuf;

/// The directory a window title points at, if it points at one at all.
///
/// Titles reach us in shell form — `~/Documents/terra`, `/etc` — so a leading
/// `~` is expanded against `$HOME`. Anything that is not an existing directory
/// (a renamed tab, a stale path, a `~user` style title) yields `None`, which
/// clears the proxy icon rather than showing a lie.
pub fn title_path(title: &str) -> Option<PathBuf> {
    let path = if title == "~" {
        PathBuf::from(std::env::var_os("HOME")?)
    } else if let Some(rest) = title.strip_prefix("~/") {
        let mut home = PathBuf::from(std::env::var_os("HOME")?);
        home.push(rest);
        home
    } else if title.starts_with('/') {
        PathBuf::from(title)
    } else {
        return None;
    };
    path.is_dir().then_some(path)
}

/// Run `f` against the `NSWindow` behind a window handle — `eframe::Frame`
/// during a frame, `eframe::CreationContext` before the first one. `None` if
/// there is no window (yet): the caller decides what that means.
///
/// Must run on the main thread, which every caller here does.
#[cfg(target_os = "macos")]
fn with_window<R>(
    handle: &impl raw_window_handle::HasWindowHandle,
    f: impl FnOnce(&objc2_app_kit::NSWindow) -> R,
) -> Option<R> {
    use objc2_app_kit::NSView;
    use raw_window_handle::RawWindowHandle;

    let handle = handle.window_handle().ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };

    // SAFETY: winit hands out a live, retained `NSView` for as long as the
    // window handle borrow is valid, and we are on the main thread, which is
    // what makes AppKit's main-thread-only types safe to touch.
    let view: &NSView = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
    // A view that is not in a window yet has nothing for us to touch.
    let window = view.window()?;
    Some(f(&window))
}

/// Point the window's proxy icon at `path` (or clear it with `None`).
///
/// Must run on the main thread; `eframe::App::ui` does.
#[cfg(target_os = "macos")]
pub fn set_represented_path(frame: &eframe::Frame, path: Option<&std::path::Path>) {
    use objc2_foundation::NSString;

    // AppKit treats the empty string as "no represented file", which is also
    // how the proxy icon is removed.
    let value = path
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    with_window(frame, |window| {
        window.setRepresentedFilename(&NSString::from_str(&value))
    });
}

#[cfg(not(target_os = "macos"))]
pub fn set_represented_path(_frame: &eframe::Frame, _path: Option<&std::path::Path>) {}

/// Bring terra to the front, even though another app is active. Used by
/// `terra select` so a CLI call (or an agent) can summon the window.
///
/// Uses `NSRunningApplication`, which is documented thread-safe — so this is
/// callable from the IPC thread. (Calling `NSApplication` activation from
/// inside the frame callback breaks winit's event-loop waker; do not.)
#[cfg(target_os = "macos")]
pub fn activate_app() {
    use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
    let app = NSRunningApplication::currentApplication();
    // Deprecated (no-op on macOS 14+) but harmless; plain activation is what
    // actually runs on modern systems.
    #[allow(deprecated)]
    app.activateWithOptions(NSApplicationActivationOptions::ActivateIgnoringOtherApps);
}

#[cfg(not(target_os = "macos"))]
pub fn activate_app() {}

// ---------------------------------------------------------------------------
// Window open/close transition
// ---------------------------------------------------------------------------
//
// Ghostty's quick terminal is the reference: it sets `window.alphaValue = 0`
// plus an off-position frame (`QuickTerminalPosition.swift:29`), then runs one
// `NSAnimationContext.runAnimationGroup` at `duration = 0.2` against
// `window.animator()` that lands on alpha 1 and the real frame
// (`QuickTerminalController.swift:468-476`, default duration at :746), and the
// exact reverse on the way out (:591-599). We copy the shape — one animation
// group, alpha and frame moving together — but keep the geometry to a 2%
// breath rather than a slide, because terra is an ordinary window, not a
// drop-down.

/// Fade/grow in. A touch under Ghostty's 0.2s: nothing travels far.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const OPEN_DURATION: f64 = 0.18;
/// Fade/shrink out. Quicker than the way in — a dismissal should not be
/// something you wait for.
pub const CLOSE_DURATION: f64 = 0.13;
/// One frame's worth of slack before we let the window actually go, so the
/// last animated frame is on screen when it does.
const CLOSE_GRACE: f64 = 0.03;

/// How small the window starts on the way in…
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const OPEN_SCALE: f64 = 0.98;
/// …and ends on the way out. Smaller travel: the fade carries the exit.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const CLOSE_SCALE: f64 = 0.985;

/// A rect scaled about its own centre — the geometry half of the transition,
/// kept free of AppKit so it can be tested anywhere.
///
/// Takes and returns `(x, y, width, height)`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn scaled_about_center(rect: (f64, f64, f64, f64), scale: f64) -> (f64, f64, f64, f64) {
    let (x, y, w, h) = rect;
    let (sw, sh) = (w * scale, h * scale);
    (x + (w - sw) / 2.0, y + (h - sh) / 2.0, sw, sh)
}

/// Where the window is in its exit animation.
///
/// A close request (red button, ⌘Q, the last tab exiting) has to be *canceled*
/// so the fade has something to fade, then re-issued once it is done. That is
/// three states and it is the only part of this file worth testing headlessly.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
enum CloseState {
    /// Nothing is closing.
    #[default]
    Idle,
    /// Fading out since this `egui` timestamp.
    Fading { since: f64 },
    /// The fade is over (or never ran): the next close request goes through.
    Confirmed,
}

/// What the caller should do about the close this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CloseStep {
    /// Nothing to do.
    Idle,
    /// Cancel the pending close and keep drawing: the window is fading.
    Fade,
    /// The fade is done — ask for the close again, and let it through.
    Confirm,
    /// Close now: tear the app down and let the window go.
    Close,
}

/// Drives [`animate_close`] from the frame loop: see [`CloseState`].
#[derive(Debug, Default)]
pub struct CloseAnimation {
    state: CloseState,
}

impl CloseAnimation {
    /// A close request arrived this frame. `start` runs the animation and says
    /// whether it actually started; if it did not — no window handle, no
    /// AppKit, an invisible window — the close goes straight through, because
    /// a window you cannot close is worse than one that closes abruptly.
    ///
    /// A *second* request during the fade also goes straight through: someone
    /// hitting ⌘Q twice means it.
    pub fn requested(&mut self, now: f64, start: impl FnOnce() -> bool) -> CloseStep {
        match self.state {
            CloseState::Idle if start() => {
                self.state = CloseState::Fading { since: now };
                CloseStep::Fade
            }
            _ => {
                self.state = CloseState::Confirmed;
                CloseStep::Close
            }
        }
    }

    /// Every other frame: has the fade run out?
    pub fn tick(&mut self, now: f64) -> CloseStep {
        match self.state {
            CloseState::Fading { since } if now - since >= CLOSE_DURATION + CLOSE_GRACE => {
                self.state = CloseState::Confirmed;
                CloseStep::Confirm
            }
            CloseState::Fading { .. } => CloseStep::Fade,
            _ => CloseStep::Idle,
        }
    }

    /// True while the window is fading out — the frame loop keeps repainting,
    /// and must not issue a close of its own.
    pub fn is_fading(&self) -> bool {
        matches!(self.state, CloseState::Fading { .. })
    }
}

/// Hide the window before it is ever drawn, so the open animation has
/// somewhere to start from.
///
/// Called from `App::new`, which runs after the window exists but before the
/// first frame is painted — the earliest we can reach AppKit, and early enough
/// that nothing flashes.
#[cfg(target_os = "macos")]
pub fn prime_open(cc: &eframe::CreationContext<'_>) {
    with_window(cc, |window| window.setAlphaValue(0.0));
}

#[cfg(not(target_os = "macos"))]
pub fn prime_open(_cc: &eframe::CreationContext<'_>) {}

/// How long the frame loop keeps waiting for a window worth animating before
/// it gives up and just shows it. See [`OpenAnimation`].
pub const OPEN_TIMEOUT: f64 = 1.0;

/// What to do about the entrance this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OpenStep {
    /// Not yet — come back next frame.
    Wait,
    /// Run [`animate_open`] now.
    Animate,
    /// The window never got into a state worth animating: [`show_now`].
    GiveUp,
    /// Already handled; nothing more to do, ever.
    Done,
}

/// Picks the frame the entrance runs on.
///
/// Two things have to have happened first, and neither is true when `ui()` is
/// first called: winit orders the window in only *after* a frame is drawn (its
/// own anti-flash measure), and it settles the window's frame in that same
/// turn — an animation started before that is stomped mid-flight and lands
/// instantly, which is exactly the pop this feature exists to remove. So we
/// wait for a visible window, and then for one more frame after it.
#[derive(Debug, Default)]
pub struct OpenAnimation {
    /// egui time of the first frame, for the give-up deadline.
    since: Option<f64>,
    /// Whether a *previous* frame already saw the window on screen.
    seen_visible: bool,
    done: bool,
}

impl OpenAnimation {
    pub fn step(&mut self, now: f64, visible: bool) -> OpenStep {
        if self.done {
            return OpenStep::Done;
        }
        let since = *self.since.get_or_insert(now);
        if visible && self.seen_visible {
            self.done = true;
            return OpenStep::Animate;
        }
        // Never leave the window invisible because AppKit would not play
        // along: past the deadline it is simply shown.
        if now - since >= OPEN_TIMEOUT {
            self.done = true;
            return OpenStep::GiveUp;
        }
        self.seen_visible |= visible;
        OpenStep::Wait
    }
}

/// Whether the window is on screen — the cue [`OpenAnimation`] waits for.
#[cfg(target_os = "macos")]
pub fn window_visible(frame: &eframe::Frame) -> bool {
    with_window(frame, |window| window.isVisible()).unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
pub fn window_visible(_frame: &eframe::Frame) -> bool {
    true
}

/// Fade and grow the window in.
///
/// Deliberately *not* used by `terra select`: summoning an already-open window
/// is not an opening.
#[cfg(target_os = "macos")]
pub fn animate_open(frame: &eframe::Frame) {
    // Nothing to report: `OpenAnimation` already decided this was the frame,
    // and the fallback for a window that will not animate is `show_now`.
    if transition(frame, OPEN_SCALE, 0.0, 1.0, OPEN_DURATION, true) != Some(true) {
        show_now(frame);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn animate_open(_frame: &eframe::Frame) {}

/// Show the window at once, animation or no animation: the escape hatch out of
/// the alpha [`prime_open`] left behind.
#[cfg(target_os = "macos")]
pub fn show_now(frame: &eframe::Frame) {
    with_window(frame, |window| window.setAlphaValue(1.0));
}

#[cfg(not(target_os = "macos"))]
pub fn show_now(_frame: &eframe::Frame) {}

/// Fade and shrink the window out. Returns whether an animation actually
/// started — `false` means the caller should close immediately.
#[cfg(target_os = "macos")]
pub fn animate_close(frame: &eframe::Frame) -> bool {
    transition(frame, CLOSE_SCALE, 1.0, 0.0, CLOSE_DURATION, false).unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
pub fn animate_close(_frame: &eframe::Frame) -> bool {
    false
}

/// The one animation both directions are made of: jump to `(scale, from)` and
/// animate to `(1.0, to)`, or the reverse when `growing` is false.
///
/// Returns `Some(true)` when an animation was started, `Some(false)` when the
/// window is in no state to animate (hidden, or on another Space — AppKit
/// animates it anyway, but nobody sees it, so the caller should not wait),
/// `None` when there is no window at all.
#[cfg(target_os = "macos")]
fn transition(
    frame: &eframe::Frame,
    scale: f64,
    from: f64,
    to: f64,
    duration: f64,
    growing: bool,
) -> Option<bool> {
    use objc2_app_kit::{NSAnimatablePropertyContainer, NSAnimationContext, NSWindowStyleMask};
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use objc2_quartz_core::{
        kCAMediaTimingFunctionEaseIn, kCAMediaTimingFunctionEaseOut, CAMediaTimingFunction,
    };

    with_window(frame, |window| {
        if !window.isVisible() || window.isMiniaturized() {
            return false;
        }
        // A full-screen window owns its frame; AppKit fights any change to it
        // and the result is a jolt. Alpha alone still reads well.
        let geometry =
            !window.styleMask().contains(NSWindowStyleMask::FullScreen) && !window.isZoomed();

        let full = window.frame();
        let (x, y, w, h) = scaled_about_center(
            (
                full.origin.x,
                full.origin.y,
                full.size.width,
                full.size.height,
            ),
            scale,
        );
        let small = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
        // Where the animation starts. Growing in, that is the small rect; on
        // the way out the window is already at `full`, which is where it is.
        let (start_rect, end_rect) = if growing {
            (small, full)
        } else {
            (full, small)
        };

        window.setAlphaValue(from);
        if geometry {
            window.setFrame_display(start_rect, false);
        }

        // Ghostty's shape, minus the completion handler: the frame loop times
        // the close itself (see `CloseAnimation`), so no block2 is needed.
        NSAnimationContext::beginGrouping();
        let context = NSAnimationContext::currentContext();
        context.setDuration(duration);
        // Ease *out* on the way in (arrives settled), ease *in* on the way out
        // (leaves decisively) — which is also the curve Ghostty animates its
        // quick terminal with.
        let curve = if growing {
            unsafe { kCAMediaTimingFunctionEaseOut }
        } else {
            unsafe { kCAMediaTimingFunctionEaseIn }
        };
        context.setTimingFunction(Some(&CAMediaTimingFunction::functionWithName(curve)));
        let animator = window.animator();
        animator.setAlphaValue(to);
        if geometry {
            animator.setFrame_display(end_rect, true);
        }
        NSAnimationContext::endGrouping();
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_tab_title_is_not_a_directory() {
        assert_eq!(title_path("build"), None);
        assert_eq!(title_path("terra 0"), None);
        assert_eq!(title_path(""), None);
    }

    #[test]
    fn a_tilde_title_expands_against_home() {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return;
        };
        assert_eq!(title_path("~"), Some(home));
    }

    #[test]
    fn an_absolute_directory_survives_and_a_missing_one_does_not() {
        assert_eq!(title_path("/"), Some(PathBuf::from("/")));
        assert_eq!(title_path("/no/such/terra/dir"), None);
    }

    /// The window breathes about its own middle — it must not drift towards a
    /// corner, which is what makes the effect read as a zoom rather than a
    /// slide.
    #[test]
    fn scaling_keeps_the_center_still() {
        let rect = (100.0, 200.0, 1000.0, 500.0);
        let (x, y, w, h) = scaled_about_center(rect, 0.98);
        assert!((w - 980.0).abs() < 1e-9, "{w}");
        assert!((h - 490.0).abs() < 1e-9, "{h}");
        assert!((x + w / 2.0 - (100.0 + 500.0)).abs() < 1e-9, "{x}");
        assert!((y + h / 2.0 - (200.0 + 250.0)).abs() < 1e-9, "{y}");
    }

    #[test]
    fn scaling_by_one_changes_nothing() {
        let rect = (10.0, 20.0, 30.0, 40.0);
        assert_eq!(scaled_about_center(rect, 1.0), rect);
    }

    /// The entrance waits for a window that is on screen *and* has been for a
    /// frame already — winit settles the window's size in the turn it shows it,
    /// and that stomps an animation started in the same turn.
    #[test]
    fn the_entrance_waits_a_frame_past_the_first_visible_one() {
        let mut open = OpenAnimation::default();
        assert_eq!(open.step(0.00, false), OpenStep::Wait);
        assert_eq!(open.step(0.01, false), OpenStep::Wait);
        assert_eq!(open.step(0.02, true), OpenStep::Wait, "first visible frame");
        assert_eq!(open.step(0.03, true), OpenStep::Animate);
        // …and never again, however many frames follow.
        assert_eq!(open.step(0.04, true), OpenStep::Done);
        assert_eq!(open.step(9.99, true), OpenStep::Done);
    }

    /// A window that never shows itself must not stay at alpha 0 forever.
    #[test]
    fn the_entrance_gives_up_rather_than_stay_invisible() {
        let mut open = OpenAnimation::default();
        assert_eq!(open.step(5.0, false), OpenStep::Wait);
        assert_eq!(open.step(5.0 + OPEN_TIMEOUT, false), OpenStep::GiveUp);
        assert_eq!(open.step(5.5 + OPEN_TIMEOUT, false), OpenStep::Done);
    }

    /// The happy path: request -> fade -> confirm -> the close goes through.
    #[test]
    fn a_close_request_fades_before_it_closes() {
        let mut anim = CloseAnimation::default();
        assert_eq!(anim.requested(10.0, || true), CloseStep::Fade);
        assert!(anim.is_fading());
        // Mid-fade the window stays.
        assert_eq!(anim.tick(10.0 + CLOSE_DURATION / 2.0), CloseStep::Fade);
        // Past the end it asks to be closed for real…
        assert_eq!(
            anim.tick(10.0 + CLOSE_DURATION + CLOSE_GRACE),
            CloseStep::Confirm
        );
        assert!(!anim.is_fading());
        // …and the close that follows is let through, not canceled again.
        assert_eq!(anim.requested(11.0, || true), CloseStep::Close);
    }

    /// No window handle (or no AppKit): never trap the user behind an
    /// animation that cannot run.
    #[test]
    fn a_close_that_cannot_animate_closes_at_once() {
        let mut anim = CloseAnimation::default();
        assert_eq!(anim.requested(0.0, || false), CloseStep::Close);
        assert!(!anim.is_fading());
        assert_eq!(anim.tick(100.0), CloseStep::Idle);
    }

    /// ⌘Q twice: the second one means it.
    #[test]
    fn a_second_request_during_the_fade_closes_at_once() {
        let mut anim = CloseAnimation::default();
        assert_eq!(anim.requested(0.0, || true), CloseStep::Fade);
        assert_eq!(
            anim.requested(0.01, || panic!("must not re-animate")),
            CloseStep::Close
        );
        assert!(!anim.is_fading());
    }

    /// An app that is not closing must not be told to do anything.
    #[test]
    fn an_idle_window_ticks_quietly() {
        let mut anim = CloseAnimation::default();
        for t in 0..5 {
            assert_eq!(anim.tick(f64::from(t)), CloseStep::Idle);
        }
        assert!(!anim.is_fading());
    }
}
