//! Lock and unlock flow, plus applying language and settings across every window.

use anyhow::{Context, Result};
use slint::{ComponentHandle, SharedString};
use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;
use ymemo_core::crypto::MasterKey;
use ymemo_core::sync::Syncthing;
use ymemo_core::vault::Vault;
use ymemo_core::{diag, Store};
use ymemo_i18n::t;

use crate::list::refresh_list;
use crate::session;
use crate::state::{Ctx, Ui};
use crate::sticky::flush_dirty;
use crate::sync::SYNC_FOLDER_ID;
use crate::window::present;
use crate::{ListWindow, LockWindow};

/// Leaves a stay-unlocked session behind after a password unlock.
///
/// The window is fixed from the moment the password was entered and is never extended by
/// use, so "the password is checked every N days" actually holds. Zero days stores nothing.
pub(crate) fn start_unlock_session(ctx: &Ctx, vault: &Vault) {
    let days = ctx.settings.borrow().unlock_days;
    session::save_session(&ctx.dir, &vault.key_bytes(), days);
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
    session::clear_session(&ctx.dir);
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
    // The cache is a plaintext copy of everything in the vault, so it goes with it — and so
    // do the sidecars WAL mode keeps beside it, or a reset hands the memos back.
    ymemo_core::Store::delete_file(ctx.dir.join("ymemo.db"))?;
    session::clear_session(&ctx.dir);

    *ctx.vault.borrow_mut() = None;
    ctx.model.set_vec(Vec::new());
    ctx.collapsed.borrow_mut().clear();
    Ok(())
}

/// Puts back the notes that were on the desk when this device was last used.
///
/// Closing every window does not close the app, so a desk full of notes is an ordinary state
/// to leave one in — but nothing put them back, and a restart (an update replacing the binary
/// underneath a running app is the one people notice) cleared the desk. Which notes those are
/// is device-local, in `settings.json` beside where each one sits.
///
/// A memo that is no longer in the vault is dropped rather than skipped: it was deleted on
/// another device, and its id would otherwise sit in the file for good.
fn reopen_desk(ctx: &Ctx) {
    let wanted: Vec<String> = ctx.settings.borrow().open_memos().to_vec();
    if wanted.is_empty() {
        return;
    }
    let mut gone = Vec::new();
    for id in &wanted {
        let memo = {
            let Some(guard) = ctx.vault_ref() else { return };
            let v = &*guard;
            match v.store().get(id) {
                Ok(Some(m)) => m,
                _ => {
                    gone.push(id.clone());
                    continue;
                }
            }
        };
        // Never focused: this happens while the user is looking at the unlock screen, and a
        // note stealing the caret from whatever they turned to next is worse than no note.
        if let Err(e) = crate::sticky::open_sticky(ctx, &memo, false) {
            ymemo_core::diag!("could not put a note back on the desk: {e}");
        }
    }
    if !gone.is_empty() {
        let mut settings = ctx.settings.borrow_mut();
        let mut changed = false;
        for id in &gone {
            changed |= settings.forget_memo(id);
        }
        if changed {
            settings.save(&ctx.dir);
        }
    }
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
        crate::list::set_vault_name(&list, &name);
    }
    // Started by the session: the vault is open, and that is all that happens. Putting the
    // list and every note on screen is what the user gets when they ask for the app — see
    // `show_desk`, which is what asking calls. Without this, "comes up quietly in the tray"
    // meant a machine that had just booted handed its owner their whole desk.
    if ctx.quiet_start.get() {
        return;
    }
    show_desk(ctx, list_weak);
}

/// Puts the list and the notes on screen, and stops the start being a quiet one.
///
/// Called by everything that means "the user asked for the app": a tray click, a second
/// launch, the tray's "bring the notes forward". Also called the moment the tray turns out
/// **not** to exist, since a quiet start with nowhere to click is an app nobody can reach.
pub(crate) fn show_desk(ctx: &Ctx, list_weak: &slint::Weak<ListWindow>) {
    ctx.quiet_start.set(false);
    if let Some(list) = list_weak.upgrade() {
        let (saved, screen) = {
            let settings = ctx.settings.borrow();
            (settings.list_window, settings.list_screen.clone())
        };
        present(&list);
        match saved {
            Some(geometry) => crate::window::restore_geometry(&list, geometry, screen),
            // First run: a Window whose root is a layout takes that layout's natural size and
            // ignores `preferred-height`, so without this the list opened at its own minimum —
            // six rows tall on any screen — and stayed there.
            None => list.window().set_size(slint::LogicalSize::new(340.0, 460.0)),
        }
    }
    // After the list, so the notes land in front of it rather than behind it — which the
    // order of these calls alone does not achieve; see `stack_desk`.
    reopen_desk(ctx);
    let ids = ctx.settings.borrow().open_memos().to_vec();
    crate::sticky::stack_desk(ctx, Some(list_weak.clone()), &ids);
}

