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
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::crypto::{MasterKey, NONCE_LEN};

/// The shortest record [`ChangeLog::append`] can write: a nonce and the AEAD tag around an
/// empty plaintext. Anything shorter is not a record.
///
/// It matters because of what a power cut leaves behind. A file system that had already
/// grown the file but not yet written its data hands back **zeros** — measured on an
/// Android emulator killed mid-session — and zeros read as a record of length 0: a whole,
/// well-framed record by the length alone, which then failed to decrypt and took every memo
/// in the log down with it.
const MIN_RECORD: u64 = NONCE_LEN as u64 + 16;

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
    ///
    /// A record that will not decrypt is **skipped, not fatal**, as long as others do: one
    /// damaged record must not cost the device every memo it ever wrote. Only when none at
    /// all decrypts is it an error — that is a different key, not a damaged record, and the
    /// caller has to hear it.
    pub fn read_all(&self) -> Result<Vec<Vec<u8>>> {
        self.read(false)
    }

    /// Like [`ChangeLog::read_all`], but **any** record that will not decrypt is an error.
    ///
    /// For asking whether a key is *the* key of this log — the healing of a diverged key.
    /// There, a log that half opens under the new key must still count as not opening, or
    /// its older records would be skipped for good instead of re-encrypted.
    pub fn read_all_strict(&self) -> Result<Vec<Vec<u8>>> {
        self.read(true)
    }

    fn read(&self, strict: bool) -> Result<Vec<Vec<u8>>> {
        let data = match std::fs::read(&self.path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut records = Vec::new();
        let mut damaged = 0usize;
        let mut skipped_bytes = 0usize;
        let mut unopened = 0usize;
        let opens = |p: usize| -> Option<(usize, Vec<u8>)> {
            let len = frame_at(&data, p)?;
            self.key.decrypt(&data[p + 4..p + 4 + len]).ok().map(|plain| (len, plain))
        };

        let mut pos = 0usize;
        while pos + 4 <= data.len() {
            if let Some((len, plain)) = opens(pos) {
                records.push(plain);
                pos += 4 + len;
                continue;
            }
            // A record whose framing is sound but which will not open: damage, or another key.
            let framed = frame_at(&data, pos).is_some();
            if framed {
                if strict {
                    anyhow::bail!(t!("core.log_unreadable", path = self.path.display()));
                }
                unopened += 1;
            }
            // Not a record that opens. Usually a torn tail — a record the file ends in the
            // middle of — and then there is nothing after it. But a record can be damaged in
            // place, and a power cut can leave a run of zeros that an older version then
            // wrote on past, with real records after it. So look for the next place a record
            // really begins, and trust only one that decrypts: AEAD makes a false start
            // effectively impossible, and a garbled length is never followed blindly.
            let next = (pos + 1..=data.len() - 4).find(|&p| opens(p).is_some());
            // Strict wants every byte accounted for, except a torn tail: a record the file
            // simply ends in the middle of was never written, so it is no sign of a wrong key.
            // (A well-framed record that did not open has already failed it, above.)
            if strict && next.is_some() {
                anyhow::bail!(t!("core.log_unreadable", path = self.path.display()));
            }
            let Some(next) = next else { break };
            damaged += 1;
            skipped_bytes += next - pos;
            pos = next;
        }
        if records.is_empty() && unopened > 0 {
            // Nothing opens at all: a different key, not a damaged record.
            anyhow::bail!(t!("core.log_unreadable", path = self.path.display()));
        }
        if damaged > 0 {
            crate::diag!(
                "read past damage in {}: {skipped_bytes} byte(s) in {damaged} place(s) skipped, {} record(s) kept",
                self.path.display(),
                records.len()
            );
        }
        Ok(records)
    }
}

