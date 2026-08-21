//! Lock and unlock flow, plus applying language and settings across every window.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, SharedString};
use ymemo_core::vault::Vault;

use crate::icon::set_window_icon;
use crate::list::refresh_list;
use crate::settings;
use crate::state::Ctx;
use crate::sticky::flush_dirty;
use crate::{apply_strings, ListWindow, LockWindow, SettingsWindow, Strings};

/// Fills the settings window's inputs from the current settings.
pub(crate) fn fill_settings_window(ctx: &Ctx, win: &SettingsWindow) {
    // The workspace version equals the release tag; release.yml checks that.
    win.set_app_version(SharedString::from(env!("CARGO_PKG_VERSION")));
    let s = ctx.settings.borrow();
    win.set_lang_sel(SharedString::from(s.lang.clone()));
    win.set_unlock_days(s.unlock_days);
    win.set_idle_minutes(s.idle_lock_minutes);
    win.set_default_color(SharedString::from(s.default_color.clone()));
    win.set_default_opacity(s.default_opacity);
    win.set_merge_seconds(s.merge_seconds);
}

/// Applies the language to **every** window.
///
/// The Slint `Strings` global is per component instance, so changing one window leaves the
/// rest in the old language. Open stickies are walked here; later ones are filled by
/// `open_sticky` on creation.
pub(crate) fn apply_lang(ctx: &Ctx, lock: &LockWindow, list: &ListWindow, settings_win: &SettingsWindow) {
    // Switch the catalog first; every later t!/apply_strings reads it, core and tray
    // included, so nothing else needs notifying.
    ymemo_i18n::set_lang(ctx.settings.borrow().effective_lang());
    apply_strings(&lock.global::<Strings>());
    apply_strings(&list.global::<Strings>());
    apply_strings(&settings_win.global::<Strings>());
    for entry in ctx.stickies.borrow().values() {
        apply_strings(&entry.window.global::<Strings>());
    }
}

/// Leaves a stay-unlocked session behind after a password unlock.
///
/// The window is fixed from the moment the password was entered and is never extended by
/// use, so "the password is checked every N days" actually holds. Zero days stores nothing.
pub(crate) fn start_unlock_session(ctx: &Ctx, vault: &Vault) {
    let days = ctx.settings.borrow().unlock_days;
    settings::save_session(&ctx.dir, &vault.key_bytes(), days);
}

/// Locks now: flush unsaved edits, close every sticky, drop the vault from memory, clear the
/// stay-unlocked session and show the lock window.
///
/// Clearing the session is the point — leaving it would reopen the vault on the next start
/// and make the lock button meaningless.
pub(crate) fn lock_now(ctx: &Ctx, lock: &LockWindow, list: &ListWindow, unlocked: &Rc<Cell<bool>>) {
    if !unlocked.get() {
        return;
    }

    // Save pending edits first; the windows are about to go away.
    let ids = flush_dirty(ctx);
    for id in &ids {
        if let Some(e) = ctx.stickies.borrow().get(id) {
            let _ = e.window.hide();
        }
    }
    // Defer dropping the window handles to the next event-loop turn (as in close_sticky).
    {
        let stickies = ctx.stickies.clone();
        slint::Timer::single_shot(Duration::ZERO, move || stickies.borrow_mut().clear());
    }

    *ctx.vault.borrow_mut() = None;
    ctx.model.set_vec(Vec::new());
    unlocked.set(false);
    settings::clear_session(&ctx.dir);

    let _ = list.hide();
    lock.invoke_clear_password();
    lock.set_lock_message(SharedString::new());
    lock.set_show_sync(false);
    let _ = lock.show();
    set_window_icon(lock.window());
    lock.window().request_redraw();
}

/// Shared tail of unlock and create-vault: fill the list, store the vault, hide the lock
/// window and show the list.
pub(crate) fn apply_opened_vault(
    v: Vault,
    ctx: &Ctx,
    lock: &LockWindow,
    list_weak: &slint::Weak<ListWindow>,
    unlocked: &Rc<Cell<bool>>,
) {
    refresh_list(&v, &ctx.model, &ctx.collapsed.borrow());
    *ctx.vault.borrow_mut() = Some(v);
    unlocked.set(true);
    let _ = lock.hide();
    if let Some(list) = list_weak.upgrade() {
        let _ = list.show();
        set_window_icon(list.window());
    }
}
