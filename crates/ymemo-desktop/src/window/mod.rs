//! Showing a window so that it is actually painted.
//!
//! Hiding a Slint window on Windows does not destroy it — the winit backend only calls
//! `set_visible(false)` — and that is where the trouble starts. Showing it again presents
//! the surface as it stands, and the **software renderer**, which is what Windows gets by
//! default (see `select_renderer`), has no damage recorded against that surface. So it
//! presents an empty buffer: the window comes back **white and stays white**, until
//! something unrelated finally marks it dirty.
//!
//! `request_redraw()` does not rescue it, on either turn of the event loop; the redraw
//! happens and draws nothing. A **resize** is what the renderer treats as damage to all of
//! it, so [`present`] grows the window a pixel and puts it back. That has to span two turns
//! of the loop: both sizes applied in one turn cancel out and no resize is ever delivered.
//!
//! Verified on Windows against both renderers — white with `software`, correct with
//! `femtovg`, and correct with `software` once the resize is in.

use std::time::Duration;

use slint::{ComponentHandle, LogicalSize};
use ymemo_core::diag;

use crate::icon::set_window_icon;

/// How long to leave the window a pixel taller. One turn of the loop is enough for the
/// resize to reach the renderer; this is simply a short wait that is certain to be one.
const NUDGE: Duration = Duration::from_millis(32);

/// Shows `component`, gives it the app icon and makes sure it gets painted.
pub(crate) fn present<T: ComponentHandle + 'static>(component: &T) {
    let _ = component.show();
    set_window_icon(component.window());
    component.window().request_redraw();
    dark_title_bar(component);

    let weak = component.as_weak();
    slint::Timer::single_shot(Duration::ZERO, move || {
        let Some(c) = weak.upgrade() else { return };
        let window = c.window();
        let scale = window.scale_factor();
        let original = window.size().to_logical(scale);
        window.set_size(LogicalSize::new(original.width, original.height + 1.0));

        let weak = c.as_weak();
        slint::Timer::single_shot(NUDGE, move || {
            let Some(c) = weak.upgrade() else { return };
            let window = c.window();
            // Only undo our own pixel. Folding a sticky resizes the window from another
            // callback, and restoring a stale height would undo that instead.
            let now = window.size().to_logical(window.scale_factor());
            if (now.height - (original.height + 1.0)).abs() < 0.5 {
                window.set_size(LogicalSize::new(original.width, original.height));
            }
            window.request_redraw();
        });
    });
}

/// The system's title bar in dark, over the windows that are always dark.
///
/// Windows draws the title bar, and on a light-mode desktop — the default — it drew a white
/// bar across the top of every dark window, the one part of the app nobody had styled.
/// winit's dark theme is Windows' dark title bar; on Windows 11 the caption is then set to
/// the windows' own background colour, so bar and window are one surface. Windows 10 ignores
/// the colour and keeps the dark bar. Elsewhere this is a hint at most. The stickies have no
/// system title bar, so it costs them nothing.
fn dark_title_bar<T: ComponentHandle + 'static>(component: &T) {
    with_window(component, |w| {
        w.set_theme(Some(i_slint_backend_winit::winit::window::Theme::Dark));
        #[cfg(windows)]
        windows_impl::caption_color(w, WINDOW_BACKGROUND);
    });
}

/// The dark windows' background, `#2e2b27` in their `.slint` files.
#[cfg(windows)]
const WINDOW_BACKGROUND: (u8, u8, u8) = (0x2e, 0x2b, 0x27);

/// Runs `f` with a window's **winit** window, as soon as there is one.
///
/// `show()` does not create it. The winit backend registers a newly shown window as
/// "inactive" and builds it on a later turn of the event loop (`create_inactive_windows`,
/// called from `resumed` and `about_to_wait`), so `with_winit_window` straight after
/// [`present`] finds nothing and drops what was asked silently. That is exactly how the
/// stickies kept their taskbar buttons after they were told not to.
///
/// The future resolves immediately when the window already exists — the usual case when an
/// open note is raised — and with an error if the window is destroyed first, so a note that
/// is closed before it is ever shown leaves nothing waiting.
fn with_window<T: ComponentHandle + 'static>(
    component: &T,
    f: impl FnOnce(&i_slint_backend_winit::winit::window::Window) + 'static,
) {
    use i_slint_backend_winit::WinitWindowAccessor;

    let weak = component.as_weak();
    let spawned = slint::spawn_local(async move {
        let Some(component) = weak.upgrade() else { return };
        // An error means the window went away while we waited. Nothing to configure, and
        // nothing wrong.
        if let Ok(window) = component.window().winit_window().await {
            f(&window);
        }
    });
    if let Err(e) = spawned {
        diag!("could not reach the event loop to configure a window: {e}");
    }
}

