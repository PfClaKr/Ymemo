//! App and tray icons: two sticky notes, gold in front and blue behind.
//!
//! The pictures are the ones `packaging/gen_icons.py` renders into `packaging/assets/`,
//! **embedded** in the binary rather than drawn here. The icon used to be drawn pixel by pixel
//! in this file, a third copy of the geometry beside the script and the Android vector that
//! had to be kept in step by hand; the notes are turned and their corner peeled, and one
//! renderer for all of it is the only way the tray, the taskbar, the `.desktop` entry, the
//! `.ico` and the launcher stay one icon. Change the picture in the script (and the Android
//! vector it names), run it, and this follows.
//!
//! Each size is the script's own rendering of that size, not a scale of another: at 22px in
//! a tray every pixel of the peel and the writing was placed for that size.

use i_slint_backend_winit::WinitWindowAccessor;

const ICON_22: &[u8] = include_bytes!("../../../packaging/assets/ymemo-22.png");
const ICON_64: &[u8] = include_bytes!("../../../packaging/assets/ymemo-64.png");

/// The 22x22 tray icon as (rgba, width, height); the backend converts as needed.
pub(crate) fn tray_icon_rgba() -> (Vec<u8>, u32, u32) {
    decode(ICON_22)
}

/// Straight (not premultiplied) RGBA, which is what both winit and the tray backends take.
fn decode(png: &[u8]) -> (Vec<u8>, u32, u32) {
    match image::load_from_memory_with_format(png, image::ImageFormat::Png) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            (rgba.into_raw(), w, h)
        }
        // Built in, so this cannot fail on a build that got this far; a blank icon beats a
        // panic in the one code path every window goes through.
        Err(_) => (vec![0; 4], 1, 1),
    }
}

/// Sets the app icon on a winit window (X11 `_NET_WM_ICON`, Windows window icon), which
/// feeds the taskbar and alt-tab. It only applies while the event loop has the window up,
/// and is silently ignored otherwise — native Wayland has no window-icon protocol, the same
/// limitation as snapping.
pub(crate) fn set_window_icon(win: &slint::Window) {
    let (rgba, w, h) = decode(ICON_64);
    let Ok(icon) = i_slint_backend_winit::winit::window::Icon::from_rgba(rgba, w, h) else {
        return;
    };
    win.with_winit_window(|ww| ww.set_window_icon(Some(icon)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_icons_decode_to_what_winit_takes() {
        // winit::Icon::from_rgba requires len == 4*w*h.
        for (png, size) in [(ICON_22, 22u32), (ICON_64, 64)] {
            let (rgba, w, h) = decode(png);
            assert_eq!((w, h), (size, size));
            assert_eq!(rgba.len(), (size * size * 4) as usize);
            // Both notes are there: some gold and some blue, which a blank or a wrong file
            // would be missing.
            let has = |f: fn(&[u8]) -> bool| rgba.chunks(4).any(|p| p[3] > 200 && f(p));
            assert!(has(|p| p[0] > 200 && p[1] > 160 && p[2] < 140), "no gold at {size}px");
            assert!(has(|p| p[2] > 200 && p[0] < 120), "no blue at {size}px");
            // And it stands on nothing: the corner is transparent.
            assert_eq!(rgba[3], 0, "corner is opaque at {size}px");
        }
    }
}
