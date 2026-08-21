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
//! `ROOT.groups: Map<group_id, {...}>`, `ROOT.attachments: Map<attachment_id, {...}>`.
//! Photo bytes stay out of the document, in `blobs/<hash>.ymblob`; an attachment only
//! points at the hash.
//!
//! Automerge merges changes order-independently: edits to different fields of one memo
//! both survive, and a conflict on the same field converges deterministically. The actor
//! id is the device id, so only our own log carries our actor.

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
use crate::crypto::{generate_salt, MasterKey, Salt, SALT_LEN};
use crate::{clamp_width_em_milli, Attachment, Group, Memo, Store};

const HEADER_FILE: &str = "vault.json";
const LOGS_DIR: &str = "logs";
const LOG_EXT: &str = "ymlog";
/// Canary plaintext, stored encrypted in the header to detect a wrong password early.
const KEY_CHECK: &[u8] = b"ymemo-key-check-v1";

/// Contents of `vault.json`. The salt is not secret.
#[derive(Serialize, Deserialize)]
struct VaultHeader {
    version: u32,
    /// Argon2id salt, hex encoded.
    salt: String,
    /// `encrypt(KEY_CHECK)`, hex encoded; decrypting it proves the password.
    key_check: String,
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
}

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
        let key = MasterKey::derive(password, &salt)?;
        let header = VaultHeader {
            version: 1,
            salt: to_hex(&salt),
            key_check: to_hex(&key.encrypt(KEY_CHECK)?),
        };
        fs::write(&header_path, serde_json::to_vec_pretty(&header)?)?;

        Self::open(dir, password, store)
    }

    /// Opens a vault: verifies the password against the header canary, then merges every
    /// device log into the document and rebuilds the cache.
    pub fn open(dir: impl AsRef<Path>, password: &[u8], store: Store) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        let header = read_header(&dir)?;

        let salt_vec = from_hex(&header.salt)?;
        let salt: Salt = salt_vec
            .try_into()
            .map_err(|_| anyhow!(t!("core.salt_length_bad", expected = SALT_LEN)))?;
        let key = MasterKey::derive(password, &salt)?;
        verify_key(&header, &key)?;

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

    /// Inserts or updates a memo, writing only changed fields so merges stay field-level.
    pub fn upsert(&mut self, memo: &Memo) -> Result<()> {
        let memos = self.memos_obj()?;
        let obj = match self.doc.get(&memos, &memo.id)? {
            Some((Value::Object(ObjType::Map), id)) => id,
            _ => self.doc.put_object(&memos, &memo.id, ObjType::Map)?,
        };
        put_str_if_changed(&mut self.doc, &obj, "title", &memo.title)?;
        put_str_if_changed(&mut self.doc, &obj, "body", &memo.body)?;
        put_str_if_changed(&mut self.doc, &obj, "color", &memo.color)?;
        put_i64_if_changed(&mut self.doc, &obj, "opacity", crate::clamp_opacity(memo.opacity))?;
        put_str_if_changed(&mut self.doc, &obj, "group_id", &memo.group_id)?;
        put_i64_if_changed(&mut self.doc, &obj, "created_at", memo.created_at)?;
        put_i64_if_changed(&mut self.doc, &obj, "updated_at", memo.updated_at)?;

        self.append_local_change()?;
        self.store.upsert(memo)
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
        put_str_if_changed(&mut self.doc, &obj, "name", &group.name)?;
        put_str_if_changed(&mut self.doc, &obj, "parent_id", &group.parent_id)?;
        put_i64_if_changed(&mut self.doc, &obj, "created_at", group.created_at)?;
        put_i64_if_changed(&mut self.doc, &obj, "updated_at", group.updated_at)?;

        self.append_local_change()?;
        self.store.upsert_group(group)
    }

    /// Deletes a group and **lifts** its groups and memos to the parent instead of deleting
    /// them — removing a folder must not remove its memos.
    pub fn delete_group(&mut self, id: &str) -> Result<()> {
        let parent = self
            .store
            .get_group(id)?
            .map(|g| g.parent_id)
            .unwrap_or_default();

        // Lift child groups.
        let children: Vec<Group> = self
            .store
            .list_groups()?
            .into_iter()
            .filter(|g| g.parent_id == id)
            .collect();
        for mut child in children {
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
        for mut memo in memos {
            memo.group_id = parent.clone();
            memo.updated_at = crate::now_millis();
            self.upsert(&memo)?;
        }

        let groups = self.groups_obj()?;
        if self.doc.get(&groups, id)?.is_some() {
            self.doc.delete(&groups, id)?;
        }
        self.append_local_change()?;
        self.store.delete_group(id)
    }

    /// Deletes a memo.
    pub fn delete(&mut self, id: &str) -> Result<()> {
        let memos = self.memos_obj()?;
        if self.doc.get(&memos, id)?.is_some() {
            self.doc.delete(&memos, id)?;
        }
        self.append_local_change()?;
        self.store.delete(id)
    }

    /// Merges every log in `logs/` into a fresh document and rebuilds the SQLite cache from
    /// scratch. One call picks up whatever Syncthing has delivered.
    pub fn rebuild(&mut self) -> Result<()> {
        let mut doc = AutoCommit::new();
        doc.apply_changes(self.read_all_changes()?)?;
        // actor = device_id, so later local changes continue our own actor sequence.
        doc.set_actor(ActorId::from(self.device_id.as_bytes()));
        self.doc = doc;
        self.materialize()
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
                    eprintln!("skipping log (decrypt failed) {}: {e}", path.display());
                    continue;
                }
            };
            for record in records {
                match Change::from_bytes(record) {
                    Ok(c) => changes.push(c),
                    Err(e) => eprintln!("skipping change (parse failed) {}: {e}", path.display()),
                }
            }
        }
        Ok(changes)
    }

    /// Commits the pending local edit and appends it, encrypted, to our own log. Writes
    /// nothing when there was no actual change.
    fn append_local_change(&mut self) -> Result<()> {
        if self.doc.commit().is_some() {
            let change = self
                .doc
                .get_last_local_change()
                .context(t!("core.no_local_change"))?;
            self.own_log.append(change.raw_bytes())?;
        }
        Ok(())
    }

    /// Materializes the document into the SQLite cache.
    fn materialize(&mut self) -> Result<()> {
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
                    title: get_str(&self.doc, &obj, "title")?,
                    body: get_str(&self.doc, &obj, "body")?,
                    // color/opacity came later, so old changes may not carry them.
                    color: get_str_or(&self.doc, &obj, "color", crate::DEFAULT_COLOR),
                    opacity: crate::clamp_opacity(get_i64_or(
                        &self.doc,
                        &obj,
                        "opacity",
                        crate::DEFAULT_OPACITY,
                    )),
                    group_id: get_str_or(&self.doc, &obj, "group_id", ""),
                    created_at: get_i64(&self.doc, &obj, "created_at")?,
                    updated_at: get_i64(&self.doc, &obj, "updated_at")?,
                };
                self.store.upsert(&memo)?;
            }
        }
        self.materialize_groups()?;
        self.materialize_attachments()
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
                name: get_str_or(&self.doc, &obj, "name", ""),
                parent_id: get_str_or(&self.doc, &obj, "parent_id", ""),
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
    // Look for the old key among the conflict headers' salts.
    for salt in conflict_salts(dir) {
        let old_key = MasterKey::derive(password, &salt)?;
        if ChangeLog::open(&own_path, old_key.clone()).read_all().is_ok() {
            reencrypt_log(&own_path, &old_key, canonical_key)?;
            eprintln!("diverged vault key: re-encrypted our log under the canonical key");
            return Ok(());
        }
    }
    // Not found: rebuild will just skip this log and merge the others.
    eprintln!(
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

/// Salts parsed out of the `vault.sync-conflict-*.json` files; unreadable ones are skipped.
fn conflict_salts(dir: &Path) -> Vec<Salt> {
    let mut salts = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return salts;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !(name.starts_with("vault.sync-conflict-") && name.ends_with(".json")) {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else { continue };
        let Ok(header) = serde_json::from_slice::<VaultHeader>(&bytes) else { continue };
        let Ok(salt_vec) = from_hex(&header.salt) else { continue };
        if let Ok(salt) = <Salt>::try_from(salt_vec) {
            salts.push(salt);
        }
    }
    salts
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

fn get_str(doc: &AutoCommit, obj: &ObjId, key: &str) -> Result<String> {
    match doc.get(obj, key)? {
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::Str(v) => Ok(v.to_string()),
            other => bail!(t!("core.field_not_string", key = key, found = format!("{other:?}"))),
        },
        _ => bail!(t!("core.field_missing", key = key)),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh temporary vault directory.
    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ymemo-vault-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
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

    /// Writes a header, used to imitate Syncthing's conflict resolution: the canonical salt
    /// lands in vault.json while the loser stays as `vault.sync-conflict-*.json`.
    fn write_header(dir: &Path, name: &str, password: &[u8], salt: &Salt) {
        let key = MasterKey::derive(password, salt).unwrap();
        let header = VaultHeader {
            version: 1,
            salt: to_hex(salt),
            key_check: to_hex(&key.encrypt(KEY_CHECK).unwrap()),
        };
        fs::write(dir.join(name), serde_json::to_vec_pretty(&header).unwrap()).unwrap();
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
        write_header(&dir, HEADER_FILE, b"pw", &canonical_salt);

        // Reopening with the same password heals the log and the memo is back.
        let v = Vault::open(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
        assert_eq!(v.store().get(&memo.id).unwrap().unwrap().title, "must survive");

        // Our log now opens under the canonical key directly.
        let device_id = Store::open(&db).unwrap().device_id().unwrap();
        let canonical_key = MasterKey::derive(b"pw", &canonical_salt).unwrap();
        let own_log = ChangeLog::open(
            dir.join(LOGS_DIR).join(format!("{device_id}.{LOG_EXT}")),
            canonical_key,
        );
        assert!(own_log.read_all().is_ok());

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

    /// A conflict on the same field converges to one value on both sides.
    #[test]
    fn same_field_conflict_converges() {
        let dir = temp_dir();

        let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let base = Memo::new("t", "");
        a.upsert(&base).unwrap();

        let mut b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();

        let mut a_edit = base.clone();
        a_edit.title = "from A".into();
        a.upsert(&a_edit).unwrap();
        let mut b_edit = base.clone();
        b_edit.title = "from B".into();
        b.upsert(&b_edit).unwrap();

        a.rebuild().unwrap();
        b.rebuild().unwrap();
        let ta = a.store().get(&base.id).unwrap().unwrap().title;
        let tb = b.store().get(&base.id).unwrap().unwrap().title;
        assert_eq!(ta, tb); // whoever wins, both must agree
        assert!(ta == "from A" || ta == "from B");

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