/// Wires the lock window: unlocking, creating a vault, and the two ways back from a forgotten password.
pub(crate) fn wire(
    ctx: &Ctx,
    ui: &Ui,
    unlocked: &Rc<Cell<bool>>,
    syncthing: &Rc<RefCell<Option<Syncthing>>>,
    pending_vault: &Rc<RefCell<Option<Vault>>>,
    created_here: &Rc<Cell<bool>>,
) {
    wire_unlock(ctx, ui, unlocked);
    wire_create_vault(ctx, ui, unlocked, syncthing, pending_vault, created_here);
    wire_resize(ui);
    wire_recovery_ack(ctx, ui, unlocked, pending_vault);
    wire_recover(ctx, ui, unlocked);
    wire_reset(ctx, ui, syncthing);
}

/// Open: derive the key from vault.json's salt; a diverged key heals inside open.
/// **Never fall back to create** — that is what made keys diverge in the first place.
fn wire_unlock(ctx: &Ctx, ui: &Ui, unlocked: &Rc<Cell<bool>>) {
    let lock = &ui.lock;
    let list = &ui.list;
    let ctx = ctx.clone();
    let lock_weak = lock.as_weak();
    let list_weak = list.as_weak();
    let unlocked = unlocked.clone();
    let dir = ctx.dir.clone();
    // Open: derive the key from vault.json's salt; a diverged key heals inside open.
    // **Never fall back to create** — that is what made keys diverge in the first place.
    lock.on_unlock(move |password| {
        let lock = lock_weak.unwrap();
        if password.is_empty() {
            lock.set_lock_message(t!("msg.enter_password").into());
            return;
        }
        let store = match Store::open(dir.join("ymemo.db")) {
            Ok(s) => s,
            Err(e) => {
                lock.set_lock_message(SharedString::from(t!("msg.cache_open_failed", error = e)));
                return;
            }
        };
        // Argon2id blocks the UI for a few hundred ms, but only on the lock screen.
        match Vault::open(dir.join("vault"), password.as_bytes(), store) {
            Ok(v) => {
                start_unlock_session(&ctx, &v);
                apply_opened_vault(v, &ctx, &lock, &list_weak, &unlocked);
            }
            Err(e) => lock.set_lock_message(SharedString::from(format!("{e}"))),
        }
    });
}

/// Create: only on a device starting fresh. The salt is generated exactly here.
fn wire_create_vault(
    ctx: &Ctx,
    ui: &Ui,
    unlocked: &Rc<Cell<bool>>,
    syncthing: &Rc<RefCell<Option<Syncthing>>>,
    pending_vault: &Rc<RefCell<Option<Vault>>>,
    created_here: &Rc<Cell<bool>>,
) {
    let lock = &ui.lock;
    let list = &ui.list;
    let ctx = ctx.clone();
    let lock_weak = lock.as_weak();
    let list_weak = list.as_weak();
    let unlocked = unlocked.clone();
    let dir = ctx.dir.clone();
    let syncthing = syncthing.clone();
    let pending_vault = pending_vault.clone();
    let created_here = created_here.clone();
    // Create: only on a device starting fresh. The salt is generated exactly here.
    lock.on_create_vault(move |password| {
        let lock = lock_weak.unwrap();
        if password.is_empty() {
            lock.set_lock_message(t!("msg.enter_new_password").into());
            return;
        }
        let store = match Store::open(dir.join("ymemo.db")) {
            Ok(s) => s,
            Err(e) => {
                lock.set_lock_message(SharedString::from(t!("msg.cache_open_failed", error = e)));
                return;
            }
        };
        match Vault::open_or_create(dir.join("vault"), password.as_bytes(), store) {
            Ok(v) => {
                created_here.set(true);
                // Register the folder here as well as at startup: a reset removes it
                // deliberately, and this is the first vault that follows one.
                if let Some(st) = syncthing.borrow().as_ref() {
                    if let Err(e) =
                        st.ensure_folder(SYNC_FOLDER_ID, "Ymemo Vault", &dir.join("vault"))
                    {
                        diag!("could not register the shared folder: {e}");
                    }
                }
                start_unlock_session(&ctx, &v);
                // Show the recovery code before anything else; the vault waits in
                // `pending_vault` until the user confirms they have written it down.
                match v.issue_recovery_code() {
                    Ok(code) => {
                        lock.set_lock_message(SharedString::new());
                        lock.set_new_recovery_code(SharedString::from(code));
                        *pending_vault.borrow_mut() = Some(v);
                    }
                    // A vault without a recovery code still works, so this never blocks
                    // the user out of the app they just set up.
                    Err(e) => {
                        diag!("could not issue a recovery code: {e}");
                        lock.set_vault_exists(true);
                        apply_opened_vault(v, &ctx, &lock, &list_weak, &unlocked);
                    }
                }
            }
            Err(e) => lock.set_lock_message(SharedString::from(format!("{e}"))),
        }
    });
}

