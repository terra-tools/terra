//! `terra screenshot --pretty` — the ray.so treatment, in plain pixel
//! arithmetic.
//!
//! The app hands back the window's framebuffer and nothing else. This module
//! turns that rectangle into something you can paste into an issue: generous
//! padding, a diagonal gradient behind it, the window pixels on a
//! rounded-corner card under a title bar with macOS traffic lights in it, and
//! a soft drop shadow underneath.
//!
//! It is deliberately ~300 lines of arithmetic rather than an image framework.
//! Everything here is one of three primitives:
//!
//! - **a rounded-rectangle distance field** ([`rounded_rect_coverage`]) —
//!   the analytic distance to the shape's edge, turned into per-pixel
//!   coverage. This is what makes the corners antialias cleanly at any radius
//!   without supersampling: a pixel one unit outside the edge is 0, one unit
//!   inside is 1, and the ones straddling it get the fraction they cover.
//! - **a box blur, three times** ([`blur`]) — three box passes approximate a
//!   Gaussian closely enough for a shadow, and cost O(pixels) instead of
//!   O(pixels × radius) because each pass is a sliding sum.
//! - **source-over compositing** ([`Image::blend`]) — everything is drawn onto an
//!   opaque canvas, so alpha only ever has to interpolate.
//!
//! Sizes are all derived from one inferred scale ([`Layout::for_image`]),
//! because the framebuffer is in
//! *physical* pixels: the same window is 1100×720 on one display and 2200×1440
//! on a Retina one, and a 12-pixel corner radius would look like two different
//! designs. Everything is expressed in logical units and multiplied once.

use anyhow::{bail, Context, Result};

/// An RGB colour, straight from `--bg` or from the defaults below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// The default background: ray.so's soft lavender, on the diagonal — a
/// bluish lilac into a paler pink-tinged one. Light on purpose: the dark
/// terminal card reads as a separate object floating on it.
pub const DEFAULT_BG: (Rgb, Rgb) = (Rgb(0xa8, 0x9b, 0xf2), Rgb(0xdc, 0xc9, 0xf2));

/// The window dots. ray.so-style: three identical muted grey discs rather
/// than the literal red/amber/green — decoration, not controls.
const TRAFFIC_LIGHTS: [Rgb; 3] = [
    Rgb(0x56, 0x51, 0x60),
    Rgb(0x56, 0x51, 0x60),
    Rgb(0x56, 0x51, 0x60),
];

/// A decoded image: 8-bit RGBA, row major, no padding.
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

impl Image {
    fn filled(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width * height * 4],
        }
    }

    fn set(&mut self, x: usize, y: usize, color: Rgb) {
        let i = (y * self.width + x) * 4;
        self.pixels[i] = color.0;
        self.pixels[i + 1] = color.1;
        self.pixels[i + 2] = color.2;
        self.pixels[i + 3] = 0xff;
    }

    /// Composite `color` over the pixel at `(x, y)` with coverage `a` (0..=1).
    fn blend(&mut self, x: usize, y: usize, color: Rgb, a: f32) {
        if a <= 0.0 {
            return;
        }
        let a = a.min(1.0);
        let i = (y * self.width + x) * 4;
        for (channel, src) in [color.0, color.1, color.2].into_iter().enumerate() {
            let dst = f32::from(self.pixels[i + channel]);
            self.pixels[i + channel] = (dst + (f32::from(src) - dst) * a).round() as u8;
        }
        self.pixels[i + 3] = 0xff;
    }

    fn pixel(&self, x: usize, y: usize) -> Rgb {
        let i = (y * self.width + x) * 4;
        Rgb(self.pixels[i], self.pixels[i + 1], self.pixels[i + 2])
    }
}

/// Every measurement of the composition, in physical pixels.
struct Layout {
    pad: f32,
    radius: f32,
    /// Height of the title bar that carries the traffic lights.
    chrome: f32,
    /// Distance from the card's left edge to the first dot's centre.
    dot_inset: f32,
    dot_radius: f32,
    dot_gap: f32,
    shadow_offset: f32,
    shadow_blur: f32,
    shadow_alpha: f32,
}

