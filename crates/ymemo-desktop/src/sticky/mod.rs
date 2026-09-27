//! Sticky windows: creating them, saving edits, closing them, and magnetic snapping.
//!
//! One window per memo, with the body doubling as the editor (debounced autosave). Snapping
//! only works where window coordinates can be read and written (X11, Windows); elsewhere
//! (native Wayland) it silently does nothing.

use anyhow::Result;
use slint::{ComponentHandle, LogicalSize, SharedString, TimerMode};
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};
use ymemo_core::diag;
use ymemo_core::{now_millis, Memo};
use ymemo_i18n::t;

use crate::list::refresh_list;
use crate::state::{Motion, touch, Ctx, StickyEntry, Stickies, APP};
use crate::window::restore_geometry;
use crate::{apply_strings, StickyWindow, Strings};

use desk::{next_stamp, present_sticky, raise_sticky, wire_window_events};
use geometry::remember_one;
use title::title_for;

mod appearance;
mod desk;
mod geometry;
mod photos;
mod snap;
mod title;

pub(crate) use desk::{hide_open_notes_from_taskbar, raise_open, stack_desk};
pub(crate) use geometry::{remember_geometry, rescue_offscreen};
pub(crate) use photos::{forget_all_photos, set_photo_models, split_photo_rows};
pub(crate) use snap::snap_tick;
pub(crate) use title::derive_title;

/// Body font size (logical px) that photo sizes in em are measured against.
/// **Must match the body `font-size` in `ui/sticky.slint`.**
const BODY_FONT_PX: f64 = 13.0;

/// Debounce between an edit and the autosave.
pub(crate) const SAVE_DEBOUNCE: Duration = Duration::from_millis(800);
/// Title bar height, i.e. the collapsed window height; must match app.slint.
pub(crate) const BAR_HEIGHT: f32 = 24.0;
/// Snap distance in logical px; within it, edges stick to the screen or another sticky.
pub(crate) const SNAP_DIST: f32 = 12.0;
/// How long after a note opens its moves are the app's own: the window manager's first
/// placement, then `restore_geometry` putting it where it was left.
const SETTLE: Duration = Duration::from_millis(1500);
/// How often sticky positions are polled for snapping.
pub(crate) const SNAP_INTERVAL: Duration = Duration::from_millis(90);
/// How often where the windows are is written to `settings.json`.
///
/// Deliberately far slower than the snap poll: this is a file write, and a drag would
/// otherwise produce one per frame. Two seconds is under the time it takes to move a note and
/// reach for something else, and a close records its own window immediately anyway.
pub(crate) const GEOMETRY_INTERVAL: Duration = Duration::from_secs(2);
/// The size a sticky opens at (logical px); **must match the preferred size in
/// `ui/sticky.slint`**, which is what the window is actually given.
pub(crate) const DEFAULT_SIZE: (f32, f32) = (200.0, 120.0);
/// Height of the colour and opacity panel; **must match the one in `ui/sticky.slint`**.
/// A sticky opens small enough that the panel would take most of the note, so the window
/// makes room for it and gives it back on close.
const PALETTE_HEIGHT: f32 = 62.0;

/// Body text for the window; an older memo with only a title promotes it to the body.
pub(crate) fn sticky_text(memo: &Memo) -> String {
    if memo.body.is_empty() && !memo.title.is_empty() {
        memo.title.clone()
    } else {
        memo.body.clone()
    }
}

