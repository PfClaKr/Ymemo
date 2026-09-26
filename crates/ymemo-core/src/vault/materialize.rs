//! Writing the merged document into the SQLite cache.

use anyhow::Result;
use automerge::{transaction::Transactable, ObjId, ObjType, ReadDoc, Value, ROOT};

use crate::{clamp_permille, clamp_width_em_milli, Attachment, Group, Memo};

use super::Vault;
use super::doc::{get_i64, get_i64_or, get_str_or, get_text, get_text_or};

impl Vault {
    /// Materializes the document into the SQLite cache.
    pub(super) fn materialize(&mut self) -> Result<()> {
        // One transaction for the whole cache, for two reasons. Every statement below was a
        // transaction of its own, so a rebuild cost one fsync per memo, folder, photo and
        // removal — a quarter of a second on an ordinary vault, on the UI thread, every time
        // the merge timer fired. And the cache was *visibly* empty between the clear and the
        // last write, which is what any reader running in between would have seen.
        self.store.begin()?;
        match self.materialize_all() {
            Ok(()) => self.store.commit(),
            Err(e) => {
                // The cache is disposable and the next rebuild writes it again, so putting it
                // back as it was is better than leaving it half-cleared.
                let _ = self.store.rollback();
                Err(e)
            }
        }
    }

    /// Everything [`Vault::materialize`] writes, inside the transaction it opens.
    pub(super) fn materialize_all(&mut self) -> Result<()> {
        // Clears memos, groups **and** attachments, so every one of them has to be written
        // back below — returning early on any of them would leave the cache short.
        self.store.clear_memos()?;
        // A vault can hold groups and no memos at all: a folder made before the first note.
        // Skipping the loop is right; skipping the rest of the rebuild is what used to delete
        // those folders on every merge.
        if let Some((Value::Object(ObjType::Map), memos)) = self.doc.get(ROOT, "memos")? {
            let ids: Vec<String> = self.doc.keys(&memos).collect();
            for id in ids {
                let Some((Value::Object(ObjType::Map), obj)) = self.doc.get(&memos, &id)? else {
                    continue;
                };
                let memo = Memo {
                    id: id.clone(),
                    title: get_text(&self.doc, &obj, "title")?,
                    body: get_text(&self.doc, &obj, "body")?,
                    // color/opacity came later, so old changes may not carry them.
                    color: get_str_or(&self.doc, &obj, "color", crate::DEFAULT_COLOR),
                    opacity: crate::clamp_opacity(get_i64_or(
                        &self.doc,
                        &obj,
                        "opacity",
                        crate::DEFAULT_OPACITY,
                    )),
                    group_id: get_str_or(&self.doc, &obj, "group_id", ""),
                    // Absent on a memo written before folders could be arranged, and on one
                    // from a device that has not been updated. Empty sorts to the top, where
                    // `updated_at` then orders it — exactly the old behaviour.
                    order_key: get_str_or(&self.doc, &obj, "order_key", ""),
                    created_at: get_i64(&self.doc, &obj, "created_at")?,
                    updated_at: get_i64(&self.doc, &obj, "updated_at")?,
                };
                self.store.upsert(&memo)?;
            }
        }
        self.materialize_groups()?;
        self.materialize_revoked()?;
        self.materialize_attachments()
    }

    /// Copies `ROOT.revoked` into the cache. Like the memos, this is rebuilt from scratch on
    /// every merge, so a device un-revoked elsewhere disappears from here by itself.
    pub(super) fn materialize_revoked(&mut self) -> Result<()> {
        // Cleared first, not merged into: this runs after an un-revoke too, and a row that is
        // no longer in the document has to leave the cache with it.
        self.store.clear_revoked()?;
        let Some((Value::Object(ObjType::Map), revoked)) = self.doc.get(ROOT, "revoked")? else {
            return Ok(()); // nothing has ever been removed
        };
        let ids: Vec<String> = self.doc.keys(&revoked).collect();
        for device_id in ids {
            let Some((Value::Object(ObjType::Map), obj)) = self.doc.get(&revoked, &device_id)?
            else {
                continue;
            };
            self.store.upsert_revoked(&crate::RevokedDevice {
                device_id,
                at: get_i64_or(&self.doc, &obj, "at", 0),
                by: get_str_or(&self.doc, &obj, "by", ""),
            })?;
        }
        Ok(())
    }