impl Layout {
    /// A screenshot carries no DPI, so the scale is inferred from its size: a
    /// terra window is at least 320 points tall (`with_min_inner_size`) and
    /// usually ~720, so dividing the shorter side by a nominal 720 recovers
    /// 1 for a non-Retina window and 2 for a Retina one. Clamped, so a very
    /// small or very large window still gets sane proportions rather than
    /// hairline or gigantic corners.
    fn for_image(width: usize, height: usize) -> Self {
        let scale = (width.min(height) as f32 / 720.0).clamp(1.0, 4.0);
        Self {
            // ray.so proportions: the card floats with a wide margin —
            // roughly an eighth of the card per side, not a thin frame.
            pad: 190.0 * scale,
            radius: 16.0 * scale,
            chrome: 44.0 * scale,
            dot_inset: 30.0 * scale,
            dot_radius: 8.5 * scale,
            dot_gap: 27.0 * scale,
            shadow_offset: 16.0 * scale,
            shadow_blur: 38.0 * scale,
            shadow_alpha: 0.30,
        }
    }
}

/// Parse `--bg`: `#4f46e5,#ec4899`, or a single colour for a flat background.
/// `#` is optional, so a shell needs no quoting.
pub fn parse_bg(spec: &str) -> Result<(Rgb, Rgb)> {
    let mut parts = spec.split(',').map(str::trim).filter(|s| !s.is_empty());
    let first = parts.next().context("--bg needs at least one colour")?;
    let from = parse_hex(first)?;
    let to = match parts.next() {
        Some(second) => parse_hex(second)?,
        None => from,
    };
    if parts.next().is_some() {
        bail!("--bg takes one or two colours, e.g. --bg '#4f46e5,#ec4899'");
    }
    Ok((from, to))
}

fn parse_hex(raw: &str) -> Result<Rgb> {
    let hex = raw.strip_prefix('#').unwrap_or(raw);
    // `#abc` is the CSS shorthand; expanding it here costs three lines and
    // saves everyone typing six digits.
    let expanded: String = match hex.len() {
        3 => hex.chars().flat_map(|c| [c, c]).collect(),
        6 => hex.to_string(),
        _ => bail!("bad colour {raw:?}: expected #rgb or #rrggbb"),
    };
    let byte = |at: usize| {
        u8::from_str_radix(&expanded[at..at + 2], 16)
            .with_context(|| format!("bad colour {raw:?}: {:?} is not hex", &expanded[at..at + 2]))
    };
    Ok(Rgb(byte(0)?, byte(2)?, byte(4)?))
}

/// Decode a PNG into straight 8-bit RGBA.
pub fn decode(png: &[u8]) -> Result<Image> {
    let decoder = png::Decoder::new(std::io::Cursor::new(png));
    let mut reader = decoder.read_info().context("not a PNG")?;
    let mut buf = vec![0; reader.output_buffer_size().context("PNG too large")?];
    let info = reader.next_frame(&mut buf).context("truncated PNG")?;
    let (width, height) = (info.width as usize, info.height as usize);

    // The app always sends 8-bit RGBA; the other shapes are handled anyway so
    // that `--pretty` can also be pointed at some other PNG later without
    // failing on a technicality.
    let pixels = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgba, png::BitDepth::Eight) => buf[..info.buffer_size()].to_vec(),
        (png::ColorType::Rgb, png::BitDepth::Eight) => buf[..info.buffer_size()]
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 0xff])
            .collect(),
        (color, depth) => bail!("unsupported PNG format: {color:?} at {depth:?} bits"),
    };
    Ok(Image {
        width,
        height,
        pixels,
    })
}

/// Encode straight 8-bit RGBA back to a PNG.
pub fn encode(image: &Image) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, image.width as u32, image.height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&image.pixels)?;
    writer.finish()?;
    Ok(out)
}

/// Signed distance from `(x, y)` to a rounded rectangle's edge — negative
/// inside, positive outside — converted to the fraction of the pixel the shape
/// covers. This is the whole antialiasing story: one subtraction per pixel,
/// exact for straight edges and near-exact on the corners.
fn rounded_rect_coverage(x: f32, y: f32, rect: (f32, f32, f32, f32), radius: f32) -> f32 {
    let (left, top, width, height) = rect;
    let (half_w, half_h) = (width / 2.0, height / 2.0);
    let radius = radius.min(half_w).min(half_h).max(0.0);
    let dx = (x - (left + half_w)).abs() - (half_w - radius);
    let dy = (y - (top + half_h)).abs() - (half_h - radius);
    let outside = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt();
    let inside = dx.max(dy).min(0.0);
    let distance = outside + inside - radius;
    (0.5 - distance).clamp(0.0, 1.0)
}

