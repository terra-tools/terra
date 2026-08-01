//! Font setup: JetBrains Mono (Ghostty's default) as the monospace face.
//!
//! # Why there is no Apple Color Emoji here
//!
//! epaint 0.35 rasterizes glyphs by pulling *outlines* out of `skrifa`
//! (`OutlineGlyphCollection::get` + `outline.draw(..)`) and filling them with
//! `vello_cpu` in solid **white**; the resulting coverage mask is then tinted
//! with the text colour by the tessellator. There is no call to
//! `skrifa`'s `color_glyphs()` / `bitmap_strikes()` anywhere in the crate, so
//! COLR, CBDT and sbix colour tables are simply never read.
//!
//! `/System/Library/Fonts/Apple Color Emoji.ttc` is an sbix font: its `glyf`
//! entries are empty stubs and all the artwork lives in the `sbix` bitmap
//! table. Loading it would therefore *hide* the working monochrome emoji
//! (its `cmap` claims the codepoints) and draw nothing at all, so we
//! deliberately don't.
//!
//! What we do instead: start from [`egui::FontDefinitions::default()`], which
//! already ships `NotoEmoji-Regular` plus `emoji-icon-font` as fallbacks in
//! both the `Monospace` and `Proportional` families, and only *prepend* our
//! own faces. Emoji stay monochrome but they do render.

/// Family name for the bold face, for callers that want it explicitly.
pub const BOLD_FAMILY: &str = "JetBrains Mono Bold";

/// Family for chrome text (tab titles, hints): the system UI face at its
/// regular weight, falling back to egui's proportional font.
pub const UI_FAMILY: &str = "terra-ui";
/// Same face at medium weight — macOS titles the active tab in SF Pro Medium.
pub const UI_MEDIUM_FAMILY: &str = "terra-ui-medium";

const JETBRAINS_MONO_REGULAR: &[u8] = include_bytes!("../assets/JetBrainsMono-Regular.ttf");
const JETBRAINS_MONO_BOLD: &[u8] = include_bytes!("../assets/JetBrainsMono-Bold.ttf");

/// macOS ships the system UI face as a single *variable* TrueType file (not a
/// `.ttc` collection), so it needs no face index — just variation coordinates.
const SF_PRO_PATH: &str = "/System/Library/Fonts/SFNS.ttf";

/// `wght` values of SF Pro's own named instances (read out of its `fvar`
/// table): "Regular" is 400 and "Medium" is 510 — note the axis runs 1..=1000,
/// not the usual 100..=900.
const SF_WGHT_REGULAR: f32 = 400.0;
const SF_WGHT_MEDIUM: f32 = 510.0;

/// SF Pro's `opsz` axis runs 17..=96 and *defaults to 28* — i.e. to the Display
/// design, which is drawn tight for headlines. Pinning it to the low end picks
/// the Text design (looser spacing, more open counters), which is what macOS
/// uses for 13px chrome like window and tab titles.
const SF_OPSZ_TEXT: f32 = 17.0;

/// Whether [`UI_MEDIUM_FAMILY`] resolves to a genuinely heavier face.
///
/// False when the system font could not be read or parsed, in which case both
/// UI families fall through to egui's single-weight proportional font and
/// callers have to fake weight (see `ui::paint_faux_medium`).
static REAL_UI_MEDIUM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// True once [`install`] has registered a real medium-weight UI face.
pub fn has_real_ui_medium() -> bool {
    REAL_UI_MEDIUM.load(std::sync::atomic::Ordering::Relaxed)
}

/// Read the system UI font, returning its bytes only if `skrifa` (the parser
/// epaint rasterizes with) accepts it *and* it exposes the weight axis we mean
/// to set. Any failure is silent: the caller falls back to the default font.
fn read_system_ui_font() -> Option<Vec<u8>> {
    let bytes = std::fs::read(SF_PRO_PATH).ok()?;

    // `variation_axes` parses the file through skrifa and yields an empty list
    // if that fails, so this doubles as a parse check.
    let has_weight_axis = egui::FontData::from_owned(bytes.clone())
        .variation_axes()
        .iter()
        .any(|axis| axis.tag == egui::epaint::text::Tag::new(b"wght"));

    has_weight_axis.then_some(bytes)
}

