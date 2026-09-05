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
        {
            use i_slint_backend_winit::winit::raw_window_handle::{
                HasWindowHandle, RawWindowHandle,
            };
            // None under native Wayland, where the handle is a Wayland surface and there is
            // nothing to ask for.
            let xid = window.window_handle().ok().and_then(|h| match h.as_raw() {
                RawWindowHandle::Xlib(h) => Some(h.window as u32),
                RawWindowHandle::Xcb(h) => Some(h.window.get()),
                _ => None,
            });
            if let Some(xid) = xid {
                x11::skip_taskbar(xid);
            }
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = window;
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

/// The Windows half of [`skip_taskbar`] that winit has no API for.
#[cfg(windows)]
mod windows_impl {
    use i_slint_backend_winit::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use i_slint_backend_winit::winit::window::Window;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
    };

    /// Marks a window as a tool window, which is the shell's own idea of "not an application":
    /// no taskbar button and no place in Alt+Tab.
    ///
    /// Safe to call on a window that already has the style — the read-modify-write leaves it
    /// exactly as it was, and every `present` comes back through here.
    pub(super) fn tool_window(window: &Window) {
        let Ok(handle) = window.window_handle() else { return };
        let RawWindowHandle::Win32(win32) = handle.as_raw() else { return };
        let hwnd = win32.hwnd.get() as *mut core::ffi::c_void;
        // SAFETY: the handle comes from the winit window we are holding, so it is live for
        // the length of this call, and GWL_EXSTYLE is an isize on every supported target.
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_TOOLWINDOW as isize);
        }
    }
}

/// The X11 half of [`skip_taskbar`].
///
/// The connection is opened once and kept for the life of the process: raising a deskful of
/// notes re-applies the hint to each of them, and a fresh connect and three `InternAtom`
/// round trips per note is a visible pause for something the user asked to be instant.
#[cfg(target_os = "linux")]
mod x11 {
    use std::cell::RefCell;

    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        AtomEnum, ClientMessageEvent, ConnectionExt, EventMask, PropMode, Window,
    };
    use x11rb::rust_connection::RustConnection;
    // `change_property32` is on this second extension trait, not on the xproto one.
    use x11rb::wrapper::ConnectionExt as _;

    /// `_NET_WM_STATE_ADD`, and "the request comes from a normal application", per EWMH.
    const STATE_ADD: u32 = 1;
    const SOURCE_APPLICATION: u32 = 1;

    struct Wm {
        conn: RustConnection,
        root: Window,
        state: u32,
        skip_taskbar: u32,
        skip_pager: u32,
    }

    thread_local! {
        // Outer Option: not tried yet. Inner: tried and failed, so it is not tried again on
        // every window — a machine with no X server will not grow one mid-run.
        static WM: RefCell<Option<Option<Wm>>> = const { RefCell::new(None) };
    }

    fn open() -> Option<Wm> {
        let run = || -> anyhow::Result<Wm> {
            let (conn, screen) = x11rb::connect(None)?;
            let root = conn.setup().roots[screen].root;
            let atom = |name: &str| -> anyhow::Result<u32> {
                Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
            };
            let state = atom("_NET_WM_STATE")?;
            let skip_taskbar = atom("_NET_WM_STATE_SKIP_TASKBAR")?;
            let skip_pager = atom("_NET_WM_STATE_SKIP_PAGER")?;
            Ok(Wm { conn, root, state, skip_taskbar, skip_pager })
        };
        match run() {
            Ok(wm) => Some(wm),
            Err(e) => {
                ymemo_core::diag!("no X11 connection for the taskbar hint: {e}");
                None
            }
        }
    }

    pub(super) fn skip_taskbar(xid: u32) {
        WM.with(|cell| {
            let mut slot = cell.borrow_mut();
            let wm = slot.get_or_insert_with(open);
            let Some(wm) = wm.as_ref() else { return };
            // Checked rather than merely flushed: the round trips are a local socket, and
            // without them a rejected hint is a note that silently keeps its taskbar button.
            if let Err(e) = apply(wm, xid) {
                ymemo_core::diag!("could not hide the window from the taskbar: {e}");
            }
        });
    }

    /// Asks for the two states **both** ways EWMH defines, because which one works depends on
    /// something we cannot see here.
    ///
    /// The spec is explicit: a *mapped* window's state is changed by a client message to the
    /// root window, and an *unmapped* one by writing `_NET_WM_STATE` directly — the window
    /// manager owns the property from the map onwards. Slint's `show()` only queues the map
    /// request, so by the time this runs the window is one or the other and there is no way
    /// to ask which. Doing both costs one round trip and is right either way.
    fn apply(wm: &Wm, xid: u32) -> anyhow::Result<()> {
        // Read-modify-write, never a plain overwrite: winit puts `_NET_WM_STATE_ABOVE` in
        // this same property for a pinned note, and replacing the list would unpin it.
        let current = wm
            .conn
            .get_property(false, xid, wm.state, AtomEnum::ATOM, 0, 64)?
            .reply()?;
        let mut states: Vec<u32> = current.value32().map(|v| v.collect()).unwrap_or_default();
        for atom in [wm.skip_taskbar, wm.skip_pager] {
            if !states.contains(&atom) {
                states.push(atom);
            }
        }
        wm.conn
            .change_property32(PropMode::REPLACE, xid, wm.state, AtomEnum::ATOM, &states)?
            .check()?;

        // One message carries two states; _NET_WM_STATE takes exactly that many.
        let ev = ClientMessageEvent::new(
            32,
            xid,
            wm.state,
            [STATE_ADD, wm.skip_taskbar, wm.skip_pager, SOURCE_APPLICATION, 0],
        );
        wm.conn
            .send_event(
                false,
                wm.root,
                EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
                ev,
            )?
            .check()?;
        Ok(())
    }
}
