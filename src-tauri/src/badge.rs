//! Unread-count badge for the tray icon and the Windows taskbar overlay.
//!
//! Renders a soft-red circle with white digits ("1"-"99", or "99+" when
//! the count exceeds 99). Three visual tricks let us ship a "modern"
//! looking badge:
//!
//! 1. **Alpha-blended red.** The badge fill is Tailwind red-500 at ~90%
//!    alpha (`230/255`) instead of opaque red-600, composited via
//!    proper src-over-dst blending. The underlying tray icon shows
//!    faintly through the badge — it reads as a translucent overlay
//!    rather than a flat sticker.
//!
//! 2. **2×2 supersampled circle edge.** For each pixel along the
//!    badge's circumference we sample 4 sub-pixel positions; the
//!    fraction inside the circle becomes the pixel's coverage. Smooth
//!    edge with no FFT-grade AA stack.
//!
//! 3. **Vector glyph rasterization via `ab_glyph`** with an embedded
//!    DejaVu Sans Bold TTF. Replaces the previous 5×7 bitmap font
//!    that looked stair-stepped on high-DPI taskbar / dock icons.
//!    `ab_glyph` does proper greyscale coverage AA, so the digits
//!    have soft anti-aliased edges at every badge size.
//!
//! The tray icon is composited from the base PNG plus the badge in the
//! bottom-right corner. The Windows taskbar overlay is the badge alone
//! at 16x16, sized for `WebviewWindow::set_overlay_icon`.

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use std::sync::OnceLock;
use tauri::image::Image;

/// Embedded font used to rasterize the badge digits. DejaVu Sans Bold
/// is BSD-licensed and bundled in `src-tauri/assets/`; the license
/// text rides alongside it (see `assets/badge_font_LICENSE.txt`) so
/// the binary remains redistributable.
const FONT_BYTES: &[u8] = include_bytes!("../assets/badge_font.ttf");

/// Parsed font handle, computed once and cached. `FontRef` borrows
/// from `FONT_BYTES`, which is `&'static`, so this can live in a
/// `OnceLock` without lifetime gymnastics.
fn font() -> &'static FontRef<'static> {
    static FONT: OnceLock<FontRef<'static>> = OnceLock::new();
    FONT.get_or_init(|| {
        FontRef::try_from_slice(FONT_BYTES)
            .expect("embedded badge font must parse — check assets/badge_font.ttf")
    })
}

/// "1".."99" verbatim, anything > 99 collapses to "99+".
fn format_label(unread: u32) -> String {
    if unread > 99 {
        "99+".to_string()
    } else {
        unread.to_string()
    }
}

/// Tailwind red-500 with ~90% alpha. Softer than the previous opaque
/// red-600 and lets the icon underneath read faintly through the
/// badge — closer to the macOS / Windows 11 dock-badge feel.
const BADGE_RGBA: [u8; 4] = [239, 68, 68, 230];
const TEXT_RGBA: [u8; 4] = [255, 255, 255, 255];

/// Composite a badge onto a copy of `base_pixels` and return it as an
/// owned Tauri image. When `unread == 0` we return the base image
/// unchanged so the tray relaxes to the plain icon.
pub fn render_tray_icon(
    base_pixels: &[u8],
    width: u32,
    height: u32,
    unread: u32,
) -> Image<'static> {
    let pixels = if unread == 0 {
        base_pixels.to_vec()
    } else {
        let mut p = base_pixels.to_vec();
        let label = format_label(unread);
        let dim = width.min(height);
        // Half the icon's short side, but never below 12 px — at smaller
        // sizes the badge needs to dominate to remain legible.
        let badge_size = (dim / 2).max(12).min(dim);
        let bx = width - badge_size;
        let by = height - badge_size;
        draw_filled_circle(&mut p, width, height, bx, by, badge_size, BADGE_RGBA);
        stamp_label(&mut p, width, height, bx, by, badge_size, &label);
        p
    };
    Image::new_owned(pixels, width, height)
}

/// Standalone badge sized for a Windows taskbar overlay (16x16). Returns
/// `None` when there's nothing to show so the caller can clear the
/// overlay with `set_overlay_icon(None)`.
///
/// Called from the `#[cfg(windows)]` branch in `main.rs`; on other
/// platforms the call site is compiled away, hence the lint suppression.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn render_taskbar_overlay(unread: u32) -> Option<Image<'static>> {
    if unread == 0 {
        return None;
    }
    const W: u32 = 16;
    const H: u32 = 16;
    let label = format_label(unread);
    let mut pixels = vec![0u8; (W * H * 4) as usize];
    draw_filled_circle(&mut pixels, W, H, 0, 0, W, BADGE_RGBA);
    stamp_label(&mut pixels, W, H, 0, 0, W, &label);
    Some(Image::new_owned(pixels, W, H))
}