/// The same idea for a disc — the traffic lights.
fn circle_coverage(x: f32, y: f32, cx: f32, cy: f32, radius: f32) -> f32 {
    let distance = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - radius;
    (0.5 - distance).clamp(0.0, 1.0)
}

/// Three box blurs ≈ a Gaussian. Separable and incremental, so the cost does
/// not grow with the radius: each pass is a sliding sum, one add and one
/// subtract per pixel however wide the kernel is.
///
/// All three horizontal passes run before the transpose rather than
/// alternating with the vertical ones — the passes are 1-D along independent
/// axes and commute, so this is the same blur with two transposes instead of
/// six, and the transpose is the only part that touches memory out of order.
fn blur(mask: &mut Vec<f32>, width: usize, height: usize, radius: f32) {
    let r = radius.round() as usize;
    if r == 0 {
        return;
    }
    for _ in 0..3 {
        box_pass(mask, width, height, r);
    }
    transpose(mask, width, height);
    for _ in 0..3 {
        box_pass(mask, height, width, r);
    }
    transpose(mask, height, width);
}

/// One horizontal box pass, as a sliding window sum.
fn box_pass(mask: &mut [f32], width: usize, height: usize, r: usize) {
    let window = (2 * r + 1) as f32;
    let mut row = vec![0.0f32; width];
    for y in 0..height {
        let line = &mut mask[y * width..(y + 1) * width];
        row.copy_from_slice(line);
        // Edges clamp rather than wrap: a shadow must not bleed from the
        // opposite side of the canvas.
        let at = |i: isize| row[i.clamp(0, width as isize - 1) as usize];
        let mut sum: f32 = (-(r as isize)..=(r as isize)).map(at).sum();
        for (x, out) in line.iter_mut().enumerate() {
            *out = sum / window;
            sum += at(x as isize + r as isize + 1) - at(x as isize - r as isize);
        }
    }
}

fn transpose(mask: &mut Vec<f32>, width: usize, height: usize) {
    let mut out = vec![0.0f32; mask.len()];
    for y in 0..height {
        for x in 0..width {
            out[x * height + y] = mask[y * width + x];
        }
    }
    *mask = out;
}

fn blend_rgb(from: Rgb, to: Rgb, t: f32) -> Rgb {
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    Rgb(mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2))
}

/// The colour of the title bar: the average of the shot's four corners, taken
/// a few pixels in.
///
/// A terminal's corners are its background in almost every case, so this makes
/// the bar read as part of the window rather than as a stripe bolted on top —
/// the whole point of putting the traffic lights in a bar instead of straight
/// over the first line of output. A full-screen TUI whose corners are not
/// background just yields a slightly different dark tone, which still looks
/// deliberate; there is nothing here that can go badly wrong.
fn chrome_color(shot: &Image) -> Rgb {
    let inset = 3
        .min(shot.width.saturating_sub(1))
        .min(shot.height.saturating_sub(1));
    let (right, bottom) = (shot.width - 1 - inset, shot.height - 1 - inset);
    let corners = [
        shot.pixel(inset, inset),
        shot.pixel(right, inset),
        shot.pixel(inset, bottom),
        shot.pixel(right, bottom),
    ];
    let mean = |channel: fn(&Rgb) -> u8| {
        (corners.iter().map(|c| u32::from(channel(c))).sum::<u32>() / 4) as u8
    };
    Rgb(mean(|c| c.0), mean(|c| c.1), mean(|c| c.2))
}

