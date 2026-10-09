//! The settings window: filling it from what is stored, saving it back as it is changed, and
//! the language every other window is relabelled in when that changes.

use slint::{ComponentHandle, SharedString, TimerMode};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;
use ymemo_core::diag;
use ymemo_core::sync::Syncthing;
use ymemo_i18n::t;

use crate::lock::lock_now;
use crate::session;
use crate::settings::Settings;
use crate::state::{touch, Ctx, Ui};
use crate::sync::{apply_folder_settings, start_merge_timer};
use crate::window::present_dialog;
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
    win.set_note_text_percent(s.note_text_percent);
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

/// How long after the last change a setting is stored and applied: long enough that a slider
/// being dragged or a number being typed is one write, short enough to feel immediate.
const APPLY_DELAY: Duration = Duration::from_millis(350);

/// Where the About page's project link goes.
const PROJECT_PAGE: &str = "https://github.com/PfClaKr/Ymemo";

/// A change waiting out [`APPLY_DELAY`], and what applying it does.
struct PendingApply {
    timer: slint::Timer,
    apply: Box<dyn Fn()>,
}

impl PendingApply {
    /// (Re)starts the wait: the last of a burst of changes is the one applied.
    fn schedule(self: &Rc<Self>) {
        let me = Rc::downgrade(self);
        self.timer.start(TimerMode::SingleShot, APPLY_DELAY, move || {
            if let Some(me) = me.upgrade() {
                (me.apply)();
            }
        });
    }

