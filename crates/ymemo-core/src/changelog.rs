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
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
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

        // One buffer, one write: a length landing without its record is exactly the torn
        // tail that [`ChangeLog::repair_tail`] exists to cut off.
        let mut framed = Vec::with_capacity(4 + record.len());
        framed.extend_from_slice(&len.to_le_bytes());
        framed.extend_from_slice(&record);

        let mut f = OpenOptions::new().create(true).append(true).open(&self.path)?;
        let before = f.metadata()?.len();
        if let Err(e) = f.write_all(&framed) {
            // A disk that filled up halfway must not leave half a record behind, or every
            // later append would land after it and be unreadable too.
            let _ = f.set_len(before);
            return Err(e.into());
        }
        Ok(())
    }

    /// Cuts off a record the log ends in the middle of, left by a crash or a power cut during
    /// [`ChangeLog::append`]. Returns whether anything was cut.
    ///
    /// **Only ever call this on our own log.** Another device's log is theirs to write, and
    /// a torn tail there is usually Syncthing mid-delivery; [`ChangeLog::read_all`] already
    /// reads past it.
    pub fn repair_tail(&self) -> Result<bool> {
        let mut f = match OpenOptions::new().read(true).write(true).open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        let total = f.metadata()?.len();
        let good = complete_len(&mut f, total)?;
        if good == total {
            return Ok(false);
        }
        f.set_len(good)?;
        crate::diag!(
            "cut a torn record off our log: {} of {total} bytes kept",
            good
        );
        Ok(true)
    }

    /// Reads and decrypts every record in order. Missing file yields an empty vec.
    pub fn read_all(&self) -> Result<Vec<Vec<u8>>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(&self.path)?;
        let mut left = file.metadata()?.len();
        let mut reader = BufReader::new(file);
        let mut records = Vec::new();

        // A torn tail — a record the file ends in the middle of — ends the read instead of
        // failing it. Failing it threw the whole log away, every memo that device ever wrote,
        // over the last few bytes of one keystroke.
        while left >= 4 {
            let mut len_buf = [0u8; 4];
            reader.read_exact(&mut len_buf)?;
            left -= 4;
            let len = u64::from(u32::from_le_bytes(len_buf));
            // Checked against what is actually there before anything is allocated: a garbled
            // length would otherwise ask for up to 4 GB and abort the process.
            if len > left {
                break;
            }
            let mut record = vec![0u8; len as usize];
            reader.read_exact(&mut record)?;
            left -= len;
            records.push(self.key.decrypt(&record)?);
        }
        Ok(records)
    }
}

/// How many leading bytes of a `total`-byte log are whole records, walking the length
/// prefixes without decrypting anything.
fn complete_len(f: &mut File, total: u64) -> Result<u64> {
    let mut pos = 0u64;
    loop {
        if total - pos < 4 {
            return Ok(pos);
        }
        f.seek(SeekFrom::Start(pos))?;
        let mut len_buf = [0u8; 4];
        f.read_exact(&mut len_buf)?;
        let len = u64::from(u32::from_le_bytes(len_buf));
        if len > total - pos - 4 {
            return Ok(pos);
        }
        pos += 4 + len;
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

    /// Every way an append can be cut short: inside the length, and inside the record.
    #[test]
    fn a_torn_tail_keeps_the_records_before_it() {
        let salt = generate_salt();
        let key = MasterKey::derive(b"pw", &salt).unwrap();
        let path = temp_path();
        let log = ChangeLog::open(&path, key.clone());
        log.append(b"first").unwrap();
        log.append(b"second").unwrap();
        let whole = std::fs::read(&path).unwrap();
        log.append(b"third").unwrap();
        let three = std::fs::read(&path).unwrap();

        for cut in whole.len() + 1..three.len() {
            std::fs::write(&path, &three[..cut]).unwrap();
            let records = ChangeLog::open(&path, key.clone()).read_all().unwrap();
            assert_eq!(records, vec![b"first".to_vec(), b"second".to_vec()], "cut at {cut}");

            assert!(log.repair_tail().unwrap());
            assert_eq!(std::fs::read(&path).unwrap(), whole);
            // And what is written next is readable, not stranded behind the torn bytes.
            log.append(b"after").unwrap();
            assert_eq!(log.read_all().unwrap().last().unwrap(), b"after");
        }
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_garbled_length_does_not_allocate_it() {
        let salt = generate_salt();
        let key = MasterKey::derive(b"pw", &salt).unwrap();
        let path = temp_path();
        let log = ChangeLog::open(&path, key);
        log.append(b"kept").unwrap();
        let mut raw = std::fs::read(&path).unwrap();
        raw.extend_from_slice(&u32::MAX.to_le_bytes());
        raw.extend_from_slice(b"junk");
        std::fs::write(&path, &raw).unwrap();

        assert_eq!(log.read_all().unwrap(), vec![b"kept".to_vec()]);
        assert!(log.repair_tail().unwrap());
        assert!(!log.repair_tail().unwrap());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn repairing_a_missing_log_is_nothing() {
        let salt = generate_salt();
        let log = ChangeLog::open(temp_path(), MasterKey::derive(b"pw", &salt).unwrap());
        assert!(!log.repair_tail().unwrap());
    }

    #[test]
    fn missing_file_is_empty() {
        let salt = generate_salt();
        let log = ChangeLog::open(temp_path(), MasterKey::derive(b"pw", &salt).unwrap());
        assert!(log.read_all().unwrap().is_empty());
    }
}