/// Puts a window back where it was left, as far as the platform allows.
///
/// The size goes on straight away; the **position has to wait for a winit window to exist**.
/// `show()` does not create one — see [`with_window`] — so setting the position on the turn
/// the note is shown is dropped without a word, which is why a remembered note came back the
/// right size in the wrong place. It is the same trap that left the stickies their taskbar
/// buttons, and it wants the same answer.
///
/// A `POS_UNKNOWN` position means the platform would not say where the window was (native
/// Wayland); the size is still worth restoring, and the compositor places the window.
///
/// **Where it goes is decided against the screens attached now** (`screens::place`), with
/// `screen` — the one it was on when the geometry was taken — to say which of them it
/// belongs to. The same numbers are a different place after a laptop moves desks.
pub(crate) fn restore_geometry<T: ComponentHandle + 'static>(
    component: &T,
    geometry: [i32; 4],
    screen: Option<crate::screens::Screen>,
) {
    use i_slint_backend_winit::winit::dpi::PhysicalPosition;

    component
        .window()
        .set_size(slint::PhysicalSize::new(geometry[2].max(1) as u32, geometry[3].max(1) as u32));
    if geometry[0] == crate::settings::POS_UNKNOWN {
        return;
    }
    with_window(component, move |window| {
        let (screens, primary) = crate::screens::current(window);
        let (x, y) = crate::screens::place(geometry, screen.as_ref(), &screens, primary);
        if (x, y) != (geometry[0], geometry[1]) {
            diag!(
                "a window's screen is not where it was; placed at ({x},{y}) instead of ({},{})",
                geometry[0],
                geometry[1]
            );
        }
        window.set_outer_position(PhysicalPosition::new(x, y));
    });
}

/// Keeps a window out of the taskbar, and out of the pager where there is one.
///
/// A sticky is not an application. Eight notes on the desktop produced eight taskbar
/// buttons, and the button was never how anyone got back to one: the note is already on
/// screen, or the tray brings it forward ([`crate::tray::request_raise_notes`]). The list
/// window is the app and keeps its button; only the stickies are hidden.
///
/// Per platform, because there is no portable way to say this:
///
/// - **Windows**: two things, and it needs both. `WS_EX_TOOLWINDOW` is the *style* that
///   keeps the shell from ever giving this window a button — and takes it out of Alt+Tab
///   too, which is the same statement — but the shell decides that when it first notices the
///   window, so a style set afterwards does not take a button away again.
///   `ITaskbarList::DeleteTab` (winit's `set_skip_taskbar`) does that. Applying only the
///   second one is a race against the shell noticing the window, which is what shipped and
///   is why the buttons came back.
/// - **X11**: `_NET_WM_STATE_SKIP_TASKBAR` and `_NET_WM_STATE_SKIP_PAGER`, sent to the root
///   window as a client message the way EWMH asks. winit has no API for either state, so
///   this talks X11 itself.
/// - **Native Wayland**: **not possible.** No Wayland protocol lets a client say it does not
///   belong in a task list, so there the stickies keep their entries. This is the same
///   split as the magnetic snapping in `sticky.rs`: X11 and Windows do it, Wayland silently
///   does not.
pub(crate) fn skip_taskbar<T: ComponentHandle + 'static>(component: &T) {
    with_window(component, |window| {
        #[cfg(windows)]
        {
            use i_slint_backend_winit::winit::platform::windows::WindowExtWindows;
            windows_impl::tool_window(window);
            window.set_skip_taskbar(true);
        }
        #[cfg(target_os = "linux")]
        if let Some(xid) = x11_id(window) {
            x11::skip_taskbar(xid);
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = window;
    });
}

