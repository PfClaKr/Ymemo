//! The Windows half of [`super::skip_taskbar`] that winit has no API for.

use i_slint_backend_winit::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use i_slint_backend_winit::winit::window::Window;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, HWND_TOP,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, WS_EX_APPWINDOW,
    WS_EX_TOOLWINDOW,
};

fn hwnd(window: &Window) -> Option<*mut core::ffi::c_void> {
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::Win32(win32) = handle.as_raw() else { return None };
    Some(win32.hwnd.get() as *mut core::ffi::c_void)
}

/// See [`super::restack_top`]. `HWND_TOP` is the top of the window's own band, so a
/// topmost (pinned) note stays topmost and an ordinary one stays under the pinned ones.
pub(super) fn bring_to_top(window: &Window) {
    let Some(hwnd) = hwnd(window) else { return };
    // SAFETY: the handle comes from the winit window we are holding, live for the call.
    unsafe {
        SetWindowPos(
            hwnd,
            HWND_TOP,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        );
    }
}

/// Marks a window as a tool window, which is the shell's own idea of "not an application":
/// no taskbar button, and no place in Alt+Tab either — the same statement.
///
/// **Both halves are needed.** The shell's rule is that a window gets a button if it has
/// `WS_EX_APPWINDOW`, *or* it is top-level and unowned without `WS_EX_TOOLWINDOW` — so
/// `WS_EX_APPWINDOW` wins over the tool-window style, and winit puts it on every window it
/// creates. Adding one flag without clearing the other leaves the button exactly where it
/// was, which is measurable: the sticky came back reading `exstyle=0x00040190`, tool window
/// and app window at once.
///
/// Safe to call on a window already in this state — the read-modify-write leaves it as it
/// was, and every `present` comes back through here.
pub(super) fn tool_window(window: &Window) {
    let Some(hwnd) = hwnd(window) else { return };
    // SAFETY: the handle comes from the winit window we are holding, so it is live for
    // the length of this call, and GWL_EXSTYLE is an isize on every supported target.
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let wanted = (style | WS_EX_TOOLWINDOW as isize) & !(WS_EX_APPWINDOW as isize);
        if wanted != style {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, wanted);
        }
    }
}

/// Colours a window's title bar (Windows 11; ignored by Windows 10, which keeps the dark bar
/// winit's theme gave it). See [`super::dark_title_bar`].
pub(super) fn caption_color(window: &Window, (r, g, b): (u8, u8, u8)) {
    use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CAPTION_COLOR};
    let Some(hwnd) = hwnd(window) else { return };
    // COLORREF is 0x00BBGGRR.
    let color: u32 = u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16);
    // SAFETY: the handle comes from the winit window we are holding, live for the call, and
    // the attribute is a 4-byte COLORREF read from a local that outlives it.
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_CAPTION_COLOR as u32,
            &color as *const u32 as *const core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
    }
}