/// Saves an edited body to the vault, deriving the title from its first line.
///
/// Returns whether the vault now holds `text`. **False means the writing is only in the
/// window**, so the caller must leave the sticky marked dirty: the merge timer skips a dirty
/// note, and that is what stops the next tick from painting the last stored version over
/// what is still being typed.
pub(crate) fn save_memo(ctx: &Ctx, id: &str, text: &str) -> bool {
    let Some(mut guard) = ctx.vault_mut() else { return false };
    let v = &mut *guard;
    let mut memo = match v.store().get(id) {
        Ok(Some(m)) => m,
        // Deleted: the edits have nowhere to go and nothing is waiting to be written.
        _ => return true,
    };
    let title = title_for(&memo, text);
    if memo.body == text && memo.title == title {
        return true;
    }
    memo.title = title;
    memo.body = text.to_string();
    memo.updated_at = now_millis();
    if let Err(e) = v.upsert(&memo) {
        diag!("could not save the memo: {e}");
        crate::list::report_write_failure(&e);
        // On the note as well: this is the one place where what is lost is still on screen,
        // and the list saying so is no use behind a closed window.
        if let Some(entry) = ctx.stickies.borrow().get(id) {
            entry
                .window
                .set_notice(SharedString::from(t!("msg.write_failed", error = e)));
        }
        return false;
    }
    refresh_list(v, &ctx.model, &ctx.collapsed.borrow(), &ctx.query.borrow());
    // A photo standing in the writing is drawn at the height of the words above it, and those
    // words have just changed — `upsert` has already moved its anchor, but the window is
    // holding the text it was given last time. Without this the picture stays where it was
    // while the writing slides out from under it. Only the photos are pushed, never the text:
    // the user is typing in it.
    if let Some(entry) = ctx.stickies.borrow().get(id) {
        if !entry.window.get_photo_busy() {
            set_photo_models(&entry.window, split_photo_rows(v, id));
        }
    }
    // Reflect the new title in the title bar.
    if let Some(entry) = ctx.stickies.borrow().get(id) {
        set_title(&entry.window, &memo.title);
        // The save went through, so whatever the last one said no longer holds.
        entry.window.set_notice(SharedString::new());
    }
    true
}

/// Writes out every sticky's pending edit, stops its debounce timer and drops the notes that
/// were never written on. Returns the ids of the open stickies, in no particular order.
///
/// Called wherever the windows are about to stop existing — locking and quitting — because
/// the autosave is debounced and the last keystrokes are otherwise still only in the widget.
/// Two passes, since `save_memo` borrows the sticky map itself.
///
/// The blank ones go the same way they go when a note is closed by hand: a sticky opened and
/// left empty is not a note, and locking used to leave "(empty memo)" in the list for good —
/// the very row [`discard_if_blank`] exists to prevent.
pub(crate) fn flush_dirty(ctx: &Ctx) -> Vec<String> {
    let ids: Vec<String> = ctx.stickies.borrow().keys().cloned().collect();
    for id in &ids {
        let pending = {
            let map = ctx.stickies.borrow();
            match map.get(id) {
                Some(e) if e.dirty.get() => {
                    e.save_timer.stop();
                    Some(e.window.get_memo_text().to_string())
                }
                Some(e) => {
                    e.save_timer.stop();
                    None
                }
                None => None,
            }
        };
        if let Some(text) = pending {
            save_memo(ctx, id, &text);
        }
    }
    // After the saves, never before: a note whose only writing is still in the widget would
    // otherwise look blank and be thrown away with it.
    for id in &ids {
        discard_if_blank(ctx, id);
    }
    ids
}

/// Creates a memo and opens its sticky; shared by the + button in both windows.
pub(crate) fn new_memo(ctx: &Ctx) {
    new_memo_in(ctx, "");
}

/// A new memo, already filed in `group_id` — empty for the top level.
///
/// Every other way of starting one puts it at the top level, so a note that belonged in a
/// folder had to be made and then dragged in.
pub(crate) fn new_memo_in(ctx: &Ctx, group_id: &str) {
    touch(ctx);
    crate::list::clear_search(ctx);
    let mut memo = Memo::new("", "");
    memo.group_id = group_id.to_string();
    {
        // Color and opacity defaults come from the settings.
        let s = ctx.settings.borrow();
        memo.color = s.default_color.clone();
        memo.opacity = s.default_opacity as i64;
    }
    {
        let Some(mut guard) = ctx.vault_mut() else { return };
        let v = &mut *guard;
        if let Err(e) = v.upsert(&memo) {
            diag!("could not create the memo: {e}");
            crate::list::report_write_failure(&e);
            return;
        }
        refresh_list(v, &ctx.model, &ctx.collapsed.borrow(), &ctx.query.borrow());
    }
    if let Err(e) = open_sticky(ctx, &memo, true) {
        diag!("could not open the sticky window: {e}");
    }
}

