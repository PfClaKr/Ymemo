//! Vault: the layer that ties the encrypted automerge change logs to the local SQLite cache.
//!
//! Layout of the synced directory (the Syncthing shared folder):
//! ```text
//! <vault_dir>/
//!   vault.json             <- header: salt + key_check. Written once, then immutable.
//!   logs/<device_id>.ymlog <- per-device append-only log; a device writes only its own.
//! ```
//!
//! A log record is an encrypted **automerge change**. Document shape:
//! `ROOT.memos: Map<memo_id, {title, body, created_at, updated_at}>`,
//! `ROOT.groups: Map<group_id, {...}>`, `ROOT.attachments: Map<attachment_id, {...}>`,
//! `ROOT.name: Str` — what the vault is called, shared by every device that has it.
//! Photo bytes stay out of the document, in `blobs/<hash>.ymblob`; an attachment only
//! points at the hash.
//!
//! Automerge merges changes order-independently: edits to different fields of one memo
//! both survive, and a conflict on the same field converges deterministically. The actor
//! id is the device id, so only our own log carries our actor.
//!
//! ## Keys
//!
//! Logs and blobs are encrypted with a random **data key**, and `vault.json` stores that
//! key wrapped — once under `Argon2id(master password)`, and once more under
//! `Argon2id(recovery code)` after the user asks for one. Nothing but the wrapper changes
//! when the password does, so a password change re-encrypts no logs, no blobs and nothing
//! on the other devices; they simply ask for the new password the next time they unlock.
//!
//! The first vault format had no wrapping and used the password key as the data key
//! directly. Those headers still open — an empty `wrapped_key` *means* "the data key is the
//! password key" — and are rewritten into the wrapped form the first time the password
//! changes, never spontaneously: `vault.json` is a synced file, and a write nobody asked
//! for is a sync conflict nobody asked for.

