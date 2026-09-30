//! What the list window's buttons, keys and drags do.

use slint::{ComponentHandle, SharedString, TimerMode};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;
use ymemo_core::{diag, now_millis};
use ymemo_i18n::t;

use crate::list::{self, move_row, refresh_list};
use crate::lock::lock_now;
use crate::state::{touch, Ctx, Ui};
use crate::sticky::{self, close_sticky, new_memo, open_sticky};
use crate::window::MadeFrom;
use std::cell::RefCell;
use crate::{update, ListWindow};

/// How long a delete can be taken back for.
///
/// Long enough to notice the wrong row went, short enough that the bar is not still offering
/// to undo something the user has since forgotten doing. The memo is really gone from the
/// document the whole time — this is an offer to write it back, not a pending deletion — so
/// nothing about the timing risks the vault or the other devices.
const UNDO_WINDOW: Duration = Duration::from_secs(30);

/// Wires the list window's callbacks.
pub(crate) fn wire(ctx: &Ctx, ui: &Ui, unlocked: &Rc<Cell<bool>>) {
    wire_open_memo(ctx, ui);
    wire_new_memo(ctx, ui);
    wire_new_memo_in(ctx, ui);
    wire_delete(ctx, ui);
    wire_undo(ctx, ui);
    wire_search(ctx, ui);
    wire_new_group(ctx, ui);
    wire_discard_group(ctx, ui);
    wire_toggle_group(ctx, ui);
    wire_rename_group(ctx, ui);
    wire_rename_vault(ctx, ui);
    wire_move_row(ctx, ui);
    wire_move_to_folder(ctx, ui);
    wire_reorder(ctx, ui);
    wire_recolor(ctx, ui);
    wire_lock_now(ctx, ui, unlocked);
    wire_close(ctx, ui);
    ui.list.on_open_update(update::open_download);
}

/// Opens the memo a row stands for.
fn wire_open_memo(ctx: &Ctx, ui: &Ui) {
    let list = &ui.list;
    let ctx = ctx.clone();
    list.on_open_memo(move |id| {
        touch(&ctx);
        let memo = {
            let Some(guard) = ctx.vault_ref() else { return };
            let v = &*guard;
            match v.store().get(&id) {
                Ok(Some(m)) => m,
                _ => return,
            }
        };
        if let Err(e) = open_sticky(&ctx, &memo, false) {
            diag!("could not open the sticky window: {e}");
        }
    });
}

/// A new memo at the top level.
fn wire_new_memo(ctx: &Ctx, ui: &Ui) {
    let list = &ui.list;
    let ctx = ctx.clone();
    let weak = list.as_weak();
    list.on_new_memo(move || {
        let Some(l) = weak.upgrade() else { return };
        new_memo(&ctx, MadeFrom::List(l.window(), sticky::note_positions(&ctx)));
    });
}

/// A new memo inside a folder, from the folder row's `+`.
fn wire_new_memo_in(ctx: &Ctx, ui: &Ui) {
    let list = &ui.list;
    let ctx = ctx.clone();
    let weak = list.as_weak();
    list.on_new_memo_in(move |group| {
        let Some(l) = weak.upgrade() else { return };
        sticky::new_memo_in(&ctx, group.as_str(), MadeFrom::List(l.window(), sticky::note_positions(&ctx)));
    });
}

/// Delete, and the offer to take it back.
///
/// Deleting is the only thing in this app that loses writing, and a deleted memo cannot be
/// reached through its own history either — the row it would be opened from is the row
/// that just went away. What stands in for a confirmation is the bar this puts at the top
/// of the list: the delete happens immediately, which is right for the many that were
/// meant, and the one that was not is one click away for the next thirty seconds.
fn wire_delete(ctx: &Ctx, ui: &Ui) {
    let ctx = ctx.clone();
    let list_weak = ui.list.as_weak();
    ui.list.on_delete_row(move |id, is_group| {
        touch(&ctx);
        if let Some(list) = list_weak.upgrade() {
            delete_and_offer_undo(&ctx, &list, &id, is_group);
        }
    });
}

