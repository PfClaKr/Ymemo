//! Writing memos and folders into the document: edits, moves, deletes and undeletes.

use anyhow::Result;
use automerge::{transaction::Transactable, ObjType, ReadDoc, Value};

use crate::{Group, Memo};

use super::doc::{put_i64_if_changed, put_str_if_changed, put_text_if_changed};
use super::{Deleted, Vault};

impl Vault {
    /// Inserts or updates a memo, writing only changed fields so merges stay field-level.
    pub fn upsert(&mut self, memo: &Memo) -> Result<()> {
        // Before anything is written: a photo standing in the writing is anchored to a line
        // number, and this is the one moment those line numbers can move. See
        // `reanchor_attachments`.
        if let Some(old) = self.store.get(&memo.id)? {
            if old.body != memo.body {
                self.reanchor_attachments(&memo.id, &old.body, &memo.body)?;
            }
        }
        let order_key = self.key_for_new(memo)?;
        let memos = self.memos_obj()?;
        let obj = match self.doc.get(&memos, &memo.id)? {
            Some((Value::Object(ObjType::Map), id)) => id,
            _ => self.doc.put_object(&memos, &memo.id, ObjType::Map)?,
        };
        // The title is a **label**, not prose: see `put_text_if_changed` for why merging one
        // is worse than losing one.
        put_str_if_changed(&mut self.doc, &obj, "title", &memo.title)?;
        put_text_if_changed(&mut self.doc, &obj, "body", &memo.body)?;
        put_str_if_changed(&mut self.doc, &obj, "color", &memo.color)?;
        put_i64_if_changed(&mut self.doc, &obj, "opacity", crate::clamp_opacity(memo.opacity))?;
        put_str_if_changed(&mut self.doc, &obj, "group_id", &memo.group_id)?;
        put_str_if_changed(&mut self.doc, &obj, "order_key", &order_key)?;
        put_i64_if_changed(&mut self.doc, &obj, "created_at", memo.created_at)?;
        put_i64_if_changed(&mut self.doc, &obj, "updated_at", memo.updated_at)?;

        self.append_local_change()?;
        if order_key == memo.order_key {
            self.store.upsert(memo)
        } else {
            self.store.upsert(&Memo { order_key, ..memo.clone() })
        }
    }

    /// Where a memo with no place of its own goes.
    ///
    /// Above everything, in a folder that has been arranged. In one that has not, **no key at
    /// all**: every memo there is still ordered by `updated_at`, the newest first, so a new
    /// memo is already at the top and inventing a key would be the one write that pushed it
    /// down. Arranging a folder is what gives its memos keys ([`Vault::move_memo`]).
    pub(super) fn key_for_new(&self, memo: &Memo) -> Result<String> {
        // A memo that has just been put in another folder is new *to that folder*, whatever
        // key it had: that key is a position among memos it no longer sits with, and carrying
        // it over drops the memo at an arbitrary point in its new folder. Every way of moving
        // one that is not a deliberate placement comes through here — the list's drag onto a
        // folder, and the phone's move — while `move_memo`, which is a placement, writes the
        // cache directly and never asks.
        let moved = matches!(self.store.get(&memo.id)?, Some(old) if old.group_id != memo.group_id);
        if !memo.order_key.is_empty() && !moved {
            return Ok(memo.order_key.clone());
        }
        Ok(match self.store.first_order_key(&memo.group_id)? {
            Some(top) => crate::order::between(None, Some(&top)),
            None => String::new(),
        })
    }

    /// Moves a memo to sit between two others, arranging its folder if nothing ever has.
    ///
    /// `after` is the memo it goes below and `before` the one it goes above, both by id and
    /// both from the destination folder; `None` on either side means the end of the folder.
    /// Passing a folder different from the memo's current one moves it there and places it in
    /// one write, which is what a drag from one folder to another is.
    ///
    /// **The first arrangement gives every memo in the folder a key.** Until then they are
    /// ordered by `updated_at` and have none, so there is nothing to sit between; the folder
    /// is stamped with the order it is already being shown in, and only then is the moved
    /// memo placed. That write is one change carrying the whole folder — the alternative is
    /// arranging on open, which would write to the vault on a device that only came to read.
    ///
    /// Timestamps are left alone. Rearranging is not editing, and bumping `updated_at` would
    /// scramble the unarranged folder this is in the middle of stamping.
    pub fn move_memo(
        &mut self,
        id: &str,
        group_id: &str,
        after: Option<&str>,
        before: Option<&str>,
    ) -> Result<()> {
        let Some(mut memo) = self.store.get(id)? else {
            anyhow::bail!("no memo {id} to move");
        };

        // Stamp the destination folder if it has never been arranged. The memo being moved is
        // left out: it is about to be given a key of its own, and if it is arriving from
        // another folder it is not there to stamp.
        let mut siblings = self.store.in_group(group_id)?;
        siblings.retain(|m| m.id != id);
        if siblings.iter().any(|m| m.order_key.is_empty()) {
            let keys = crate::order::spread(siblings.len());
            for (m, key) in siblings.iter_mut().zip(keys) {
                self.put_order_key(&m.id, &key)?;
                m.order_key = key;
            }
        }

        let key_of = |neighbour: Option<&str>| -> Option<String> {
            let id = neighbour?;
            siblings.iter().find(|m| m.id == id).map(|m| m.order_key.clone())
        };
        memo.order_key = crate::order::between(key_of(after).as_deref(), key_of(before).as_deref());
        memo.group_id = group_id.to_string();

        // The move itself, again without touching `updated_at`.
        let memos = self.memos_obj()?;
        if let Some((Value::Object(ObjType::Map), obj)) = self.doc.get(&memos, id)? {
            put_str_if_changed(&mut self.doc, &obj, "group_id", &memo.group_id)?;
            put_str_if_changed(&mut self.doc, &obj, "order_key", &memo.order_key)?;
        }
        self.append_local_change()?;
        self.store.upsert(&memo)
    }

