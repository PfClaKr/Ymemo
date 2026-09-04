//! App and tray icons, drawn pixel by pixel: no decoder dependency and no file that has to
//! sit next to the executable.
//!
//! The picture is the same one every other platform shows — a dog-eared note on the app's
//! gold — and the geometry below is the same 108-unit viewport
//! `packaging/gen_icons.py` draws, so the tray, the taskbar, the `.desktop` entry, the
//! Windows `.ico` and the Android launcher are one icon. **Change one, change all three:**
//! this file, that script, and `apps/mobile/.../res/drawable/ic_launcher_foreground.xml`.
//!
//! The one difference is framing. Android hands its icon to a launcher that will mask it, so
//! the note there sits small and centred; nothing masks a tray or taskbar icon, so here the
//! note fills the badge — [`NOTE_SCALE`], the same factor the script's desktop pass uses.

use i_slint_backend_winit::WinitWindowAccessor;

/// The 22x22 tray icon as (rgba, width, height); the backend converts as needed.
pub(crate) fn tray_icon_rgba() -> (Vec<u8>, u32, u32) {
    note_icon_rgba(22)
}

const GOLD: [u8; 3] = [0xE6, 0xD2, 0x4A]; // background: the app's accent
const PAPER: [u8; 3] = [0xFF, 0xFC, 0xE3]; // note body: the yellow palette's paper
const EDGE: [u8; 3] = [0x8C, 0x7B, 0x1E]; // outline
const FOLD: [u8; 3] = [0xD8, 0xC6, 0x5C]; // the dog-eared corner
const RULE: [u8; 3] = [0xB8, 0xA6, 0x3A]; // the ruled lines

/// How much bigger the note is drawn than in the Android viewport, where a launcher mask
/// keeps it small.
const NOTE_SCALE: f32 = 1.44;

/// Subpixel samples per axis. The fold's diagonal and the rounded corners are unreadable
/// without them at 22px, which is the size that matters most.
const SS: usize = 4;

/// Draws the icon at `size`x`size` in RGBA (straight, not premultiplied).
///
/// Only the size varies, so the tray (22px) and window (64px) icons are the same picture.
pub(crate) fn note_icon_rgba(size: usize) -> (Vec<u8>, u32, u32) {
    let mut data = vec![0u8; size * size * 4]; // RGBA, transparent
    let unit = size as f32 / 108.0; // one viewport unit in pixels
    let samples = (SS * SS) as f32;
    for y in 0..size {
        for x in 0..size {
            let (mut sum, mut covered) = ([0f32; 3], 0f32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = (x as f32 + (sx as f32 + 0.5) / SS as f32) / unit;
                    let py = (y as f32 + (sy as f32 + 0.5) / SS as f32) / unit;
                    if let Some(c) = sample(px, py) {
                        for i in 0..3 {
                            sum[i] += c[i] as f32;
                        }
                        covered += 1.0;
                    }
                }
            }
            if covered == 0.0 {
                continue; // outside the badge's rounded corner
            }
            // Averaged over the covered samples only, so an edge pixel keeps its own colour
            // and carries the coverage in alpha instead of fading towards black.
            let i = (y * size + x) * 4;
            for c in 0..3 {
                data[i + c] = (sum[c] / covered).round() as u8;
            }
            data[i + 3] = (255.0 * covered / samples).round() as u8;
        }
    }
    (data, size as u32, size as u32)
}