/// Deletes a memo or folder and puts the offer to take it back at the top of the list, for
/// [`UNDO_WINDOW`]. The list's own delete, and a sticky's.
pub(crate) fn delete_and_offer_undo(ctx: &Ctx, list: &ListWindow, id: &str, is_group: bool) {
    let removed = {
        let Some(mut guard) = ctx.vault_mut() else { return };
        let v = &mut *guard;
        // Deleting a group lifts its contents instead of removing them.
        let res = if is_group { v.delete_group(id) } else { v.delete(id) };
        let removed = match res {
            Ok(removed) => removed,
            Err(e) => {
                diag!("delete failed: {e}");
                list::report_write_failure(&e);
                return;
            }
        };
        refresh_list(v, &ctx.model, &ctx.collapsed.borrow(), &ctx.query.borrow());
        removed
    };
    if !is_group {
        // Where it stood on the desk, if it was open, for the undo to put it back there.
        let was_open = ctx.stickies.borrow().contains_key(id);
        DESK_PLACE.with(|p| {
            let s = ctx.settings.borrow();
            *p.borrow_mut() = was_open.then(|| DeskPlace {
                id: id.to_string(),
                window: s.memo_window(id),
                screen: s.memo_screen(id),
                folded: s.memo_folded(id),
                pinned: s.memo_pinned(id),
            });
        });
        close_sticky(&ctx.stickies, id); // clean up an open sticky
        // Drop its pin and its window too, or settings.json accumulates the ids of
        // memos that no longer exist. Only a local delete can do this; one that
        // arrives over sync leaves its entry behind, which costs a string and nothing
        // else. The undo below writes them back from `DESK_PLACE`.
        let mut settings = ctx.settings.borrow_mut();
        let changed = settings.forget_memo(id);
        if changed {
            settings.save(&ctx.dir);
        }
    }
    let Some(removed) = removed else { return };
    *ctx.undo.borrow_mut() = Some(removed);
    list.set_undo_message(SharedString::from(if is_group {
        t!("ui.list_deleted_group")
    } else {
        t!("ui.list_deleted_memo")
    }));
    // The offer expires: a bar that never goes away is furniture, and one still
    // sitting there tomorrow says nothing about what it would put back.
    let list_weak = list.as_weak();
    let undo = ctx.undo.clone();
    ctx.undo_timer.start(TimerMode::SingleShot, UNDO_WINDOW, move || {
        *undo.borrow_mut() = None;
        if let Some(list) = list_weak.upgrade() {
            list.set_undo_message(SharedString::new());
        }
    });
}

/// The undo bar's button: writes the last delete back.
fn wire_undo(ctx: &Ctx, ui: &Ui) {
    let list = &ui.list;
    let ctx = ctx.clone();
    let list_weak = list.as_weak();
    let undo = ctx.undo.clone();
    let undo_timer = ctx.undo_timer.clone();
    list.on_undo_delete(move || {
        touch(&ctx);
        let Some(deleted) = undo.borrow_mut().take() else { return };
        undo_timer.stop();
        if let Some(list) = list_weak.upgrade() {
            list.set_undo_message(SharedString::new());
        }
        let Some(mut guard) = ctx.vault_mut() else { return };
        let v = &mut *guard;
        if let Err(e) = v.undelete(&deleted) {
            diag!("undo failed: {e}");
            list::report_write_failure(&e);
            return;
        }
        refresh_list(v, &ctx.model, &ctx.collapsed.borrow(), &ctx.query.borrow());
        drop(guard);
        if let ymemo_core::vault::Deleted::Memo(memo) = &deleted {
            put_back_on_desk(&ctx, memo);
        }
    });
}

/// Where a deleted memo's note stood, while its delete can still be taken back.
struct DeskPlace {
    id: String,
    window: Option<[i32; 4]>,
    screen: Option<crate::screens::Screen>,
    folded: bool,
    pinned: bool,
}

thread_local! {
    /// The last deleted memo's place on the desk; `None` when it was not open. Holds no memo
    /// text, only an id and a rectangle.
    static DESK_PLACE: RefCell<Option<DeskPlace>> = const { RefCell::new(None) };
}