/// Composite `shot` ray.so-style. The returned image is the shot, plus a title
/// bar above it, plus padding on every side.
pub fn compose(shot: &Image, bg: (Rgb, Rgb)) -> Image {
    let layout = Layout::for_image(shot.width, shot.height);
    let pad = layout.pad.round() as usize;
    // The traffic lights get a bar of their own rather than being painted over
    // the window: a terminal's first line is its most interesting one, and
    // three opaque discs on top of the prompt is exactly the wrong trade.
    let chrome = layout.chrome.round() as usize;
    let width = shot.width + pad * 2;
    let height = shot.height + chrome + pad * 2;
    let mut canvas = Image::filled(width, height);

    // 1. the background: a linear gradient along the top-left → bottom-right
    //    diagonal, which is `x + y` normalised.
    let span = (width + height) as f32;
    for y in 0..height {
        for x in 0..width {
            let t = (x + y) as f32 / span;
            canvas.set(x, y, blend_rgb(bg.0, bg.1, t));
        }
    }

    let card = (
        pad as f32,
        pad as f32,
        shot.width as f32,
        (shot.height + chrome) as f32,
    );

    // 2. the shadow: the card's own silhouette, dropped down and blurred.
    let mut shadow = vec![0.0f32; width * height];
    for y in 0..height {
        for x in 0..width {
            let sample_y = y as f32 + 0.5 - layout.shadow_offset;
            shadow[y * width + x] =
                rounded_rect_coverage(x as f32 + 0.5, sample_y, card, layout.radius);
        }
    }
    blur(&mut shadow, width, height, layout.shadow_blur);
    for y in 0..height {
        for x in 0..width {
            let a = shadow[y * width + x] * layout.shadow_alpha;
            canvas.blend(x, y, Rgb(0, 0, 0), a);
        }
    }

    // 3. the card: the title bar, then the window's own pixels, both masked to
    //    rounded corners. Only the card's bounding box is touched — the
    //    corners are the only place the coverage is not exactly 1.
    let bar = chrome_color(shot);
    for y in 0..shot.height + chrome {
        for x in 0..shot.width {
            let (cx, cy) = (x + pad, y + pad);
            let coverage =
                rounded_rect_coverage(cx as f32 + 0.5, cy as f32 + 0.5, card, layout.radius);
            let color = match y.checked_sub(chrome) {
                Some(row) => shot.pixel(x, row),
                None => bar,
            };
            canvas.blend(cx, cy, color, coverage);
        }
    }

    // 4. the traffic lights, centred in the title bar.
    let dot_y = card.1 + chrome as f32 / 2.0;
    for (i, color) in TRAFFIC_LIGHTS.iter().enumerate() {
        let dot_x = card.0 + layout.dot_inset + layout.dot_gap * i as f32;
        let reach = layout.dot_radius + 1.0;
        let x0 = (dot_x - reach).floor().max(0.0) as usize;
        let x1 = ((dot_x + reach).ceil() as usize).min(width - 1);
        let y0 = (dot_y - reach).floor().max(0.0) as usize;
        let y1 = ((dot_y + reach).ceil() as usize).min(height - 1);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let a = circle_coverage(
                    x as f32 + 0.5,
                    y as f32 + 0.5,
                    dot_x,
                    dot_y,
                    layout.dot_radius,
                );
                canvas.blend(x, y, *color, a);
            }
        }
    }

    // A Retina framebuffer is 2-3x physical pixels, which would make the
    // export two or three times the size ray.so hands out. Composited at
    // full resolution for crisp AA, then brought back to logical size.
    let scale = (shot.width.min(shot.height) as f32 / 720.0).clamp(1.0, 4.0);
    if scale > 1.0 {
        downscale(&canvas, scale)
    } else {
        canvas
    }
}

