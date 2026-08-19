//! App and tray icons, drawn pixel by pixel: no decoder dependency and no file that has to
//! sit next to the executable.

use i_slint_backend_winit::WinitWindowAccessor;

/// The 22x22 tray icon as (rgba, width, height); the backend converts as needed.
pub(crate) fn tray_icon_rgba() -> (Vec<u8>, u32, u32) {
    note_icon_rgba(22)
}

/// Draws a dog-eared note icon at `size`x`size` in RGBA. Only the size varies, so the tray
/// (22px) and window (64px) icons are the same picture; margin, border and fold scale with
/// it.
pub(crate) fn note_icon_rgba(size: usize) -> (Vec<u8>, u32, u32) {
    let s = size;
    let m = (size / 10).max(2); // ~10% margin
    let f = (size * 7 / 22).max(7); // folded top-right corner (7 at 22px)
    let border = (size / 22).max(1); // border and fold thickness
    let mut data = vec![0u8; s * s * 4]; // RGBA, transparent
    for y in 0..s {
        for x in 0..s {
            let in_note = (m..s - m).contains(&x) && (m..s - m).contains(&y);
            if !in_note {
                continue;
            }
            let from_right = s - m - 1 - x;
            let from_top = y - m;
            let (r, g, b) = if from_right + from_top < f {
                if from_right + from_top >= f - border {
                    (0xb8u8, 0xa6u8, 0x3au8) // the fold's diagonal
                } else {
                    continue; // outside the triangle: transparent
                }
            } else if x < m + border || x >= s - m - border || y < m + border || y >= s - m - border
            {
                (0xb8, 0xa6, 0x3a) // border
            } else {
                (0xf7, 0xe9, 0x8c) // note body
            };
            let i = (y * s + x) * 4;
            data[i] = r;
            data[i + 1] = g;
            data[i + 2] = b;
            data[i + 3] = 0xff; // A
        }
    }
    (data, s as u32, s as u32)
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
            // At least one body pixel, i.e. not a blank icon.
            assert!(rgba.chunks(4).any(|p| p == [0xf7, 0xe9, 0x8c, 0xff]));
        }
    }
}