/// Size the window to whichever panel is on it.
/// Every panel here has its own size and `lock.slint` knows them all; this only applies
/// what it asks for.
fn wire_resize(ui: &Ui) {
    let lock = &ui.lock;
    let weak = lock.as_weak();
    lock.on_resize(move |w, h| {
        if let Some(win) = weak.upgrade() {
            win.window().set_size(slint::LogicalSize::new(w, h));
        }
    });
}

/// Recovery code written down: open the vault that was waiting for it.
fn wire_recovery_ack(
    ctx: &Ctx,
    ui: &Ui,
    unlocked: &Rc<Cell<bool>>,
    pending_vault: &Rc<RefCell<Option<Vault>>>,
) {
    let lock = &ui.lock;
    let list = &ui.list;
    let ctx = ctx.clone();
    let lock_weak = lock.as_weak();
    let list_weak = list.as_weak();
    let unlocked = unlocked.clone();
    let pending_vault = pending_vault.clone();
    lock.on_recovery_ack(move || {
        let Some(v) = pending_vault.borrow_mut().take() else { return };
        let lock = lock_weak.unwrap();
        lock.set_new_recovery_code(SharedString::new());
        lock.set_vault_exists(true);
        lock.set_has_recovery(true);
        apply_opened_vault(v, &ctx, &lock, &list_weak, &unlocked);
    });
}

/// Forgotten password: the recovery code sets a new one.
fn wire_recover(ctx: &Ctx, ui: &Ui, unlocked: &Rc<Cell<bool>>) {
    let lock = &ui.lock;
    let list = &ui.list;
    let ctx = ctx.clone();
    let lock_weak = lock.as_weak();
    let list_weak = list.as_weak();
    let unlocked = unlocked.clone();
    let dir = ctx.dir.clone();
    lock.on_recover(move |code, new_password| {
        let lock = lock_weak.unwrap();
        let vault_dir = dir.join("vault");
        // Only the header is rewritten, so a wrong code costs one Argon2id run and
        // leaves the vault exactly as it was.
        if let Err(e) = ymemo_core::vault::reset_password_with_recovery(
            &vault_dir,
            &code,
            new_password.as_bytes(),
        ) {
            lock.set_lock_message(SharedString::from(format!("{e}")));
            return;
        }
        let store = match Store::open(dir.join("ymemo.db")) {
            Ok(s) => s,
            Err(e) => {
                lock.set_lock_message(SharedString::from(t!("msg.cache_open_failed", error = e)));
                return;
            }
        };
        match Vault::open(&vault_dir, new_password.as_bytes(), store) {
            Ok(v) => {
                start_unlock_session(&ctx, &v);
                lock.invoke_leave_recovery();
                lock.set_lock_message(SharedString::new());
                apply_opened_vault(v, &ctx, &lock, &list_weak, &unlocked);
            }
            Err(e) => lock.set_lock_message(SharedString::from(format!("{e}"))),
        }
    });
}

/// Forgotten password, no recovery code: wipe and start over.
fn wire_reset(ctx: &Ctx, ui: &Ui, syncthing: &Rc<RefCell<Option<Syncthing>>>) {
    let lock = &ui.lock;
    let ctx = ctx.clone();
    let lock_weak = lock.as_weak();
    let syncthing = syncthing.clone();
    lock.on_reset_vault(move || {
        let lock = lock_weak.unwrap();
        match reset_vault(&ctx, &syncthing) {
            Ok(()) => {
                lock.invoke_leave_recovery();
                lock.invoke_clear_password();
                lock.set_vault_exists(false);
                lock.set_has_recovery(false);
                lock.set_lock_message(SharedString::from(t!("msg.reset_done")));
            }
            Err(e) => {
                lock.set_lock_message(SharedString::from(t!("msg.reset_failed", error = e)))
            }
        }
    });
}

/// Stay unlocked: a valid session key skips the password prompt. Returns whether it did.
///
/// On any failure (wrong key, damaged cache, diverged key) the session is dropped and the
/// lock screen comes up.
pub(crate) fn unlock_from_session(
    ctx: &Ctx,
    ui: &Ui,
    unlocked: &Rc<Cell<bool>>,
    vault_dir: &Path,
) -> bool {
    let dir = ctx.dir.as_path();
    session::load_session(dir).is_some_and(|key_bytes| {
        let opened = MasterKey::from_bytes(&key_bytes).and_then(|key| {
            let store = Store::open(dir.join("ymemo.db"))?;
            Vault::open_with_key(vault_dir, key, store)
        });
        match opened {
            Ok(v) => {
                apply_opened_vault(v, ctx, &ui.lock, &ui.list.as_weak(), unlocked);
                true
            }
            Err(e) => {
                diag!("could not unlock from the session, asking for the password: {e}");
                session::clear_session(dir);
                false
            }
        }
    })
}
