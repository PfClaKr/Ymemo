//! History window: the past versions of one memo or folder, and putting one back.
//!
//! The revisions come from `ymemo_core::history`, which reads them out of the change logs.
//! Nothing here caches them: a history is read when the window opens and again after a
//! restore, because a restore is itself a new revision.

use std::cell::RefCell;
use std::rc::Rc;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use ymemo_core::history::{Entity, Revision, RevisionKind};
use ymemo_i18n::t;

use crate::state::{touch, Ctx};
use crate::sticky::format_created_at;
use crate::window::present;
use crate::{HistoryWindow, RevisionRow};

/// What the open history window is showing. `None` while it is closed.
pub(crate) type Subject = Rc<RefCell<Option<(Entity, String)>>>;

/// Connects the window's callbacks. `subject` is shared with the callers that open it.
pub(crate) fn wire(ctx: &Ctx, win: &HistoryWindow, subject: &Subject) {
    {
        let weak = win.as_weak();
        win.on_close_requested(move || {
            if let Some(w) = weak.upgrade() {
                let _ = w.hide();
            }
        });
    }
    {
        let ctx = ctx.clone();
        let weak = win.as_weak();
        let subject = subject.clone();
        win.on_restore(move |index| {
            let Some(w) = weak.upgrade() else { return };
            let Some((entity, id)) = subject.borrow().clone() else { return };
            touch(&ctx);

            // Re-read rather than trusting a list that may be a restore old; the index is
            // into the core's own ordering, so it has to come from the same source.
            let restored = {
                let mut guard = ctx.vault.borrow_mut();
                let Some(v) = guard.as_mut() else { return };
                match v.history(entity, &id) {
                    Ok(revisions) => match revisions.get(index as usize) {
                        Some(rev) => v.restore(entity, &id, rev),
                        None => Err(anyhow::anyhow!(t!("msg.history_gone"))),
                    },
                    Err(e) => Err(e),
                }
            };
            match restored {
                Ok(()) => w.set_status(SharedString::from(t!("msg.history_restored"))),
                Err(e) => {
                    w.set_status(SharedString::from(format!("{e}")));
                    return;
                }
            }
            // The restore is now the newest revision, and the list has to show it.
            refresh(&ctx, &w, entity, &id);
            crate::list::refresh_after_restore(&ctx, entity, &id);
        });
    }
}

/// Opens the window on one memo or folder.
pub(crate) fn show(ctx: &Ctx, win: &HistoryWindow, subject: &Subject, entity: Entity, id: &str) {
    *subject.borrow_mut() = Some((entity, id.to_string()));
    win.set_status(SharedString::new());
    win.set_selected(-1);
    refresh(ctx, win, entity, id);
    present(win);
}

/// Reloads the revisions and the heading.
fn refresh(ctx: &Ctx, win: &HistoryWindow, entity: Entity, id: &str) {
    let guard = ctx.vault.borrow();
    let Some(v) = guard.as_ref() else { return };

    let name = match entity {
        Entity::Memo => v.store().get(id).ok().flatten().map(|m| m.title),
        Entity::Group => v.store().get_group(id).ok().flatten().map(|g| g.name),
    };
    win.set_subject(SharedString::from(match name {
        Some(n) if !n.trim().is_empty() => n,
        // Deleted, or never named: say so rather than showing an empty heading.
        _ => t!("ui.list_memo_untitled"),
    }));

    let revisions = match v.history(entity, id) {
        Ok(r) => r,
        Err(e) => {
            win.set_status(SharedString::from(format!("{e}")));
            return;
        }
    };
    let device_id = v.device_id().to_string();
    // Newest first: the version you want back is nearly always a recent one.
    let rows: Vec<RevisionRow> = revisions
        .iter()
        .enumerate()
        .rev()
        .map(|(i, r)| row(i, r, entity, &device_id))
        .collect();
    win.set_revisions(ModelRc::new(VecModel::from(rows)));
}

/// One core revision as a display row.
fn row(index: usize, rev: &Revision, entity: Entity, this_device: &str) -> RevisionRow {
    let kind = match rev.kind {
        RevisionKind::Created => t!("ui.history_created"),
        RevisionKind::Edited => t!("ui.history_edited"),
        RevisionKind::Deleted => t!("ui.history_deleted"),
    };
    // Field names are the document's, so they are translated for display here. `created_at`
    // is left out: it only moves when a deleted memo is brought back, and listing it beside
    // the fields the user actually changed is noise.
    let changed: Vec<String> = rev
        .changed
        .iter()
        .filter(|f| *f != "created_at")
        .map(|f| field_label(f))
        .collect();
    let (heading, body) = match entity {
        Entity::Memo => (rev.field("title").to_string(), rev.field("body").to_string()),
        Entity::Group => (rev.field("name").to_string(), String::new()),
    };
    RevisionRow {
        index: index as i32,
        when: SharedString::from(format_created_at(rev.at)),
        device: SharedString::from(if rev.device == this_device {
            t!("ui.history_this_device")
        } else {
            t!("ui.history_other_device")
        }),
        kind: SharedString::from(kind),
        changed: SharedString::from(changed.join(", ")),
        heading: SharedString::from(heading),
        body: SharedString::from(body),
        color: SharedString::from(rev.field("color")),
        restorable: rev.kind != RevisionKind::Deleted,
    }
}

/// A document field name in the user's language.
fn field_label(field: &str) -> String {
    match field {
        "title" => t!("ui.history_field_title"),
        "body" => t!("ui.history_field_body"),
        "name" => t!("ui.history_field_name"),
        "color" => t!("ui.history_field_color"),
        "opacity" => t!("ui.history_field_opacity"),
        "group_id" | "parent_id" => t!("ui.history_field_folder"),
        // created_at only moves on a restore of a resurrected memo; not worth a string.
        other => other.to_string(),
    }
}