/// The system UI face pinned to one weight and to the Text optical size.
fn sf_pro_at(weight: f32) -> egui::FontTweak {
    egui::FontTweak {
        coords: egui::epaint::text::VariationCoords::new([
            (b"wght", weight),
            (b"opsz", SF_OPSZ_TEXT),
        ]),
        ..Default::default()
    }
}

/// Install JetBrains Mono as the first monospace font, keeping egui's built-in
/// fonts (including the emoji fallbacks) behind it, and pin the glyph
/// anti-aliasing settings that make light-on-dark text render at the right
/// brightness (see [`pin_text_rendering`]).
pub fn install(ctx: &egui::Context) {
    use std::sync::Arc;

    pin_text_rendering(ctx);

    // `default()` brings Hack, Ubuntu-Light, NotoEmoji-Regular and
    // emoji-icon-font along; we only push our faces in front of them.
    let mut fonts = egui::FontDefinitions::default();

    fonts.font_data.insert(
        "JetBrains Mono".to_owned(),
        Arc::new(egui::FontData::from_static(JETBRAINS_MONO_REGULAR)),
    );
    fonts.font_data.insert(
        BOLD_FAMILY.to_owned(),
        Arc::new(egui::FontData::from_static(JETBRAINS_MONO_BOLD)),
    );

    // First in the monospace list => `FontId::monospace(..)` picks it up, and
    // anything it lacks (emoji, √, box drawing) still falls through to the
    // egui defaults.
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, "JetBrains Mono".to_owned());

    // A named family so bold monospace text can be requested explicitly.
    let mut bold_family = vec![BOLD_FAMILY.to_owned()];
    bold_family.extend(
        fonts
            .families
            .get(&egui::FontFamily::Monospace)
            .cloned()
            .unwrap_or_default(),
    );
    fonts
        .families
        .insert(egui::FontFamily::Name(BOLD_FAMILY.into()), bold_family);

    install_ui_family(&mut fonts);

    ctx.set_fonts(fonts);
}

