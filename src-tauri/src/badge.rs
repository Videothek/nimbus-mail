//! Unread-count badge for the tray icon and the Windows taskbar overlay.
//!
//! Renders a soft-red circle with white digits ("1"-"99", or "99+" when
//! the count exceeds 99). Two visual tricks let us ship a "modern"
//! looking badge without pulling in a font crate:
//!
//! 1. **Alpha-blended red.** The badge fill is Tailwind red-500 at ~90%
//!    alpha (`230/255`) instead of opaque red-600, composited via
//!    proper src-over-dst blending. The underlying tray icon shows
//!    faintly through the badge — it reads as a translucent overlay
//!    rather than a flat sticker.
//!
//! 2. **2×2 supersampled circle edge.** For each pixel along the
//!    badge's circumference we sample 4 sub-pixel positions; the
//!    fraction inside the circle becomes the pixel's coverage. The
//!    edge is smooth without an FFT-grade AA stack.
//!
//! Glyphs are a hand-rolled 5×7 bitmap (digits + "+"). 5×7 has enough
//! shape vocabulary to give each digit proper proportions — a real
//! waist on the 8, a hooked top on the 4 — instead of the blocky
//! silhouettes the previous 3×5 grid produced.
//!
//! The tray icon is composited from the base PNG plus the badge in the
//! bottom-right corner. The Windows taskbar overlay is the badge alone
//! at 16x16, sized for `WebviewWindow::set_overlay_icon`.

use tauri::image::Image;

const GLYPH_W: usize = 5;
const GLYPH_H: usize = 7;
type Glyph = [u8; GLYPH_W * GLYPH_H];

// Each glyph: 1 = ink, 0 = transparent, row-major 5×7.
#[rustfmt::skip]
const GLYPHS: &[(char, Glyph)] = &[
    ('0', [
        0,1,1,1,0,
        1,0,0,0,1,
        1,0,0,0,1,
        1,0,0,0,1,
        1,0,0,0,1,
        1,0,0,0,1,
        0,1,1,1,0,
    ]),
    ('1', [
        0,0,1,0,0,
        0,1,1,0,0,
        0,0,1,0,0,
        0,0,1,0,0,
        0,0,1,0,0,
        0,0,1,0,0,
        0,1,1,1,0,
    ]),
    ('2', [
        0,1,1,1,0,
        1,0,0,0,1,
        0,0,0,0,1,
        0,0,0,1,0,
        0,0,1,0,0,
        0,1,0,0,0,
        1,1,1,1,1,
    ]),
    ('3', [
        1,1,1,1,1,
        0,0,0,0,1,
        0,0,0,1,0,
        0,0,1,1,0,
        0,0,0,0,1,
        1,0,0,0,1,
        0,1,1,1,0,
    ]),
    ('4', [
        0,0,0,1,0,
        0,0,1,1,0,
        0,1,0,1,0,
        1,0,0,1,0,
        1,1,1,1,1,
        0,0,0,1,0,
        0,0,0,1,0,
    ]),
    ('5', [
        1,1,1,1,1,
        1,0,0,0,0,
        1,1,1,1,0,
        0,0,0,0,1,
        0,0,0,0,1,
        1,0,0,0,1,
        0,1,1,1,0,
    ]),
    ('6', [
        0,1,1,1,0,
        1,0,0,0,0,
        1,0,0,0,0,
        1,1,1,1,0,
        1,0,0,0,1,
        1,0,0,0,1,
        0,1,1,1,0,
    ]),
    ('7', [
        1,1,1,1,1,
        0,0,0,0,1,
        0,0,0,1,0,
        0,0,1,0,0,
        0,1,0,0,0,
        0,1,0,0,0,
        0,1,0,0,0,
    ]),
    ('8', [
        0,1,1,1,0,
        1,0,0,0,1,
        1,0,0,0,1,
        0,1,1,1,0,
        1,0,0,0,1,
        1,0,0,0,1,
        0,1,1,1,0,
    ]),
    ('9', [
        0,1,1,1,0,
        1,0,0,0,1,
        1,0,0,0,1,
        0,1,1,1,1,
        0,0,0,0,1,
        0,0,0,0,1,
        0,1,1,1,0,
    ]),
    ('+', [
        0,0,0,0,0,
        0,0,1,0,0,
        0,0,1,0,0,
        1,1,1,1,1,
        0,0,1,0,0,
        0,0,1,0,0,
        0,0,0,0,0,
    ]),
];

fn glyph(c: char) -> Option<&'static Glyph> {
    GLYPHS.iter().find(|(g, _)| *g == c).map(|(_, bits)| bits)
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

/// Center the label horizontally and vertically inside the badge box.
///
/// Glyph pixels are stamped via `blend_pixel` (coverage=255) so the
/// white digits composite cleanly on top of the soft-red disc instead
/// of overwriting it, preserving any sub-pixel coverage from the
/// circle's AA edge.
fn stamp_label(
    pixels: &mut [u8],
    img_w: u32,
    img_h: u32,
    bx: u32,
    by: u32,
    size: u32,
    label: &str,
) {
    let chars: Vec<char> = label.chars().collect();
    if chars.is_empty() {
        return;
    }
    let count = chars.len() as u32;

    // Pick the largest integer scale where the rendered label still
    // fits inside ~70% of the badge. Below that the digits crowd the
    // edge of the circle and look cramped.
    let max_w = (size * 7) / 10;
    let max_h = (size * 7) / 10;
    let label_unit_w = GLYPH_W as u32 * count + count.saturating_sub(1); // glyphs + 1px gaps
    let label_unit_h = GLYPH_H as u32;
    let scale_x = (max_w / label_unit_w).max(1);
    let scale_y = (max_h / label_unit_h).max(1);
    let scale = scale_x.min(scale_y);

    let digit_w = GLYPH_W as u32 * scale;
    let digit_h = GLYPH_H as u32 * scale;
    let gap = scale;
    let total_w = digit_w * count + gap * count.saturating_sub(1);

    let start_x = bx + size.saturating_sub(total_w) / 2;
    let start_y = by + size.saturating_sub(digit_h) / 2;

    for (i, c) in chars.iter().enumerate() {
        let Some(g) = glyph(*c) else { continue };
        let gx = start_x + (digit_w + gap) * i as u32;
        for row in 0..GLYPH_H {
            for col in 0..GLYPH_W {
                if g[row * GLYPH_W + col] == 0 {
                    continue;
                }
                let px0 = gx + col as u32 * scale;
                let py0 = start_y + row as u32 * scale;
                for sy in 0..scale {
                    for sx in 0..scale {
                        let px = px0 + sx;
                        let py = py0 + sy;
                        if px < img_w && py < img_h {
                            let idx = ((py * img_w + px) * 4) as usize;
                            blend_pixel(&mut pixels[idx..idx + 4], TEXT_RGBA, 255);
                        }
                    }
                }
            }
        }
    }
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
