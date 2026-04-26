//! Unread-count badge for the tray icon and the Windows taskbar overlay.
//!
//! Renders a red circle with white digits ("1"-"99", or "99+" when the
//! count exceeds 99). Hand-rolled 3x5 bitmap font for the glyphs so we
//! ship no font file and pull in no font crate — the digits are tiny
//! and a vector font would alias badly at this scale anyway.
//!
//! The tray icon is composited from the base PNG plus the badge in the
//! bottom-right corner. The Windows taskbar overlay is the badge alone
//! at 16x16, sized for `WebviewWindow::set_overlay_icon`.

use tauri::image::Image;

const GLYPH_W: usize = 3;
const GLYPH_H: usize = 5;
type Glyph = [u8; GLYPH_W * GLYPH_H];

// Each glyph: 1 = ink, 0 = transparent, row-major 3x5.
#[rustfmt::skip]
const GLYPHS: &[(char, Glyph)] = &[
    ('0', [
        1,1,1,
        1,0,1,
        1,0,1,
        1,0,1,
        1,1,1,
    ]),
    ('1', [
        0,1,0,
        1,1,0,
        0,1,0,
        0,1,0,
        1,1,1,
    ]),
    ('2', [
        1,1,1,
        0,0,1,
        1,1,1,
        1,0,0,
        1,1,1,
    ]),
    ('3', [
        1,1,1,
        0,0,1,
        0,1,1,
        0,0,1,
        1,1,1,
    ]),
    ('4', [
        1,0,1,
        1,0,1,
        1,1,1,
        0,0,1,
        0,0,1,
    ]),
    ('5', [
        1,1,1,
        1,0,0,
        1,1,1,
        0,0,1,
        1,1,1,
    ]),
    ('6', [
        1,1,1,
        1,0,0,
        1,1,1,
        1,0,1,
        1,1,1,
    ]),
    ('7', [
        1,1,1,
        0,0,1,
        0,0,1,
        0,0,1,
        0,0,1,
    ]),
    ('8', [
        1,1,1,
        1,0,1,
        1,1,1,
        1,0,1,
        1,1,1,
    ]),
    ('9', [
        1,1,1,
        1,0,1,
        1,1,1,
        0,0,1,
        1,1,1,
    ]),
    ('+', [
        0,0,0,
        0,1,0,
        1,1,1,
        0,1,0,
        0,0,0,
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

const BADGE_RGBA: [u8; 4] = [220, 38, 38, 255]; // tailwind red-600
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

/// Solid filled circle. Square box `size x size`, centered inside that
/// box, no anti-aliasing — at tray-icon scale a hard edge reads better
/// than a smudgy AA edge.
fn draw_filled_circle(
    pixels: &mut [u8],
    img_w: u32,
    img_h: u32,
    x: u32,
    y: u32,
    size: u32,
    color: [u8; 4],
) {
    let cx = x as i32 * 2 + size as i32; // 2*center, keeps integer math
    let cy = y as i32 * 2 + size as i32;
    let r2_4 = (size as i32) * (size as i32); // (size/2)^2 * 4 == size^2
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
            let dx = (abs_x as i32) * 2 + 1 - cx;
            let dy = (abs_y as i32) * 2 + 1 - cy;
            if dx * dx + dy * dy <= r2_4 {
                let idx = ((abs_y * img_w + abs_x) * 4) as usize;
                pixels[idx..idx + 4].copy_from_slice(&color);
            }
        }
    }
}

/// Center the label horizontally and vertically inside the badge box.
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
                            pixels[idx..idx + 4].copy_from_slice(&TEXT_RGBA);
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
    fn nonzero_unread_paints_red() {
        let base = vec![0u8; 32 * 32 * 4];
        let img = render_tray_icon(&base, 32, 32, 5);
        // At least one pixel in the bottom-right quadrant should now be red.
        let pixels = img.rgba();
        let mut found_red = false;
        for y in 16..32 {
            for x in 16..32 {
                let idx = (y * 32 + x) * 4;
                if pixels[idx] == 220 && pixels[idx + 1] == 38 && pixels[idx + 2] == 38 {
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
}