/// Register [`UI_FAMILY`] / [`UI_MEDIUM_FAMILY`], preferring the macOS system
/// face so tab titles are set in the same font (and the same weights) as the
/// native tab bars they imitate.
///
/// Both families are always registered, with egui's proportional list appended
/// behind whatever we found: if the system font is missing or unparsable the
/// families still resolve — to the default font, at its one weight — so callers
/// never have to branch on availability, and nothing here can panic.
fn install_ui_family(fonts: &mut egui::FontDefinitions) {
    use std::sync::Arc;

    let fallback = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();

    let mut regular = Vec::new();
    let mut medium = Vec::new();

    if let Some(bytes) = read_system_ui_font() {
        // The same file twice, at two points on its weight axis.
        fonts.font_data.insert(
            "SF Pro".to_owned(),
            Arc::new(egui::FontData::from_owned(bytes.clone()).tweak(sf_pro_at(SF_WGHT_REGULAR))),
        );
        fonts.font_data.insert(
            "SF Pro Medium".to_owned(),
            Arc::new(egui::FontData::from_owned(bytes).tweak(sf_pro_at(SF_WGHT_MEDIUM))),
        );
        regular.push("SF Pro".to_owned());
        medium.push("SF Pro Medium".to_owned());
        REAL_UI_MEDIUM.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    regular.extend(fallback.iter().cloned());
    medium.extend(fallback);

    fonts
        .families
        .insert(egui::FontFamily::Name(UI_FAMILY.into()), regular);
    fonts
        .families
        .insert(egui::FontFamily::Name(UI_MEDIUM_FAMILY.into()), medium);
}

/// Pin the glyph coverage -> alpha transfer function to the dark-mode curve.
///
/// # Why
///
/// egui/wgpu alpha-blends in **gamma (sRGB-encoded) space**: the fragment
/// shader computes `vertex_color * atlas_alpha` on the 0-255 sRGB values and
/// the blender does `src + dst * (1 - a)` on those same encoded values
/// (`egui-wgpu-0.35.0/src/egui.wgsl`, `fs_main_gamma_framebuffer`). For
/// light-on-dark text that systematically *under*-lights partially covered
/// pixels: a 50%-covered pixel should end up at ~73% of the text colour
/// (sRGB(0.5) in linear light), not 50%.
///
/// epaint compensates for this when it bakes the coverage mask into the font
/// atlas, via `FontColorTransferFunction`
/// (`epaint-0.35.0/src/image.rs`, and `TextOptions::color_transfer_function`
/// in `epaint-0.35.0/src/text/mod.rs`):
///
/// * `Off` / `Gamma(1.0)` — `alpha = coverage`. egui's **light**-mode default
///   (`Visuals::light()`, `egui-0.35.0/src/style.rs`), correct for dark text on
///   a light background.
/// * `TwoCoverageMinusCoverageSq` — `alpha = 2c - c²` (≈ `c^0.5`, i.e. roughly
///   the sRGB encoding curve). egui's **dark**-mode default
///   (`Visuals::dark()`), correct for light text on a dark background.
///
/// egui picks between them from the *system* theme, because
/// `Options::theme_preference` defaults to `ThemePreference::System`
/// (`egui-0.35.0/src/memory/mod.rs`). Terra's terminal surface is
/// unconditionally dark (`#1e1e1e`), so on a macOS account set to Light
/// appearance egui would bake `alpha = coverage` and every anti-aliased glyph
/// edge would come out visibly thinner and dimmer than Ghostty's — which
/// always gamma-corrects light-on-dark coverage.
///
/// So: pin the preference to Dark, and additionally force the dark-mode curve
/// into *both* theme styles so the setting survives a later `set_visuals` /
/// theme flip from anywhere else in the app.
fn pin_text_rendering(ctx: &egui::Context) {
    use egui::epaint::FontColorTransferFunction;

    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.all_styles_mut(|style| {
        style.visuals.text_options.color_transfer_function =
            FontColorTransferFunction::DARK_MODE_DEFAULT;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The UI families must exist after `install` whether or not the system
    /// font was there, so `ui.rs` can name them unconditionally.
    #[test]
    fn ui_families_are_always_registered() {
        let mut fonts = egui::FontDefinitions::default();
        install_ui_family(&mut fonts);

        for family in [UI_FAMILY, UI_MEDIUM_FAMILY] {
            let list = fonts
                .families
                .get(&egui::FontFamily::Name(family.into()))
                .unwrap_or_else(|| panic!("{family} not registered"));
            assert!(!list.is_empty(), "{family} has no faces to fall back on");
        }
    }

    /// On macOS the system face should be the *first* choice in both families,
    /// at two different weights. This is what makes the tab bar look native, so
    /// a regression here should fail loudly rather than silently fall back.
    #[test]
    #[cfg(target_os = "macos")]
    fn system_ui_face_loads_at_two_weights() {
        let mut fonts = egui::FontDefinitions::default();
        install_ui_family(&mut fonts);

        assert!(has_real_ui_medium(), "{SF_PRO_PATH} did not load");
        assert_eq!(
            fonts.families[&egui::FontFamily::Name(UI_FAMILY.into())][0],
            "SF Pro"
        );
        assert_eq!(
            fonts.families[&egui::FontFamily::Name(UI_MEDIUM_FAMILY.into())][0],
            "SF Pro Medium"
        );

        let weight = |name: &str| fonts.font_data[name].tweak.coords.as_ref()[0].1;
        assert_eq!(weight("SF Pro"), SF_WGHT_REGULAR);
        assert_eq!(weight("SF Pro Medium"), SF_WGHT_MEDIUM);
        assert!(weight("SF Pro Medium") > weight("SF Pro"));
    }
}
