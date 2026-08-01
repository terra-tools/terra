//! macOS titlebar proxy icon: the little folder next to the window title that
//! Finder, TextEdit and Ghostty show, and that can be dragged or ⌘-clicked to
//! reveal the enclosing folders.
//!
//! AppKit draws it for any window with a *represented filename*, so all we do
//! is keep `NSWindow.representedFilename` pointing at the active tab's working
//! directory. The tab title already carries that directory (zsh reports the cwd
//! via OSC, see `tabs.rs`), so `title_path` just has to recognise it.
//!
//! Everything AppKit-flavoured is `cfg`-gated; other platforms get a no-op.

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

/// Point the window's proxy icon at `path` (or clear it with `None`).
///
/// Must run on the main thread; `eframe::App::ui` does.
#[cfg(target_os = "macos")]
pub fn set_represented_path(frame: &eframe::Frame, path: Option<&std::path::Path>) {
    use objc2_app_kit::NSView;
    use objc2_foundation::NSString;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = frame.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };

    // SAFETY: winit hands out a live, retained `NSView` for as long as the
    // window handle borrow is valid, and we are on the main thread (this runs
    // inside `ui()`), which is what makes AppKit's main-thread-only types safe
    // to touch.
    let view: &NSView = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
    let Some(window) = view.window() else {
        return; // view not in a window (yet) — nothing to decorate
    };

    // AppKit treats the empty string as "no represented file", which is also
    // how the proxy icon is removed.
    let value = path
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    window.setRepresentedFilename(&NSString::from_str(&value));
}

#[cfg(not(target_os = "macos"))]
pub fn set_represented_path(_frame: &eframe::Frame, _path: Option<&std::path::Path>) {}

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
}

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
