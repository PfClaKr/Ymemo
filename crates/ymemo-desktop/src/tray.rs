//! Tray icon, with one backend per platform behind cfg:
//!
//!  - **Linux**: `ksni` (StatusNotifierItem over D-Bus, pure Rust — no gtk/libdbus)
//!  - **Windows**: `tray-icon` (Shell_NotifyIcon, default features off to avoid gtk)
//!  - **Anything else (macOS, ...)**: not implemented; the app runs without a tray
//!
//! [`start`] returns a handle that keeps the tray alive; dropping it removes the tray.
//! Clicks and menu items go through the `request_*` functions below, because tray callbacks
//! do not run on the UI thread and slint components are not `Send`.

// TrayHandle is public as start()'s return type, but main only ever infers it.
#[allow(unused_imports)]
pub use imp::{start, TrayHandle};

use slint::ComponentHandle;

use crate::lock::lock_now;
use crate::state::{touch, APP};
use crate::window::present;

/// Tray click or menu: raise the lock window while locked, otherwise toggle the list. The
/// call is handed to the event loop first, whatever thread it came from.
pub(crate) fn request_toggle() {
    let _ = slint::invoke_from_event_loop(|| {
        APP.with(|a| {
            let borrow = a.borrow();
            let Some(app) = borrow.as_ref() else { return };
            touch(&app.ctx);
            if !app.unlocked.get() {
                present(&app.lock);
            } else if app.list.window().is_visible() {
                let _ = app.list.hide();
            } else {
                present(&app.list);
            }
        });
    });
}

/// A second launch asked us to come forward (see `instance::serve`). Unlike a tray
/// click this only ever shows: the user just asked for the app, so hiding it would be absurd.
pub(crate) fn request_show() {
    let _ = slint::invoke_from_event_loop(|| {
        APP.with(|a| {
            let borrow = a.borrow();
            let Some(app) = borrow.as_ref() else { return };
            touch(&app.ctx);
            if app.unlocked.get() {
                present(&app.list);
            } else {
                present(&app.lock);
            }
        });
    });
}

/// Tray "lock": same as the list window's lock button.
pub(crate) fn request_lock() {
    let _ = slint::invoke_from_event_loop(|| {
        APP.with(|a| {
            let borrow = a.borrow();
            let Some(app) = borrow.as_ref() else { return };
            lock_now(&app.ctx, &app.lock, &app.list, &app.unlocked);
        });
    });
}

/// Tray "quit": save what is unsaved, then end the event loop.
///
/// Ending the loop drops the sticky windows and, with them, the debounced edits still sitting
/// in the text widgets, so the flush has to happen here. `main` returning then drops the
/// `Syncthing` handle, which shuts the daemon down.
pub(crate) fn request_quit() {
    let _ = slint::invoke_from_event_loop(|| {
        APP.with(|a| {
            let borrow = a.borrow();
            if let Some(app) = borrow.as_ref() {
                crate::sticky::flush_dirty(&app.ctx);
            }
        });
        let _ = slint::quit_event_loop();
    });
}

// ===========================================================================
// Linux: ksni (StatusNotifierItem)
// ===========================================================================
#[cfg(target_os = "linux")]
mod imp {
    use super::{request_lock, request_quit, request_toggle};
    use crate::icon::tray_icon_rgba;
    use ymemo_i18n::t;

    struct YmemoTray;

    impl ksni::Tray for YmemoTray {
        fn id(&self) -> String {
            "dev.ymemo.Ymemo".into()
        }