    pub(super) fn materialize_attachments(&mut self) -> Result<()> {
        let Some((Value::Object(ObjType::Map), attachments)) = self.doc.get(ROOT, "attachments")?
        else {
            return Ok(()); // no attachments yet
        };
        let ids: Vec<String> = self.doc.keys(&attachments).collect();
        for id in ids {
            let Some((Value::Object(ObjType::Map), obj)) = self.doc.get(&attachments, &id)? else {
                continue;
            };
            let a = Attachment {
                id: id.clone(),
                memo_id: get_str_or(&self.doc, &obj, "memo_id", ""),
                hash: get_str_or(&self.doc, &obj, "hash", ""),
                name: get_str_or(&self.doc, &obj, "name", ""),
                mime: get_str_or(&self.doc, &obj, "mime", ""),
                width_px: get_i64_or(&self.doc, &obj, "width_px", 0),
                height_px: get_i64_or(&self.doc, &obj, "height_px", 0),
                width_em_milli: clamp_width_em_milli(get_i64_or(
                    &self.doc,
                    &obj,
                    "width_em_milli",
                    crate::DEFAULT_WIDTH_EM_MILLI,
                )),
                // Missing on records written before photos could be placed; those fall back
                // to the default corner rather than to 0,0 flush against the edge.
                x_permille: clamp_permille(get_i64_or(
                    &self.doc,
                    &obj,
                    "x_permille",
                    crate::PLACE_ORIGIN_PERMILLE,
                )),
                y_permille: clamp_permille(get_i64_or(
                    &self.doc,
                    &obj,
                    "y_permille",
                    crate::PLACE_ORIGIN_PERMILLE,
                )),
                // Missing, or a mode this version does not know, is the float every photo
                // was before there was a choice.
                mode: get_str_or(&self.doc, &obj, "mode", ""),
                anchor_line: get_i64_or(&self.doc, &obj, "anchor_line", 0).max(0),
                created_at: get_i64_or(&self.doc, &obj, "created_at", 0),
            };
            // No hash means an unusable record (old version or damage); skip it.
            if !a.hash.is_empty() {
                self.store.upsert_attachment(&a)?;
            }
        }
        Ok(())
    }

    /// Materializes `ROOT.groups` into the cache.
    pub(super) fn materialize_groups(&mut self) -> Result<()> {
        let Some((Value::Object(ObjType::Map), groups)) = self.doc.get(ROOT, "groups")? else {
            return Ok(()); // no groups yet
        };
        let ids: Vec<String> = self.doc.keys(&groups).collect();
        for id in ids {
            let Some((Value::Object(ObjType::Map), obj)) = self.doc.get(&groups, &id)? else {
                continue;
            };
            let group = Group {
                id: id.clone(),
                // Text now; `get_text_or` still reads the plain string older changes carry.
                name: get_text_or(&self.doc, &obj, "name", ""),
                parent_id: get_str_or(&self.doc, &obj, "parent_id", ""),
                // Folders had no colour before, so old changes carry none.
                color: get_str_or(&self.doc, &obj, "color", crate::DEFAULT_COLOR),
                created_at: get_i64_or(&self.doc, &obj, "created_at", 0),
                updated_at: get_i64_or(&self.doc, &obj, "updated_at", 0),
            };
            self.store.upsert_group(&group)?;
        }
        Ok(())
    }

    /// The `ROOT.memos` map, created on first use.
    pub(super) fn memos_obj(&mut self) -> Result<ObjId> {
        Ok(match self.doc.get(ROOT, "memos")? {
            Some((Value::Object(ObjType::Map), id)) => id,
            _ => self.doc.put_object(ROOT, "memos", ObjType::Map)?,
        })
    }

    /// The `ROOT.attachments` map, created on first use.
    pub(super) fn attachments_obj(&mut self) -> Result<ObjId> {
        Ok(match self.doc.get(ROOT, "attachments")? {
            Some((Value::Object(ObjType::Map), id)) => id,
            _ => self.doc.put_object(ROOT, "attachments", ObjType::Map)?,
        })
    }

    /// The `ROOT.revoked` map, created on first use.
    pub(super) fn revoked_obj(&mut self) -> Result<ObjId> {
        Ok(match self.doc.get(ROOT, "revoked")? {
            Some((Value::Object(ObjType::Map), id)) => id,
            _ => self.doc.put_object(ROOT, "revoked", ObjType::Map)?,
        })
    }

    /// The `ROOT.groups` map, created on first use.
    pub(super) fn groups_obj(&mut self) -> Result<ObjId> {
        Ok(match self.doc.get(ROOT, "groups")? {
            Some((Value::Object(ObjType::Map), id)) => id,
            _ => self.doc.put_object(ROOT, "groups", ObjType::Map)?,
        })
    }
}