/// Hides a sticky and schedules its removal from the registry; dropping the window inside
/// its own callback is unsafe, so it waits for the next event-loop turn.
pub(crate) fn close_sticky(stickies: &Stickies, id: &str) {
    if let Some(entry) = stickies.borrow().get(id) {
        entry.save_timer.stop();
        let _ = entry.window.hide();
    }
    let stickies = stickies.clone();
    let id = id.to_string();
    slint::Timer::single_shot(Duration::ZERO, move || {
        stickies.borrow_mut().remove(&id);
        photos::forget_photos_of(&id);
    });
}

/// Epoch millis to a local `YYYY-MM-DD HH:MM` stamp; empty when the value is unusable.
///
/// Local time, not UTC: the stamp exists to answer "when did I write this", and an offset
/// answer is worse than none. The layout is the same in both languages, so it needs no
/// catalog entry.
pub(crate) fn format_created_at(millis: i64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_millis_opt(millis) {
        chrono::offset::LocalResult::Single(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        _ => String::new(),
    }
}

/// Sets a memo's title on its sticky, in both the places it appears.
///
/// Two properties for one string: `memo-title` is drawn by Slint and goes through
/// [`crate::hangul::for_slint`], `window-title` is the desktop's own title bar and must carry
/// the name as it is really spelled. Behind one function because setting one and not the
/// other is exactly the bug this replaces — two of the four callers were passing the title
/// straight through, so the same memo's title bar read one way after a save and another after
/// a merge.
pub(crate) fn set_title(window: &StickyWindow, title: &str) {
    window.set_memo_title(SharedString::from(crate::hangul::for_slint(title)));
    window.set_window_title(SharedString::from(title));
}

/// Sets a sticky's body and the blocks its read view draws, which must never disagree: the
/// read view is the same words, and showing yesterday's next to today's would be worse than
/// showing none.
pub(crate) fn set_body_text(window: &StickyWindow, text: &str) {
    window.set_memo_text(SharedString::from(text));
    window.set_blocks(slint::ModelRc::new(slint::VecModel::from(
        crate::markdown::blocks(text),
    )));
}

/// A new sticky window showing `memo`, with nothing wired to it yet.
fn build_window(ctx: &Ctx, memo: &Memo) -> Result<StickyWindow> {
    let window = StickyWindow::new()?;
    // The globals are per instance, so fill this one with the current strings.
    apply_strings(&window.global::<Strings>());
    set_title(&window, &memo.title);
    set_body_text(&window, &sticky_text(memo));
    window.set_sticky_color(SharedString::from(memo.color.clone()));
    window.set_sticky_opacity(memo.opacity as f32);
    window.set_pinned(ctx.settings.borrow().memo_pinned(&memo.id));
    window.set_text_scale(text_scale(ctx));
    window.set_created_at(SharedString::from(format_created_at(memo.created_at)));
    if let Some(v) = ctx.vault_ref() {
        set_photo_models(&window, split_photo_rows(&v, &memo.id));
    }
    Ok(window)
}

/// Puts a note back where it was left, folded if it was, and writes down that it is on the
/// desk.
///
/// Applied after the window is on screen: a size set on a window that has not been shown is
/// what the backends disagree about, and the position needs a real window underneath it.
fn put_back(ctx: &Ctx, window: &StickyWindow, id: &str, expanded_height: &Rc<Cell<f32>>) {
    let (saved_geometry, saved_screen, folded) = {
        let settings = ctx.settings.borrow();
        (settings.memo_window(id), settings.memo_screen(id), settings.memo_folded(id))
    };
    if let Some(geometry) = saved_geometry {
        restore_geometry(window, geometry, saved_screen);
        // What it should go back to when it is unfolded; the geometry above is always the
        // note's expanded size, folded or not.
        expanded_height.set(geometry[3] as f32 / window.window().scale_factor());
    }
    // Folded is a state of its own, applied after the size above rather than instead of it.
    if folded {
        window.set_collapsed(true);
        let logical_w = window.window().size().width as f32 / window.window().scale_factor();
        window.window().set_size(LogicalSize::new(logical_w, BAR_HEIGHT));
    }
    let mut settings = ctx.settings.borrow_mut();
    if settings.set_memo_open(id, true) {
        settings.save(&ctx.dir);
    }
}

/// Opens a memo's sticky, or raises it when already open.
pub(crate) fn open_sticky(ctx: &Ctx, memo: &Memo, focus: bool) -> Result<()> {
    if let Some(entry) = ctx.stickies.borrow().get(&memo.id) {
        raise_sticky(ctx, &entry.window);
        return Ok(());
    }

    let window = build_window(ctx, memo)?;
    let dirty = Rc::new(Cell::new(false));
    let expanded_height = Rc::new(Cell::new(0.0f32));
    wire_history(&window, &memo.id);
    wire_editing(ctx, &window, &memo.id, &dirty);
    wire_close(ctx, &window, &memo.id, &dirty);
    wire_wm_close(&window);
    wire_new_memo(ctx, &window);
    wire_zoom(ctx, &window);
    wire_delete(&window, &memo.id);
    photos::wire(ctx, &window, &memo.id);
    appearance::wire(ctx, &window, &memo.id, &expanded_height);
    snap::wire_drag(ctx, &window, &memo.id);

    present_sticky(ctx, &window);
    // Dropping a picture on a note puts it in the note, and activating it stamps its place in
    // the stacking order. Registered after the window is on screen, because until then there
    // is no winit window to hang the filter on and this is quietly a no-op.
    let last_active = Rc::new(Cell::new(next_stamp()));
    let motion = Motion::new();
    wire_window_events(ctx, &window, &memo.id, last_active.clone(), motion.clone());
    put_back(ctx, &window, &memo.id, &expanded_height);
    // A note opens at its first line, whatever the widget's scroll offset happened to be.
    window.invoke_body_to_top();
    if focus {
        window.invoke_focus_body();
    }
    ctx.stickies.borrow_mut().insert(
        memo.id.clone(),
        StickyEntry {
            window,
            save_timer: slint::Timer::default(),
            dirty,
            motion,
            drag_grab: Cell::new(None),
            settle_until: Cell::new(Instant::now() + SETTLE),
            last_active,
        },
    );
    Ok(())
}

/// Quits when the window that just closed was the last one, on a desktop with no tray.
///
/// The app is tray-resident, and closing every window is meant to leave it waiting in the
/// tray. Where no tray registered there is nothing to wait in: the app went on running with
/// no window and no icon, and clicking the launcher again was the only way back — which is
/// indistinguishable from a crash, and left a vault open in a process the user believed they
/// had closed, since the idle auto-lock is off by default.
///
/// With a tray this does nothing at all; the notes are one click away there.
pub(crate) fn quit_if_last_window(ctx: &Ctx) {
    if ctx.has_tray.get() {
        return;
    }
    // The window that triggered this is still on screen while its close is being handled — a
    // `close_requested` handler runs *before* the hide it asks for — so "the last one" can
    // only be counted on the next turn of the event loop, once it is really gone.
    let _ = slint::invoke_from_event_loop(|| {
        APP.with(|a| {
            let borrow = a.borrow();
            let Some(app) = borrow.as_ref() else { return };
            if app.list.window().is_visible() {
                return;
            }
            if app.ctx.stickies.borrow().values().any(|e| e.window.window().is_visible()) {
                return;
            }
            crate::tray::request_quit();
        });
    });
}

/// Deletes a memo that was never written in, so closing an empty note leaves nothing behind.
///
/// Only a memo with **nothing at all** on it: no title, no body, and no photo. A blank note
/// with a picture on it is a note. Nothing is offered to undo here on purpose — there is
/// nothing in it to lose, and an undo bar after every stray `+` would be its own kind of
/// clutter.
pub(crate) fn discard_if_blank(ctx: &Ctx, id: &str) {
    {
        let Some(mut guard) = ctx.vault_mut() else { return };
        let v = &mut *guard;
        match v.store().get(id) {
            Ok(Some(memo)) if memo.title.is_empty() && memo.body.is_empty() => {
                // A photo makes it a note, and a cache that cannot be read is not grounds for
                // deleting anything.
                match v.store().attachments_of(id) {
                    Ok(photos) if photos.is_empty() => {}
                    _ => return,
                }
            }
            _ => return,
        }
        if let Err(e) = v.delete(id) {
            diag!("could not discard the blank memo: {e}");
            return;
        }
        refresh_list(v, &ctx.model, &ctx.collapsed.borrow(), &ctx.query.borrow());
    }
    let mut settings = ctx.settings.borrow_mut();
    let changed = settings.forget_memo(id);
    if changed {
        settings.save(&ctx.dir);
    }
}

/// Past versions of this memo. The window belongs to main, so it is reached through the
/// same thread_local the tray uses.
fn wire_history(window: &StickyWindow, id: &str) {
    let id = id.to_string();
    window.on_show_history(move || {
        APP.with(|a| {
            let borrow = a.borrow();
            let Some(app) = borrow.as_ref() else { return };
            touch(&app.ctx);
            crate::history::show(
                &app.ctx,
                &app.history,
                &app.history_subject,
                ymemo_core::history::Entity::Memo,
                &id,
            );
        });
    });
}

/// An edit marks the sticky dirty and arms the debounced save.
fn wire_editing(ctx: &Ctx, window: &StickyWindow, id: &str, dirty: &Rc<Cell<bool>>) {
    let ctx = ctx.clone();
    let id = id.to_string();
    let dirty = dirty.clone();
    let weak = window.as_weak();
    window.on_edited(move |text| {
        touch(&ctx);
        // The read view is rebuilt as it is typed rather than when the caret leaves: it
        // is cheap, and doing it on the way out would show the old words for a frame.
        if let Some(w) = weak.upgrade() {
            w.set_blocks(slint::ModelRc::new(slint::VecModel::from(
                crate::markdown::blocks(text.as_str()),
            )));
        }
        dirty.set(true);
        let ctx2 = ctx.clone();
        let id2 = id.clone();
        let dirty2 = dirty.clone();
        let weak2 = weak.clone();
        if let Some(entry) = ctx.stickies.borrow().get(&id) {
            entry.save_timer.start(TimerMode::SingleShot, SAVE_DEBOUNCE, move || {
                if let Some(w) = weak2.upgrade() {
                    // Stays dirty when the write failed, so the note keeps what was
                    // typed instead of being merged back to the last stored version.
                    if save_memo(&ctx2, &id2, w.get_memo_text().as_str()) {
                        dirty2.set(false);
                    }
                }
            });
        }
    });
}

/// Close: save first, then hide the window; the memo stays.
fn wire_close(ctx: &Ctx, window: &StickyWindow, id: &str, dirty: &Rc<Cell<bool>>) {
    let ctx = ctx.clone();
    let id = id.to_string();
    let dirty = dirty.clone();
    let weak = window.as_weak();
    window.on_close_requested(move || {
        touch(&ctx);
        if let Some(w) = weak.upgrade() {
            // Where it was when it was closed, not where the last poll saw it.
            remember_one(&ctx, &id, w.window());
            if dirty.get() && save_memo(&ctx, &id, w.get_memo_text().as_str()) {
                dirty.set(false);
            }
        }
        // A note that was opened and left blank is not a note. Closing one used to leave
        // an "(empty memo)" row in the list for good, which the user then had to find and
        // delete — and the button that does that is one row away from the memos that are
        // not blank.
        discard_if_blank(&ctx, &id);
        // Off the desk on purpose, so the next launch does not put it back.
        {
            let mut settings = ctx.settings.borrow_mut();
            if settings.set_memo_open(&id, false) {
                settings.save(&ctx.dir);
            }
        }
        close_sticky(&ctx.stickies, &id);
        APP.with(|a| {
            let borrow = a.borrow();
            if let Some(app) = borrow.as_ref() {
                quit_if_last_window(&app.ctx);
            }
        });
    });
}

/// A close from the window manager — Alt+F4, a taskbar button's menu — takes the same way
/// out as the note's own ×. `close-requested` above is a callback of the component, not of
/// the window, and Slint's default for the window's own request is simply to hide it: the
/// note left the screen, stayed in `open_memos` and came back on the next start, a blank
/// one was never discarded, and with no tray the last note closed that way left the app
/// running with no window and the vault open. Measured under openbox with `wmctrl -c`.
fn wire_wm_close(window: &StickyWindow) {
    let weak = window.as_weak();
    window.window().on_close_requested(move || {
        if let Some(w) = weak.upgrade() {
            w.invoke_close_requested(); // hides it, through `close_sticky`
        }
        slint::CloseRequestResponse::KeepWindowShown
    });
}

/// New memo.
fn wire_new_memo(ctx: &Ctx, window: &StickyWindow) {
    let ctx = ctx.clone();
    window.on_new_memo(move || new_memo(&ctx));
}

/// Ctrl+= / Ctrl+- / Ctrl+0 on a note: the writing of every note larger, smaller, or back to
/// the default. One size for the desk rather than one per note, because it is a matter of how
/// well this screen reads, not of the note — and it is the same setting the dialog shows.
fn wire_zoom(ctx: &Ctx, window: &StickyWindow) {
    let ctx = ctx.clone();
    window.on_zoom(move |step| {
        let next = {
            let mut s = ctx.settings.borrow_mut();
            s.note_text_percent = if step == 0 {
                crate::settings::NOTE_TEXT_PERCENT.0
            } else {
                s.note_text_percent + step.signum() * crate::settings::NOTE_TEXT_STEP
            };
            s.sanitize();
            s.clone()
        };
        next.save(&ctx.dir);
        apply_text_size(&ctx);
    });
}

/// The note text size as the factor the sticky multiplies its font sizes by.
fn text_scale(ctx: &Ctx) -> f32 {
    ctx.settings.borrow().note_text_percent as f32 / 100.0
}

/// Puts the stored note text size on every open note.
pub(crate) fn apply_text_size(ctx: &Ctx) {
    let scale = text_scale(ctx);
    for entry in ctx.stickies.borrow().values() {
        entry.window.set_text_scale(scale);
    }
}

/// Delete, from the note's colour panel. The list is brought up with its undo bar, since a
/// deleted note leaves nothing else on screen to take it back from.
fn wire_delete(window: &StickyWindow, id: &str) {
    let id = id.to_string();
    window.on_delete_memo(move || {
        APP.with(|a| {
            let borrow = a.borrow();
            let Some(app) = borrow.as_ref() else { return };
            touch(&app.ctx);
            crate::list::actions::delete_and_offer_undo(&app.ctx, &app.list, &id, false);
            if app.list.window().is_visible() {
                crate::window::raise(&app.list);
            } else {
                crate::window::present(&app.list);
            }
        });
    });
}
