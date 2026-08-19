//! Content-addressed blob store — where the bytes of a photo attachment live.
//!
//! Layout: `<vault_dir>/blobs/<sha256(plaintext) hex>.ymblob`
//!
//! Three properties carry the design:
//!
//! 1. **Files are immutable**, because the name is the content hash. Like the per-device
//!    logs, that means zero sync conflicts, and the same photo added on two devices
//!    collapses into one file.
//! 2. **Encryption is convergent.** The nonce is derived from the plaintext hash, so the
//!    same photo encrypts to identical bytes anywhere. A random nonce would give the same
//!    file name different contents and Syncthing would create conflict copies. Different
//!    plaintexts hash differently, so nonces are never reused.
//! 3. **Nothing is deleted (no GC).** Detaching a photo leaves its blob behind: an
//!    append-only model cannot tell whether another device still references it, and a
//!    wrong delete would make the photo vanish from that device's memo. Disk space traded
//!    for safety.

use anyhow::Result;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

use crate::crypto::{MasterKey, NONCE_LEN};

const BLOBS_DIR: &str = "blobs";
const BLOB_EXT: &str = "ymblob";
/// Domain separator, so nonce derivation never collides with other hashes.
const NONCE_DOMAIN: &[u8] = b"ymemo-blob-nonce-v1";

/// The vault's blob directory. Owns the key and en/decrypts on access.
pub struct BlobStore {
    dir: PathBuf,
    key: MasterKey,
}

impl BlobStore {
    /// Opens `<vault_dir>/blobs`; the directory is created on first write.
    pub fn open(vault_dir: impl AsRef<Path>, key: MasterKey) -> Self {
        Self {
            dir: vault_dir.as_ref().join(BLOBS_DIR),
            key,
        }
    }

    /// Stores plaintext bytes and returns their content hash.
    ///
    /// Existing content is never rewritten: files are immutable, so the same photo on
    /// several memos is stored once.
    pub fn put(&self, plaintext: &[u8]) -> Result<String> {
        let hash = content_hash(plaintext);
        let path = self.path(&hash);
        if path.exists() {
            return Ok(hash);
        }
        fs::create_dir_all(&self.dir)?;
        let sealed = self.key.encrypt_with_nonce(plaintext, &nonce_for(&hash))?;
        // Write beside it and rename, so a half-written file is never synced.
        let tmp = path.with_extension("part");
        fs::write(&tmp, &sealed)?;
        fs::rename(&tmp, &path)?;
        Ok(hash)
    }

    /// Reads plaintext by hash. Errors if the file has not synced yet or the key is wrong.
    pub fn get(&self, hash: &str) -> Result<Vec<u8>> {
        let data = fs::read(self.path(hash))?;
        self.key.decrypt(&data)
    }

    /// Whether the blob has arrived on this device; if not, the UI shows a placeholder.
    pub fn has(&self, hash: &str) -> bool {
        self.path(hash).exists()
    }

    /// On-disk path. The hash is hex, so it cannot escape the directory.
    pub fn path(&self, hash: &str) -> PathBuf {
        self.dir.join(format!("{hash}.{BLOB_EXT}"))
    }
}

/// sha256 of the plaintext as hex: the blob's name and the id an attachment points at.
pub fn content_hash(plaintext: &[u8]) -> String {
    let digest = Sha256::digest(plaintext);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Derives the nonce from the content hash — the crux of convergent encryption.
fn nonce_for(hash: &str) -> [u8; NONCE_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(NONCE_DOMAIN);
    hasher.update(hash.as_bytes());
    let digest = hasher.finalize();
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&digest[..NONCE_LEN]);
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::generate_salt;

    fn store(dir: &Path) -> BlobStore {
        let key = MasterKey::derive(b"pw", &generate_salt()).unwrap();
        BlobStore::open(dir, key)
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ymemo-blob-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trips_and_dedupes() {
        let dir = tmp_dir("round");
        let s = store(&dir);

        let data = b"pretend this is a photo".repeat(100);
        let hash = s.put(&data).unwrap();
        assert!(s.has(&hash));
        assert_eq!(s.get(&hash).unwrap(), data);

        // Re-putting the same content yields one file.
        assert_eq!(s.put(&data).unwrap(), hash);
        let files: Vec<_> = fs::read_dir(dir.join(BLOBS_DIR)).unwrap().collect();
        assert_eq!(files.len(), 1);
    }

    /// Same key and plaintext must produce byte-identical files; otherwise two devices
    /// write different contents under one name and sync conflicts.
    #[test]
    fn encryption_is_convergent() {
        let dir_a = tmp_dir("conv-a");
        let dir_b = tmp_dir("conv-b");
        let salt = generate_salt();
        let key_a = MasterKey::derive(b"pw", &salt).unwrap();
        let key_b = MasterKey::derive(b"pw", &salt).unwrap();
        let (a, b) = (BlobStore::open(&dir_a, key_a), BlobStore::open(&dir_b, key_b));

        let data = b"same photo on two devices";
        let hash_a = a.put(data).unwrap();
        let hash_b = b.put(data).unwrap();
        assert_eq!(hash_a, hash_b);
        assert_eq!(fs::read(a.path(&hash_a)).unwrap(), fs::read(b.path(&hash_b)).unwrap());
    }

    /// Different plaintexts must get different nonces, or the keystream repeats.
    #[test]
    fn different_content_gets_different_nonce() {
        assert_ne!(nonce_for(&content_hash(b"a")), nonce_for(&content_hash(b"b")));
    }

    #[test]
    fn missing_blob_is_an_error_not_a_panic() {
        let dir = tmp_dir("missing");
        let s = store(&dir);
        assert!(!s.has("00ff"));
        assert!(s.get("00ff").is_err());
    }
}
