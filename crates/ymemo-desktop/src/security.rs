//! Security window: changing the master password and issuing the recovery code.
//!
//! Both are header-only operations in the core, so there is nothing to do on a background
//! thread and nothing to refresh in the list — see `ymemo_core::vault`.

use slint::{ComponentHandle, SharedString};
use ymemo_i18n::t;

use crate::state::{touch, Ctx};
use crate::window::present;
use crate::{SecurityWindow, SettingsWindow};

/// Connects the security window to the settings button that opens it.
pub(crate) fn wire(ctx: &Ctx, settings_win: &SettingsWindow, win: &SecurityWindow) {
    {
        let ctx = ctx.clone();
        let weak = win.as_weak();
        settings_win.on_open_security(move || {
            touch(&ctx);
            let Some(w) = weak.upgrade() else { return };
            reset_window(&ctx, &w);
            present(&w);
        });
    }
    {
        let ctx = ctx.clone();
        let weak = win.as_weak();
        win.on_close_requested(move || {
            let Some(w) = weak.upgrade() else { return };
            // Clear before hiding: the passwords and the one-time code must not be sitting
            // in the window the next time it opens.
            reset_window(&ctx, &w);
            let _ = w.hide();
        });
    }
    {
        let ctx = ctx.clone();
        let weak = win.as_weak();
        win.on_change_password(move |current, new| {
            let Some(w) = weak.upgrade() else { return };
            touch(&ctx);
            let guard = ctx.vault.borrow();
            let Some(v) = guard.as_ref() else {
                return set_status(&w, t!("msg.vault_locked"), true, true);
            };
            match v.change_password(current.as_bytes(), new.as_bytes()) {
                Ok(()) => {
                    w.invoke_clear_passwords();
                    set_status(&w, t!("msg.password_changed"), false, true);
                }
                Err(e) => set_status(&w, format!("{e}"), true, true),
            }
        });
    }
    {
        let ctx = ctx.clone();
        let weak = win.as_weak();
        win.on_issue_recovery(move || {
            let Some(w) = weak.upgrade() else { return };
            touch(&ctx);
            let guard = ctx.vault.borrow();
            let Some(v) = guard.as_ref() else {
                return set_status(&w, t!("msg.vault_locked"), true, false);
            };
            match v.issue_recovery_code() {
                Ok(code) => {
                    w.set_recovery_code(SharedString::from(code));
                    w.set_has_recovery(true);
                    make_room_for_code(&w);
                    set_status(&w, t!("msg.recovery_issued"), false, false);
                }
                Err(e) => set_status(&w, format!("{e}"), true, false),
            }
        });
    }
}

/// Empties the window: no password left in a field, no code left on screen.
fn reset_window(ctx: &Ctx, win: &SecurityWindow) {
    win.invoke_clear_passwords();
    win.set_recovery_code(SharedString::new());
    win.set_status(SharedString::new());
    win.set_status_is_error(false);
    win.set_has_recovery(
        ctx.vault.borrow().as_ref().is_some_and(|v| v.has_recovery_code()),
    );
}

/// Height (logical px) the window needs once a recovery code is on screen.
///
/// The window is sized for the two sections; the code and the line telling the user to write
/// it down are another panel below them, and a window that does not grow puts the one thing
/// that can never be shown again under the fold of a scroll view nobody knew to scroll.
const CODE_PANEL_HEIGHT: f32 = 600.0;

/// Grows the window so a freshly issued code is visible without scrolling. Only ever grows:
/// a window the user has made larger is left alone.
fn make_room_for_code(win: &SecurityWindow) {
    let window = win.window();
    let size = window.size().to_logical(window.scale_factor());
    if size.height < CODE_PANEL_HEIGHT {
        window.set_size(slint::LogicalSize::new(size.width, CODE_PANEL_HEIGHT));
    }
}

/// Shows one message, under the section that produced it — `on_password` picks which.
fn set_status(
    win: &SecurityWindow,
    text: impl Into<SharedString>,
    is_error: bool,
    on_password: bool,
) {
    win.set_status_is_error(is_error);
    win.set_status_on_password(on_password);
    win.set_status(text.into());
}
