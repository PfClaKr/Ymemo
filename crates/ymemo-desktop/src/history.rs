//! History window: the past versions of one memo or folder, and putting one back.
//!
//! The revisions come from `ymemo_core::history`, which reads them out of the change logs.
//! Nothing here caches them: a history is read when the window opens and again after a
//! restore, because a restore is itself a new revision.

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use ymemo_core::history::{Entity, Revision, RevisionKind};
use ymemo_i18n::t;

use crate::state::{touch, Ctx};
use crate::window::present;
use crate::{HistoryWindow, ListWindow, RevisionRow};

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
                let Some(mut guard) = ctx.vault_mut() else { return };
                let v = &mut *guard;
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
            //
            // The selection goes to the top — the version just written, which is what the memo
            // now says. Not left where it was: `selected` is an index into a list that has just
            // grown a row at the top, so it would sit on the revision *below* the one it was
            // pointing at, and a second press would put back a version nobody chose.
            refresh(&ctx, &w, entity, &id);
            w.set_selected(0);
            crate::list::refresh_after_restore(&ctx, entity, &id);
        });
    }
}

/// The list's history button: past versions of the memo or folder a row stands for.
pub(crate) fn wire_list(ctx: &Ctx, list: &ListWindow, win: &HistoryWindow, subject: &Subject) {
    let ctx = ctx.clone();
    let win = win.as_weak();
    let subject = subject.clone();
    list.on_show_history(move |id, is_group| {
        touch(&ctx);
        let Some(w) = win.upgrade() else { return };
        let entity = if is_group { Entity::Group } else { Entity::Memo };
        show(&ctx, &w, &subject, entity, &id);
    });
}

/// Opens the window on one memo or folder.
pub(crate) fn show(ctx: &Ctx, win: &HistoryWindow, subject: &Subject, entity: Entity, id: &str) {
    *subject.borrow_mut() = Some((entity, id.to_string()));
    win.set_status(SharedString::new());
    // Opened on the newest version, so the pane beside the list shows something straight away
    // — it used to open on a sentence asking for a click. Up and down step from there.
    win.set_selected(0);
    refresh(ctx, win, entity, id);
    present(win);
}

/// Reloads the revisions and the heading.
fn refresh(ctx: &Ctx, win: &HistoryWindow, entity: Entity, id: &str) {
    // Mutable because reading a memo's past reads the document, and `AutoCommit` settles any
    // pending edit before it hands over its changes.
    let Some(mut guard) = ctx.vault_mut() else { return };
    let v = &mut *guard;

    let name = match entity {
        Entity::Memo => v.store().get(id).ok().flatten().map(|m| m.title),
        Entity::Group => v.store().get_group(id).ok().flatten().map(|g| g.name),
    };
    win.set_subject(SharedString::from(crate::hangul::for_slint(&match name {
        Some(n) if !n.trim().is_empty() => n,
        // Deleted, or never named: say so rather than showing an empty heading.
        _ => t!("ui.list_memo_untitled"),
    })));

    let revisions = match v.history(entity, id) {
        Ok(r) => r,
        Err(e) => {
            win.set_status(SharedString::from(format!("{e}")));
            return;
        }
    };
    let device_id = v.device_id().to_string();
    // Newest first: the version you want back is nearly always a recent one.
    let newest = revisions.len().saturating_sub(1);
    let rows: Vec<RevisionRow> = revisions
        .iter()
        .enumerate()
        .rev()
        .map(|(i, r)| row(i, r, entity, &device_id, i == newest))
        .collect();
    win.set_revisions(ModelRc::new(VecModel::from(rows)));
}

/// One core revision as a display row. `current` is the newest one, which is what the memo
/// already holds: putting it back would change nothing and only add a row, so it is marked
/// rather than offered.
fn row(index: usize, rev: &Revision, entity: Entity, this_device: &str, current: bool) -> RevisionRow {
    let mut kind = match rev.kind {
        RevisionKind::Created => t!("ui.history_created"),
        RevisionKind::Edited => t!("ui.history_edited"),
        RevisionKind::Deleted => t!("ui.history_deleted"),
    };
    if current && rev.kind != RevisionKind::Deleted {
        kind = format!("{kind} · {}", t!("ui.history_current"));
    }
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
        when: SharedString::from(crate::hangul::for_slint(&revision_time(rev.at, chrono::Local::now()))),
        device: SharedString::from(crate::hangul::for_slint(&if rev.device == this_device {
            t!("ui.history_this_device")
        } else {
            t!("ui.history_other_device")
        })),
        kind: SharedString::from(crate::hangul::for_slint(&kind)),
        changed: SharedString::from(crate::hangul::for_slint(&changed.join(", "))),
        heading: SharedString::from(crate::hangul::for_slint(&heading)),
        body: SharedString::from(crate::hangul::for_slint(&body)),
        color: SharedString::from(rev.field("color")),
        restorable: !current && rev.kind != RevisionKind::Deleted,
    }
}

/// When a revision was made, as a person reads a list of them: "오늘 13:04", "어제 18:20",
/// "9월 27일 13:04", and the year only once it is not this one. The time always stays —
/// two revisions a minute apart are what this window is for telling apart.
pub(crate) fn revision_time<Tz: chrono::TimeZone>(millis: i64, now: chrono::DateTime<Tz>) -> String {
    use chrono::{Datelike, Timelike};
    let tz = now.timezone();
    let Some(t) = tz.timestamp_millis_opt(millis).single() else { return String::new() };
    let time = format!("{:02}:{:02}", t.hour(), t.minute());
    let days = now.date_naive().signed_duration_since(t.date_naive()).num_days();
    match days {
        0 => t!("msg.when_today", time = time),
        1 => t!("msg.when_yesterday", time = time),
        _ if t.year() == now.year() => t!("msg.when_date", month = t.month(), day = t.day(), time = time),
        _ => t!("msg.when_date_year", year = t.year(), month = t.month(), day = t.day(), time = time),
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn revisions_read_by_day_and_keep_their_time() {
        let now = Utc.with_ymd_and_hms(2026, 9, 27, 15, 0, 0).unwrap();
        let at = |y, mo, d, h, mi| Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap().timestamp_millis();
        assert_eq!(revision_time(at(2026, 9, 27, 13, 4), now), t!("msg.when_today", time = "13:04"));
        assert_eq!(revision_time(at(2026, 9, 26, 18, 20), now), t!("msg.when_yesterday", time = "18:20"));
        assert_eq!(
            revision_time(at(2026, 3, 1, 9, 5), now),
            t!("msg.when_date", month = 3, day = 1, time = "09:05")
        );
        assert_eq!(
            revision_time(at(2024, 12, 31, 23, 59), now),
            t!("msg.when_date_year", year = 2024, month = 12, day = 31, time = "23:59")
        );
    }
}
