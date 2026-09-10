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
//! way back. [`crate::sync::Syncthing::set_folder_versioning`] turns it on for that reason, and
//! calls it what it is — a backup, not a history.
//!
//! ## How it is read
//!
//! The way `git log -- one/file` reads: walk the changes in order, ask each one **whether it
//! touched this memo**, and reconstruct only the ones that did. Nothing is replayed — the
//! live document is read at each of those points with `get_at`, which takes a place in the
//! history and reads the fields there.
//!
//! What this replaces was a second copy of the whole vault: every change applied to a
//! throwaway document one at a time, and after each one all six fields read back — 31,000
//! reconstructions to answer a question about the thirty that concerned the memo. On a vault
//! with a year in it that was three seconds of the UI thread on a single click. Measured.
//!
//! **Which diff you ask for is the whole difference**, and the obvious two are both traps:
//!
//! - `diff(before, after)` walks from the document root, so it costs the whole vault on every
//!   change — 5.1 s where the replay it replaced took 0.27 s. Nineteen times *worse*.
//! - `diff_obj` on the map the entity lives in costs every memo in the vault per change,
//!   because that is how many keys the map has: 0.44 s.
//!
//! What works is asking about the entity and nothing else: one key lookup to see whether it
//! is there at this point, and `diff_obj` scoped to its own object when it is. Both cost the
//! entity's own handful of fields. 0.27 s -> 0.11 s on an ordinary vault, 3.2 s -> 1.6 s on
//! a very large one. Measured, all of it — the numbers are why the shape is what it is.
//!
//! The point read at is the **causal frontier** after each change, not the change's own hash:
//! two devices editing at once produce changes neither has seen, and reading one alone shows
//! that branch without the other's edit. The newest revision would then be missing an edit
//! that `restore` writes back field by field — putting the latest version back would undo it.

use std::collections::BTreeMap;

use anyhow::Result;
use automerge::{AutoCommit, ChangeHash, ObjType, ReadDoc, ScalarValue, Value, ROOT};

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

/// The revisions of one entity, oldest first.
///
/// Walks the document's changes in causal order and stops only at the ones that touched
/// `id` — see the note at the top of this file for why that is the whole trick.
pub(crate) fn revisions(doc: &mut AutoCommit, entity: Entity, id: &str) -> Result<Vec<Revision>> {
    struct Step {
        hash: ChangeHash,
        deps: Vec<ChangeHash>,
        /// Automerge timestamps are seconds; the rest of the model speaks millis.
        at: i64,
        device: String,
    }
    let changes: Vec<Step> = doc
        .get_changes(&[])
        .iter()
        .map(|c| Step {
            hash: c.hash(),
            deps: c.deps().to_vec(),
            at: c.timestamp().saturating_mul(1000),
            device: String::from_utf8(c.actor_id().to_bytes().to_vec()).unwrap_or_default(),
        })
        .collect();

    let mut previous: Option<BTreeMap<String, String>> = None;
    let mut out: Vec<Revision> = Vec::new();
    let mut before: Vec<ChangeHash> = Vec::new();
    let mut after: Vec<ChangeHash> = Vec::new();

    // The map the entity lives in, resolved once; `diff` from the document root costs a walk
    // of the whole document per change, which is the thing being avoided.
    let map = match doc.get(ROOT, entity.root_key()) {
        Ok(Some((Value::Object(ObjType::Map), m))) => m,
        _ => return Ok(out),
    };
    // The entity's own object, once it has been created. Not known before that, and gone
    // again after a delete.
    let mut obj: Option<automerge::ObjId> = match doc.get(&map, id) {
        Ok(Some((Value::Object(ObjType::Map), o))) => Some(o),
        _ => None,
    };

    for step in changes {
        // The frontier after this change: everything seen so far, with the changes it
        // supersedes dropped. **Not** the change's own hash on its own — that reads the
        // state on one branch, and on two devices editing at once the newest revision then
        // shows one device's work without the other's. Restoring it would quietly undo the
        // other edit, which is the one thing a history must never do.
        after.retain(|h| !step.deps.contains(h));
        after.push(step.hash);
        // Is it there at this point? One key lookup — cheaper than diffing the map it lives
        // in, which costs a walk of every memo in the vault on every change.
        let here = match doc.get_at(&map, id, &after) {
            Ok(Some((Value::Object(ObjType::Map), o))) => Some(o),
            _ => None,
        };
        let touched = match (&obj, &here) {
            // Appeared, or was deleted: either way this change is one of ours.
            (None, Some(_)) | (Some(_), None) => true,
            // Still there: only a change inside it counts, and that diff is scoped to the
            // entity, so it costs its own handful of fields and not the whole document.
            (Some(_), Some(o)) => doc
                .diff_obj(o, &before, &after, true)
                .map(|ps| !ps.is_empty())
                .unwrap_or(false),
            (None, None) => false,
        };
        obj = here;
        before.clone_from(&after);
        if !touched {
            continue; // about some other memo, which is nearly all of them
        }

        let current = snapshot_at(doc, entity, id, &before);
        let kind = match (&previous, &current) {
            (None, Some(_)) => RevisionKind::Created,
            (Some(before), Some(after)) if before != after => RevisionKind::Edited,
            (Some(_), None) => RevisionKind::Deleted,
            // Touched without changing anything this records — a field outside `fields()`.
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
            at: step.at,
            device: step.device,
            kind,
            fields: current.clone().unwrap_or_default(),
            changed,
        });
        previous = current;
    }
    Ok(out)
}

/// The entity's fields as they stood at `heads`, or `None` when it was not there.
fn snapshot_at(
    doc: &AutoCommit,
    entity: Entity,
    id: &str,
    heads: &[ChangeHash],
) -> Option<BTreeMap<String, String>> {
    let Ok(Some((Value::Object(ObjType::Map), map))) =
        doc.get_at(ROOT, entity.root_key(), heads)
    else {
        return None;
    };
    let Ok(Some((Value::Object(ObjType::Map), obj))) = doc.get_at(&map, id, heads) else {
        return None;
    };
    let mut fields = BTreeMap::new();
    for name in entity.fields() {
        match doc.get_at(&obj, *name, heads) {
            // What the user typed is a text object — see `put_text_if_changed` in `vault.rs`.
            // Reading only scalars here left every title and body **blank** in the history.
            Ok(Some((Value::Object(ObjType::Text), text))) => {
                if let Ok(v) = doc.text_at(&text, heads) {
                    fields.insert((*name).to_string(), v);
                }
            }
            Ok(Some((Value::Scalar(s), _))) => {
                if let Some(v) = scalar_to_string(s.as_ref()) {
                    fields.insert((*name).to_string(), v);
                }
            }
            _ => {}
        }
    }
    Some(fields)
}

/// How a field's value reads. Nothing but these appears in these maps today; anything else is
/// skipped rather than given an invented shape.
fn scalar_to_string(s: &ScalarValue) -> Option<String> {
    match s {
        ScalarValue::Str(v) => Some(v.to_string()),
        ScalarValue::Int(v) => Some(v.to_string()),
        ScalarValue::Uint(v) => Some(v.to_string()),
        ScalarValue::Boolean(v) => Some(v.to_string()),
        _ => None,
    }
}
