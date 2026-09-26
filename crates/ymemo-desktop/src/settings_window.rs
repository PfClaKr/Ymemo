//! The settings window: filling it from what is stored, saving it back, and the language
//! every other window is relabelled in when that changes.

use slint::{ComponentHandle, SharedString};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use ymemo_core::diag;
use ymemo_core::sync::Syncthing;
use ymemo_i18n::t;

use crate::lock::lock_now;
use crate::session;
use crate::settings::Settings;
use crate::state::{touch, Ctx, Ui};
use crate::sync::{apply_folder_settings, start_merge_timer};
use crate::window::present;
use crate::{
    apply_strings, autostart, tray, update, ApproveWindow, HistoryWindow, ListWindow,
    LockWindow, SecurityWindow, SettingsWindow, Strings,
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
    // Not from `settings.json`: the desktop's own autostart entry is the record, so this is
    // read back from it each time the window opens — including after a failed write, which is
    // how the toggle stays honest about what actually happened.
    win.set_autostart_supported(crate::autostart::supported());
    win.set_start_at_login(crate::autostart::enabled());
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

/// Wires the settings window.
pub(crate) fn wire(
    ctx: &Ctx,
    ui: &Ui,
    unlocked: &Rc<Cell<bool>>,
    syncthing: &Rc<RefCell<Option<Syncthing>>>,
    merge_timer: &Rc<slint::Timer>,
    tray_handle: &Rc<RefCell<Option<tray::TrayHandle>>>,
) {
    wire_open(ctx, ui, unlocked);
    wire_close(ctx, ui);
    wire_apply(ctx, ui, syncthing, merge_timer, tray_handle);
    wire_lock_now(ctx, ui, unlocked);
    wire_updates(ctx, ui);
}

/// The list's gear button: fill the settings window from what is stored and show it.
fn wire_open(ctx: &Ctx, ui: &Ui, unlocked: &Rc<Cell<bool>>) {
    let list = &ui.list;
    let settings_win = &ui.settings;
    let ctx = ctx.clone();
    let win = settings_win.as_weak();
    let unlocked = unlocked.clone();
    list.on_open_settings(move || {
        touch(&ctx);
        let Some(w) = win.upgrade() else { return };
        fill_settings_window(&ctx, &w);
        w.set_unlocked(unlocked.get());
        w.set_status(SharedString::new());
        w.set_close_warned(false);
        present(&w);
    });
}

/// Closing only hides it, so its position survives — but not over unsaved changes without a
/// word. The dialog applies on Save, and "Close" used to throw away whatever had been changed
/// with nothing said; now the first close says so and a second one means it. The window's own
/// close button takes the same way out.
fn wire_close(ctx: &Ctx, ui: &Ui) {
    let settings_win = &ui.settings;
    let ctx = ctx.clone();
    let win = settings_win.as_weak();
    settings_win.on_close_requested(move || {
        let Some(w) = win.upgrade() else { return };
        if !w.get_close_warned() && has_unsaved_changes(&ctx, &w) {
            w.set_close_warned(true);
            w.set_status(SharedString::from(t!("msg.settings_unsaved")));
            return;
        }
        w.set_close_warned(false);
        let _ = w.hide();
    });
    let win = settings_win.as_weak();
    settings_win.window().on_close_requested(move || {
        if let Some(w) = win.upgrade() {
            w.invoke_close_requested();
        }
        slint::CloseRequestResponse::KeepWindowShown
    });
}

/// Save: store the dialog's fields and apply whatever of them takes effect straight away.
fn wire_apply(
    ctx: &Ctx,
    ui: &Ui,
    syncthing: &Rc<RefCell<Option<Syncthing>>>,
    merge_timer: &Rc<slint::Timer>,
    tray_handle: &Rc<RefCell<Option<tray::TrayHandle>>>,
) {
    let lock = &ui.lock;
    let list = &ui.list;
    let settings_win = &ui.settings;
    let security_win = &ui.security;
    let history_win = &ui.history;
    let approve_win = &ui.approve;
    let ctx = ctx.clone();
    let win = settings_win.as_weak();
    let lock_weak = lock.as_weak();
    let list_weak = list.as_weak();
    let merge_timer = merge_timer.clone();
    let tray_handle = tray_handle.clone();
    let security_weak = security_win.as_weak();
    let history_weak = history_win.as_weak();
    let approve_weak = approve_win.as_weak();
    let syncthing_for_settings = syncthing.clone();
    settings_win.on_apply(move || {
        let Some(w) = win.upgrade() else { return };
        let (Some(lock), Some(list)) = (lock_weak.upgrade(), list_weak.upgrade()) else {
            return;
        };
        touch(&ctx);
        let prev_unlock_days = ctx.settings.borrow().unlock_days;
        let next = store_dialog(&ctx, &w);
        w.set_close_warned(false);

        // Write the sanitized values back, so out-of-range input never changes silently.
        fill_settings_window(&ctx, &w);
        apply_lang(
            &ctx,
            &lock,
            &list,
            &w,
            &security_weak.unwrap(),
            &history_weak.unwrap(),
            &approve_weak.unwrap(),
        );
        if let Some(t) = tray_handle.borrow().as_ref() {
            t.refresh();
        }
        start_merge_timer(&merge_timer, &ctx, list.as_weak());
        // The watch delay lives in Syncthing's folder config, not ours, so saving has to
        // push it across; `set_folder_timing` does nothing when the daemon already agrees.
        apply_folder_settings(&syncthing_for_settings, &next);

        // Shortening or disabling the stay-unlocked window leaves an existing session in
        // violation of it, so drop the session and ask for the password next time.
        let status = if next.unlock_days != prev_unlock_days {
            session::clear_session(&ctx.dir);
            t!("msg.settings_saved_relock")
        } else {
            t!("msg.settings_saved")
        };
        w.set_status(SharedString::from(status));
    });
}

/// Lock now, from the settings window.
fn wire_lock_now(ctx: &Ctx, ui: &Ui, unlocked: &Rc<Cell<bool>>) {
    let lock = &ui.lock;
    let list = &ui.list;
    let settings_win = &ui.settings;
    let ctx = ctx.clone();
    let win = settings_win.as_weak();
    let lock_weak = lock.as_weak();
    let list_weak = list.as_weak();
    let unlocked = unlocked.clone();
    settings_win.on_lock_now(move || {
        let (Some(lock), Some(list)) = (lock_weak.upgrade(), list_weak.upgrade()) else {
            return;
        };
        lock_now(&ctx, &lock, &list, &unlocked);
        if let Some(w) = win.upgrade() {
            w.set_unlocked(false);
            let _ = w.hide();
        }
    });
}

/// The update check and the log folder, both reached from the settings window.
fn wire_updates(ctx: &Ctx, ui: &Ui) {
    let settings_win = &ui.settings;
    {
        let ctx = ctx.clone();
        // The button asks regardless of the daily gap, and says what came back.
        settings_win.on_check_update(move || update::spawn_check(&ctx, true));
    }
    settings_win.on_open_update(update::open_download);

    // The log is a file the user is asked for, never one they read here: open the folder and
    // let the desktop's own file manager do the rest.
    let dir = ctx.dir.clone();
    settings_win.on_open_log(move || {
        if let Err(e) = update::open_url(&dir.to_string_lossy()) {
            diag!("could not open the log folder: {e}");
        }
    });
}

/// The dialog's fields laid over what is stored, sanitized — what Save would store.
///
/// Starts from what is stored and overwrites only the fields this dialog owns. Building the
/// struct from scratch here meant every other field — the pins, where the windows are, which
/// notes are folded, which are on the desk — had to be copied back across by hand, and one
/// forgotten line would have quietly reset it the next time anybody pressed Save.
fn read_dialog(ctx: &Ctx, w: &SettingsWindow) -> Settings {
    let mut next = ctx.settings.borrow().clone();
    next.lang = w.get_lang_sel().to_string();
    next.unlock_days = w.get_unlock_days();
    next.idle_lock_minutes = w.get_idle_minutes();
    next.default_color = w.get_default_color().to_string();
    next.default_opacity = w.get_default_opacity();
    next.merge_seconds = w.get_merge_seconds();
    next.watch_delay_seconds = w.get_watch_delay_seconds();
    next.rescan_seconds = w.get_rescan_seconds();
    next.keep_versions_days = w.get_keep_versions_days();
    next.update_check = w.get_update_check();
    next.sanitize();
    next
}

/// Whether closing now would throw away something the user changed in the dialog.
fn has_unsaved_changes(ctx: &Ctx, w: &SettingsWindow) -> bool {
    read_dialog(ctx, w) != *ctx.settings.borrow()
        || (crate::autostart::supported() && w.get_start_at_login() != crate::autostart::enabled())
}

/// Stores the dialog (see [`read_dialog`]) and makes it current.
fn store_dialog(ctx: &Ctx, w: &SettingsWindow) -> Settings {
    let next = read_dialog(ctx, w);
    // Starting with the session is written to the desktop, not to `settings.json`, so it is
    // applied here on its own. A failure is reported and then read back when the window is
    // refilled, which leaves the toggle showing what is actually true rather than what was
    // asked for.
    if let Err(e) = autostart::set(w.get_start_at_login()) {
        diag!("could not change the start-at-login setting: {e}");
    }
    next.save(&ctx.dir);
    *ctx.settings.borrow_mut() = next.clone();
    next
}