/// Standard "src over dst" alpha compositing for one RGBA pixel.
///
/// `coverage` (0..=255) scales the source's alpha — used by the
/// supersampled circle to express partial pixel coverage at the badge
/// edge. coverage=255 means "full pixel inside the shape", coverage=0
/// means "fully outside" (early-exit).
fn blend_pixel(dst: &mut [u8], src: [u8; 4], coverage: u32) {
    let src_a = src[3] as u32 * coverage / 255;
    if src_a == 0 {
        return;
    }
    let inv = 255 - src_a;
    dst[0] = ((src[0] as u32 * src_a + dst[0] as u32 * inv) / 255) as u8;
    dst[1] = ((src[1] as u32 * src_a + dst[1] as u32 * inv) / 255) as u8;
    dst[2] = ((src[2] as u32 * src_a + dst[2] as u32 * inv) / 255) as u8;
    let dst_a = dst[3] as u32;
    dst[3] = (src_a + dst_a * inv / 255).min(255) as u8;
}

/// Filled circle with a 2×2 supersampled edge. For each output pixel
/// we sample 4 sub-pixel positions on a half-pixel grid; the fraction
/// of samples inside the circle becomes the pixel's coverage. This
/// keeps the centre at full opacity and lets the boundary fade to 0
/// over a one-pixel band — visibly smoother than the old binary
/// "inside vs outside" test, with no perceivable softness at the
/// badge sizes we render.
fn draw_filled_circle(
    pixels: &mut [u8],
    img_w: u32,
    img_h: u32,
    x: u32,
    y: u32,
    size: u32,
    color: [u8; 4],
) {
    // Centre + squared radius in 4×-precision integer space so we never
    // hit floats. Each unit step in absolute pixel coordinates is 4
    // units in this space (one half-pixel sub-sample step is 2 units),
    // so a radius of `size/2` becomes `size*2`.
    let cx = x as i32 * 4 + size as i32 * 2;
    let cy = y as i32 * 4 + size as i32 * 2;
    let r = size as i32 * 2;
    let r2 = r * r;
    // 4×-space offsets for the four sub-samples within one pixel —
    // (1,1), (3,1), (1,3), (3,3). Centred on the pixel's quadrants.
    const SUBPIXELS: [(i32, i32); 4] = [(1, 1), (3, 1), (1, 3), (3, 3)];

    for py in 0..size {
        let abs_y = y + py;
        if abs_y >= img_h {
            break;
        }
        for px in 0..size {
            let abs_x = x + px;
            if abs_x >= img_w {
                break;
            }
            let base_x = abs_x as i32 * 4;
            let base_y = abs_y as i32 * 4;
            let mut hits = 0u32;
            for (sx, sy) in SUBPIXELS {
                let dx = base_x + sx - cx;
                let dy = base_y + sy - cy;
                if dx * dx + dy * dy <= r2 {
                    hits += 1;
                }
            }
            if hits == 0 {
                continue;
            }
            let coverage = hits * 255 / 4;
            let idx = ((abs_y * img_w + abs_x) * 4) as usize;
            blend_pixel(&mut pixels[idx..idx + 4], color, coverage);
        }
    }
}