/// The colour at one point of the 108-unit viewport, or `None` outside the badge.
fn sample(x: f32, y: f32) -> Option<[u8; 3]> {
    if !rounded_rect(x, y, 0.0, 0.0, 108.0, 108.0, 22.0) {
        return None;
    }
    // Back into the note's own coordinates, so every constant below is the one the vector
    // and the Python use; scaling the sample point beats scaling nine shapes.
    let nx = 54.0 + (x - 54.0) / NOTE_SCALE;
    let ny = 54.0 + (y - 54.0) / NOTE_SCALE;

    // The page: a rounded rectangle with its top-right corner cut away by the fold, which
    // is the line nx - ny = 36 through (63,27) and (77,41).
    let page = rounded_rect(nx, ny, 31.0, 27.0, 77.0, 81.0, 6.0) && nx - ny <= 36.0;
    if !page {
        return Some(GOLD);
    }
    // Inset by the stroke width to leave the outline behind; the diagonal insets along its
    // own normal, hence the 2.
    let inside = rounded_rect(nx, ny, 33.5, 29.5, 74.5, 78.5, 3.5)
        && nx - ny <= 36.0 - 2.5 * std::f32::consts::SQRT_2;
    if !inside {
        return Some(EDGE);
    }
    // The two straight sides of the dog ear, then its fill.
    let fold_edge = ((nx - 63.0).abs() <= 1.25 && (27.0..=41.0).contains(&ny))
        || ((ny - 41.0).abs() <= 1.25 && (63.0..=77.0).contains(&nx));
    if fold_edge {
        return Some(EDGE);
    }
    if nx >= 63.0 && ny <= 41.0 {
        return Some(FOLD);
    }
    // Three ruled lines, the last one short.
    for (top, right) in [(50.0, 69.0), (58.0, 69.0), (66.0, 57.0)] {
        if capsule(nx, ny, 40.5, top + 1.5, right - 1.5, top + 1.5, 1.5) {
            return Some(RULE);
        }
    }
    Some(PAPER)
}

/// Whether the point is inside the rounded rectangle `(x0,y0)-(x1,y1)` with radius `r`.
fn rounded_rect(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> bool {
    if px < x0 || px > x1 || py < y0 || py > y1 {
        return false;
    }
    // Nearest point of the inner rectangle the corner circles are centred on: inside it the
    // distance is zero, and only in the corners does it become a circle test.
    let cx = px.clamp(x0 + r, x1 - r);
    let cy = py.clamp(y0 + r, y1 - r);
    (px - cx).powi(2) + (py - cy).powi(2) <= r * r
}

/// Whether the point is within `r` of the segment `(ax,ay)-(bx,by)`: a line with round caps.
fn capsule(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32, r: f32) -> bool {
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
    };
    (px - (ax + t * dx)).powi(2) + (py - (ay + t * dy)).powi(2) <= r * r
}

/// Sets the app icon on a winit window (X11 `_NET_WM_ICON`, Windows window icon), which
/// feeds the taskbar and alt-tab. It only applies while the event loop has the window up,
/// and is silently ignored otherwise — native Wayland has no window-icon protocol, the same
/// limitation as snapping.
pub(crate) fn set_window_icon(win: &slint::Window) {
    let (rgba, w, h) = note_icon_rgba(64);
    let Ok(icon) = i_slint_backend_winit::winit::window::Icon::from_rgba(rgba, w, h) else {
        return;
    };
    win.with_winit_window(|ww| ww.set_window_icon(Some(icon)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_icon_buffer_is_well_formed() {
        // winit::Icon::from_rgba requires len == 4*w*h at every size.
        for size in [16usize, 22, 32, 64] {
            let (rgba, w, h) = note_icon_rgba(size);
            assert_eq!(w, size as u32);
            assert_eq!(h, size as u32);
            assert_eq!(rgba.len(), size * size * 4);
            // The badge is there at every size...
            let opaque = |c: [u8; 3]| [c[0], c[1], c[2], 0xff];
            assert!(
                rgba.chunks(4).any(|p| p == opaque(GOLD)),
                "no background at {size}px"
            );
            // ...and so is the note, which is what a blank icon would be missing.
            assert!(
                rgba.chunks(4).any(|p| p == opaque(PAPER)),
                "no note at {size}px"
            );
        }
    }

    #[test]
    fn note_icon_corners_are_transparent() {
        // The badge is a rounded square, so the very corner is outside it. A fully opaque
        // corner would mean the rounding was lost and the icon is a hard square.
        let (rgba, size, _) = note_icon_rgba(64);
        let size = size as usize;
        for (x, y) in [(0, 0), (size - 1, 0), (0, size - 1), (size - 1, size - 1)] {
            assert_eq!(rgba[(y * size + x) * 4 + 3], 0, "corner ({x},{y}) is opaque");
        }
    }
}

