//! Where a memo or a folder has been: every past version, read out of the change logs.
//!
//! ## Why not Syncthing's file versioning
//!
//! Syncthing can keep copies of files it replaces, and it is tempting to call those the
//! history. They are not. A `.ymlog` is **append-only**, so the copy Syncthing would keep is
//! a prefix of the file it already has — the same edits, stored twice, with no record of
//! which memo they belong to. Worse, the versions live outside the encryption's reach as far
//! as meaning goes: to read one you would have to decrypt it and replay it anyway, which is
//! exactly what this module does against the live log, only without the duplicate on disk.
//!
//! The history is already in the vault. Every edit is an automerge change carrying the
//! device that made it and when, no device ever rewrites another's log, and nothing is
//! deleted — [`crate::vault`]. This module reads that.
//!
//! Syncthing's versioning is still worth having, for a different job: an own log truncated
//! by a full disk or a crash syncs that truncation everywhere, and a kept copy is the only
//! way back. [`crate::sync::Syncthing::ensure_versioning`] turns it on for that reason, and
//! calls it what it is — a backup, not a history.
//!
//! ## How it is read
//!
//! The changes are replayed into a fresh document one at a time, and after each one the
//! entity's fields are read back. A change that leaves them untouched — most changes, since
//! they belong to other memos — produces no revision. This costs one pass over the log per
//! query, which for a memo app is nothing, and it keeps the live document out of it.

use std::collections::BTreeMap;

use anyhow::Result;
use automerge::{AutoCommit, ObjType, ReadDoc, ScalarValue, Value, ROOT};

/// Which map in the document a history is being read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entity {
    Memo,
    Group,
}

impl Entity {
    /// The `ROOT` key holding this kind.
    pub(crate) fn root_key(self) -> &'static str {
        match self {
            Entity::Memo => "memos",
            Entity::Group => "groups",
        }
    }

    /// The fields a revision records, in display order. Anything not listed here — a photo's
    /// display width, say — is left to its own record.
    pub(crate) fn fields(self) -> &'static [&'static str] {
        match self {
            Entity::Memo => &["title", "body", "color", "opacity", "group_id", "created_at"],
            Entity::Group => &["name", "parent_id", "color", "created_at"],
        }
    }
}

/// What a revision did to the entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionKind {
    /// The first revision: the memo or folder appeared.
    Created,
    /// A later revision that changed at least one field.
    Edited,
    /// It was removed. Its earlier revisions stay readable, and it can be restored.
    Deleted,
}

/// One point in an entity's past.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision {
    /// When the change was made, in unix epoch **millis**. Automerge records seconds, so
    /// this is that value scaled up; two edits in the same second share a timestamp.
    pub at: i64,
    /// The device that made it — the actor id, which the vault sets to the device id.
    /// Empty when the actor is not a device id (a document written by another tool).
    pub device: String,
    pub kind: RevisionKind,
    /// Every field's value **as of this revision**, not just the ones that moved.
    /// Missing fields are absent rather than empty. Empty for a [`RevisionKind::Deleted`].
    pub fields: BTreeMap<String, String>,
    /// The fields this revision actually changed, in the order the entity lists them.
    pub changed: Vec<String>,
}

impl Revision {
    /// A field's value at this revision, or `""` when it carried none.
    pub fn field(&self, name: &str) -> &str {
        self.fields.get(name).map(String::as_str).unwrap_or("")
    }
}

/// Replays `changes` and returns the revisions that touched `id`.
///
/// `changes` must already be in causal order, which is what `AutoCommit::get_changes` gives.
pub(crate) fn replay(
    changes: Vec<automerge::Change>,
    entity: Entity,
    id: &str,
) -> Result<Vec<Revision>> {
    let mut doc = AutoCommit::new();
    let mut previous: Option<BTreeMap<String, String>> = None;
    let mut out: Vec<Revision> = Vec::new();

    for change in changes {
        // Automerge timestamps are seconds; the rest of the model speaks millis.
        let at = change.timestamp().saturating_mul(1000);
        let device = String::from_utf8(change.actor_id().to_bytes().to_vec()).unwrap_or_default();
        doc.apply_changes([change])?;

        let current = snapshot(&doc, entity, id);
        let kind = match (&previous, &current) {
            (None, Some(_)) => RevisionKind::Created,
            (Some(before), Some(after)) if before != after => RevisionKind::Edited,
            (Some(_), None) => RevisionKind::Deleted,
            // Either it does not exist yet, or this change was about something else.
            _ => continue,
        };

        let changed = match (&previous, &current) {
            (Some(before), Some(after)) => entity
                .fields()
                .iter()
                .filter(|f| before.get(**f) != after.get(**f))
                .map(|f| (*f).to_string())
                .collect(),
            // A creation "changes" whatever it arrived with; a deletion changes nothing.
            (None, Some(after)) => entity
                .fields()
                .iter()
                .filter(|f| after.contains_key(**f))
                .map(|f| (*f).to_string())
                .collect(),
            _ => Vec::new(),
        };

        out.push(Revision {
            at,
            device,
            kind,
            fields: current.clone().unwrap_or_default(),
            changed,
        });
        previous = current;
    }
    Ok(out)
}

/// The entity's fields as the document currently holds them, or `None` when it is not there.
fn snapshot(doc: &AutoCommit, entity: Entity, id: &str) -> Option<BTreeMap<String, String>> {
    let Ok(Some((Value::Object(ObjType::Map), map))) = doc.get(ROOT, entity.root_key()) else {
        return None;
    };
    let Ok(Some((Value::Object(ObjType::Map), obj))) = doc.get(&map, id) else {
        return None;
    };
    let mut fields = BTreeMap::new();
    for name in entity.fields() {
        if let Ok(Some((Value::Scalar(s), _))) = doc.get(&obj, *name) {
            let value = match s.as_ref() {
                ScalarValue::Str(v) => v.to_string(),
                ScalarValue::Int(v) => v.to_string(),
                ScalarValue::Uint(v) => v.to_string(),
                ScalarValue::Boolean(v) => v.to_string(),
                // Nothing else appears in these maps today; skip rather than invent a shape.
                _ => continue,
            };
            fields.insert((*name).to_string(), value);
        }
    }
    Some(fields)
}