/// Shrink by `factor` with box (area-average) sampling — the right filter for
/// a downscale: every source pixel contributes once, so thin AA edges dim
/// smoothly instead of shimmering the way point sampling would.
fn downscale(src: &Image, factor: f32) -> Image {
    let width = (src.width as f32 / factor).round().max(1.0) as usize;
    let height = (src.height as f32 / factor).round().max(1.0) as usize;
    let mut out = Image::filled(width, height);
    for y in 0..height {
        let y0 = (y as f32 * factor) as usize;
        let y1 = (((y + 1) as f32 * factor).ceil() as usize).min(src.height);
        for x in 0..width {
            let x0 = (x as f32 * factor) as usize;
            let x1 = (((x + 1) as f32 * factor).ceil() as usize).min(src.width);
            let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
            for sy in y0..y1.max(y0 + 1) {
                for sx in x0..x1.max(x0 + 1) {
                    let p = src.pixel(sx.min(src.width - 1), sy.min(src.height - 1));
                    r += u32::from(p.0);
                    g += u32::from(p.1);
                    b += u32::from(p.2);
                    n += 1;
                }
            }
            out.set(
                x,
                y,
                Rgb((r / n) as u8, (g / n) as u8, (b / n) as u8),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A solid shot, so any pixel that is not this colour came from the
    /// composition rather than from the window.
    const SHOT: Rgb = Rgb(0x1e, 0x1e, 0x1e);

    fn shot(width: usize, height: usize) -> Image {
        let mut image = Image::filled(width, height);
        for y in 0..height {
            for x in 0..width {
                image.set(x, y, SHOT);
            }
        }
        image
    }

    #[test]
    fn hex_colours_parse_in_both_lengths_and_with_or_without_a_hash() {
        assert_eq!(parse_hex("#4f46e5").unwrap(), Rgb(0x4f, 0x46, 0xe5));
        assert_eq!(parse_hex("4f46e5").unwrap(), Rgb(0x4f, 0x46, 0xe5));
        assert_eq!(parse_hex("#abc").unwrap(), Rgb(0xaa, 0xbb, 0xcc));
        assert!(parse_hex("#12345").is_err());
        assert!(parse_hex("#gggggg").is_err());
    }

    #[test]
    fn a_single_bg_colour_means_a_flat_background() {
        let (from, to) = parse_bg("#102030").unwrap();
        assert_eq!(from, to);
        assert_eq!(from, Rgb(0x10, 0x20, 0x30));
        let (from, to) = parse_bg("#000, #fff").unwrap();
        assert_eq!((from, to), (Rgb(0, 0, 0), Rgb(0xff, 0xff, 0xff)));
        assert!(parse_bg("#000,#111,#222").is_err());
        assert!(parse_bg("").is_err());
    }

    #[test]
    fn the_composition_is_the_shot_plus_a_title_bar_plus_padding() {
        let composed = compose(&shot(400, 300), DEFAULT_BG);
        let layout = Layout::for_image(400, 300);
        let pad = layout.pad.round() as usize;
        let chrome = layout.chrome.round() as usize;
        assert_eq!(composed.width, 400 + pad * 2);
        assert_eq!(composed.height, 300 + chrome + pad * 2);
    }

    /// The window's pixels are pushed down by exactly the bar's height —
    /// nothing is cropped and nothing is painted over.
    #[test]
    fn the_title_bar_sits_above_the_window_rather_than_on_it() {
        let mut original = shot(400, 300);
        // A distinctive first row, which is what the traffic lights used to
        // land on.
        let marker = Rgb(0x00, 0xff, 0x00);
        for x in 0..400 {
            original.set(x, 0, marker);
        }
        let composed = compose(&original, DEFAULT_BG);
        let layout = Layout::for_image(400, 300);
        let pad = layout.pad.round() as usize;
        let chrome = layout.chrome.round() as usize;
        // Every pixel of the shot's first row survives, including the ones
        // behind the dots.
        for x in [200, 300, 399 - 1] {
            assert_eq!(composed.pixel(pad + x, pad + chrome), marker, "column {x}");
        }
        // …and the bar above it took the window's own background colour.
        assert_eq!(composed.pixel(pad + 200, pad + chrome / 2), SHOT);
    }

    /// The middle of the card is the window, untouched — compositing must not
    /// tint the pixels it is there to show.
    #[test]
    fn the_window_pixels_survive_in_the_middle_of_the_card() {
        let composed = compose(&shot(400, 300), DEFAULT_BG);
        assert_eq!(
            composed.pixel(composed.width / 2, composed.height / 2),
            SHOT
        );
    }

    /// The very corner of the canvas is pure gradient, and the two ends of the
    /// diagonal are the two `--bg` colours.
    #[test]
    fn the_background_runs_diagonally_between_the_two_colours() {
        let bg = (Rgb(0, 0, 0), Rgb(0xff, 0xff, 0xff));
        let composed = compose(&shot(200, 200), bg);
        let top_left = composed.pixel(0, 0);
        let bottom_right = composed.pixel(composed.width - 1, composed.height - 1);
        assert!(top_left.0 < 8, "{top_left:?}");
        assert!(bottom_right.0 > 0xf0, "{bottom_right:?}");
        // …and it is a gradient, not two halves: the far corners differ from
        // the middle of the same edge.
        let mid_top = composed.pixel(composed.width / 2, 0);
        assert!(mid_top.0 > top_left.0, "{mid_top:?} vs {top_left:?}");
    }

    /// The card's corners must be rounded *and* antialiased: the exact corner
    /// pixel is background, and somewhere along the arc there are pixels that
    /// are neither pure background nor pure window.
    #[test]
    fn the_card_corners_are_rounded_and_antialiased() {
        // A flat white background, so "not the window colour" is unambiguous.
        let white = Rgb(0xff, 0xff, 0xff);
        let composed = compose(&shot(400, 300), (white, white));
        let layout = Layout::for_image(400, 300);
        let pad = layout.pad.round() as usize;

        // The card's own corner pixel lies outside the rounded shape.
        let corner = composed.pixel(pad, pad);
        assert_ne!(corner, SHOT, "the corner must not be square");

        // Walk the diagonal into the corner: there must be at least one pixel
        // strictly between the two colours, which is antialiasing.
        let partial = (0..(layout.radius as usize + 2)).any(|d| {
            let p = composed.pixel(pad + d, pad + d);
            p != SHOT && p.0 < 0xff && p.0 > SHOT.0
        });
        assert!(partial, "no partially covered pixel along the corner arc");
    }

    /// Three dots, in macOS order, in the title bar's top-left.
    #[test]
    fn the_traffic_lights_are_drawn_in_the_cards_corner() {
        let composed = compose(&shot(400, 300), DEFAULT_BG);
        let layout = Layout::for_image(400, 300);
        let pad = layout.pad;
        for (i, expected) in TRAFFIC_LIGHTS.iter().enumerate() {
            let x = (pad + layout.dot_inset + layout.dot_gap * i as f32) as usize;
            let y = (pad + layout.chrome / 2.0) as usize;
            let got = composed.pixel(x, y);
            let close = |a: u8, b: u8| a.abs_diff(b) <= 2;
            assert!(
                close(got.0, expected.0) && close(got.1, expected.1) && close(got.2, expected.2),
                "dot {i}: got {got:?}, expected {expected:?}"
            );
        }
    }

    /// The shadow is under and *below* the card: the padding directly beneath
    /// it is darker than the padding directly above it, on the same gradient.
    #[test]
    fn the_shadow_falls_below_the_card() {
        let flat = Rgb(0x80, 0x80, 0x80);
        let composed = compose(&shot(400, 300), (flat, flat));
        let layout = Layout::for_image(400, 300);
        let pad = layout.pad.round() as usize;
        let chrome = layout.chrome.round() as usize;
        let x = composed.width / 2;
        let below = composed.pixel(x, pad + chrome + 300 + 4);
        let above = composed.pixel(x, pad - 4);
        assert!(
            below.0 < above.0,
            "below {below:?} should be darker than above {above:?}"
        );
        assert!(below.0 < flat.0, "the shadow must actually darken");
    }

    /// A blurred point spreads, keeps its total mass, and never goes negative.
    #[test]
    fn the_blur_spreads_without_leaking() {
        let (w, h) = (64, 64);
        let mut mask = vec![0.0f32; w * h];
        mask[32 * w + 32] = 1.0;
        blur(&mut mask, w, h, 4.0);
        assert!(
            mask[32 * w + 32] > 0.0 && mask[32 * w + 32] < 1.0,
            "it spread"
        );
        assert!(mask[32 * w + 36] > 0.0, "neighbours got some");
        // The sliding sum accumulates float error, so "never negative" is a
        // tolerance rather than an equality. `Image::blend` ignores anything
        // <= 0 anyway, so a -1e-9 can never darken a pixel.
        assert!(mask.iter().all(|v| *v >= -1e-6), "went negative");
        // Mass is preserved to within the edge clamping (nothing near an edge
        // here), so this is a tight bound.
        let total: f32 = mask.iter().sum();
        assert!((total - 1.0).abs() < 0.01, "mass {total}");
    }

    /// A PNG survives the decode/encode round trip byte-for-byte in content.
    #[test]
    fn images_round_trip_through_the_codec() {
        let original = shot(7, 5);
        let decoded = decode(&encode(&original).unwrap()).unwrap();
        assert_eq!(decoded.width, 7);
        assert_eq!(decoded.height, 5);
        assert_eq!(decoded.pixels, original.pixels);
    }

    #[test]
    fn a_non_png_is_rejected_with_a_readable_error() {
        let err = match decode(b"this is not a png at all") {
            Err(err) => err,
            Ok(_) => panic!("garbage must not decode as a PNG"),
        };
        assert!(format!("{err:#}").contains("not a PNG"), "{err:#}");
    }
}

#[cfg(test)]
mod preview {
    use super::*;

    /// Not a test: a compositor preview for design iteration.
    /// `cargo test -p terra-cli preview -- --ignored` after putting a plain
    /// screenshot at /tmp/plain.png; writes /tmp/pretty-preview.png.
    #[test]
    #[ignore = "design preview, needs /tmp/plain.png"]
    fn composite_a_local_shot() {
        let bytes = std::fs::read("/tmp/plain.png").expect("no /tmp/plain.png");
        let shot = decode(&bytes).expect("decode");
        let out = compose(&shot, DEFAULT_BG);
        std::fs::write("/tmp/pretty-preview.png", encode(&out).expect("encode"))
            .expect("write");
    }
}
