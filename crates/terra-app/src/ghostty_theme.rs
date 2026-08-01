//! Ghostty's "Apple System Colors" theme, transcribed exactly.
//!
//! Source: iTerm2-Color-Schemes, `ghostty/Apple System Colors`
//! <https://raw.githubusercontent.com/mbadolato/iTerm2-Color-Schemes/master/ghostty/Apple%20System%20Colors>
//!
//! ```text
//! background = #1e1e1e      foreground = #ffffff
//! cursor-color = #98989d    cursor-text = #ffffff
//! selection-background = #3f638b
//! selection-foreground = #ffffff
//! palette 0..7  = #1a1a1a #cc372e #26a439 #cdac08 #0869cb #9647bf #479ec2 #98989d
//! palette 8..15 = #464646 #ff453a #32d74b #ffd60a #0a84ff #bf5af2 #76d6ff #ffffff
//! ```
//!
//! The theme defines no bold color, so `bright_foreground` is the foreground.
//! `dim_*` is not part of the theme; each normal color is scaled to ~66%
//! brightness (round(channel * 0.66)), matching how Ghostty derives dim/faint.
//!
//! The selection colors and `cursor-color` are wired through (terra patch #3
//! to the vendored egui_term); `cursor-text` is unused now that the cursor is
//! a beam rather than a filled block.

pub fn palette() -> egui_term::ColorPalette {
    egui_term::ColorPalette {
        foreground: "#ffffff".into(),
        background: "#1e1e1e".into(),

        // palette 0-7
        black: "#1a1a1a".into(),
        red: "#cc372e".into(),
        green: "#26a439".into(),
        yellow: "#cdac08".into(),
        blue: "#0869cb".into(),
        magenta: "#9647bf".into(),
        cyan: "#479ec2".into(),
        white: "#98989d".into(),

        // palette 8-15
        bright_black: "#464646".into(),
        bright_red: "#ff453a".into(),
        bright_green: "#32d74b".into(),
        bright_yellow: "#ffd60a".into(),
        bright_blue: "#0a84ff".into(),
        bright_magenta: "#bf5af2".into(),
        bright_cyan: "#76d6ff".into(),
        bright_white: "#ffffff".into(),

        // no bold color in the theme -> foreground
        bright_foreground: Some("#ffffff".into()),

        // derived: normal colors at ~66% brightness
        dim_foreground: "#a8a8a8".into(),
        dim_black: "#111111".into(),
        dim_red: "#87241e".into(),
        dim_green: "#196c26".into(),
        dim_yellow: "#877205".into(),
        dim_blue: "#054586".into(),
        dim_magenta: "#632f7e".into(),
        dim_cyan: "#2f6880".into(),
        dim_white: "#646468".into(),

        // selection: opaque overlay, white text
        selection_background: "#3f638b".into(),
        selection_foreground: Some("#ffffff".into()),

        // cursor beam
        cursor_color: "#98989d".into(),
    }
}