use anyhow::{anyhow, bail, Context, Result};
use ymemo_i18n::t;
use automerge::{
    transaction::Transactable, ActorId, AutoCommit, Change, ObjId, ObjType, ReadDoc, ScalarValue,
    Value, ROOT,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::blob::BlobStore;
use crate::changelog::ChangeLog;
use crate::history::{Entity, Revision, RevisionKind};
use crate::crypto::{generate_salt, MasterKey, Salt, SALT_LEN};
use crate::{clamp_permille, clamp_width_em_milli, Attachment, Group, Memo, Store};

const HEADER_FILE: &str = "vault.json";
const LOGS_DIR: &str = "logs";
const LOG_EXT: &str = "ymlog";
/// Canary plaintext, stored encrypted in the header to detect a wrong password early.
const KEY_CHECK: &[u8] = b"ymemo-key-check-v1";
/// Header version written today: a wrapped data key. 1 was the unwrapped format.
const HEADER_VERSION: u32 = 2;

/// What a delete removed, and enough of its surroundings to put it back.
///
/// Deleting is the one thing in this app that loses writing, and a deleted memo cannot be
/// reached through [`Vault::history`] either — the row it was opened from is gone. So a
/// delete hands this back and the UI keeps it for as long as the offer to undo stands.
///
/// It carries values, not log positions, so it stays valid across a `rebuild()` and across
/// changes arriving from other devices in the meantime.
#[derive(Debug, Clone)]
pub enum Deleted {
    Memo(Memo),
    /// A folder, plus the ids of what got lifted out of it, so an undo can gather them again.
    Group {
        group: Group,
        lifted_groups: Vec<String>,
        lifted_memos: Vec<String>,
    },
}

/// Contents of `vault.json`. Neither salt is secret, and every key in it is wrapped.
///
/// The optional fields are absent in the original format; `#[serde(default)]` reads those
/// headers, and `skip_serializing_if` keeps us from writing empty ones back.
#[derive(Serialize, Deserialize, Clone)]
struct VaultHeader {
    version: u32,
    /// Argon2id salt for the master password, hex encoded.
    salt: String,
    /// `encrypt(data key, KEY_CHECK)`, hex encoded; decrypting it proves the data key.
    key_check: String,
    /// `encrypt(password key, data key)`, hex encoded. Empty means the original format,
    /// where the password key *is* the data key.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    wrapped_key: String,
    /// Argon2id salt for the recovery code; empty when no code was ever issued.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    recovery_salt: String,
    /// `encrypt(recovery key, data key)`, hex encoded; empty when no code was issued.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    recovery_key: String,
}

pub struct Vault {
    dir: PathBuf,
    store: Store,
    key: MasterKey,
    device_id: String,
    own_log: ChangeLog,
    /// All device logs merged: the in-memory source of truth.
    doc: AutoCommit,
    /// Photo bytes (`<vault_dir>/blobs`).
    blobs: BlobStore,
    /// What the logs looked like when [`Vault::rebuild`] last read them; see [`LogState`].
    built_from: Option<LogState>,
}

/// A fingerprint of the log directory: every log's name, length and modified time.
///
/// The logs are **append-only, one per device**, so anything new — typed here or arrived over
/// sync — moves one of these. When none of them moved, re-reading every log, decrypting every
/// record and replaying it into a fresh document produces exactly the document already in
/// memory. That is what the merge timer was doing every fifteen seconds, on the UI thread:
/// 43 ms on a vault with a few hundred memos in it and 365 ms on one with a year, growing
/// forever, almost always for nothing. Measured.
type LogState = Vec<(std::ffi::OsString, u64, Option<std::time::SystemTime>)>;

impl Vault {
    /// Creates a vault: new salt plus header. Errors if one already exists.
    pub fn create(dir: impl AsRef<Path>, password: &[u8], store: Store) -> Result<Self> {
        let dir = dir.as_ref();
        let header_path = dir.join(HEADER_FILE);
        if header_path.exists() {
            bail!(t!("core.vault_exists", path = header_path.display()));
        }
        fs::create_dir_all(dir)?;

        let salt = generate_salt();
        let password_key = MasterKey::derive(password, &salt)?;
        // The data key is random, not derived: the password only ever wraps it.
        let data_key = MasterKey::from_bytes(&crate::crypto::generate_key())?;
        let header = VaultHeader {
            version: HEADER_VERSION,
            salt: to_hex(&salt),
            key_check: to_hex(&data_key.encrypt(KEY_CHECK)?),
            wrapped_key: to_hex(&password_key.encrypt(&data_key.to_bytes())?),
            recovery_salt: String::new(),
            recovery_key: String::new(),
        };
        fs::write(&header_path, serde_json::to_vec_pretty(&header)?)?;

        Self::open(dir, password, store)
    }

    /// Opens a vault: verifies the password against the header canary, then merges every
    /// device log into the document and rebuilds the cache.
    pub fn open(dir: impl AsRef<Path>, password: &[u8], store: Store) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        let header = read_header(&dir)?;
        let key = unlock_header(&header, password)?;

        let device_id = store.device_id()?;
        fs::create_dir_all(dir.join(LOGS_DIR))?;

        // Self-heal a diverged key: two devices could each create a vault.json with its own
        // salt, and Syncthing's conflict resolution then picks one as canonical. If our log
        // will not open under the canonical key, look for the old salt in the conflict
        // headers and re-encrypt.
        heal_divergent_log(&dir, &device_id, password, &key)?;

        Self::finish_open(dir, store, key, device_id)
    }

    /// Opens with an already-derived key: the "stay unlocked" path, no password prompt.
    ///
    /// Without a password there is no `heal_divergent_log`, so a diverged key surfaces as an
    /// error here and the caller should fall back to the lock screen; healing happens in
    /// [`Self::open`].
    pub fn open_with_key(dir: impl AsRef<Path>, key: MasterKey, store: Store) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        let header = read_header(&dir)?;
        verify_key(&header, &key)?;

        let device_id = store.device_id()?;
        fs::create_dir_all(dir.join(LOGS_DIR))?;

        Self::finish_open(dir, store, key, device_id)
    }

    /// Shared tail of both open paths: open our log and merge everything.
    fn finish_open(dir: PathBuf, store: Store, key: MasterKey, device_id: String) -> Result<Self> {
        let own_log = ChangeLog::open(
            dir.join(LOGS_DIR).join(format!("{device_id}.{LOG_EXT}")),
            key.clone(),
        );

        let blobs = BlobStore::open(&dir, key.clone());
        let mut vault = Self {
            doc: AutoCommit::new(),
            dir,
            store,
            key,
            device_id,
            own_log,
            blobs,
            // Nothing has been read yet, so the rebuild below is never the one that is skipped.
            built_from: None,
        };
        vault.rebuild()?;
        Ok(vault)
    }

    /// Raw key for the "stay unlocked" cache; see [`MasterKey::to_bytes`] for what that costs.
    pub fn key_bytes(&self) -> [u8; crate::crypto::KEY_LEN] {
        self.key.to_bytes()
    }

    /// Opens the vault, creating it if there is no header yet.
    pub fn open_or_create(dir: impl AsRef<Path>, password: &[u8], store: Store) -> Result<Self> {
        if dir.as_ref().join(HEADER_FILE).exists() {
            Self::open(dir, password, store)
        } else {
            Self::create(dir, password, store)
        }
    }

    /// Replaces the master password, after checking the current one.
    ///
    /// Only the wrapper is rewritten, so this is instant however large the vault is, and
    /// the other devices keep syncing without interruption — they ask for the new password
    /// the next time they unlock. A version older than the wrapped format cannot open the
    /// vault afterwards, since the canary is no longer under the password key.
    ///
    /// The recovery code, if one was issued, keeps working: it wraps the same data key.
    pub fn change_password(&self, current: &[u8], new: &[u8]) -> Result<()> {
        if new.is_empty() {
            bail!(t!("core.empty_password"));
        }
        let header = read_header(&self.dir)?;
        let current_key = unlock_header(&header, current)?;
        // The header on disk must still be the one this vault was opened with; another
        // device may have changed the password while this one sat unlocked.
        if current_key.to_bytes() != self.key.to_bytes() {
            bail!(t!("core.vault_key_changed"));
        }
        rewrap_password(&self.dir, &self.key, new)
    }

    /// Issues a fresh recovery code, replacing any earlier one, and returns it.
    ///
    /// **The only time the code is ever readable.** Only its Argon2id wrapper is stored, so
    /// a lost code cannot be recovered — it can only be reissued from an unlocked vault.
    pub fn issue_recovery_code(&self) -> Result<String> {
        let code = crate::recovery::generate();
        let salt = generate_salt();
        let recovery_key = MasterKey::derive(crate::recovery::normalize(&code)?.as_bytes(), &salt)?;

        let mut header = read_header(&self.dir)?;
        header.recovery_salt = to_hex(&salt);
        header.recovery_key = to_hex(&recovery_key.encrypt(&self.key.to_bytes())?);
        // `version` is left alone: adding a recovery wrapper does not move an unwrapped
        // header to the new format, and both shapes unlock the same way.
        write_header(&self.dir, &header)?;
        Ok(code)
    }

    /// Whether a recovery code was issued for this vault.
    pub fn has_recovery_code(&self) -> bool {
        recovery_code_exists(&self.dir)
    }

    /// Inserts or updates a memo, writing only changed fields so merges stay field-level.
    pub fn upsert(&mut self, memo: &Memo) -> Result<()> {
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
    fn key_for_new(&self, memo: &Memo) -> Result<String> {
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
    fn put_order_key(&mut self, id: &str, key: &str) -> Result<()> {
        let memos = self.memos_obj()?;
        if let Some((Value::Object(ObjType::Map), obj)) = self.doc.get(&memos, id)? {
            put_str_if_changed(&mut self.doc, &obj, "order_key", key)?;
        }
        self.store.set_order_key(id, key)
    }

    /// Attaches a photo: bytes go to the blob store, only the record is synced.
    ///
    /// The UI passes `width_px`/`height_px` so the core needs no image decoder — both UIs
    /// already have one. Pass 0 when unknown; the aspect ratio then falls back to 1:1.
    pub fn attach(
        &mut self,
        memo_id: &str,
        data: &[u8],
        name: &str,
        mime: &str,
        width_px: i64,
        height_px: i64,
    ) -> Result<Attachment> {
        let hash = self.blobs.put(data)?;
        let mut a = Attachment::new(memo_id, hash);
        a.name = name.to_string();
        a.mime = mime.to_string();
        a.width_px = width_px;
        a.height_px = height_px;
        // Offset from the photos already on this memo, so the new one is not dropped exactly
        // on top of the last and left unreachable.
        let placed = self.store.attachments_of(memo_id).map(|l| l.len()).unwrap_or(0);
        let (x, y) = crate::cascade_permille(placed);
        a.x_permille = x;
        a.y_permille = y;
        self.upsert_attachment(&a)?;
        Ok(a)
    }

    /// Inserts or updates an attachment; display-size changes go through here too.
    pub fn upsert_attachment(&mut self, a: &Attachment) -> Result<()> {
        let attachments = self.attachments_obj()?;
        let obj = match self.doc.get(&attachments, &a.id)? {
            Some((Value::Object(ObjType::Map), id)) => id,
            _ => self.doc.put_object(&attachments, &a.id, ObjType::Map)?,
        };
        put_str_if_changed(&mut self.doc, &obj, "memo_id", &a.memo_id)?;
        put_str_if_changed(&mut self.doc, &obj, "hash", &a.hash)?;
        put_str_if_changed(&mut self.doc, &obj, "name", &a.name)?;
        put_str_if_changed(&mut self.doc, &obj, "mime", &a.mime)?;
        put_i64_if_changed(&mut self.doc, &obj, "width_px", a.width_px)?;
        put_i64_if_changed(&mut self.doc, &obj, "height_px", a.height_px)?;
        put_i64_if_changed(
            &mut self.doc,
            &obj,
            "width_em_milli",
            clamp_width_em_milli(a.width_em_milli),
        )?;
        put_i64_if_changed(&mut self.doc, &obj, "x_permille", clamp_permille(a.x_permille))?;
        put_i64_if_changed(&mut self.doc, &obj, "y_permille", clamp_permille(a.y_permille))?;
        put_str_if_changed(&mut self.doc, &obj, "mode", a.mode().as_stored())?;
        put_i64_if_changed(&mut self.doc, &obj, "created_at", a.created_at)?;

        self.append_local_change()?;
        self.store.upsert_attachment(a)
    }

    /// Sets just the display width (1/1000 em); resizing on one device carries to the others.
    pub fn set_attachment_width(&mut self, id: &str, width_em_milli: i64) -> Result<()> {
        let Some(mut a) = self.store.get_attachment(id)? else {
            bail!(t!("core.attachment_not_found", id = id));
        };
        a.width_em_milli = clamp_width_em_milli(width_em_milli);
        self.upsert_attachment(&a)
    }

    /// Sets position and size together — one write, because dragging a photo's corner moves
    /// and resizes it at once and two writes would leave two revisions in the history.
    pub fn set_attachment_layout(
        &mut self,
        id: &str,
        x_permille: i64,
        y_permille: i64,
        width_em_milli: i64,
    ) -> Result<()> {
        let Some(mut a) = self.store.get_attachment(id)? else {
            bail!(t!("core.attachment_not_found", id = id));
        };
        a.x_permille = clamp_permille(x_permille);
        a.y_permille = clamp_permille(y_permille);
        a.width_em_milli = clamp_width_em_milli(width_em_milli);
        self.upsert_attachment(&a)
    }

    /// Sets how a photo sits against the writing: floating over it, or in the flow below it.
    ///
    /// A choice about the memo, not about this device, so it travels like everything else —
    /// a photo put in the flow on the phone is out of the way on the desktop too.
    pub fn set_attachment_mode(&mut self, id: &str, mode: crate::PhotoMode) -> Result<()> {
        let Some(mut a) = self.store.get_attachment(id)? else {
            bail!(t!("core.attachment_not_found", id = id));
        };
        a.mode = mode.as_stored().to_string();
        self.upsert_attachment(&a)
    }

    /// Detaches a photo. **The blob file stays** — no GC, other devices may still show it.
    pub fn detach(&mut self, id: &str) -> Result<()> {
        let attachments = self.attachments_obj()?;
        if self.doc.get(&attachments, id)?.is_some() {
            self.doc.delete(&attachments, id)?;
            self.append_local_change()?;
        }
        self.store.delete_attachment(id)
    }

    /// Photo bytes. Errors while the blob has not synced yet; the UI shows a placeholder.
    pub fn attachment_bytes(&self, hash: &str) -> Result<Vec<u8>> {
        self.blobs.get(hash)
    }

    /// Whether the photo has arrived on this device.
    pub fn has_blob(&self, hash: &str) -> bool {
        self.blobs.has(hash)
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

    /// Merges every log in `logs/` into a fresh document and rebuilds the SQLite cache from
    /// scratch. One call picks up whatever Syncthing has delivered.
    /// Returns whether anything was actually re-read, so a caller with work to do **after** a
    /// merge — pushing changes into open windows, redrawing a list, decoding photos — can
    /// skip it too. Nearly every tick has nothing in it.
    pub fn rebuild(&mut self) -> Result<bool> {
        // Read *before* the logs are, not after: reading them is not one atomic act, and a
        // record that lands halfway through must leave the vault looking out of date rather
        // than be recorded as already merged. The cost of being wrong this way is one more
        // rebuild; the cost of being wrong the other way is a change that never arrives.
        let state = self.log_state();
        if self.built_from.as_ref() == Some(&state) {
            return Ok(false); // nothing new to merge, and the cache already says so
        }
        let mut doc = AutoCommit::new();
        doc.apply_changes(self.read_all_changes()?)?;
        // actor = device_id, so later local changes continue our own actor sequence.
        doc.set_actor(ActorId::from(self.device_id.as_bytes()));
        self.doc = doc;
        self.materialize()?;
        self.built_from = Some(state);
        Ok(true)
    }

    /// Records our own log at its current length as already merged, leaving every other
    /// device's entry alone. See the call in [`Vault::append_local_change`].
    fn mark_own_log_merged(&mut self) {
        let Some(state) = self.built_from.as_mut() else {
            return; // nothing has been read yet; the first rebuild must still do the work
        };
        let name = std::ffi::OsString::from(format!("{}.{LOG_EXT}", self.device_id));
        let Ok(meta) = fs::metadata(self.dir.join(LOGS_DIR).join(&name)) else {
            return;
        };
        let fresh = (name.clone(), meta.len(), meta.modified().ok());
        match state.iter_mut().find(|(n, _, _)| *n == name) {
            Some(slot) => *slot = fresh,
            None => {
                state.push(fresh);
                state.sort();
            }
        }
    }

    /// The fingerprint [`LogState`] describes, sorted so two readings compare.
    fn log_state(&self) -> LogState {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(self.dir.join(LOGS_DIR)) else {
            return out; // no logs yet, which is a state like any other
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some(LOG_EXT) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            out.push((entry.file_name(), meta.len(), meta.modified().ok()));
        }
        out.sort();
        out
    }

    /// Every past version of one memo or folder, oldest first.
    ///
    /// Read from the logs rather than the live document, so it neither disturbs nor is
    /// disturbed by the merge timer. See [`crate::history`] for what a revision is and why
    /// this is not built on Syncthing's file versioning.
    pub fn history(&mut self, entity: Entity, id: &str) -> Result<Vec<Revision>> {
        // The document already holds every change in causal order, which is what the logs
        // were being re-read and re-decrypted to reconstruct — a second full copy of the
        // vault built to answer a question about one memo. On a vault with a year in it that
        // was seconds of the UI thread on a single click. Measured.
        crate::history::revisions(&mut self.doc, entity, id)
    }

    /// Writes the values from `revision` back, as a new edit.
    ///
    /// **Nothing is rewritten.** A restore appends a change like any other, so the versions
    /// it stepped over stay readable and the restore itself becomes the newest revision.
    /// That is also what makes it safe on several devices at once: two restores merge like
    /// two edits instead of fighting over the log.
    ///
    /// An entity deleted in the meantime comes back, since the revision carries every field.
    pub fn restore(&mut self, entity: Entity, id: &str, revision: &Revision) -> Result<()> {
        if revision.kind == RevisionKind::Deleted {
            bail!(t!("core.cannot_restore_deletion"));
        }
        let now = crate::now_millis();
        match entity {
            Entity::Memo => {
                let mut memo = self.store.get(id)?.unwrap_or_else(|| {
                    let mut m = Memo::new("", "");
                    m.id = id.to_string();
                    m
                });
                memo.title = revision.field("title").to_string();
                memo.body = revision.field("body").to_string();
                memo.color = non_empty(revision.field("color"), crate::DEFAULT_COLOR);
                memo.opacity = revision.field("opacity").parse().unwrap_or(crate::DEFAULT_OPACITY);
                memo.group_id = revision.field("group_id").to_string();
                memo.created_at = revision.field("created_at").parse().unwrap_or(memo.created_at);
                memo.updated_at = now;
                self.upsert(&memo)
            }
            Entity::Group => {
                let mut group = self.store.get_group(id)?.unwrap_or_else(|| {
                    let mut g = Group::new("");
                    g.id = id.to_string();
                    g
                });
                group.name = revision.field("name").to_string();
                group.parent_id = revision.field("parent_id").to_string();
                group.color = non_empty(revision.field("color"), crate::DEFAULT_COLOR);
                group.created_at = revision.field("created_at").parse().unwrap_or(group.created_at);
                group.updated_at = now;
                self.upsert_group(&group)
            }
        }
    }

    // -----------------------------------------------------------------------------------
    // Removed devices
    //
    // Pairing is between two devices and the vault is not, so every peer is an introducer
    // (see `Syncthing::upsert_peer`) and the devices sharing a vault close into a mesh. That
    // is what makes a removal a **synced** fact rather than a local one: a device dropped
    // only here is handed straight back by the peers that still have it — measured, and it
    // flaps for a minute and then returns.
    //
    // So the decision goes in the document, next to the memos, and every device applies it
    // on its own. See [`crate::RevokedDevice`] for what this is and is not.
    // -----------------------------------------------------------------------------------

    /// Records that a device is no longer part of this vault, on every device that shares it.
    ///
    /// Idempotent, and never a no-op on the timestamp alone: re-revoking an already revoked
    /// device would otherwise write a change per call, and the callers poll.
    pub fn revoke_device(&mut self, device_id: &str) -> Result<()> {
        if device_id == self.device_id {
            bail!(t!("core.cannot_unshare_self"));
        }
        let revoked = self.revoked_obj()?;
        if self.doc.get(&revoked, device_id)?.is_some() {
            return Ok(());
        }
        let obj = self.doc.put_object(&revoked, device_id, ObjType::Map)?;
        self.doc.put(&obj, "at", crate::now_millis())?;
        self.doc.put(&obj, "by", self.device_id.clone())?;
        self.append_local_change()?;
        self.materialize_revoked()
    }

    /// Takes a device off the removed list, which is what pairing with it again means.
    ///
    /// Without this a mis-click would be permanent: the peers would go on refusing a device
    /// the user has since decided to keep, and no amount of re-pairing would stick.
    pub fn unrevoke_device(&mut self, device_id: &str) -> Result<()> {
        let revoked = self.revoked_obj()?;
        if self.doc.get(&revoked, device_id)?.is_none() {
            return Ok(());
        }
        self.doc.delete(&revoked, device_id)?;
        self.append_local_change()?;
        self.materialize_revoked()
    }

    /// Every device the vault says has been removed, oldest first.
    pub fn revoked_devices(&self) -> Result<Vec<crate::RevokedDevice>> {
        self.store.list_revoked()
    }

    /// Whether the vault says **this** device has been removed.
    ///
    /// A device that finds itself here stops syncing rather than fighting the others for the
    /// folder. It keeps what it already has: taking a user's memos off their own machine
    /// because another machine said so is a different act, and not one this promises.
    pub fn is_revoked_here(&self) -> Result<bool> {
        Ok(self.revoked_devices()?.iter().any(|d| d.device_id == self.device_id))
    }

    /// Read-only access to the local cache.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// This device's id.
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// The synced directory, i.e. the Syncthing shared folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// What this vault is called, empty when it has never been named.
    ///
    /// The name lives in the automerge document, next to the memos, and not in `vault.json`:
    /// the header is a synced *file*, so two devices renaming at once would leave syncthing
    /// two versions of it to pick between, while the document merges them the way it merges
    /// everything else. It follows a pairing for free — a device that receives the logs
    /// receives the name in them.
    pub fn name(&self) -> String {
        get_str_or(&self.doc, &ROOT, "name", "")
    }

    /// Renames the vault, on every device that shares it.
    pub fn set_name(&mut self, name: &str) -> Result<()> {
        let name = crate::clamp_vault_name(name);
        if self.name() == name {
            return Ok(());
        }
        self.doc.put(ROOT, "name", name)?;
        self.append_local_change()
    }

    /// Decrypts every `.ymlog` and parses the automerge changes.
    ///
    /// **One bad log never blocks the merge** — it is skipped. A log can fail because it was
    /// written under a diverged key, or because Syncthing has only delivered part of it;
    /// letting that stop the healthy logs would look like sync had died altogether
    /// (especially in the console-less Windows build).
    fn read_all_changes(&self) -> Result<Vec<Change>> {
        let logs_dir = self.dir.join(LOGS_DIR);
        let mut changes = Vec::new();
        if !logs_dir.exists() {
            return Ok(changes);
        }
        for entry in fs::read_dir(&logs_dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some(LOG_EXT) {
                continue;
            }
            let records = match ChangeLog::open(&path, self.key.clone()).read_all() {
                Ok(r) => r,
                Err(e) => {
                    crate::diag!("skipping log (decrypt failed) {}: {e}", path.display());
                    continue;
                }
            };
            for record in records {
                match Change::from_bytes(record) {
                    Ok(c) => changes.push(c),
                    Err(e) => crate::diag!("skipping change (parse failed) {}: {e}", path.display()),
                }
            }
        }
        Ok(changes)
    }

    /// Commits the pending local edit and appends it, encrypted, to our own log. Writes
    /// nothing when there was no actual change.
    ///
    /// The commit carries **the time it was made**, in seconds, which is what
    /// [`crate::history`] reads back as a revision's date. Automerge's plain `commit()`
    /// leaves it at zero, and a history where every version happened in 1970 is no history.
    fn append_local_change(&mut self) -> Result<()> {
        let options = automerge::transaction::CommitOptions::default()
            .with_time(crate::now_millis() / 1000);
        if self.doc.commit_with(options).is_some() {
            let change = self
                .doc
                .get_last_local_change()
                .context(t!("core.no_local_change"))?;
            self.own_log.append(change.raw_bytes())?;
            // This change is already in the document and in the cache — that is what a local
            // write *is* — so our own log growing by it is not news. Without this every
            // keystroke that reached the vault bought a full re-read on the next merge tick.
            //
            // **Only our own log.** Taking the whole directory's state here would mark another
            // device's changes as merged the moment we typed anything, and that change would
            // then never be read — which is exactly what the two merge tests caught.
            self.mark_own_log_merged();
        }
        Ok(())
    }

    /// Materializes the document into the SQLite cache.
    fn materialize(&mut self) -> Result<()> {
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
    fn materialize_all(&mut self) -> Result<()> {
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
    fn materialize_revoked(&mut self) -> Result<()> {
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

    fn materialize_attachments(&mut self) -> Result<()> {
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
    fn materialize_groups(&mut self) -> Result<()> {
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
    fn memos_obj(&mut self) -> Result<ObjId> {
        Ok(match self.doc.get(ROOT, "memos")? {
            Some((Value::Object(ObjType::Map), id)) => id,
            _ => self.doc.put_object(ROOT, "memos", ObjType::Map)?,
        })
    }

    /// The `ROOT.attachments` map, created on first use.
    fn attachments_obj(&mut self) -> Result<ObjId> {
        Ok(match self.doc.get(ROOT, "attachments")? {
            Some((Value::Object(ObjType::Map), id)) => id,
            _ => self.doc.put_object(ROOT, "attachments", ObjType::Map)?,
        })
    }

    /// The `ROOT.revoked` map, created on first use.
    fn revoked_obj(&mut self) -> Result<ObjId> {
        Ok(match self.doc.get(ROOT, "revoked")? {
            Some((Value::Object(ObjType::Map), id)) => id,
            _ => self.doc.put_object(ROOT, "revoked", ObjType::Map)?,
        })
    }

    /// The `ROOT.groups` map, created on first use.
    fn groups_obj(&mut self) -> Result<ObjId> {
        Ok(match self.doc.get(ROOT, "groups")? {
            Some((Value::Object(ObjType::Map), id)) => id,
            _ => self.doc.put_object(ROOT, "groups", ObjType::Map)?,
        })
    }
}

/// Heals a diverged vault key.
///
/// Background: a device that entered its password before pairing had delivered vault.json
/// used to create its own header with a different salt. Syncthing resolves the two as a
/// conflict, keeping one as `vault.json` and renaming the loser to
/// `vault.sync-conflict-*.json`, so every device converges on the canonical salt.
///
/// If our log does not open under `canonical_key` (already verified against the header),
/// this looks for the old key among the conflict headers' salts and re-encrypts our log.
/// Other devices' logs are left alone; each heals itself.
///
/// Conflict files are never deleted: a device that has not healed yet may still need its
/// old salt, and the deletion would sync over and strip that away. Once healed, the log
/// opens under the canonical key and this search never runs again.
fn heal_divergent_log(
    dir: &Path,
    device_id: &str,
    password: &[u8],
    canonical_key: &MasterKey,
) -> Result<()> {
    let own_path = dir.join(LOGS_DIR).join(format!("{device_id}.{LOG_EXT}"));
    if !own_path.exists() {
        return Ok(()); // no local log, nothing to heal
    }
    // Already opens under the canonical key: healthy, or healed earlier.
    if ChangeLog::open(&own_path, canonical_key.clone()).read_all().is_ok() {
        return Ok(());
    }
    // Look for the old key among the conflict headers. Each is unlocked the same way the
    // canonical one is, so a wrapped and an unwrapped header are both candidates.
    for header in conflict_headers(dir) {
        let Ok(old_key) = unlock_header(&header, password) else { continue };
        if ChangeLog::open(&own_path, old_key.clone()).read_all().is_ok() {
            reencrypt_log(&own_path, &old_key, canonical_key)?;
            crate::diag!("diverged vault key: re-encrypted our log under the canonical key");
            return Ok(());
        }
    }
    // Not found: rebuild will just skip this log and merge the others.
    crate::diag!(
        "warning: no key opens our log ({}) — vault.json may have changed unexpectedly",
        own_path.display()
    );
    Ok(())
}

/// Reads the `vault.json` header.
fn read_header(dir: &Path) -> Result<VaultHeader> {
    let path = dir.join(HEADER_FILE);
    let bytes =
        fs::read(&path).with_context(|| t!("core.header_missing", path = path.display()))?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Writes the header through a temporary file, so a crash mid-write cannot leave a
/// truncated `vault.json` — the one file without which no device can open the vault.
fn write_header(dir: &Path, header: &VaultHeader) -> Result<()> {
    let path = dir.join(HEADER_FILE);
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(header)?)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Derives a wrapping key from a password (or a recovery code) and a hex-encoded salt.
fn derive_wrapping_key(secret: &[u8], salt_hex: &str) -> Result<MasterKey> {
    let salt: Salt = from_hex(salt_hex)?
        .try_into()
        .map_err(|_| anyhow!(t!("core.salt_length_bad", expected = SALT_LEN)))?;
    MasterKey::derive(secret, &salt)
}

/// Unwraps a data key that was wrapped under `wrapper`.
fn unwrap_key(wrapper: &MasterKey, wrapped_hex: &str, wrong: &str) -> Result<MasterKey> {
    let raw = wrapper
        .decrypt(&from_hex(wrapped_hex)?)
        .map_err(|_| anyhow!(wrong.to_string()))?;
    let raw: [u8; crate::crypto::KEY_LEN] = raw
        .try_into()
        .map_err(|_| anyhow!(t!("core.wrapped_key_length_bad")))?;
    MasterKey::from_bytes(&raw)
}

/// The vault's data key, from a header plus the master password.
///
/// An empty `wrapped_key` is the original format, where the password key was used to
/// encrypt the logs directly; there the data key simply *is* the password key.
fn unlock_header(header: &VaultHeader, password: &[u8]) -> Result<MasterKey> {
    let password_key = derive_wrapping_key(password, &header.salt)?;
    let data_key = if header.wrapped_key.is_empty() {
        password_key
    } else {
        unwrap_key(&password_key, &header.wrapped_key, &t!("core.wrong_password"))?
    };
    verify_key(header, &data_key)?;
    Ok(data_key)
}

/// The vault's data key, from a header plus a recovery code.
fn unlock_header_with_recovery(header: &VaultHeader, code: &str) -> Result<MasterKey> {
    if header.recovery_key.is_empty() || header.recovery_salt.is_empty() {
        bail!(t!("core.no_recovery_code"));
    }
    let normalized = crate::recovery::normalize(code)?;
    let recovery_key = derive_wrapping_key(normalized.as_bytes(), &header.recovery_salt)?;
    let data_key = unwrap_key(
        &recovery_key,
        &header.recovery_key,
        &t!("core.wrong_recovery_code"),
    )?;
    verify_key(header, &data_key)?;
    Ok(data_key)
}

/// Rewrites the header so `new_password` wraps `data_key`, keeping any recovery wrapper.
///
/// The data key itself never changes, which is the whole point: not one log record or blob
/// is re-encrypted, and the other devices carry on appending to their logs untouched. They
/// only need the new password the next time they ask for one.
fn rewrap_password(dir: &Path, data_key: &MasterKey, new_password: &[u8]) -> Result<()> {
    let mut header = read_header(dir)?;
    let salt = generate_salt();
    let password_key = MasterKey::derive(new_password, &salt)?;
    header.version = HEADER_VERSION;
    header.salt = to_hex(&salt);
    header.key_check = to_hex(&data_key.encrypt(KEY_CHECK)?);
    header.wrapped_key = to_hex(&password_key.encrypt(&data_key.to_bytes())?);
    write_header(dir, &header)
}

/// Checks the key by decrypting the header canary.
fn verify_key(header: &VaultHeader, key: &MasterKey) -> Result<()> {
    let check = key
        .decrypt(&from_hex(&header.key_check)?)
        .map_err(|_| anyhow!(t!("core.wrong_password")))?;
    if check != KEY_CHECK {
        bail!(t!("core.key_check_mismatch"));
    }
    Ok(())
}

/// Headers parsed out of the `vault.sync-conflict-*.json` files; unreadable ones are
/// skipped. Each one is a key the vault may have been encrypted under before Syncthing
/// picked a winner, so healing tries them all.
fn conflict_headers(dir: &Path) -> Vec<VaultHeader> {
    let mut headers = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return headers;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !(name.starts_with("vault.sync-conflict-") && name.ends_with(".json")) {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else { continue };
        if let Ok(header) = serde_json::from_slice::<VaultHeader>(&bytes) {
            headers.push(header);
        }
    }
    headers
}

/// Rewrites a log from `old_key` to `new_key` and swaps it in atomically.
fn reencrypt_log(path: &Path, old_key: &MasterKey, new_key: &MasterKey) -> Result<()> {
    let records = ChangeLog::open(path, old_key.clone()).read_all()?;
    let tmp = path.with_extension("ymlog.tmp");
    let _ = fs::remove_file(&tmp);
    let new_log = ChangeLog::open(&tmp, new_key.clone());
    for r in &records {
        new_log.append(r)?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Writes a field the user **types** — a memo's title or body, a folder's name.
///
/// These are automerge `Text`, not the plain strings the rest of the document uses, and that
/// is the whole difference between two devices merging and one of them losing what somebody
/// wrote. A `put` of a string is last-write-wins: edit the same memo on two devices inside
/// the time it takes to sync and one version silently becomes the memo, with the other left
/// only in the change history. A `Text` merges the two edits instead.
///
/// It is not magic. The UI hands over the **whole** body on every save, so the change has to
/// be recovered by diffing (`update_text`) rather than captured as it was typed; two people
/// typing at the same spot get their letters interleaved. Nothing is lost, which is the part
/// that matters.
///
/// A field that is a plain string here was written by a build from before this, and is
/// replaced by a text object the first time it changes — which is also the one moment two
/// devices converting the same memo at once can still lose an edit, since replacing the key
/// is itself a `put`.
///
/// **Only the body.** The rule is not "anything the user types" — it is *prose that is added
/// to*. A title and a folder's name are short labels, replaced whole rather than extended, and
/// merging two of them is worse than losing one: renaming a folder to "Work" on one device and
/// "Home" on the other gives **"WHorkme"**, which is not a name anybody chose and which the
/// user now has to notice and repair. Measured. Last-write-wins leaves a name somebody meant,
/// and the one that lost is still in the change history. Colours and ids stay strings for the
/// same reason, only more obviously.
fn put_text_if_changed(doc: &mut AutoCommit, obj: &ObjId, key: &str, val: &str) -> Result<()> {
    if let Some((Value::Object(ObjType::Text), text)) = doc.get(obj, key)? {
        if doc.text(&text)? != val {
            doc.update_text(&text, val)?;
        }
        return Ok(());
    }
    let text = doc.put_object(obj, key, ObjType::Text)?;
    doc.splice_text(&text, 0, 0, val)?;
    Ok(())
}

/// Reads a field written by [`put_text_if_changed`], or the plain string an older build left.
fn get_text(doc: &AutoCommit, obj: &ObjId, key: &str) -> Result<String> {
    match doc.get(obj, key)? {
        Some((Value::Object(ObjType::Text), text)) => Ok(doc.text(&text)?),
        // Written before the change above; still perfectly readable.
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::Str(v) => Ok(v.to_string()),
            other => bail!(t!("core.field_not_string", key = key, found = format!("{other:?}"))),
        },
        _ => bail!(t!("core.field_missing", key = key)),
    }
}

/// [`get_text`] with a fallback, for a field an old change may not carry at all.
fn get_text_or(doc: &AutoCommit, obj: &ObjId, key: &str, default: &str) -> String {
    get_text(doc, obj, key).unwrap_or_else(|_| default.to_string())
}

fn put_str_if_changed(doc: &mut AutoCommit, obj: &ObjId, key: &str, val: &str) -> Result<()> {
    let same = matches!(
        doc.get(obj, key)?,
        Some((Value::Scalar(ref s), _)) if matches!(s.as_ref(), ScalarValue::Str(cur) if cur.as_str() == val)
    );
    if !same {
        doc.put(obj, key, val)?;
    }
    Ok(())
}

fn put_i64_if_changed(doc: &mut AutoCommit, obj: &ObjId, key: &str, val: i64) -> Result<()> {
    let same = matches!(
        doc.get(obj, key)?,
        Some((Value::Scalar(ref s), _)) if matches!(s.as_ref(), ScalarValue::Int(cur) if *cur == val)
    );
    if !same {
        doc.put(obj, key, val)?;
    }
    Ok(())
}


/// Reads a string field, falling back to `default` when missing or of another type.
fn get_str_or(doc: &AutoCommit, obj: &ObjId, key: &str, default: &str) -> String {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Str(v) => v.to_string(),
            _ => default.to_string(),
        },
        _ => default.to_string(),
    }
}

/// Reads an integer field, falling back to `default` when missing or of another type.
fn get_i64_or(doc: &AutoCommit, obj: &ObjId, key: &str, default: i64) -> i64 {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Int(v) => *v,
            _ => default,
        },
        _ => default,
    }
}

fn get_i64(doc: &AutoCommit, obj: &ObjId, key: &str) -> Result<i64> {
    match doc.get(obj, key)? {
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::Int(v) => Ok(*v),
            other => bail!(t!("core.field_not_int", key = key, found = format!("{other:?}"))),
        },
        _ => bail!(t!("core.field_missing", key = key)),
    }
}

/// `value`, or `fallback` when it is empty — a revision from before a field existed.
fn non_empty(value: &str, fallback: &str) -> String {
    if value.is_empty() { fallback.to_string() } else { value.to_string() }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        bail!(t!("core.hex_odd_length"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| anyhow!(t!("core.hex_parse_failed", error = e))))
        .collect()
}

/// Whether `dir` holds a vault with a recovery code, without opening it.
///
/// The lock screen asks before anything is unlocked, so it cannot go through [`Vault`].
pub fn recovery_code_exists(dir: impl AsRef<Path>) -> bool {
    read_header(dir.as_ref()).is_ok_and(|h| !h.recovery_key.is_empty() && !h.recovery_salt.is_empty())
}

/// Sets a new master password using the recovery code, for a vault nobody can unlock.
///
/// Nothing is decrypted beyond the header, so this is as fast as two Argon2id runs. The
/// recovery code stays valid afterwards — it wraps the same data key — and can be replaced
/// from an unlocked vault with [`Vault::issue_recovery_code`].
pub fn reset_password_with_recovery(
    dir: impl AsRef<Path>,
    code: &str,
    new_password: &[u8],
) -> Result<()> {
    if new_password.is_empty() {
        bail!(t!("core.empty_password"));
    }
    let dir = dir.as_ref();
    let header = read_header(dir)?;
    let data_key = unlock_header_with_recovery(&header, code)?;
    rewrap_password(dir, &data_key, new_password)
}

/// Deletes a vault directory outright: header, logs and blobs.
///
/// For "I forgot the password and want to start over", which is the only way out when
/// neither the password nor a recovery code is left — the data is unreadable by design.
///
/// **Stop sharing the directory before calling this.** Syncthing propagates deletions, so
/// wiping a folder it still carries wipes the other devices too
/// ([`crate::sync::Syncthing::remove_folder`]).
pub fn wipe(dir: impl AsRef<Path>) -> Result<()> {
    let dir = dir.as_ref();
    if !dir.exists() {
        return Ok(());
    }
    // Refuse to empty a directory that is not a vault; the caller passes a path from
    // settings, and a wrong one would take the user's files with it.
    let looks_like_vault = dir.join(HEADER_FILE).exists()
        || dir.join(LOGS_DIR).exists()
        || dir.join("blobs").exists();
    if !looks_like_vault {
        bail!(t!("core.not_a_vault", path = dir.display()));
    }
    fs::remove_dir_all(dir)?;
    fs::create_dir_all(dir)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh temporary vault directory.
    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ymemo-vault-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Ids in the order the list draws them.
    fn order(v: &Vault) -> Vec<String> {
        v.store().list().unwrap().into_iter().map(|m| m.title).collect()
    }

    /// A folder nobody has arranged is still the newest-first list it always was, and a new
    /// memo lands at the top of it without anything being written to say so.
    #[test]
    fn an_unarranged_folder_is_still_newest_first() {
        let dir = temp_dir();
        let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        for title in ["one", "two", "three"] {
            let mut m = Memo::new(title, "");
            m.updated_at = crate::now_millis() + order(&v).len() as i64;
            v.upsert(&m).unwrap();
        }
        assert_eq!(order(&v), ["three", "two", "one"]);
        assert!(v.store().list().unwrap().iter().all(|m| m.order_key.is_empty()));
    }

    /// The first drag stamps the folder with the order it was already being shown in, so
    /// nothing jumps except the memo being moved.
    #[test]
    fn arranging_keeps_what_was_on_screen_and_moves_only_the_one() {
        let dir = temp_dir();
        let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let mut ids = Vec::new();
        for (i, title) in ["a", "b", "c", "d"].iter().enumerate() {
            let mut m = Memo::new(*title, "");
            m.updated_at = 1_000 + i as i64;
            v.upsert(&m).unwrap();
            ids.push(m.id);
        }
        // Newest first: d c b a
        assert_eq!(order(&v), ["d", "c", "b", "a"]);

        // Drag "a" to the very top.
        let a = ids[0].clone();
        v.move_memo(&a, "", None, Some(&ids[3])).unwrap();
        assert_eq!(order(&v), ["a", "d", "c", "b"]);
        // Everything now has a key, so the arrangement survives a later edit.
        assert!(v.store().list().unwrap().iter().all(|m| !m.order_key.is_empty()));
    }

    /// Rearranging is not editing: it must not restamp `updated_at`, or the folder it is
    /// arranging reorders underneath it.
    #[test]
    fn arranging_leaves_the_timestamps_alone() {
        let dir = temp_dir();
        let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let mut m = Memo::new("only", "");
        m.updated_at = 4_242;
        v.upsert(&m).unwrap();
        let other = Memo::new("other", "");
        v.upsert(&other).unwrap();

        v.move_memo(&m.id, "", Some(&other.id), None).unwrap();
        assert_eq!(v.store().get(&m.id).unwrap().unwrap().updated_at, 4_242);
    }

    /// An arrangement is a memo field like any other, so the logs alone must restore it.
    #[test]
    fn the_arrangement_survives_a_rebuild_from_logs() {
        let dir = temp_dir();
        let ids: Vec<String>;
        {
            let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
            let mut made = Vec::new();
            for (i, title) in ["a", "b", "c"].iter().enumerate() {
                let mut m = Memo::new(*title, "");
                m.updated_at = 1_000 + i as i64;
                v.upsert(&m).unwrap();
                made.push(m.id);
            }
            // "a" to the top. Both neighbours given: `None` on both sides is not "the
            // bottom", it is the middle of an empty folder.
            v.move_memo(&made[0], "", None, Some(&made[2])).unwrap();
            assert_eq!(order(&v), ["a", "c", "b"]);
            ids = made;
        }
        let v = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        assert_eq!(order(&v), ["a", "c", "b"]);
        assert!(!v.store().get(&ids[0]).unwrap().unwrap().order_key.is_empty());
    }

    /// Placing into an arranged folder puts a new memo on top, where a new memo belongs.
    #[test]
    fn a_new_memo_lands_on_top_of_an_arranged_folder() {
        let dir = temp_dir();
        let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let first = Memo::new("first", "");
        v.upsert(&first).unwrap();
        let second = Memo::new("second", "");
        v.upsert(&second).unwrap();
        v.move_memo(&first.id, "", Some(&second.id), None).unwrap();
        assert_eq!(order(&v), ["second", "first"]);

        v.upsert(&Memo::new("newest", "")).unwrap();
        assert_eq!(order(&v), ["newest", "second", "first"]);
    }

    /// The case a position number cannot survive, and the reason the order is a fractional
    /// index: two devices rearrange the same folder without seeing each other.
    ///
    /// Neither arrangement may be thrown away, and both devices must end up drawing the
    /// folder the same way round — which is what a renumbering scheme cannot promise, because
    /// each device would write a new number for memos it never touched.
    #[test]
    fn two_devices_rearranging_at_once_agree_on_the_result() {
        let dir = temp_dir();

        let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let mut ids = Vec::new();
        for (i, title) in ["a", "b", "c", "d"].iter().enumerate() {
            let mut m = Memo::new(*title, "");
            m.updated_at = 1_000 + i as i64;
            a.upsert(&m).unwrap();
            ids.push(m.id);
        }
        // Arrange it once so both devices start from the same keys, not from timestamps.
        a.move_memo(&ids[0], "", None, Some(&ids[3])).unwrap();
        assert_eq!(order(&a), ["a", "d", "c", "b"]);

        let mut b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        assert_eq!(order(&b), ["a", "d", "c", "b"]);

        // Neither can see the other: A drops "c" at the top, B drops "b" at the top.
        a.move_memo(&ids[2], "", None, Some(&ids[0])).unwrap();
        b.move_memo(&ids[1], "", None, Some(&ids[0])).unwrap();

        a.rebuild().unwrap();
        b.rebuild().unwrap();

        // Both moves survived — neither device's drag was silently undone by the other's.
        let merged = order(&a);
        assert_eq!(merged, order(&b), "the two devices disagree about the folder");
        assert!(
            merged.iter().position(|t| t == "c").unwrap() < merged.iter().position(|t| t == "a").unwrap(),
            "A's move was lost: {merged:?}",
        );
        assert!(
            merged.iter().position(|t| t == "b").unwrap() < merged.iter().position(|t| t == "a").unwrap(),
            "B's move was lost: {merged:?}",
        );
    }

    /// Moving a memo to another folder puts it at the top of that folder, not at whatever
    /// position its old key happens to name among memos it has never been beside.
    #[test]
    fn a_memo_moved_to_another_folder_lands_on_top_of_it() {
        let dir = temp_dir();
        let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let folder = crate::Group::new("folder");
        v.upsert_group(&folder).unwrap();

        // Two memos in the folder, arranged, so it has real keys to land among.
        let mut inside = Vec::new();
        for (i, title) in ["in-one", "in-two"].iter().enumerate() {
            let mut m = Memo::new(*title, "");
            m.group_id = folder.id.clone();
            m.updated_at = 1_000 + i as i64;
            v.upsert(&m).unwrap();
            inside.push(m.id);
        }
        v.move_memo(&inside[0], &folder.id, Some(&inside[1]), None).unwrap();

        // One at the top level, arranged to the bottom so its key is a large one.
        let mut outside = Memo::new("outsider", "");
        outside.updated_at = 5_000;
        v.upsert(&outside).unwrap();
        let filler = Memo::new("filler", "");
        v.upsert(&filler).unwrap();
        v.move_memo(&outside.id, "", Some(&filler.id), None).unwrap();

        // Now move it into the folder the ordinary way — as the list and the phone both do.
        let mut moved = v.store().get(&outside.id).unwrap().unwrap();
        moved.group_id = folder.id.clone();
        v.upsert(&moved).unwrap();

        let in_folder: Vec<String> = v
            .store()
            .in_group(&folder.id)
            .unwrap()
            .into_iter()
            .map(|m| m.title)
            .collect();
        assert_eq!(in_folder[0], "outsider", "landed at {in_folder:?}");
    }

    /// Blob to a file, record to the log — and the logs alone must restore both.
    #[test]
    fn attachment_survives_a_rebuild_from_logs() {
        let dir = temp_dir();
        let photo = b"\x89PNG fake bytes".repeat(50);
        let (memo_id, att_id);
        {
            let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
            let memo = Memo::new("memo with a photo", "");
            v.upsert(&memo).unwrap();
            let a = v.attach(&memo.id, &photo, "photo.png", "image/png", 4000, 3000).unwrap();
            memo_id = memo.id;
            att_id = a.id;
        }

        // Reopening with an empty cache replays the logs.
        let v = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let list = v.store().attachments_of(&memo_id).unwrap();
        assert_eq!(list.len(), 1);
        let a = &list[0];
        assert_eq!(a.id, att_id);
        assert_eq!(a.name, "photo.png");
        assert_eq!((a.width_px, a.height_px), (4000, 3000));
        assert_eq!(a.width_em_milli, crate::DEFAULT_WIDTH_EM_MILLI);
        // The bytes must come back too.
        assert!(v.has_blob(&a.hash));
        assert_eq!(v.attachment_bytes(&a.hash).unwrap(), photo);

        fs::remove_dir_all(&dir).ok();
    }

    /// A display size set on one device carries to the other: it is em, not pixels, so it
    /// stays "so many characters wide" whatever the font size.
    #[test]
    fn display_width_syncs_between_devices() {
        let dir = temp_dir();
        let mut phone = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let memo = Memo::new("photo", "");
        phone.upsert(&memo).unwrap();
        let a = phone.attach(&memo.id, b"jpeg", "p.jpg", "image/jpeg", 1000, 500).unwrap();

        // Shrink to 8em on the phone.
        phone.set_attachment_width(&a.id, 8_000).unwrap();

        // The desktop (another cache, another device) merges and sees the same value.
        let desktop = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let got = desktop.store().get_attachment(&a.id).unwrap().unwrap();
        assert_eq!(got.width_em_milli, 8_000);

        // Same 8em, different base fonts: different pixels, same ratio.
        assert_eq!(got.display_size(16.0), (128.0, 64.0));
        assert_eq!(got.display_size(20.0), (160.0, 80.0));

        fs::remove_dir_all(&dir).ok();
    }

    /// Where a photo was dropped on the note carries to the other device too, and as a
    /// fraction of the note it lands on the same part of a phone screen and a small sticky.
    #[test]
    fn photo_placement_syncs_between_devices() {
        let dir = temp_dir();
        let mut phone = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let memo = Memo::new("photo", "");
        phone.upsert(&memo).unwrap();
        let a = phone.attach(&memo.id, b"jpeg", "p.jpg", "image/jpeg", 1000, 500).unwrap();

        // Drag it to the middle and shrink it, in one write.
        phone.set_attachment_layout(&a.id, 500, 250, 10_000).unwrap();

        let desktop = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let got = desktop.store().get_attachment(&a.id).unwrap().unwrap();
        assert_eq!((got.x_permille, got.y_permille), (500, 250));
        assert_eq!(got.width_em_milli, 10_000);

        // Half across, a quarter down, on either canvas.
        assert_eq!(got.display_pos(800.0, 600.0, 16.0), (400.0, 150.0));
        assert_eq!(got.display_pos(400.0, 1000.0, 16.0), (200.0, 250.0));

        fs::remove_dir_all(&dir).ok();
    }

    /// The float/flow choice travels, and a photo from before the choice existed is a float.
    #[test]
    fn photo_mode_syncs_and_defaults_to_floating() {
        let dir = temp_dir();
        let mut phone = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let memo = Memo::new("photo", "");
        phone.upsert(&memo).unwrap();
        let a = phone.attach(&memo.id, b"jpeg", "p.jpg", "image/jpeg", 1000, 500).unwrap();

        // Every photo starts where photos have always been: lying on top of the note.
        assert_eq!(a.mode(), crate::PhotoMode::Float);
        assert!(a.mode.is_empty(), "a float stores nothing, so old memos need no migration");

        phone.set_attachment_mode(&a.id, crate::PhotoMode::Flow).unwrap();

        let mut desktop = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        assert_eq!(
            desktop.store().get_attachment(&a.id).unwrap().unwrap().mode(),
            crate::PhotoMode::Flow
        );

        // And back again.
        desktop.set_attachment_mode(&a.id, crate::PhotoMode::Float).unwrap();
        let back = desktop.store().get_attachment(&a.id).unwrap().unwrap();
        assert_eq!(back.mode(), crate::PhotoMode::Float);
        assert!(back.mode.is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    /// A mode written by a version this one has never met is drawn as a float rather than
    /// dropping the photo.
    #[test]
    fn an_unknown_photo_mode_reads_as_floating() {
        assert_eq!(crate::PhotoMode::parse("wrapped-around"), crate::PhotoMode::Float);
        assert_eq!(crate::PhotoMode::parse(""), crate::PhotoMode::Float);
        assert_eq!(crate::PhotoMode::parse("flow"), crate::PhotoMode::Flow);
    }

    /// A photo dropped on a note narrower than itself is pulled back on, not left hanging
    /// off the right edge where its resize handle cannot be reached.
    #[test]
    fn photo_placement_stays_on_a_small_note() {
        let mut a = crate::Attachment::new("m", "h");
        a.width_px = 100;
        a.height_px = 100;
        a.width_em_milli = 20_000; // 20em = 320px at a 16px font
        a.x_permille = 900;
        a.y_permille = 900;
        assert_eq!(a.display_pos(400.0, 400.0, 16.0), (80.0, 80.0));
        // Narrower than the photo: flush left rather than off the edge.
        assert_eq!(a.display_pos(200.0, 200.0, 16.0), (0.0, 0.0));
    }

    /// Two photos on one memo do not land on the same spot, or the lower one could not be
    /// picked up again.
    #[test]
    fn photos_cascade_instead_of_stacking() {
        let dir = temp_dir();
        let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let memo = Memo::new("photos", "");
        v.upsert(&memo).unwrap();
        let a = v.attach(&memo.id, b"one", "1.jpg", "image/jpeg", 10, 10).unwrap();
        let b = v.attach(&memo.id, b"two", "2.jpg", "image/jpeg", 10, 10).unwrap();
        assert_ne!((a.x_permille, a.y_permille), (b.x_permille, b.y_permille));

        fs::remove_dir_all(&dir).ok();
    }

    /// Detaching keeps the blob file — no GC, another device may still reference it.
    #[test]
    fn detach_keeps_the_blob_file() {
        let dir = temp_dir();
        let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let memo = Memo::new("memo", "");
        v.upsert(&memo).unwrap();
        let a = v.attach(&memo.id, b"bytes", "x.png", "image/png", 10, 10).unwrap();

        v.detach(&a.id).unwrap();
        assert!(v.store().attachments_of(&memo.id).unwrap().is_empty());
        assert!(v.has_blob(&a.hash), "the blob file must survive");

        // The detach must survive a replay, i.e. it reached the log.
        let v2 = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        assert!(v2.store().attachments_of(&memo.id).unwrap().is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    /// A log written under a foreign key must not fail the rebuild; the readable logs still
    /// apply.
    #[test]
    fn rebuild_skips_undecryptable_foreign_log() {
        let dir = temp_dir();
        let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let memo = Memo::new("my memo", "");
        a.upsert(&memo).unwrap();

        // Plant a log encrypted with a different key.
        let foreign = ChangeLog::open(
            dir.join(LOGS_DIR).join("ffffffff.ymlog"),
            MasterKey::derive(b"other password", &generate_salt()).unwrap(),
        );
        foreign.append(b"not an automerge change").unwrap();

        // The merge still succeeds and our memo survives.
        a.rebuild().unwrap();
        assert_eq!(a.store().get(&memo.id).unwrap().unwrap().title, "my memo");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn create_reopen_rebuilds_cache() {
        let dir = temp_dir();
        let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

        let m1;
        {
            let mut vault = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
            m1 = Memo::new("keeper", "body");
            vault.upsert(&m1).unwrap();
            let dead = Memo::new("doomed", "");
            vault.upsert(&dead).unwrap();
            vault.delete(&dead.id).unwrap();
        } // vault dropped; the log files are the source of truth

        // Reopen with the same cache (same device id, same actor): the seq must continue.
        let m2;
        {
            let mut vault = Vault::open(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
            assert_eq!(vault.store().list().unwrap(), vec![m1.clone()]);
            m2 = Memo::new("second session", "");
            vault.upsert(&m2).unwrap();
        }

        // A brand-new cache restores everything from the logs alone.
        let vault = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let mut titles: Vec<String> =
            vault.store().list().unwrap().into_iter().map(|m| m.title).collect();
        titles.sort();
        assert_eq!(titles, vec!["keeper", "second session"]);

        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&db).ok();
    }

    /// Writes a header wrapping `data_key` under `password`, to imitate Syncthing's conflict
    /// resolution: the winner lands in vault.json while the loser stays as
    /// `vault.sync-conflict-*.json`.
    fn write_test_header(dir: &Path, name: &str, password: &[u8], salt: &Salt, data_key: &MasterKey) {
        let password_key = MasterKey::derive(password, salt).unwrap();
        let header = VaultHeader {
            version: HEADER_VERSION,
            salt: to_hex(salt),
            key_check: to_hex(&data_key.encrypt(KEY_CHECK).unwrap()),
            wrapped_key: to_hex(&password_key.encrypt(&data_key.to_bytes()).unwrap()),
            recovery_salt: String::new(),
            recovery_key: String::new(),
        };
        fs::write(dir.join(name), serde_json::to_vec_pretty(&header).unwrap()).unwrap();
    }

    /// A header in the original unwrapped format, where the password key is the data key.
    fn write_legacy_header(dir: &Path, password: &[u8], salt: &Salt) {
        let key = MasterKey::derive(password, salt).unwrap();
        let header = serde_json::json!({
            "version": 1,
            "salt": to_hex(salt),
            "key_check": to_hex(&key.encrypt(KEY_CHECK).unwrap()),
        });
        fs::write(dir.join(HEADER_FILE), serde_json::to_vec_pretty(&header).unwrap()).unwrap();
    }

    /// With our log under the old salt and vault.json converged on the canonical one, open
    /// must re-encrypt the log and bring the memos back.
    #[test]
    fn heals_divergent_vault_key_on_open() {
        let dir = temp_dir();
        let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

        // This device creates the vault under the old salt and writes a memo.
        let memo;
        {
            let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
            memo = Memo::new("must survive", "body");
            v.upsert(&memo).unwrap();
        }
        let old_salt: Salt = from_hex(
            &serde_json::from_slice::<VaultHeader>(&fs::read(dir.join(HEADER_FILE)).unwrap())
                .unwrap()
                .salt,
        )
        .unwrap()
        .try_into()
        .unwrap();

        // Imitate the conflict resolution: loser to conflict, canonical salt to vault.json.
        fs::rename(
            dir.join(HEADER_FILE),
            dir.join("vault.sync-conflict-20260101-120000-AAAAAAA.json"),
        )
        .unwrap();
        let canonical_salt = generate_salt();
        assert_ne!(canonical_salt, old_salt);
        // The winning device made its own data key, so healing has to re-encrypt the log
        // rather than merely re-derive from another salt.
        let canonical_key = MasterKey::from_bytes(&crate::crypto::generate_key()).unwrap();
        write_test_header(&dir, HEADER_FILE, b"pw", &canonical_salt, &canonical_key);

        // Reopening with the same password heals the log and the memo is back.
        let v = Vault::open(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
        assert_eq!(v.store().get(&memo.id).unwrap().unwrap().title, "must survive");

        // Our log now opens under the canonical key directly.
        let device_id = Store::open(&db).unwrap().device_id().unwrap();
        let own_log = ChangeLog::open(
            dir.join(LOGS_DIR).join(format!("{device_id}.{LOG_EXT}")),
            canonical_key,
        );
        assert!(own_log.read_all().is_ok());

        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&db).ok();
    }

    /// A password change must rewrap the same data key: instant, and invisible to the logs.
    #[test]
    fn change_password_keeps_the_data_key_and_the_memos() {
        let dir = temp_dir();
        let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

        let memo = Memo::new("survives the change", "body");
        let key_before = {
            let mut v = Vault::create(&dir, b"old-pw", Store::open(&db).unwrap()).unwrap();
            v.upsert(&memo).unwrap();
            v.change_password(b"old-pw", b"new-pw").unwrap();
            v.key_bytes()
        };

        // The old password is gone, the new one opens, and the memo never moved.
        assert!(Vault::open(&dir, b"old-pw", Store::open_in_memory().unwrap()).is_err());
        let v = Vault::open(&dir, b"new-pw", Store::open(&db).unwrap()).unwrap();
        assert_eq!(v.store().get(&memo.id).unwrap().unwrap().title, "survives the change");
        // Same data key, so no log or blob had to be rewritten.
        assert_eq!(v.key_bytes(), key_before);

        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&db).ok();
    }

    #[test]
    fn change_password_rejects_a_wrong_current_password() {
        let dir = temp_dir();
        let v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        assert!(v.change_password(b"not-the-password", b"new-pw").is_err());
        assert!(v.change_password(b"pw", b"").is_err(), "an empty new password is rejected");
        // Still the original password.
        assert!(Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).is_ok());
        fs::remove_dir_all(&dir).ok();
    }

    /// The recovery code opens a vault whose password is lost, without touching the data.
    #[test]
    fn recovery_code_sets_a_new_password() {
        let dir = temp_dir();
        let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

        let memo = Memo::new("behind a forgotten password", "body");
        let code = {
            let mut v = Vault::create(&dir, b"forgotten", Store::open(&db).unwrap()).unwrap();
            v.upsert(&memo).unwrap();
            assert!(!v.has_recovery_code());
            let code = v.issue_recovery_code().unwrap();
            assert!(v.has_recovery_code());
            code
        };
        assert!(recovery_code_exists(&dir));

        assert!(reset_password_with_recovery(&dir, "WRONG-C0DE-0000-0000-0000-0000-0000-0000", b"x").is_err());
        // Formatting is not part of the secret: lower case and no dashes still works.
        let typed = code.to_lowercase().replace('-', " ");
        reset_password_with_recovery(&dir, &typed, b"brand-new").unwrap();

        assert!(Vault::open(&dir, b"forgotten", Store::open_in_memory().unwrap()).is_err());
        let v = Vault::open(&dir, b"brand-new", Store::open(&db).unwrap()).unwrap();
        assert_eq!(v.store().get(&memo.id).unwrap().unwrap().title, "behind a forgotten password");
        // The code is not spent by using it.
        assert!(v.has_recovery_code());

        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&db).ok();
    }

    #[test]
    fn recovery_is_refused_when_no_code_was_issued() {
        let dir = temp_dir();
        Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        assert!(!recovery_code_exists(&dir));
        assert!(reset_password_with_recovery(&dir, &crate::recovery::generate(), b"new").is_err());
        fs::remove_dir_all(&dir).ok();
    }

    /// Vaults written before key wrapping must keep opening, and be upgraded in place by a
    /// password change without losing a single record.
    #[test]
    fn legacy_unwrapped_header_opens_and_upgrades() {
        let dir = temp_dir();
        let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

        // Build a vault the old way: the password key encrypts the log directly.
        let salt = generate_salt();
        write_legacy_header(&dir, b"pw", &salt);
        let memo = Memo::new("written before wrapping", "body");
        {
            let mut v = Vault::open(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
            // The data key is the password key while the header stays unwrapped.
            assert_eq!(v.key_bytes(), MasterKey::derive(b"pw", &salt).unwrap().to_bytes());
            v.upsert(&memo).unwrap();
            v.change_password(b"pw", b"pw2").unwrap();
        }

        let v = Vault::open(&dir, b"pw2", Store::open(&db).unwrap()).unwrap();
        assert_eq!(v.store().get(&memo.id).unwrap().unwrap().title, "written before wrapping");
        // Upgraded in place, and the data key still is the original one, so the log written
        // under it is readable without re-encryption.
        assert_eq!(v.key_bytes(), MasterKey::derive(b"pw", &salt).unwrap().to_bytes());
        assert_eq!(read_header(&dir).unwrap().version, HEADER_VERSION);

        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&db).ok();
    }

    #[test]
    fn wipe_empties_a_vault_but_refuses_anything_else() {
        let dir = temp_dir();
        Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        assert!(dir.join(HEADER_FILE).exists());

        let not_a_vault = temp_dir();
        fs::write(not_a_vault.join("holiday-photo.jpg"), b"not ours").unwrap();
        assert!(wipe(&not_a_vault).is_err());
        assert!(not_a_vault.join("holiday-photo.jpg").exists());

        wipe(&dir).unwrap();
        assert!(!dir.join(HEADER_FILE).exists());
        assert!(!dir.join(LOGS_DIR).exists());
        // A wiped directory is a first run again.
        Vault::create(&dir, b"fresh", Store::open_in_memory().unwrap()).unwrap();

        fs::remove_dir_all(&dir).ok();
        fs::remove_dir_all(&not_a_vault).ok();
    }

    /// A folder's colour is part of the document, so it reaches the other devices the same
    /// way its name does.
    #[test]
    fn group_colour_syncs_across_devices() {
        let dir = temp_dir();
        let db_a = std::env::temp_dir().join(format!("ymemo-a-{}.db", uuid::Uuid::new_v4()));
        let db_b = std::env::temp_dir().join(format!("ymemo-b-{}.db", uuid::Uuid::new_v4()));

        let mut group = Group::new("shared folder");
        {
            let mut a = Vault::create(&dir, b"pw", Store::open(&db_a).unwrap()).unwrap();
            a.upsert_group(&group).unwrap();
            group.color = "blue".into();
            a.upsert_group(&group).unwrap();
        }

        // A second device merges the same logs and sees the colour, not the default.
        let b = Vault::open(&dir, b"pw", Store::open(&db_b).unwrap()).unwrap();
        let seen = b.store().get_group(&group.id).unwrap().unwrap();
        assert_eq!(seen.color, "blue");
        assert_eq!(seen.name, "shared folder");

        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&db_a).ok();
        fs::remove_file(&db_b).ok();
    }

    /// The vault's name travels in the logs, so a paired device shows the same one.
    #[test]
    fn vault_name_syncs_across_devices() {
        let dir = temp_dir();
        let db_a = std::env::temp_dir().join(format!("ymemo-a-{}.db", uuid::Uuid::new_v4()));
        let db_b = std::env::temp_dir().join(format!("ymemo-b-{}.db", uuid::Uuid::new_v4()));

        {
            let mut a = Vault::create(&dir, b"pw", Store::open(&db_a).unwrap()).unwrap();
            assert_eq!(a.name(), "", "a new vault has no name until one is given");
            a.set_name("  집 메모  ").unwrap();
            assert_eq!(a.name(), "집 메모", "surrounding space is not part of the name");
        }

        let b = Vault::open(&dir, b"pw", Store::open(&db_b).unwrap()).unwrap();
        assert_eq!(b.name(), "집 메모");

        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&db_a).ok();
        fs::remove_file(&db_b).ok();
    }

    /// A rename is a document change like any other, so it survives the cache being thrown
    /// away and rebuilt from the logs.
    #[test]
    fn vault_name_survives_a_rebuild() {
        let dir = temp_dir();
        let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));
        let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();

        v.set_name("work").unwrap();
        v.set_name("work notes").unwrap();
        v.rebuild().unwrap();
        assert_eq!(v.name(), "work notes");

        // Longer than a heading can hold; cut by characters, so Korean stays whole.
        let long = "가".repeat(crate::VAULT_NAME_MAX + 20);
        v.set_name(&long).unwrap();
        assert_eq!(v.name().chars().count(), crate::VAULT_NAME_MAX);

        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&db).ok();
    }

    /// Folders written before they had colours must still open, at the default.
    #[test]
    fn group_without_colour_falls_back_to_the_default() {
        let dir = temp_dir();
        let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

        let id = {
            let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
            let group = Group::new("no colour here");
            v.upsert_group(&group).unwrap();
            // Imitate the older document shape by dropping the field again.
            let groups = v.groups_obj().unwrap();
            let (_, obj) = v.doc.get(&groups, &group.id).unwrap().unwrap();
            v.doc.delete(&obj, "color").unwrap();
            v.append_local_change().unwrap();
            group.id
        };

        let v = Vault::open(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
        assert_eq!(v.store().get_group(&id).unwrap().unwrap().color, crate::DEFAULT_COLOR);

        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&db).ok();
    }

    /// Every edit leaves a revision, in order, with the values it had at the time.
    #[test]
    fn memo_history_records_each_edit() {
        let dir = temp_dir();
        let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

        let mut memo = Memo::new("first", "one");
        {
            let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
            v.upsert(&memo).unwrap();
            memo.body = "one two".into();
            v.upsert(&memo).unwrap();
            memo.title = "second".into();
            memo.color = "blue".into();
            v.upsert(&memo).unwrap();
        }

        let mut v = Vault::open(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
        let hist = v.history(Entity::Memo, &memo.id).unwrap();
        assert_eq!(hist.len(), 3, "creation plus two edits");
        assert_eq!(hist[0].kind, RevisionKind::Created);
        assert_eq!(hist[0].field("title"), "first");
        assert_eq!(hist[0].field("body"), "one");

        assert_eq!(hist[1].kind, RevisionKind::Edited);
        assert_eq!(hist[1].field("body"), "one two");
        assert_eq!(hist[1].changed, vec!["body".to_string()]);

        // The last revision reports both fields it moved, in the order fields() lists them.
        assert_eq!(hist[2].changed, vec!["title".to_string(), "color".to_string()]);
        // Every revision names the device that wrote it.
        let device = Store::open(&db).unwrap().device_id().unwrap();
        assert!(hist.iter().all(|r| r.device == device));

        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&db).ok();
    }

    /// Revisions must be dated. Automerge's default commit leaves the time at zero, which
    /// showed every version as 1970 until the vault started stamping its own.
    #[test]
    fn revisions_carry_the_time_they_were_written() {
        let dir = temp_dir();
        let before = crate::now_millis();

        let memo = Memo::new("dated", "body");
        let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        v.upsert(&memo).unwrap();

        let rev = &v.history(Entity::Memo, &memo.id).unwrap()[0];
        // Automerge keeps seconds, so the millis it comes back as are rounded down.
        assert!(rev.at >= before - 1000, "revision dated before the write: {}", rev.at);
        assert!(rev.at <= crate::now_millis() + 1000, "revision dated in the future: {}", rev.at);

        fs::remove_dir_all(&dir).ok();
    }

    /// Restoring is a new edit, not a rewrite: the versions it stepped over stay readable.
    #[test]
    fn restoring_appends_rather_than_rewrites() {
        let dir = temp_dir();
        let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

        let mut memo = Memo::new("keep", "original");
        let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
        v.upsert(&memo).unwrap();
        memo.body = "ruined".into();
        v.upsert(&memo).unwrap();
        assert_eq!(v.store().get(&memo.id).unwrap().unwrap().body, "ruined");

        let first = v.history(Entity::Memo, &memo.id).unwrap()[0].clone();
        v.restore(Entity::Memo, &memo.id, &first).unwrap();
        assert_eq!(v.store().get(&memo.id).unwrap().unwrap().body, "original");

        // Three revisions now: the two edits and the restore. Nothing was removed.
        let hist = v.history(Entity::Memo, &memo.id).unwrap();
        assert_eq!(hist.len(), 3);
        assert_eq!(hist[1].field("body"), "ruined", "the bad version is still there");
        assert_eq!(hist[2].field("body"), "original");

        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&db).ok();
    }

    /// A deleted memo keeps its history, and can be brought back from it.
    #[test]
    fn deletion_is_a_revision_and_can_be_undone() {
        let dir = temp_dir();
        let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

        let memo = Memo::new("gone", "body");
        let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
        v.upsert(&memo).unwrap();
        v.delete(&memo.id).unwrap();
        assert!(v.store().get(&memo.id).unwrap().is_none());

        let hist = v.history(Entity::Memo, &memo.id).unwrap();
        assert_eq!(hist.last().unwrap().kind, RevisionKind::Deleted);
        // The deletion itself is not a thing to restore; the version before it is.
        assert!(v.restore(Entity::Memo, &memo.id, hist.last().unwrap()).is_err());

        v.restore(Entity::Memo, &memo.id, &hist[0]).unwrap();
        let back = v.store().get(&memo.id).unwrap().unwrap();
        assert_eq!(back.title, "gone");
        assert_eq!(back.created_at, memo.created_at, "the original creation time comes back");

        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&db).ok();
    }

    /// What a delete hands back is enough to put the memo, and its photos, straight back.
    #[test]
    fn undelete_restores_the_memo_it_removed() {
        let dir = temp_dir();
        let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

        let mut memo = Memo::new("shopping", "milk");
        memo.color = "pink".into();
        let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
        v.upsert(&memo).unwrap();
        let photo = v.attach(&memo.id, b"pretend-jpeg", "photo.jpg", "image/jpeg", 4, 3).unwrap();

        let removed = v.delete(&memo.id).unwrap().expect("the memo was there");
        assert!(v.store().get(&memo.id).unwrap().is_none());

        v.undelete(&removed).unwrap();
        let back = v.store().get(&memo.id).unwrap().unwrap();
        assert_eq!(back.title, "shopping");
        assert_eq!(back.body, "milk");
        assert_eq!(back.color, "pink", "everything about it comes back, not just the text");
        assert_eq!(back.created_at, memo.created_at);
        // Attachments point at the memo, not the other way round, so they were never touched.
        let photos = v.store().attachments_of(&memo.id).unwrap();
        assert_eq!(photos.len(), 1);
        assert_eq!(photos[0].id, photo.id);

        // Deleting something that is not there is not an error, and offers nothing to undo.
        assert!(v.delete("no-such-memo").unwrap().is_none());

        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&db).ok();
    }

    /// Undoing a folder's deletion gathers its contents back up.
    #[test]
    fn undelete_puts_a_folders_contents_back_into_it() {
        let dir = temp_dir();
        let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

        let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
        let folder = Group::new("work");
        v.upsert_group(&folder).unwrap();
        let mut inside = Memo::new("report", "");
        inside.group_id = folder.id.clone();
        v.upsert(&inside).unwrap();
        let mut moved_away = Memo::new("elsewhere", "");
        moved_away.group_id = folder.id.clone();
        v.upsert(&moved_away).unwrap();

        let removed = v.delete_group(&folder.id).unwrap().expect("the folder was there");
        assert_eq!(v.store().get(&inside.id).unwrap().unwrap().group_id, "", "lifted out");

        // One of them is deliberately put somewhere else before the undo, standing in for a
        // user who reorganised in the meantime — an undo must not drag it back.
        let other = Group::new("home");
        v.upsert_group(&other).unwrap();
        let mut moved_away = v.store().get(&moved_away.id).unwrap().unwrap();
        moved_away.group_id = other.id.clone();
        v.upsert(&moved_away).unwrap();

        v.undelete(&removed).unwrap();
        assert_eq!(v.store().get_group(&folder.id).unwrap().unwrap().name, "work");
        assert_eq!(v.store().get(&inside.id).unwrap().unwrap().group_id, folder.id);
        assert_eq!(
            v.store().get(&moved_away.id).unwrap().unwrap().group_id,
            other.id,
            "a memo moved since the deletion stays where the user put it"
        );

        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&db).ok();
    }

    /// A removal is a fact about the vault, so it survives a rebuild and can be taken back.
    #[test]
    fn a_removed_device_is_remembered_and_can_be_taken_back() {
        let dir = temp_dir();
        let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));
        let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();

        assert!(v.revoked_devices().unwrap().is_empty());
        v.revoke_device("GONE-DEVICE").unwrap();
        let revoked = v.revoked_devices().unwrap();
        assert_eq!(revoked.len(), 1);
        assert_eq!(revoked[0].device_id, "GONE-DEVICE");
        assert_eq!(revoked[0].by, v.device_id(), "the deciding device is recorded");
        assert!(revoked[0].at > 0);

        // Saying it twice is not a second change; the callers poll.
        v.revoke_device("GONE-DEVICE").unwrap();
        assert_eq!(v.revoked_devices().unwrap().len(), 1);

        // It comes out of the logs, not out of the cache, so a rebuild keeps it.
        v.rebuild().unwrap();
        assert_eq!(v.revoked_devices().unwrap().len(), 1);
        assert!(!v.is_revoked_here().unwrap(), "this device removed another, not itself");

        // Pairing with it again lifts the removal, and that survives a rebuild too.
        v.unrevoke_device("GONE-DEVICE").unwrap();
        assert!(v.revoked_devices().unwrap().is_empty());
        v.rebuild().unwrap();
        assert!(v.revoked_devices().unwrap().is_empty());

        // A device cannot remove itself; that is what wiping the vault is for.
        let me = v.device_id().to_string();
        assert!(v.revoke_device(&me).is_err());

        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&db).ok();
    }

    /// Folders have a history too, colour included.
    #[test]
    fn group_history_follows_renames_and_colours() {
        let dir = temp_dir();
        let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

        let mut group = Group::new("Inbox");
        let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
        v.upsert_group(&group).unwrap();
        group.name = "Archive".into();
        group.color = "green".into();
        v.upsert_group(&group).unwrap();

        let hist = v.history(Entity::Group, &group.id).unwrap();
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].field("name"), "Inbox");
        assert_eq!(hist[1].changed, vec!["name".to_string(), "color".to_string()]);

        v.restore(Entity::Group, &group.id, &hist[0]).unwrap();
        let back = v.store().get_group(&group.id).unwrap().unwrap();
        assert_eq!(back.name, "Inbox");
        assert_eq!(back.color, crate::DEFAULT_COLOR);

        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&db).ok();
    }

    /// A memo's history must not pick up edits that belong to other memos.
    #[test]
    fn history_ignores_other_memos() {
        let dir = temp_dir();
        let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

        let mine = Memo::new("mine", "");
        let mut other = Memo::new("other", "");
        let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
        v.upsert(&mine).unwrap();
        for i in 0..5 {
            other.body = format!("edit {i}");
            v.upsert(&other).unwrap();
        }

        assert_eq!(v.history(Entity::Memo, &mine.id).unwrap().len(), 1);
        // The first pass through the loop creates it, the other four edit it.
        assert_eq!(v.history(Entity::Memo, &other.id).unwrap().len(), 5);

        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&db).ok();
    }

    #[test]
    fn wrong_password_rejected_by_key_check() {
        let dir = temp_dir();
        Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        // The header canary alone rejects it, even with an empty log.
        assert!(Vault::open(&dir, b"wrong", Store::open_in_memory().unwrap()).is_err());
        fs::remove_dir_all(&dir).ok();
    }

    /// "Stay unlocked": the cached raw key alone opens the same vault.
    #[test]
    fn cached_key_opens_vault_without_password() {
        let dir = temp_dir();
        let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

        let memo = Memo::new("seen while unlocked", "body");
        let key_bytes = {
            let mut vault = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
            vault.upsert(&memo).unwrap();
            vault.key_bytes()
        };

        let key = MasterKey::from_bytes(&key_bytes).unwrap();
        let vault = Vault::open_with_key(&dir, key, Store::open(&db).unwrap()).unwrap();
        assert_eq!(vault.store().list().unwrap(), vec![memo]);

        // A bogus key is caught by the header canary.
        let bogus = MasterKey::from_bytes(&[7u8; crate::crypto::KEY_LEN]).unwrap();
        assert!(Vault::open_with_key(&dir, bogus, Store::open_in_memory().unwrap()).is_err());

        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&db).ok();
    }

    /// The point of automerge: concurrent edits to **different fields** of one memo both
    /// survive. The old last-write-wins model dropped one side wholesale.
    #[test]
    fn concurrent_field_edits_both_survive() {
        let dir = temp_dir();

        // Device A creates the memo.
        let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let base = Memo::new("original title", "original body");
        a.upsert(&base).unwrap();

        // Device B starts from the same state.
        let mut b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        assert_ne!(a.device_id(), b.device_id());
        assert_eq!(b.store().list().unwrap().len(), 1);

        // Concurrent edits: A the title, B the body.
        let mut a_edit = base.clone();
        a_edit.title = "title from A".into();
        a.upsert(&a_edit).unwrap();

        let mut b_edit = base.clone();
        b_edit.body = "body from B".into();
        b.upsert(&b_edit).unwrap();

        // Both sides must converge on the same merge.
        a.rebuild().unwrap();
        b.rebuild().unwrap();
        for v in [&a, &b] {
            let merged = v.store().get(&base.id).unwrap().unwrap();
            assert_eq!(merged.title, "title from A");
            assert_eq!(merged.body, "body from B");
        }

        // One log file per device.
        assert_eq!(fs::read_dir(dir.join(LOGS_DIR)).unwrap().count(), 2);

        fs::remove_dir_all(&dir).ok();
    }

    /// A rebuild with nothing new is skipped, and one with something new is never skipped.
    ///
    /// The skip is what keeps the merge timer off the UI thread when no change has arrived,
    /// and getting it wrong is silent: the vault simply stops seeing the other device. The
    /// case that matters is a **local write while another device's change is waiting** —
    /// marking the whole log directory merged there would call that change read.
    #[test]
    fn a_rebuild_skips_nothing_it_has_not_already_read() {
        let dir = std::env::temp_dir().join(format!("ymemo-skip-{}", uuid::Uuid::new_v4()));
        let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let base = Memo::new("from A", "body");
        a.upsert(&base).unwrap();

        let mut b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();

        // Nothing has happened since B read the logs, so this rebuild has nothing to do —
        // and must leave the document it already has alone.
        b.rebuild().unwrap();
        assert_eq!(b.store().list().unwrap().len(), 1);

        // A writes. B has not read it yet.
        let mut edit = base.clone();
        edit.title = "A wrote again".into();
        a.upsert(&edit).unwrap();

        // B writes something of its own *first*. Its own log growing is not news; A's is.
        let mut own = Memo::new("from B", "body");
        own.id = "b-memo".into();
        b.upsert(&own).unwrap();

        b.rebuild().unwrap();
        let merged = b.store().get(&base.id).unwrap().unwrap();
        assert_eq!(merged.title, "A wrote again", "A's change must survive B's own write");
        assert!(b.store().get("b-memo").unwrap().is_some());

        fs::remove_dir_all(&dir).ok();
    }

    /// A revision is the memo as it stood *after* that change, merges included.
    ///
    /// Two devices editing different fields at once produce two changes that neither has
    /// seen the other make. Reading either one on its own shows that device's branch — and
    /// the newest revision would then be missing the other's edit, which `restore` writes
    /// back field by field: putting the latest version back would silently undo it.
    #[test]
    fn a_revision_after_a_concurrent_edit_carries_the_merge() {
        let dir = std::env::temp_dir().join(format!("ymemo-conc-{}", uuid::Uuid::new_v4()));
        let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let mut b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();

        let m = Memo::new("start", "body");
        a.upsert(&m).unwrap();
        b.rebuild().unwrap();

        // Neither has seen the other's edit when it makes its own.
        let mut a_edit = m.clone();
        a_edit.title = "A title".into();
        a.upsert(&a_edit).unwrap();
        let mut b_edit = m.clone();
        b_edit.body = "B body".into();
        b.upsert(&b_edit).unwrap();
        a.rebuild().unwrap();
        b.rebuild().unwrap();

        for v in [&mut a, &mut b] {
            let hist = v.history(Entity::Memo, &m.id).unwrap();
            let last = hist.last().unwrap();
            assert_eq!(last.field("title"), "A title", "the newest revision holds A's edit");
            assert_eq!(last.field("body"), "B body", "and B's");
            // It names only what that one change moved. *Which* of the two is last is up to
            // the order the changes merge in and differs between the devices — the field
            // values above are what both agree on, and what `restore` writes back.
            assert_eq!(last.changed.len(), 1);
            assert!(["title", "body"].contains(&last.changed[0].as_str()));
        }

        fs::remove_dir_all(&dir).ok();
    }

    /// Groups propagate through the logs, and so does a memo's membership.
    #[test]
    fn groups_sync_across_devices() {
        let dir = temp_dir();

        let group;
        let memo;
        {
            let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
            group = Group::new("Work");
            a.upsert_group(&group).unwrap();
            memo = {
                let mut m = Memo::new("report", "");
                m.group_id = group.id.clone();
                m
            };
            a.upsert(&memo).unwrap();
        }

        // Another device with an empty cache restores from the logs.
        let b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let groups = b.store().list_groups().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "Work");
        assert_eq!(b.store().get(&memo.id).unwrap().unwrap().group_id, group.id);

        fs::remove_dir_all(&dir).ok();
    }

    /// Deleting a group lifts its memos and subgroups instead of destroying them.
    #[test]
    fn deleting_group_lifts_children_instead_of_destroying() {
        let dir = temp_dir();
        let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();

        let outer = Group::new("outer");
        v.upsert_group(&outer).unwrap();
        let mut inner = Group::new("inner");
        inner.parent_id = outer.id.clone();
        v.upsert_group(&inner).unwrap();
        let mut memo = Memo::new("memo inside", "");
        memo.group_id = outer.id.clone();
        v.upsert(&memo).unwrap();

        v.delete_group(&outer.id).unwrap();

        // Only the outer group is gone; the rest moved to the top level.
        let groups = v.store().list_groups().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].id, inner.id);
        assert_eq!(groups[0].parent_id, "");
        let survived = v.store().get(&memo.id).unwrap().unwrap();
        assert_eq!(survived.group_id, "");

        fs::remove_dir_all(&dir).ok();
    }

    /// Two devices editing the **same field** keep both edits and agree on the result.
    ///
    /// This is what a memo being `Text` rather than a string buys. A plain string is
    /// last-write-wins: measured on two real devices, typing into the same memo inside the
    /// twenty seconds it takes to sync made one version simply become the memo, and the other
    /// was only findable in the change history. Nothing said so.
    ///
    /// Replacing the *whole* field on both sides, as here, is the worst case and reads oddly —
    /// the two versions end up run together. It is still the right trade: odd beats gone, and
    /// editing different parts of a real note (the test below) merges cleanly.
    #[test]
    fn same_field_conflict_converges() {
        let dir = temp_dir();

        let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let base = Memo::new("t", "");
        a.upsert(&base).unwrap();

        let mut b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();

        let mut a_edit = base.clone();
        a_edit.body = "from A".into();
        a.upsert(&a_edit).unwrap();
        let mut b_edit = base.clone();
        b_edit.body = "from B".into();
        b.upsert(&b_edit).unwrap();

        a.rebuild().unwrap();
        b.rebuild().unwrap();
        let ta = a.store().get(&base.id).unwrap().unwrap().body;
        let tb = b.store().get(&base.id).unwrap().unwrap().body;
        assert_eq!(ta, tb, "both devices must agree");
        assert!(ta.contains("from A"), "A's edit survived: {ta:?}");
        assert!(tb.contains("from B"), "B's edit survived: {tb:?}");

        fs::remove_dir_all(&dir).ok();
    }
    /// Two devices adding a line each to the same note end up with both lines.
    ///
    /// The everyday shape of the case above: nobody replaces a whole note, they add to it.
    #[test]
    fn edits_in_different_places_merge_cleanly() {
        let dir = temp_dir();
        let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let base = Memo::new("list", "milk\nbread\n");
        a.upsert(&base).unwrap();
        let mut b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();

        // A adds to the end, B to the front; neither has seen the other.
        let mut a_edit = base.clone();
        a_edit.body = "milk\nbread\napples\n".into();
        a.upsert(&a_edit).unwrap();
        let mut b_edit = base.clone();
        b_edit.body = "eggs\nmilk\nbread\n".into();
        b.upsert(&b_edit).unwrap();

        a.rebuild().unwrap();
        b.rebuild().unwrap();
        let ba = a.store().get(&base.id).unwrap().unwrap().body;
        let bb = b.store().get(&base.id).unwrap().unwrap().body;
        assert_eq!(ba, bb, "both devices must agree");
        for line in ["milk", "bread", "apples", "eggs"] {
            assert!(ba.contains(line), "{line:?} survived the merge: {ba:?}");
        }

        fs::remove_dir_all(&dir).ok();
    }

    /// A **label** edited on two devices at once settles on one somebody chose.
    ///
    /// The other half of the rule in `put_text_if_changed`: a folder's name and a memo's
    /// title are replaced whole, not added to, so they stay last-write-wins. Merging them
    /// character by character turned "Work" and "Home" into **"WHorkme"** — both edits
    /// technically kept, and a folder nobody named that the user now has to repair. The one
    /// that lost is still in the change history.
    #[test]
    fn two_devices_renaming_one_folder_settle_on_a_real_name() {
        let dir = temp_dir();
        let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let mut g = Group::new("Inbox");
        g.id = "g".into();
        a.upsert_group(&g).unwrap();
        let mut b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();

        let mut ga = g.clone();
        ga.name = "Work".into();
        a.upsert_group(&ga).unwrap();
        let mut gb = g.clone();
        gb.name = "Home".into();
        b.upsert_group(&gb).unwrap();

        a.rebuild().unwrap();
        b.rebuild().unwrap();
        let na = a.store().get_group("g").unwrap().unwrap().name;
        let nb = b.store().get_group("g").unwrap().unwrap().name;
        assert_eq!(na, nb, "both devices must agree");
        assert!(na == "Work" || na == "Home", "a name somebody chose, not a merge: {na:?}");

        fs::remove_dir_all(&dir).ok();
    }

    /// A delete on one device beats an edit on the other, and both agree it is gone.
    ///
    /// Not an accident of the merge — worth pinning, because the alternative (an edit
    /// resurrecting a memo somebody deleted) is the more surprising of the two. What was
    /// typed is still in the change history, and the delete itself is what the undo bar
    /// hands back.
    #[test]
    fn a_delete_beats_an_edit_made_at_the_same_time() {
        let dir = temp_dir();
        let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let m = Memo::new("keep", "line one\n");
        a.upsert(&m).unwrap();
        let mut b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();

        a.delete(&m.id).unwrap();
        let mut edit = m.clone();
        edit.body = "line one\nline two\n".into();
        b.upsert(&edit).unwrap();

        a.rebuild().unwrap();
        b.rebuild().unwrap();
        assert!(a.store().get(&m.id).unwrap().is_none());
        assert!(b.store().get(&m.id).unwrap().is_none(), "both devices agree it is gone");

        fs::remove_dir_all(&dir).ok();
    }

    /// A group must survive the cache being rebuilt from the logs — the merge timer does that
    /// every few seconds, so anything it drops disappears while the user is looking at it.
    #[test]
    fn rebuild_keeps_groups() {
        let dir = std::env::temp_dir().join(format!("ymemo-rebuild-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(dir.join("cache.db")).unwrap();
        let mut v = Vault::open_or_create(dir.join("vault"), b"pw", store).unwrap();

        let group = crate::Group::new("work");
        v.upsert_group(&group).unwrap();
        assert_eq!(v.store().list_groups().unwrap().len(), 1, "just created");

        v.rebuild().unwrap();
        let after = v.store().list_groups().unwrap();
        assert_eq!(after.len(), 1, "the group vanished when the cache was rebuilt");
        assert_eq!(after[0].name, "work");

        std::fs::remove_dir_all(&dir).ok();
    }
}