    /// One memo's arrangement key in the document and the cache, and nothing else.
    pub(super) fn put_order_key(&mut self, id: &str, key: &str) -> Result<()> {
        let memos = self.memos_obj()?;
        if let Some((Value::Object(ObjType::Map), obj)) = self.doc.get(&memos, id)? {
            put_str_if_changed(&mut self.doc, &obj, "order_key", key)?;
        }
        self.store.set_order_key(id, key)
    }

    /// Inserts or updates a group; renames and re-parenting both go through here.
    pub fn upsert_group(&mut self, group: &Group) -> Result<()> {
        let groups = self.groups_obj()?;
        let obj = match self.doc.get(&groups, &group.id)? {
            Some((Value::Object(ObjType::Map), id)) => id,
            _ => self.doc.put_object(&groups, &group.id, ObjType::Map)?,
        };
        // A label, like a memo's title — last-write-wins on purpose.
        put_str_if_changed(&mut self.doc, &obj, "name", &group.name)?;
        put_str_if_changed(&mut self.doc, &obj, "parent_id", &group.parent_id)?;
        put_str_if_changed(&mut self.doc, &obj, "color", &group.color)?;
        put_i64_if_changed(&mut self.doc, &obj, "created_at", group.created_at)?;
        put_i64_if_changed(&mut self.doc, &obj, "updated_at", group.updated_at)?;

        self.append_local_change()?;
        self.store.upsert_group(group)
    }

    /// Deletes a group and **lifts** its groups and memos to the parent instead of deleting
    /// them — removing a folder must not remove its memos.
    ///
    /// Returns what it removed, so the caller can offer to put it back; see [`Deleted`].
    pub fn delete_group(&mut self, id: &str) -> Result<Option<Deleted>> {
        let Some(group) = self.store.get_group(id)? else {
            return Ok(None);
        };
        let parent = group.parent_id.clone();

        // Lift child groups.
        let children: Vec<Group> = self
            .store
            .list_groups()?
            .into_iter()
            .filter(|g| g.parent_id == id)
            .collect();
        let mut lifted_groups = Vec::new();
        for mut child in children {
            lifted_groups.push(child.id.clone());
            child.parent_id = parent.clone();
            child.updated_at = crate::now_millis();
            self.upsert_group(&child)?;
        }
        // Lift the memos.
        let memos: Vec<Memo> = self
            .store
            .list()?
            .into_iter()
            .filter(|m| m.group_id == id)
            .collect();
        let mut lifted_memos = Vec::new();
        for mut memo in memos {
            lifted_memos.push(memo.id.clone());
            memo.group_id = parent.clone();
            memo.updated_at = crate::now_millis();
            self.upsert(&memo)?;
        }

        let groups = self.groups_obj()?;
        if self.doc.get(&groups, id)?.is_some() {
            self.doc.delete(&groups, id)?;
        }
        self.append_local_change()?;
        self.store.delete_group(id)?;
        Ok(Some(Deleted::Group { group, lifted_groups, lifted_memos }))
    }

    /// Deletes a memo.
    ///
    /// Returns the memo it removed, so the caller can offer to put it back; see [`Deleted`].
    /// Attachments are left alone — they point at the memo rather than the other way round,
    /// so an undelete brings the photos back with it.
    pub fn delete(&mut self, id: &str) -> Result<Option<Deleted>> {
        let Some(memo) = self.store.get(id)? else {
            return Ok(None);
        };
        let memos = self.memos_obj()?;
        if self.doc.get(&memos, id)?.is_some() {
            self.doc.delete(&memos, id)?;
        }
        self.append_local_change()?;
        self.store.delete(id)?;
        Ok(Some(Deleted::Memo(memo)))
    }

    /// Puts back what [`Vault::delete`] or [`Vault::delete_group`] removed.
    ///
    /// Like [`Vault::restore`], this is an ordinary edit and not a rewrite: the deletion
    /// stays in the log and in the history, and the undo becomes the newest revision. So two
    /// devices that both act on the same deletion merge instead of fighting, and an undo is
    /// itself undoable by deleting again.
    ///
    /// A folder's contents are put back only where they still sit where the deletion left
    /// them; anything moved in the meantime is left where the user put it.
    pub fn undelete(&mut self, deleted: &Deleted) -> Result<()> {
        let now = crate::now_millis();
        match deleted {
            Deleted::Memo(memo) => {
                let mut memo = memo.clone();
                memo.updated_at = now;
                self.upsert(&memo)
            }
            Deleted::Group { group, lifted_groups, lifted_memos } => {
                let mut group = group.clone();
                group.updated_at = now;
                self.upsert_group(&group)?;
                for id in lifted_groups {
                    match self.store.get_group(id)? {
                        Some(mut child) if child.parent_id == group.parent_id => {
                            child.parent_id = group.id.clone();
                            child.updated_at = now;
                            self.upsert_group(&child)?;
                        }
                        _ => {}
                    }
                }
                for id in lifted_memos {
                    match self.store.get(id)? {
                        Some(mut memo) if memo.group_id == group.parent_id => {
                            memo.group_id = group.id.clone();
                            memo.updated_at = now;
                            self.upsert(&memo)?;
                        }
                        _ => {}
                    }
                }
                Ok(())
            }
        }
    }
}