/// Rasterize the label as antialiased vector glyphs and centre it
/// inside the badge box.
///
/// Pipeline:
///   1. Pick a `PxScale` that makes the label fit inside ~70% of the
///      badge's diameter (matches the old bitmap layout's breathing
///      room). We binary-search rather than guess so the chosen size
///      is the largest that still fits in both dimensions.
///   2. Lay the glyphs out side-by-side using `ab_glyph`'s advance
///      widths so digits with different widths (e.g. "1" vs "8")
///      kern naturally instead of sitting on a fixed grid.
///   3. For each glyph, draw its coverage bitmap via
///      `outline.draw(|x, y, c|)`. `c` is the per-pixel coverage in
///      [0.0, 1.0] — feed it straight into `blend_pixel` as a
///      0..=255 alpha multiplier. White text composites cleanly on
///      top of the soft-red disc, and partial-coverage pixels at
///      glyph edges produce the AA the bitmap font couldn't.
fn stamp_label(
    pixels: &mut [u8],
    img_w: u32,
    img_h: u32,
    bx: u32,
    by: u32,
    size: u32,
    label: &str,
) {
    if label.is_empty() {
        return;
    }
    let font = font();
    // The badge content area — what the label has to fit inside.
    let max_w = (size as f32) * 0.72;
    let max_h = (size as f32) * 0.72;

    // Find the largest pixel size at which the label still fits both
    // dimensions. ab_glyph reports h_advance / ascent / descent in
    // px-scale-relative units, so we just probe candidate scales.
    // The cap (`size * 1.0`) is a sanity bound — at badge sizes 12-32
    // px, the label never wants more than that.
    let mut chosen_scale = 1.0_f32;
    let mut best_layout: Option<LabelLayout> = None;
    let mut probe = (size as f32).clamp(8.0, 64.0);
    while probe >= 4.0 {
        let layout = layout_label(font, label, probe);
        if layout.width <= max_w && layout.height <= max_h {
            chosen_scale = probe;
            best_layout = Some(layout);
            break;
        }
        probe -= 1.0;
    }
    let layout = match best_layout {
        Some(l) => l,
        // If nothing fit (extremely tiny badge), fall back to the
        // smallest we tried — better cramped digits than no digits.
        None => layout_label(font, label, chosen_scale),
    };

    // Centre the rendered label inside the badge box. `ascent` is the
    // distance from the baseline to the top of the tallest glyph;
    // `descent` is negative. Vertical centre = box centre - bbox/2 +
    // ascent (so the baseline lands where it needs to).
    let scaled = font.as_scaled(PxScale::from(chosen_scale));
    let ascent = scaled.ascent();
    let descent = scaled.descent();
    let bbox_h = ascent - descent;
    let start_x = bx as f32 + (size as f32 - layout.width) / 2.0;
    let baseline_y = by as f32 + (size as f32 - bbox_h) / 2.0 + ascent;

    let mut cursor_x = start_x;
    for c in label.chars() {
        let glyph_id = font.glyph_id(c);
        let glyph = glyph_id.with_scale_and_position(
            PxScale::from(chosen_scale),
            ab_glyph::point(cursor_x, baseline_y),
        );
        if let Some(outline) = font.outline_glyph(glyph) {
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, coverage| {
                let px = bounds.min.x as i32 + gx as i32;
                let py = bounds.min.y as i32 + gy as i32;
                if px < 0 || py < 0 || (px as u32) >= img_w || (py as u32) >= img_h {
                    return;
                }
                let alpha = (coverage.clamp(0.0, 1.0) * 255.0) as u32;
                if alpha == 0 {
                    return;
                }
                let idx = ((py as u32 * img_w + px as u32) * 4) as usize;
                blend_pixel(&mut pixels[idx..idx + 4], TEXT_RGBA, alpha);
            });
        }
        cursor_x += scaled.h_advance(glyph_id);
    }
}

/// Result of measuring a label at a candidate pixel size — total
/// advance width and the rendered bbox height (ascent + |descent|).
struct LabelLayout {
    width: f32,
    height: f32,
}

fn layout_label(font: &FontRef<'_>, label: &str, scale: f32) -> LabelLayout {
    let scaled = font.as_scaled(PxScale::from(scale));
    let width: f32 = label
        .chars()
        .map(|c| scaled.h_advance(font.glyph_id(c)))
        .sum();
    let height = scaled.ascent() - scaled.descent();
    LabelLayout { width, height }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_formats() {
        assert_eq!(format_label(0), "0");
        assert_eq!(format_label(1), "1");
        assert_eq!(format_label(99), "99");
        assert_eq!(format_label(100), "99+");
        assert_eq!(format_label(9999), "99+");
    }

    #[test]
    fn zero_unread_returns_unchanged_pixels() {
        let base = vec![10u8; 32 * 32 * 4];
        let img = render_tray_icon(&base, 32, 32, 0);
        assert_eq!(img.rgba(), base.as_slice());
    }

    #[test]
    fn nonzero_unread_paints_reddish() {
        let base = vec![0u8; 32 * 32 * 4];
        let img = render_tray_icon(&base, 32, 32, 5);
        // Alpha-blending red onto transparent black no longer produces
        // exactly (239, 68, 68) — the source alpha (230) scales each
        // channel down. We just want to see "this pixel is dominated
        // by red" somewhere in the badge area.
        let pixels = img.rgba();
        let mut found_red = false;
        for y in 16..32 {
            for x in 16..32 {
                let idx = (y * 32 + x) * 4;
                let (r, g, b, a) = (pixels[idx], pixels[idx + 1], pixels[idx + 2], pixels[idx + 3]);
                if r > 150 && r > g + 50 && r > b + 50 && a > 0 {
                    found_red = true;
                    break;
                }
            }
        }
        assert!(found_red, "expected at least one red badge pixel");
    }

    #[test]
    fn taskbar_overlay_none_when_zero() {
        assert!(render_taskbar_overlay(0).is_none());
    }

    #[test]
    fn taskbar_overlay_some_when_nonzero() {
        let img = render_taskbar_overlay(7).expect("expected overlay");
        assert_eq!(img.width(), 16);
        assert_eq!(img.height(), 16);
    }

    #[test]
    fn circle_edge_is_antialiased() {
        // Render a standalone badge onto a transparent canvas and look
        // for at least one partially-covered (non-zero, non-fully-opaque)
        // pixel — proof the supersampled edge is producing intermediate
        // alpha values, not a binary inside/outside mask.
        let img = render_taskbar_overlay(1).expect("expected overlay");
        let pixels = img.rgba();
        let has_partial = pixels
            .chunks_exact(4)
            .any(|p| (1..230).contains(&p[3]));
        assert!(has_partial, "expected at least one partially-covered AA edge pixel");
    }
}
