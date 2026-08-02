//! terra patch (#19): colour emoji in the grid.
//!
//! epaint rasterizes glyphs as outline coverage masks and tints them with the
//! text colour — it never reads a font's sbix/CBDT/COLR colour tables, so a
//! colour emoji font cannot work through the text pipeline at all. Instead,
//! emoji cells are painted as textured quads: the system emoji font's `sbix`
//! strikes are embedded PNGs, read here with `skrifa`, decoded once per
//! (character, pixel-size) and cached as egui textures for the lifetime of
//! the context.
//!
//! Scope: single-codepoint emoji, plus a trailing U+FE0F variation selector
//! deciding presentation for the legacy symbol blocks. ZWJ sequences, flags
//! and keycaps render as their component glyphs, exactly as before this
//! patch. Off macOS the font file is absent and every lookup falls through
//! to the monochrome text path.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use egui::{ColorImage, Context, Id, TextureHandle, TextureOptions};
use skrifa::bitmap::BitmapData;
use skrifa::instance::Size;
use skrifa::{FontRef, MetadataProvider};

const APPLE_EMOJI: &str = "/System/Library/Fonts/Apple Color Emoji.ttc";

/// The font bytes, read at most once. `None` when the file does not exist
/// (any non-macOS system) or cannot be read.
fn font_data() -> Option<&'static [u8]> {
    static DATA: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    DATA.get_or_init(|| std::fs::read(APPLE_EMOJI).ok())
        .as_deref()
}

/// Whether `ch` should be tried as a colour emoji. Deliberately loose: a hit
/// only means "ask the emoji font", and a character the font has no bitmap
/// for falls back to the text path, so false positives cost a cache miss,
/// not a wrong glyph.
///
/// The legacy symbol blocks (arrows, dingbats, misc symbols) default to text
/// presentation and turn colourful only behind a variation selector, which
/// the terminal grid stores as a zero-width companion.
pub fn wants_color(ch: char, vs16: bool) -> bool {
    match ch as u32 {
        // Emoji-first planes: pictographs, transport, supplemental, faces.
        0x1F000..=0x1FAFF => true,
        // Legacy blocks: only with an explicit emoji presentation selector.
        0x2190..=0x2BFF | 0x3030 | 0x303D | 0x3297 | 0x3299 => vs16,
        0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139 => vs16,
        _ => false,
    }
}

type Cache = Arc<Mutex<HashMap<(char, u32), Option<TextureHandle>>>>;

/// The texture for `ch` at (roughly) `px` pixels, or `None` when the emoji
/// font is absent or has no bitmap for it. Cached per egui context.
pub fn texture(ctx: &Context, ch: char, px: u32) -> Option<TextureHandle> {
    let cache: Cache = ctx.data_mut(|d| {
        d.get_temp_mut_or_insert_with(Id::new("terra_emoji_atlas"), Cache::default)
            .clone()
    });
    let mut cache = cache.lock().expect("emoji atlas poisoned");
    if let Some(hit) = cache.get(&(ch, px)) {
        return hit.clone();
    }
    let tex = rasterize(ch, px).map(|image| {
        ctx.load_texture(
            format!("emoji:{ch}:{px}"),
            image,
            TextureOptions::LINEAR,
        )
    });
    cache.insert((ch, px), tex.clone());
    tex
}

/// Decode the best-fitting sbix strike for `ch` into an image.
fn rasterize(ch: char, px: u32) -> Option<ColorImage> {
    let font = FontRef::from_index(font_data()?, 0).ok()?;
    let glyph = font.charmap().map(ch)?;
    let strikes = font.bitmap_strikes();
    let bitmap = strikes.glyph_for_size(Size::new(px as f32), glyph)?;
    match bitmap.data {
        BitmapData::Png(bytes) => decode_png(bytes),
        // Apple Color Emoji is sbix/PNG; other formats mean an unexpected
        // font, and falling back to text beats guessing at raw layouts.
        _ => None,
    }
}

fn decode_png(bytes: &[u8]) -> Option<ColorImage> {
    let decoder = png::Decoder::new(bytes);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    let (w, h) = (info.width as usize, info.height as usize);
    match info.color_type {
        png::ColorType::Rgba => {
            Some(ColorImage::from_rgba_unmultiplied([w, h], &buf))
        }
        png::ColorType::Rgb => Some(ColorImage::from_rgb([w, h], &buf)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictographs_are_always_emoji() {
        assert!(wants_color('🙂', false));
        assert!(wants_color('🚀', false));
        assert!(wants_color('🦔', false));
    }

    #[test]
    fn legacy_symbols_need_the_selector() {
        assert!(!wants_color('☀', false));
        assert!(wants_color('☀', true));
        assert!(!wants_color('⚡', false));
        assert!(wants_color('⚡', true));
    }

    #[test]
    fn plain_text_is_never_emoji() {
        assert!(!wants_color('a', false));
        assert!(!wants_color('א', true));
        assert!(!wants_color('~', true));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_font_yields_a_bitmap() {
        let image = rasterize('🙂', 32).expect("🙂 has sbix art");
        assert!(image.width() > 0 && image.height() > 0);
        // Colour, not a white coverage mask: some pixel differs across
        // channels.
        assert!(image
            .pixels
            .iter()
            .any(|p| p.r() != p.g() || p.g() != p.b()));
    }
}