/// An undone delete of a note that was open opens it again, where it was, folded and pinned
/// as it was. Taking a delete back used to bring the memo back to the list only, and the note
/// someone had just been looking at stayed gone from the desk.
fn put_back_on_desk(ctx: &Ctx, memo: &ymemo_core::Memo) {
    let Some(place) = DESK_PLACE.with(|p| p.borrow_mut().take()) else { return };
    if place.id != memo.id {
        return;
    }
    {
        let mut s = ctx.settings.borrow_mut();
        if let Some(g) = place.window {
            s.set_memo_window(&memo.id, g);
        }
        if let Some(screen) = place.screen {
            s.set_memo_screen(&memo.id, screen);
        }
        s.set_memo_folded(&memo.id, place.folded);
        s.set_memo_pinned(&memo.id, place.pinned);
        s.save(&ctx.dir);
    }
    if let Err(e) = open_sticky(ctx, memo, false) {
        diag!("could not reopen the note: {e}");
    }
}

/// Find. The model is rebuilt from the query, so every later refresh honours it.
fn wire_search(ctx: &Ctx, ui: &Ui) {
    let list = &ui.list;
    let ctx = ctx.clone();
    list.on_search(move |query| {
        touch(&ctx);
        *ctx.query.borrow_mut() = query.to_string();
        let Some(mut guard) = ctx.vault_mut() else { return };
        let v = &mut *guard;
        refresh_list(v, &ctx.model, &ctx.collapsed.borrow(), &ctx.query.borrow());
    });
}

/// Groups: create, expand/collapse, rename, drag to move.
fn wire_new_group(ctx: &Ctx, ui: &Ui) {
    let list = &ui.list;
    let ctx = ctx.clone();
    let list_weak = list.as_weak();
    list.on_new_group(move || {
        touch(&ctx);
        list::clear_search(&ctx);
        let group = ymemo_core::Group::new(t!("msg.new_group_name"));
        {
            let Some(mut guard) = ctx.vault_mut() else { return };
            let v = &mut *guard;
            if let Err(e) = v.upsert_group(&group) {
                diag!("could not create the group: {e}");
                list::report_write_failure(&e);
                return;
            }
            refresh_list(v, &ctx.model, &ctx.collapsed.borrow(), &ctx.query.borrow());
        }
        // Go straight into rename mode so the user can type.
        if let Some(w) = list_weak.upgrade() {
            w.set_editing_text(SharedString::from(group.name.clone()));
            w.set_editing_id(SharedString::from(group.id.clone()));
            // Escape in that box un-makes it; see `fresh-group-id` in `list.slint`.
            w.set_fresh_group_id(SharedString::from(group.id));
        }
    });
}

/// A folder made and immediately backed out of.
fn wire_discard_group(ctx: &Ctx, ui: &Ui) {
    let list = &ui.list;
    let ctx = ctx.clone();
    list.on_discard_group(move |id| {
        touch(&ctx);
        // **On the next event-loop turn**, not now. This is called from the key handler of
        // the name box inside the row being removed, and rebuilding the model here tears
        // that box down while it is still handling its own key — which panics inside the
        // generated code. Same reason `close_sticky` defers dropping a window.
        let ctx = ctx.clone();
        slint::Timer::single_shot(Duration::ZERO, move || {
            let Some(mut guard) = ctx.vault_mut() else { return };
            let v = &mut *guard;
            // A failure is worth a line but not a notice: nothing the user wrote is at
            // stake and the folder is empty.
            if let Err(e) = v.delete_group(id.as_str()) {
                diag!("could not discard the new folder: {e}");
                return;
            }
            refresh_list(v, &ctx.model, &ctx.collapsed.borrow(), &ctx.query.borrow());
        });
    });
}

/// Opens or shuts a folder. Which ones are shut is device-local view state.
fn wire_toggle_group(ctx: &Ctx, ui: &Ui) {
    let list = &ui.list;
    let ctx = ctx.clone();
    list.on_toggle_group(move |id| {
        touch(&ctx);
        {
            let mut collapsed = ctx.collapsed.borrow_mut();
            if !collapsed.remove(id.as_str()) {
                collapsed.insert(id.to_string());
            }
        }
        let Some(guard) = ctx.vault_ref() else { return };
        let v = &*guard;
        refresh_list(v, &ctx.model, &ctx.collapsed.borrow(), &ctx.query.borrow());
    });
}