    /// Applies a change still waiting, at once: the window is closing, or the vault is about
    /// to be locked under it.
    fn flush(&self) {
        if self.timer.running() {
            self.timer.stop();
            (self.apply)();
        }
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
    let pending = wire_apply(ctx, ui, syncthing, merge_timer, tray_handle);
    wire_open(ctx, ui, unlocked);
    wire_close(ui, &pending);
    wire_lock_now(ctx, ui, unlocked, &pending);
    wire_updates(ctx, ui);
}

/// The list's gear button: fill the settings window from what is stored and show it.
fn wire_open(ctx: &Ctx, ui: &Ui, unlocked: &Rc<Cell<bool>>) {
    let list = &ui.list;
    let settings_win = &ui.settings;
    let ctx = ctx.clone();
    let win = settings_win.as_weak();
    let unlocked = unlocked.clone();
    let list_weak = list.as_weak();
    list.on_open_settings(move || {
        touch(&ctx);
        let Some(w) = win.upgrade() else { return };
        fill_settings_window(&ctx, &w);
        w.set_unlocked(unlocked.get());
        w.set_status(SharedString::new());
        // Keep in step with `preferred-*` in settings.slint.
        present_dialog(&w, (680.0, 500.0), list_weak.upgrade().as_ref().map(|l| l.window()));
    });
}

/// Closing only hides it, so its position and page survive. A change still waiting out the
/// delay is applied first — closing is not "never mind", there being nothing to confirm. The
/// window's own close button takes the same way out.
fn wire_close(ui: &Ui, pending: &Rc<PendingApply>) {
    let settings_win = &ui.settings;
    let win = settings_win.as_weak();
    let pending = pending.clone();
    settings_win.on_close_requested(move || {
        let Some(w) = win.upgrade() else { return };
        pending.flush();
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

/// Every change in the window is stored and applied a moment after it is made, as on the
/// phone: what the window shows *is* the settings, with no Save to forget. Returns the waiting
/// change, which closing and locking apply at once.
fn wire_apply(
    ctx: &Ctx,
    ui: &Ui,
    syncthing: &Rc<RefCell<Option<Syncthing>>>,
    merge_timer: &Rc<slint::Timer>,
    tray_handle: &Rc<RefCell<Option<tray::TrayHandle>>>,
) -> Rc<PendingApply> {
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
    let apply = move || {
        let Some(w) = win.upgrade() else { return };
        let (Some(lock), Some(list)) = (lock_weak.upgrade(), list_weak.upgrade()) else {
            return;
        };
        let prev = ctx.settings.borrow().clone();
        let next = read_dialog(&ctx, &w);
        // Starting with the session is written to the desktop, not to `settings.json`, so it
        // is compared and applied on its own.
        let login = w.get_start_at_login();
        let autostart_moved = crate::autostart::supported() && login != crate::autostart::enabled();
        if next == prev && !autostart_moved {
            return; // the window was only being filled in
        }
        touch(&ctx);
        if autostart_moved {
            // A failure is reported and then read back when the window is refilled below,
            // which leaves the switch showing what is actually true rather than what was asked.
            if let Err(e) = autostart::set(login) {
                diag!("could not change the start-at-login setting: {e}");
            }
        }
        if next != prev {
            next.save(&ctx.dir);
            *ctx.settings.borrow_mut() = next.clone();
        }
        // Write the sanitized values back, so out-of-range input never changes silently.
        fill_settings_window(&ctx, &w);

        // Only what moved is applied: this runs for every nudge of a slider.
        if next.lang != prev.lang {
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
        }
        if next.merge_seconds != prev.merge_seconds {
            start_merge_timer(&merge_timer, &ctx, list.as_weak());
        }
        if next.note_text_percent != prev.note_text_percent {
            crate::sticky::apply_text_size(&ctx);
        }
        // The watch delay lives in Syncthing's folder config, not ours, so a change has to be
        // pushed across; `set_folder_timing` does nothing when the daemon already agrees.
        if (next.watch_delay_seconds, next.rescan_seconds, next.keep_versions_days)
            != (prev.watch_delay_seconds, prev.rescan_seconds, prev.keep_versions_days)
        {
            apply_folder_settings(&syncthing_for_settings, &next);
        }
        // Shortening or disabling the stay-unlocked window leaves an existing session in
        // violation of it, so drop the session and ask for the password next time.
        if next.unlock_days != prev.unlock_days {
            session::clear_session(&ctx.dir);
            w.set_status(SharedString::from(t!("msg.settings_relock")));
        }
    };
    let pending = Rc::new(PendingApply { timer: slint::Timer::default(), apply: Box::new(apply) });
    let scheduled = Rc::downgrade(&pending);
    settings_win.on_edited(move || {
        if let Some(p) = scheduled.upgrade() {
            p.schedule();
        }
    });
    pending
}

/// Lock now, from the settings window. A change still waiting is stored first.
fn wire_lock_now(ctx: &Ctx, ui: &Ui, unlocked: &Rc<Cell<bool>>, pending: &Rc<PendingApply>) {
    let lock = &ui.lock;
    let list = &ui.list;
    let settings_win = &ui.settings;
    let ctx = ctx.clone();
    let win = settings_win.as_weak();
    let lock_weak = lock.as_weak();
    let list_weak = list.as_weak();
    let unlocked = unlocked.clone();
    let pending = pending.clone();
    settings_win.on_lock_now(move || {
        let (Some(lock), Some(list)) = (lock_weak.upgrade(), list_weak.upgrade()) else {
            return;
        };
        pending.flush();
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
    {
        let ctx = ctx.clone();
        let weak = settings_win.as_weak();
        settings_win.on_export_memos(move || export_memos(&ctx, weak.clone()));
    }
    settings_win.on_open_log(move || {
        if let Err(e) = update::open_url(&dir.to_string_lossy()) {
            diag!("could not open the log folder: {e}");
        }
    });
    settings_win.on_open_project(|| {
        if let Err(e) = update::open_url(PROJECT_PAGE) {
            diag!("could not open the project page: {e}");
        }
    });
}

/// Writes every memo out as a zip of Markdown files, to a file the user picks.
///
/// The zip is built here, on the UI thread, because that is where the vault lives; it is a
/// copy of text and already-encrypted blobs decrypted once, which is quick. Only the dialog
/// and the write go to a worker, for the same reason the photo dialogs do.
fn export_memos(ctx: &Ctx, weak: slint::Weak<SettingsWindow>) {
    touch(ctx);
    let zip = {
        let Some(v) = ctx.vault_ref() else { return };
        match ymemo_core::export::markdown_zip(&v) {
            Ok(zip) => zip,
            Err(e) => {
                diag!("could not export the memos: {e}");
                if let Some(w) = weak.upgrade() {
                    w.set_status(SharedString::from(t!("msg.export_failed", error = e)));
                }
                return;
            }
        }
    };
    let title = t!("msg.export_dialog_title");
    let file_name = format!("Ymemo-{}.zip", chrono::Local::now().format("%Y-%m-%d"));
    std::thread::spawn(move || {
        let Some(path) = rfd::FileDialog::new()
            .set_title(&title)
            .set_file_name(&file_name)
            .add_filter("zip", &["zip"])
            .save_file()
        else {
            return; // cancelled
        };
        let status = match std::fs::write(&path, &zip) {
            Ok(()) => t!("msg.exported", path = path.display()),
            Err(e) => {
                diag!("could not write the export: {e}");
                t!("msg.export_failed", error = e)
            }
        };
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = weak.upgrade() {
                w.set_status(SharedString::from(status));
            }
        });
    });
}

/// The window's fields laid over what is stored, sanitized — what applying them stores.
///
/// Starts from what is stored and overwrites only the fields this window owns. Building the
/// struct from scratch here meant every other field — the pins, where the windows are, which
/// notes are folded, which are on the desk — had to be copied back across by hand, and one
/// forgotten line would have quietly reset it the next time anything was changed.
fn read_dialog(ctx: &Ctx, w: &SettingsWindow) -> Settings {
    let mut next = ctx.settings.borrow().clone();
    next.lang = w.get_lang_sel().to_string();
    next.unlock_days = w.get_unlock_days();
    next.idle_lock_minutes = w.get_idle_minutes();
    next.default_color = w.get_default_color().to_string();
    next.default_opacity = w.get_default_opacity();
    next.note_text_percent = w.get_note_text_percent();
    next.merge_seconds = w.get_merge_seconds();
    next.watch_delay_seconds = w.get_watch_delay_seconds();
    next.rescan_seconds = w.get_rescan_seconds();
    next.keep_versions_days = w.get_keep_versions_days();
    next.update_check = w.get_update_check();
    next.sanitize();
    next
}
