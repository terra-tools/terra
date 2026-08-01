use egui::{Context, FontId};

use crate::types::Size;

#[derive(Debug, Clone)]
pub struct FontSettings {
    pub font_type: FontId,
    /// Cell-height multiplier (terra patch): 1.0 = font's natural row height,
    /// 1.3 = Ghostty's `adjust-cell-height = 30%`. Glyphs are centered
    /// vertically inside the taller cell (see view.rs).
    pub line_height: f32,
}

impl Default for FontSettings {
    fn default() -> Self {
        Self {
            font_type: FontId::monospace(14.0),
            line_height: 1.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TerminalFont {
    font_type: FontId,
    line_height: f32,
}

impl Default for TerminalFont {
    fn default() -> Self {
        let settings = FontSettings::default();
        Self {
            font_type: settings.font_type,
            line_height: settings.line_height,
        }
    }
}

impl TerminalFont {
    pub fn new(settings: FontSettings) -> Self {
        Self {
            font_type: settings.font_type,
            line_height: settings.line_height.max(0.5),
        }
    }

    pub fn font_type(&self) -> FontId {
        self.font_type.clone()
    }

    pub fn font_measure(&self, ctx: &Context) -> Size {
        let (width, height) = ctx.fonts_mut(|f| {
            (
                f.glyph_width(&self.font_type, 'm'),
                f.row_height(&self.font_type),
            )
        });

        Size::new(width, height * self.line_height)
    }
}
