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

use anyhow::{bail, Context, Result};
use automerge::{transaction::Transactable, ActorId, AutoCommit, Change, ROOT};
use std::fs;
use std::path::{Path, PathBuf};
use ymemo_i18n::t;

use crate::blob::BlobStore;
use crate::changelog::ChangeLog;
use crate::crypto::{generate_salt, MasterKey};
use crate::history::{Entity, Revision, RevisionKind};
use crate::{Group, Memo, Store};

mod devices;
mod doc;
mod header;
mod materialize;
mod memos;
mod photos;
#[cfg(test)]
mod tests;

pub use header::{recovery_code_exists, reset_password_with_recovery, wipe};
use header::{
    heal_divergent_log, read_header, to_hex, unlock_header, verify_key, write_header, VaultHeader,
};
use doc::{get_str_or, non_empty};

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
        write_header(dir, &header)?;

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
        // A crash in the middle of an append leaves half a record at the end of our log, and
        // the next append would land after it. Cut it off before anything is written.
        if let Err(e) = own_log.repair_tail() {
            crate::diag!("could not check our log for a torn record: {e}");
        }

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

    /// Throws away the document in memory, uncommitted edits included, and reads it back
    /// from the logs — [`Vault::rebuild`] without the shortcut that skips an unchanged
    /// directory. For a vault a panic interrupted mid-edit: what is in the logs is what was
    /// really written, and whatever the panic left half-done in memory is not.
    pub fn reload(&mut self) -> Result<()> {
        self.built_from = None;
        self.rebuild().map(|_| ())
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
}
