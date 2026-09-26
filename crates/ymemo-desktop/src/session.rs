//! The "stay unlocked" session: the vault key cached in `<data_dir>/session.json`, with an
//! expiry. Never synced — the key must never leave the device it was typed on.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ymemo_core::crypto::KEY_LEN;
use ymemo_core::fsutil::write_atomic_private;
use ymemo_core::{diag, now_millis};

const SESSION_FILE: &str = "session.json";

/// The vault key cached on disk, with its expiry.
///
/// **While this file exists the memos are readable without the master password.** That is
/// what "stay unlocked" means, and its price: at-rest encryption is suspended for that
/// window. Hence 0600 on unix, and deletion on a manual lock, a settings change or expiry.
#[derive(Serialize, Deserialize)]
struct Session {
    /// The 32-byte vault key, hex encoded.
    key: String,
    /// Expiry, in unix epoch millis.
    expires_at: i64,
}

fn session_path(dir: &Path) -> PathBuf {
    dir.join(SESSION_FILE)
}

/// Loads a still-valid session key; an expired or broken file is deleted and `None` returned.
pub fn load_session(dir: &Path) -> Option<[u8; KEY_LEN]> {
    let bytes = fs::read(session_path(dir)).ok()?;
    let session: Session = match serde_json::from_slice(&bytes) {
        Ok(s) => s,
        Err(_) => {
            clear_session(dir);
            return None;
        }
    };
    if now_millis() >= session.expires_at {
        clear_session(dir);
        return None;
    }
    match from_hex(&session.key) {
        Some(key) => Some(key),
        None => {
            clear_session(dir);
            None
        }
    }
}

/// Called right after unlocking; `days` of 0 stores nothing, so the password is always asked.
pub fn save_session(dir: &Path, key: &[u8; KEY_LEN], days: i32) {
    if days <= 0 {
        clear_session(dir);
        return;
    }
    let session = Session {
        key: to_hex(key),
        expires_at: now_millis() + days as i64 * 24 * 60 * 60 * 1000,
    };
    let Ok(bytes) = serde_json::to_vec(&session) else {
        return;
    };
    // Owner-only from the moment it exists on unix; Windows relies on the profile's ACL.
    if let Err(e) = write_atomic_private(&session_path(dir), &bytes) {
        diag!("could not save the session: {e}");
    }
}

/// Discards the session (manual lock, changed window, expiry).
pub fn clear_session(dir: &Path) {
    let path = session_path(dir);
    if path.exists() {
        if let Err(e) = fs::remove_file(&path) {
            diag!("could not delete the session: {e}");
        }
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Option<[u8; KEY_LEN]> {
    if s.len() != KEY_LEN * 2 {
        return None;
    }
    let mut out = [0u8; KEY_LEN];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ymemo-session-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn session_survives_until_expiry_then_vanishes() {
        let dir = temp_dir();
        let key = [3u8; KEY_LEN];

        save_session(&dir, &key, 30);
        assert_eq!(load_session(&dir), Some(key));

        // Zero days stores nothing.
        save_session(&dir, &key, 0);
        assert_eq!(load_session(&dir), None);

        // A past expiry takes the whole file with it on read.
        save_session(&dir, &key, 30);
        let past = Session {
            key: to_hex(&key),
            expires_at: now_millis() - 1,
        };
        fs::write(session_path(&dir), serde_json::to_vec(&past).unwrap()).unwrap();
        assert_eq!(load_session(&dir), None);
        assert!(!session_path(&dir).exists());
    }
}
