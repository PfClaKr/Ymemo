//! `vault.json`: the header that holds the data key wrapped under the password (and the
//! recovery code), unlocking it, and the repairs a diverged key needs.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use ymemo_i18n::t;

use crate::changelog::ChangeLog;
use crate::crypto::{generate_salt, MasterKey, Salt, SALT_LEN};

use super::{HEADER_FILE, HEADER_VERSION, KEY_CHECK, LOG_EXT, LOGS_DIR, Vault};

/// Contents of `vault.json`. Neither salt is secret, and every key in it is wrapped.
///
/// The optional fields are absent in the original format; `#[serde(default)]` reads those
/// headers, and `skip_serializing_if` keeps us from writing empty ones back.
#[derive(Serialize, Deserialize, Clone)]
pub(super) struct VaultHeader {
    pub(super) version: u32,
    /// Argon2id salt for the master password, hex encoded.
    pub(super) salt: String,
    /// `encrypt(data key, KEY_CHECK)`, hex encoded; decrypting it proves the data key.
    pub(super) key_check: String,
    /// `encrypt(password key, data key)`, hex encoded. Empty means the original format,
    /// where the password key *is* the data key.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(super) wrapped_key: String,
    /// Argon2id salt for the recovery code; empty when no code was ever issued.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(super) recovery_salt: String,
    /// `encrypt(recovery key, data key)`, hex encoded; empty when no code was issued.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(super) recovery_key: String,
}

impl Vault {
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
pub(super) fn heal_divergent_log(
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
pub(super) fn read_header(dir: &Path) -> Result<VaultHeader> {
    let path = dir.join(HEADER_FILE);
    let bytes =
        fs::read(&path).with_context(|| t!("core.header_missing", path = path.display()))?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Writes the header through a temporary file, so a crash mid-write cannot leave a
/// truncated `vault.json` — the one file without which no device can open the vault.
pub(super) fn write_header(dir: &Path, header: &VaultHeader) -> Result<()> {
    crate::fsutil::write_atomic(&dir.join(HEADER_FILE), &serde_json::to_vec_pretty(header)?)?;
    Ok(())
}

/// Derives a wrapping key from a password (or a recovery code) and a hex-encoded salt.
pub(super) fn derive_wrapping_key(secret: &[u8], salt_hex: &str) -> Result<MasterKey> {
    let salt: Salt = from_hex(salt_hex)?
        .try_into()
        .map_err(|_| anyhow!(t!("core.salt_length_bad", expected = SALT_LEN)))?;
    MasterKey::derive(secret, &salt)
}

/// Unwraps a data key that was wrapped under `wrapper`.
pub(super) fn unwrap_key(wrapper: &MasterKey, wrapped_hex: &str, wrong: &str) -> Result<MasterKey> {
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
pub(super) fn unlock_header(header: &VaultHeader, password: &[u8]) -> Result<MasterKey> {
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
pub(super) fn unlock_header_with_recovery(header: &VaultHeader, code: &str) -> Result<MasterKey> {
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
pub(super) fn rewrap_password(dir: &Path, data_key: &MasterKey, new_password: &[u8]) -> Result<()> {
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
pub(super) fn verify_key(header: &VaultHeader, key: &MasterKey) -> Result<()> {
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
pub(super) fn conflict_headers(dir: &Path) -> Vec<VaultHeader> {
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
pub(super) fn reencrypt_log(path: &Path, old_key: &MasterKey, new_key: &MasterKey) -> Result<()> {
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

pub(super) fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(super) fn from_hex(s: &str) -> Result<Vec<u8>> {
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
