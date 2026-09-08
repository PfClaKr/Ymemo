//! Lock and unlock flow, plus applying language and settings across every window.

use std::cell::{Cell, RefCell};
use std::fs;
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context, Result};
use slint::{ComponentHandle, SharedString};
use ymemo_core::sync::Syncthing;
use ymemo_core::vault::Vault;
use ymemo_i18n::t;

use crate::list::refresh_list;
use crate::settings;
use crate::state::Ctx;
use crate::sticky::flush_dirty;
use crate::sync::SYNC_FOLDER_ID;
use crate::window::present;
use crate::{
    apply_strings, ApproveWindow, HistoryWindow, ListWindow, LockWindow, SecurityWindow,
    SettingsWindow, Strings,
};

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
    win.set_watch_delay_seconds(s.watch_delay_seconds);
    win.set_rescan_seconds(s.rescan_seconds);
    win.set_keep_versions_days(s.keep_versions_days);
    win.set_update_check(s.update_check);
}

/// Applies the language to **every** window.
///
/// The Slint `Strings` global is per component instance, so changing one window leaves the
/// rest in the old language. Open stickies are walked here; later ones are filled by
/// `open_sticky` on creation.
pub(crate) fn apply_lang(
    ctx: &Ctx,
    lock: &LockWindow,
    list: &ListWindow,
    settings_win: &SettingsWindow,
    security_win: &SecurityWindow,
    history_win: &HistoryWindow,
    approve_win: &ApproveWindow,
) {
    // Switch the catalog first; every later t!/apply_strings reads it, core and tray
    // included, so nothing else needs notifying.
    ymemo_i18n::set_lang(ctx.settings.borrow().effective_lang());
    apply_strings(&lock.global::<Strings>());
    apply_strings(&list.global::<Strings>());
    apply_strings(&settings_win.global::<Strings>());
    apply_strings(&security_win.global::<Strings>());
    apply_strings(&history_win.global::<Strings>());
    apply_strings(&approve_win.global::<Strings>());
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

/// Drops the pending undo and takes the offer off the list.
pub(crate) fn forget_undo(ctx: &Ctx, list: &ListWindow) {
    ctx.undo_timer.stop();
    *ctx.undo.borrow_mut() = None;
    list.set_undo_message(SharedString::new());
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
    // A pending undo holds the removed memo's title and body. A locked vault leaves no memo
    // text behind it, and the offer must not outlive the session either: unlocking used to
    // find the bar still there, ready to write a memo from before the lock back — into
    // whatever vault happens to be open by then.
    forget_undo(ctx, list);

    let _ = list.hide();
    lock.invoke_clear_password();
    lock.invoke_leave_recovery();
    lock.set_lock_message(SharedString::new());
    lock.set_show_sync(false);
    // A code may have been issued during the session that just ended.
    lock.set_has_recovery(ymemo_core::vault::recovery_code_exists(ctx.dir.join("vault")));
    present(lock);
}

/// Wipes this device's vault, cache and session: the way out of a forgotten password when
/// there is no recovery code either.
///
/// **Unsharing comes first and is not optional.** Syncthing propagates deletions, so
/// emptying a folder it still carries would delete the memos on every paired device too.
/// If the folder cannot be released, nothing is deleted at all.
pub(crate) fn reset_vault(ctx: &Ctx, syncthing: &Rc<RefCell<Option<Syncthing>>>) -> Result<()> {
    if let Some(st) = syncthing.borrow().as_ref() {
        st.remove_folder(SYNC_FOLDER_ID)
            .context(t!("msg.unshare_before_reset"))?;
    }

    ymemo_core::vault::wipe(ctx.dir.join("vault"))?;
    // The cache is a plaintext copy of everything in the vault, so it goes with it.
    let db = ctx.dir.join("ymemo.db");
    if db.exists() {
        fs::remove_file(&db)?;
    }
    settings::clear_session(&ctx.dir);

    *ctx.vault.borrow_mut() = None;
    ctx.model.set_vec(Vec::new());
    ctx.collapsed.borrow_mut().clear();
    Ok(())
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
    refresh_list(&v, &ctx.model, &ctx.collapsed.borrow(), &ctx.query.borrow());
    let name = v.name();
    *ctx.vault.borrow_mut() = Some(v);
    unlocked.set(true);
    let _ = lock.hide();
    if let Some(list) = list_weak.upgrade() {
        // The name comes out of the vault, so it is only knowable once one is open.
        list.set_vault_name(SharedString::from(name));
        let saved = ctx.settings.borrow().list_window;
        present(&list);
        match saved {
            Some(geometry) => crate::window::restore_geometry(&list, geometry),
            // First run: a Window whose root is a layout takes that layout's natural size and
            // ignores `preferred-height`, so without this the list opened at its own minimum —
            // six rows tall on any screen — and stayed there.
            None => list.window().set_size(slint::LogicalSize::new(340.0, 460.0)),
        }
    }
}