/// The length of the record framed at `pos`, if a plausible one is: its length prefix is all
/// there, it is at least [`MIN_RECORD`], and the record fits in what is left. Checked before
/// anything is allocated, since a garbled length can ask for up to 4 GB.
fn frame_at(data: &[u8], pos: usize) -> Option<usize> {
    let prefix: [u8; 4] = data.get(pos..pos + 4)?.try_into().ok()?;
    let len = u32::from_le_bytes(prefix) as usize;
    (len as u64 >= MIN_RECORD && len <= data.len() - pos - 4).then_some(len)
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
        // Too short to be a record: the zeros a power cut leaves. Cut only if nothing but
        // zeros follows. A device on an older version may have written real records after
        // them, and those are the user's writing — the reader steps over the zeros instead.
        if len < MIN_RECORD {
            let mut rest = Vec::new();
            f.seek(SeekFrom::Start(pos))?;
            f.read_to_end(&mut rest)?;
            return Ok(if rest.iter().all(|b| *b == 0) { pos } else { total });
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

    /// What a power cut left on a real (emulated) phone: the file grown, its new bytes zeros.
    /// The records before them are kept, the zeros are cut off our own log, and what is
    /// written next is readable.
    #[test]
    fn a_zero_filled_tail_keeps_the_records_before_it() {
        let salt = generate_salt();
        let key = MasterKey::derive(b"pw", &salt).unwrap();
        let path = temp_path();
        let log = ChangeLog::open(&path, key);
        log.append(b"first").unwrap();
        log.append(b"second").unwrap();
        let whole = std::fs::read(&path).unwrap();
        let mut raw = whole.clone();
        raw.extend_from_slice(&[0u8; 300]);
        std::fs::write(&path, &raw).unwrap();

        assert_eq!(log.read_all().unwrap(), vec![b"first".to_vec(), b"second".to_vec()]);
        assert!(log.repair_tail().unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), whole);
        log.append(b"after").unwrap();
        assert_eq!(log.read_all().unwrap().last().unwrap(), b"after");
        std::fs::remove_file(&path).ok();
    }

    /// A record damaged in the middle costs that record, not the log.
    #[test]
    fn one_damaged_record_is_skipped_not_fatal() {
        let salt = generate_salt();
        let key = MasterKey::derive(b"pw", &salt).unwrap();
        let path = temp_path();
        let log = ChangeLog::open(&path, key);
        log.append(b"first").unwrap();
        let first_len = std::fs::metadata(&path).unwrap().len() as usize;
        log.append(b"second").unwrap();
        log.append(b"third").unwrap();
        let mut raw = std::fs::read(&path).unwrap();
        raw[first_len + 4 + 30] ^= 0xFF; // inside the second record's ciphertext
        std::fs::write(&path, &raw).unwrap();

        assert_eq!(log.read_all().unwrap(), vec![b"first".to_vec(), b"third".to_vec()]);
        std::fs::remove_file(&path).ok();
    }

    /// Zeros in the **middle** — a power cut, then an older version writing on past it. The
    /// records after them are the user's writing: read, and never cut off.
    #[test]
    fn records_after_a_run_of_zeros_are_kept() {
        let salt = generate_salt();
        let key = MasterKey::derive(b"pw", &salt).unwrap();
        let path = temp_path();
        let log = ChangeLog::open(&path, key.clone());
        log.append(b"before").unwrap();
        let mut raw = std::fs::read(&path).unwrap();
        raw.extend_from_slice(&[0u8; 77]);
        std::fs::write(&path, &raw).unwrap();
        log.append(b"after one").unwrap();
        log.append(b"after two").unwrap();

        let want = vec![b"before".to_vec(), b"after one".to_vec(), b"after two".to_vec()];
        assert_eq!(log.read_all().unwrap(), want);
        assert!(!log.repair_tail().unwrap(), "nothing of the user's is cut");
        assert_eq!(log.read_all().unwrap(), want);
        assert!(log.read_all_strict().is_err(), "strict still calls it damaged");
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
