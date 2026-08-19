//! Per-device append-only log of encrypted records.
//!
//! Core of the data model: a device only ever appends to its own log and never rewrites
//! it, so syncing produces no file conflicts. Every record is encrypted separately with
//! XChaCha20-Poly1305.
//!
//! Record payloads are opaque here — today they are automerge change binaries (actor,
//! seq, timestamp and dependencies live inside the change). The vault interprets them.
//!
//! On-disk format, repeated per record:
//! ```text
//! [u32 LE record length][nonce(24B) || ciphertext+tag] ...
//! ```

use anyhow::{anyhow, Result};
use ymemo_i18n::t;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use crate::crypto::MasterKey;

/// An encrypted append-only record log file. Owns the key and en/decrypts on the fly.
pub struct ChangeLog {
    path: PathBuf,
    key: MasterKey,
}

impl ChangeLog {
    /// Opens a log; the file is created on the first append.
    pub fn open(path: impl AsRef<Path>, key: MasterKey) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            key,
        }
    }

    /// Encrypts one record and appends it.
    pub fn append(&self, plaintext: &[u8]) -> Result<()> {
        let record = self.key.encrypt(plaintext)?;
        let len = u32::try_from(record.len())
            .map_err(|_| anyhow!(t!("core.record_too_large", len = record.len())))?;

        let mut f = OpenOptions::new().create(true).append(true).open(&self.path)?;
        f.write_all(&len.to_le_bytes())?;
        f.write_all(&record)?;
        Ok(())
    }

    /// Reads and decrypts every record in order. Missing file yields an empty vec.
    pub fn read_all(&self) -> Result<Vec<Vec<u8>>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let mut reader = BufReader::new(File::open(&self.path)?);
        let mut records = Vec::new();

        loop {
            let mut len_buf = [0u8; 4];
            match reader.read_exact(&mut len_buf) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
            let len = u32::from_le_bytes(len_buf) as usize;

            let mut record = vec![0u8; len];
            reader.read_exact(&mut record)?;
            records.push(self.key.decrypt(&record)?);
        }
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{generate_salt, MasterKey};

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("ymemo-log-{}.bin", uuid::Uuid::new_v4()))
    }

    #[test]
    fn append_read_roundtrip() {
        let path = temp_path();
        let salt = generate_salt();
        let log = ChangeLog::open(&path, MasterKey::derive(b"pw", &salt).unwrap());

        log.append(b"record-1").unwrap();
        log.append("두 번째 🦀".as_bytes()).unwrap();

        // The file must not contain plaintext.
        let raw = std::fs::read(&path).unwrap();
        assert!(!raw.windows(8).any(|w| w == b"record-1"));

        // A separate instance with the same key restores the order.
        let log2 = ChangeLog::open(&path, MasterKey::derive(b"pw", &salt).unwrap());
        let records = log2.read_all().unwrap();
        assert_eq!(records, vec![b"record-1".to_vec(), "두 번째 🦀".as_bytes().to_vec()]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn wrong_key_fails() {
        let path = temp_path();
        let salt = generate_salt();
        ChangeLog::open(&path, MasterKey::derive(b"pw", &salt).unwrap())
            .append(b"secret")
            .unwrap();
        let wrong = ChangeLog::open(&path, MasterKey::derive(b"nope", &salt).unwrap());
        assert!(wrong.read_all().is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn missing_file_is_empty() {
        let salt = generate_salt();
        let log = ChangeLog::open(temp_path(), MasterKey::derive(b"pw", &salt).unwrap());
        assert!(log.read_all().unwrap().is_empty());
    }
}