/// Makes a window that was **created** always-on-top actually be on top, on X11.
///
/// Slint builds a window hidden and maps it later, and winit asks for the level only once, at
/// creation, as a client message — which EWMH reserves for *mapped* windows, so the window
/// manager drops it. A pinned note that was opened pinned (every one put back after a restart,
/// and every pinned one opened from the list) came up as an ordinary window, under whatever
/// was focused next; toggling the pin by hand worked because by then the note was mapped.
/// Measured under openbox: `_NET_WM_STATE` empty on a restored pinned note, `ABOVE` after a
/// click on its pin. Windows and Wayland are left to winit.
pub(crate) fn keep_above<T: ComponentHandle + 'static>(component: &T) {
    with_window(component, |window| {
        #[cfg(target_os = "linux")]
        if let Some(xid) = x11_id(window) {
            x11::keep_above(xid);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = window;
    });
}

/// The monitors, straight from RandR on X11; `None` elsewhere (and on Wayland, where the
/// caller falls back to winit).
pub(crate) fn x11_screens(
    window: &i_slint_backend_winit::winit::window::Window,
) -> Option<(Vec<crate::screens::Screen>, Option<usize>)> {
    #[cfg(target_os = "linux")]
    if x11_id(window).is_some() {
        return x11::screens();
    }
    let _ = window;
    None
}

/// The X11 window id, or `None` under native Wayland, where the handle is a Wayland surface
/// and there is nothing to ask for.
#[cfg(target_os = "linux")]
fn x11_id(window: &i_slint_backend_winit::winit::window::Window) -> Option<u32> {
    use i_slint_backend_winit::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    window.window_handle().ok().and_then(|h| match h.as_raw() {
        RawWindowHandle::Xlib(h) => Some(h.window as u32),
        RawWindowHandle::Xcb(h) => Some(h.window.get()),
        _ => None,
    })
}

/// Re-applies [`skip_taskbar`] after the window level has been changed.
///
/// winit keeps its own `WindowFlags` and rebuilds the **whole** ex-style from them whenever
/// one of them changes (`WindowFlags::to_window_styles`) — `WS_EX_APPWINDOW` back on, and
/// anything added by hand gone. Pinning a note is exactly such a change, so pinning put the
/// note straight back in the taskbar: measured on Windows, `0x00000190` before the pin and
/// `0x00040118` after it.
///
/// Slint applies the changed property on the next turn of the event loop, so the hint is
/// re-asserted on the turn after that. It cannot be done in the same breath as the toggle,
/// because at that moment winit has not clobbered it yet.
pub(crate) fn reassert_taskbar<T: ComponentHandle + 'static>(component: &T) {
    let weak = component.as_weak();
    slint::Timer::single_shot(Duration::ZERO, move || {
        if let Some(component) = weak.upgrade() {
            skip_taskbar(&component);
        }
    });
}

/// Raises a window above the others and gives it the keyboard focus.
///
/// `present` only makes a window visible; a window that is already visible but buried stays
/// buried, which since the stickies left the taskbar is the state there is no other way out
/// of. On X11 this is an `_NET_ACTIVE_WINDOW` request and on Windows a `SetForegroundWindow`,
/// both of which the window manager may refuse — hence no return value to check: this asks,
/// it does not promise.
pub(crate) fn raise<T: ComponentHandle + 'static>(component: &T) {
    with_window(component, |window| window.focus_window());
}

/// Puts a window on top of the others in its layer **without focusing it** — a pinned note
/// stays among the pinned ones. winit has no such call: `focus_window` raises by activating,
/// and putting the desk back must not take the caret from whatever the user turned to.
///
/// X11 asks with a `ConfigureWindow` restack, which the window manager receives as a request
/// and may refuse; Windows is `SetWindowPos(HWND_TOP, SWP_NOACTIVATE)`. Nothing on Wayland,
/// where a client does not get a say in stacking at all.
pub(crate) fn restack_top(window: &i_slint_backend_winit::winit::window::Window) {
    #[cfg(windows)]
    windows_impl::bring_to_top(window);
    #[cfg(target_os = "linux")]
    if let Some(xid) = x11_id(window) {
        x11::restack_top(xid);
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    let _ = window;
}

#[cfg(windows)]
mod windows_impl;

#[cfg(target_os = "linux")]
mod x11;