/// Renames a folder from its row.
fn wire_rename_group(ctx: &Ctx, ui: &Ui) {
    let list = &ui.list;
    let ctx = ctx.clone();
    list.on_rename_group(move |id, name| {
        touch(&ctx);
        // The box was seeded from the row, which is drawn in the shape Slint can render;
        // what gets stored is the shape everything else uses. See `hangul.rs`.
        let name = SharedString::from(crate::hangul::from_slint(&name));
        let Some(mut guard) = ctx.vault_mut() else { return };
        let v = &mut *guard;
        let Ok(Some(mut g)) = v.store().get_group(&id) else { return };
        if g.name == name.as_str() {
            return;
        }
        g.name = name.to_string();
        g.updated_at = now_millis();
        if let Err(e) = v.upsert_group(&g) {
            diag!("could not rename the group: {e}");
            list::report_write_failure(&e);
            return;
        }
        refresh_list(v, &ctx.model, &ctx.collapsed.borrow(), &ctx.query.borrow());
    });
}

/// Rename the vault. The name is in the synced document, so this reaches every
/// paired device the way a memo does.
fn wire_rename_vault(ctx: &Ctx, ui: &Ui) {
    let list = &ui.list;
    let ctx = ctx.clone();
    let weak = list.as_weak();
    list.on_rename_vault(move |name| {
        touch(&ctx);
        let name = SharedString::from(crate::hangul::from_slint(&name));
        let stored = {
            let Some(mut guard) = ctx.vault_mut() else { return };
            let v = &mut *guard;
            if let Err(e) = v.set_name(name.as_str()) {
                diag!("could not rename the vault: {e}");
                return;
            }
            v.name()
        };
        // Read back what was stored rather than what was typed: the core trims it and
        // cuts it to length, and the heading must show the name that actually synced.
        if let Some(list) = weak.upgrade() {
            list::set_vault_name(&list, &stored);
        }
    });
}

/// Dragging a row onto a folder moves it there.
fn wire_move_row(ctx: &Ctx, ui: &Ui) {
    let list = &ui.list;
    let ctx = ctx.clone();
    list.on_move_row(move |src, dst| {
        touch(&ctx);
        move_row(&ctx, src, dst);
    });
}

/// The right-click menu's "Move to".
fn wire_move_to_folder(ctx: &Ctx, ui: &Ui) {
    let list = &ui.list;
    let ctx = ctx.clone();
    list.on_move_to_folder(move |id, is_group, folder| {
        touch(&ctx);
        list::move_to_folder(&ctx, &id, is_group, &folder);
    });
}

/// Dragging a memo into a gap between rows: the folder's own arrangement.
fn wire_reorder(ctx: &Ctx, ui: &Ui) {
    let list = &ui.list;
    let ctx = ctx.clone();
    list.on_reorder_row(move |src, gap| {
        touch(&ctx);
        list::reorder_row(&ctx, src, gap);
    });
}

/// Recolouring a row (folder or memo) from the list.
fn wire_recolor(ctx: &Ctx, ui: &Ui) {
    let list = &ui.list;
    let ctx = ctx.clone();
    list.on_set_row_color(move |id, is_group, color| {
        touch(&ctx);
        list::set_row_color(&ctx, &id, is_group, &color);
    });
}

/// The list's own lock button.
fn wire_lock_now(ctx: &Ctx, ui: &Ui, unlocked: &Rc<Cell<bool>>) {
    let lock = &ui.lock;
    let list = &ui.list;
    let ctx = ctx.clone();
    let lock_weak = lock.as_weak();
    let list_weak = list.as_weak();
    let unlocked = unlocked.clone();
    list.on_lock_now(move || {
        let (Some(lock), Some(list)) = (lock_weak.upgrade(), list_weak.upgrade()) else {
            return;
        };
        lock_now(&ctx, &lock, &list, &unlocked);
    });
}

/// Closing the list on a desktop with no tray. See `quit_if_last_window`.
fn wire_close(ctx: &Ctx, ui: &Ui) {
    let list = &ui.list;
    let ctx = ctx.clone();
    let list_weak = list.as_weak();
    list.window().on_close_requested(move || {
        if let Some(list) = list_weak.upgrade() {
            // Recorded here as well as on the timer: this is the last chance to see where
            // the window was before it goes.
            sticky::remember_geometry(&ctx, &list);
            sticky::quit_if_last_window(&ctx);
        }
        slint::CloseRequestResponse::HideWindow
    });
}