        fn title(&self) -> String {
            "Ymemo".into()
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            // ksni wants ARGB32 in network byte order, so convert from RGBA.
            let (rgba, w, h) = tray_icon_rgba();
            let mut data = vec![0u8; rgba.len()];
            // as_chunks over chunks_exact: the pixel width is a constant, so the compiler is
            // told so once instead of on every iteration (and clippy asks for it).
            let (pixels, _) = rgba.as_chunks::<4>();
            let (out_pixels, _) = data.as_chunks_mut::<4>();
            for (px, out) in pixels.iter().zip(out_pixels.iter_mut()) {
                out[0] = px[3]; // A
                out[1] = px[0]; // R
                out[2] = px[1]; // G
                out[3] = px[2]; // B
            }
            vec![ksni::Icon {
                width: w as i32,
                height: h as i32,
                data,
            }]
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            request_toggle();
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::*;
            vec![
                StandardItem {
                    label: t!("tray.memo_list"),
                    activate: Box::new(|_| request_toggle()),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: t!("tray.lock"),
                    activate: Box::new(|_| request_lock()),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem {
                    label: t!("tray.quit"),
                    activate: Box::new(|_| request_quit()),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    /// Handle keeping the tray alive; `None` inside when spawning failed.
    pub struct TrayHandle(Option<ksni::blocking::Handle<YmemoTray>>);

    impl TrayHandle {
        /// Redraws the menu, e.g. after a language change.
        pub fn refresh(&self) {
            if let Some(h) = &self.0 {
                h.update(|_| {});
            }
        }
    }

    pub fn start() -> TrayHandle {
        use ksni::blocking::TrayMethods;
        match YmemoTray.spawn() {
            Ok(handle) => TrayHandle(Some(handle)),
            Err(e) => {
                ymemo_core::diag!("could not register the tray icon, continuing without it: {e}");
                TrayHandle(None)
            }
        }
    }
}

// ===========================================================================
// Windows: tray-icon (Shell_NotifyIcon)
// ===========================================================================
#[cfg(windows)]
mod imp {
    use std::time::Duration;

    use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    use super::{request_lock, request_quit, request_toggle};
    use crate::icon::tray_icon_rgba;
    use ymemo_i18n::t;

    /// Holds the tray icon together with the timer that polls its events; both must stay
    /// alive for the tray to work.
    pub struct TrayHandle {
        _tray: Option<TrayIcon>,
        _poll: slint::Timer,
        /// Kept so the labels can be rewritten on a language change.
        items: Vec<MenuItem>,
    }

    impl TrayHandle {
        /// Rewrites the labels in the current language, in start()'s append order.
        pub fn refresh(&self) {
            let labels = [t!("tray.memo_list"), t!("tray.lock"),
                          t!("tray.quit")];
            for (item, label) in self.items.iter().zip(labels) {
                item.set_text(label);
            }
        }
    }

    pub fn start() -> TrayHandle {
        // Menu: list, lock, separator, quit. Events are dispatched by item id.
        let list_item = MenuItem::new(t!("tray.memo_list"), true, None);
        let lock_item = MenuItem::new(t!("tray.lock"), true, None);
        let quit_item = MenuItem::new(t!("tray.quit"), true, None);
        let list_id: MenuId = list_item.id().clone();
        let lock_id: MenuId = lock_item.id().clone();
        let quit_id: MenuId = quit_item.id().clone();

        let menu = Menu::new();
        // append returns muda's Result (tray_icon::menu), not tray_icon::Result.
        let build_menu = || -> tray_icon::menu::Result<()> {
            menu.append(&list_item)?;
            menu.append(&lock_item)?;
            menu.append(&PredefinedMenuItem::separator())?;
            menu.append(&quit_item)?;
            Ok(())
        };

        let (rgba, w, h) = tray_icon_rgba();
        let tray = Icon::from_rgba(rgba, w, h)
            .map_err(|e| format!("could not create the icon: {e}"))
            .and_then(|icon| build_menu().map(|_| icon).map_err(|e| format!("could not build the menu: {e}")))
            .and_then(|icon| {
                TrayIconBuilder::new()
                    .with_menu(Box::new(menu))
                    // Left click toggles the list, right click opens the menu.
                    .with_menu_on_left_click(false)
                    .with_tooltip("Ymemo")
                    .with_icon(icon)
                    .build()
                    .map_err(|e| format!("could not create the tray: {e}"))
            });

        let tray = match tray {
            Ok(t) => Some(t),
            Err(e) => {
                ymemo_core::diag!("could not register the tray icon, continuing without it: {e}");
                None
            }
        };

        // Drain the event channels from the event-loop thread; on Windows the winit message
        // pump delivers the tray messages that show up here.
        let poll = slint::Timer::default();
        poll.start(slint::TimerMode::Repeated, Duration::from_millis(120), move || {
            while let Ok(ev) = MenuEvent::receiver().try_recv() {
                if ev.id == list_id {
                    request_toggle();
                } else if ev.id == lock_id {
                    request_lock();
                } else if ev.id == quit_id {
                    request_quit();
                }
            }
            while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
                // One left click, on release, toggles the list.
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = ev
                {
                    request_toggle();
                }
            }
        });

        TrayHandle {
            _tray: tray,
            _poll: poll,
            items: vec![list_item, lock_item, quit_item],
        }
    }
}

// ===========================================================================
// Other platforms (macOS, ...): no tray backend
// ===========================================================================
#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    pub struct TrayHandle;

    impl TrayHandle {
        pub fn refresh(&self) {}
    }

    pub fn start() -> TrayHandle {
        ymemo_core::diag!("no tray backend on this platform, running without a tray");
        TrayHandle
    }
}
