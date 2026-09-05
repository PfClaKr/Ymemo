//! Device-local preferences and the "stay unlocked" session.
//!
//! Neither is **ever synced**: both live in the app data directory, not the vault. Language
//! and lock timeouts belong to a device, and the session key must never leave one.
//!
//! ```text
//! <data_dir>/settings.json   <- preferences
//! <data_dir>/session.json    <- cached vault key (0600 on unix)
//! ```

use ymemo_core::diag;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use ymemo_core::crypto::KEY_LEN;
use ymemo_i18n::Lang;

const SETTINGS_FILE: &str = "settings.json";
const SESSION_FILE: &str = "session.json";

/// Bounds for the merge interval in seconds: too short hammers the disk, too long feels dead.
pub const MERGE_SECONDS_RANGE: (i32, i32) = (3, 3600);
/// Maximum stay-unlocked window in days; 0 asks every time.
pub const UNLOCK_DAYS_MAX: i32 = 365;
/// Maximum idle auto-lock in minutes; 0 disables it.
pub const IDLE_MINUTES_MAX: i32 = 1440;
/// Bounds for Syncthing's watch delay in seconds. One second is as eager as the daemon will
/// usefully go — below that it starts shipping half-written files and undoing itself — and a
/// minute is already slower than anyone would sit through.
pub const WATCH_DELAY_RANGE: (i32, i32) = (1, 60);
/// Bounds for the fallback rescan in seconds. Syncthing's own floor is a minute; an hour is
/// its default, and there is no point offering less often than the thing it is a fallback for.
pub const RESCAN_SECONDS_RANGE: (i32, i32) = (60, 3600);
/// Days of Syncthing's own file copies to keep; 0 keeps none. A year is already far past the
/// point where the copies cost more disk than the vault they are protecting.
pub const KEEP_VERSIONS_DAYS_MAX: i32 = 365;
/// Gap between automatic update checks. Daily is often enough for a release cadence measured
/// in weeks, and it keeps the request rare enough to be unremarkable.
const UPDATE_CHECK_INTERVAL_MS: i64 = 24 * 60 * 60 * 1000;

/// Device-local preferences; every field defaults, so older files still load.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// UI language: `"auto"` (system locale), `"ko"` or `"en"`.
    pub lang: String,
    /// Days the vault opens without a password after one unlock; 0 asks every time.
    pub unlock_days: i32,
    /// Minutes of inactivity before locking automatically; 0 disables it.
    pub idle_lock_minutes: i32,
    /// Palette key for new memos.
    pub default_color: String,
    /// Opacity of new memos, in percent.
    pub default_opacity: i32,
    /// How often other devices' changes are pulled in, in seconds.
    pub merge_seconds: i32,
    /// How long Syncthing waits after a write before it ships the file, in seconds.
    ///
    /// The other half of how quickly a memo appears elsewhere: what a user notices is this
    /// **plus** [`Settings::merge_seconds`] on the receiving device. Lower is faster and
    /// costs more wakeups; see `ymemo_core::sync::Syncthing::set_folder_timing`.
    pub watch_delay_seconds: i32,
    /// How often Syncthing sweeps the vault for changes its watcher missed, in seconds.
    pub rescan_seconds: i32,
    /// Days of Syncthing's replaced-file copies to keep in `.stversions`; 0 keeps none.
    ///
    /// The safety net under a truncated log, **not** the memo history — that is read from the
    /// logs themselves. It costs real disk: the archive is a series of snapshots of a file
    /// that only grows, which is why it is a setting.
    pub keep_versions_days: i32,
    /// Whether to ask GitHub about newer releases. The app's only outbound request, so it is
    /// the user's to refuse; see `ymemo_core::update`.
    pub update_check: bool,
    /// When that last happened (epoch millis), so a restart does not mean another request.
    pub last_update_check: i64,
    /// Ids of the memos whose sticky window stays above other windows.
    ///
    /// The exceptions, because a note is **not** on top unless it was asked to be. A window
    /// that sits over whatever is being read is only wanted for the note being kept in view
    /// on purpose, and every note behaving that way is a wall over the work. Device-local on
    /// purpose — which window covers which is a property of a desktop, not of a memo, and
    /// syncing it would also put one log entry per pin toggle in front of every other device.
    pub pinned_memos: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            lang: "auto".into(),
            unlock_days: 30,
            idle_lock_minutes: 0,
            default_color: "yellow".into(),
            default_opacity: 100,
            merge_seconds: 15,
            // Syncthing's own defaults, so an existing install behaves exactly as it did
            // before these became settings.
            watch_delay_seconds: 10,
            rescan_seconds: 60,
            keep_versions_days: 30,
            update_check: true,
            last_update_check: 0,
            pinned_memos: Vec::new(),
        }
    }
}

impl Settings {
    /// Loads the settings, falling back to defaults; a bad file must never block startup.
    pub fn load(dir: &Path) -> Self {
        match fs::read(dir.join(SETTINGS_FILE)) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                diag!("could not read the settings, using defaults: {e}");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, dir: &Path) {
        match serde_json::to_vec_pretty(self) {
            Ok(bytes) => {
                if let Err(e) = fs::write(dir.join(SETTINGS_FILE), bytes) {
                    diag!("could not save the settings: {e}");
                }
            }
            Err(e) => diag!("could not serialize the settings: {e}"),
        }
    }

    /// Clamps user input into usable ranges, whatever the UI sends.
    pub fn sanitize(&mut self) {
        // Either "auto" or a language the catalog knows.
        if self.lang != "auto" && Lang::parse(&self.lang).is_none() {
            self.lang = "auto".into();
        }
        self.unlock_days = self.unlock_days.clamp(0, UNLOCK_DAYS_MAX);
        self.idle_lock_minutes = self.idle_lock_minutes.clamp(0, IDLE_MINUTES_MAX);
        if !matches!(
            self.default_color.as_str(),
            "yellow" | "pink" | "green" | "blue" | "purple"
        ) {
            self.default_color = "yellow".into();
        }
        self.default_opacity = ymemo_core::clamp_opacity(self.default_opacity as i64) as i32;
        self.merge_seconds = self
            .merge_seconds
            .clamp(MERGE_SECONDS_RANGE.0, MERGE_SECONDS_RANGE.1);
        self.watch_delay_seconds = self
            .watch_delay_seconds
            .clamp(WATCH_DELAY_RANGE.0, WATCH_DELAY_RANGE.1);
        self.rescan_seconds = self
            .rescan_seconds
            .clamp(RESCAN_SECONDS_RANGE.0, RESCAN_SECONDS_RANGE.1);
        self.keep_versions_days = self.keep_versions_days.clamp(0, KEEP_VERSIONS_DAYS_MAX);
        // A clock that went backwards, or a hand-edited file, must not park the next check
        // in the future forever.
        if self.last_update_check < 0 || self.last_update_check > now_millis() {
            self.last_update_check = 0;
        }
        // A hand-edited file, or a memo pinned on two runs before the first save landed.
        self.pinned_memos.sort();
        self.pinned_memos.dedup();
    }

    /// Whether this memo's sticky stays above other windows.
    pub fn memo_pinned(&self, id: &str) -> bool {
        self.pinned_memos.iter().any(|m| m == id)
    }

    /// Records the pin state of one memo. Returns whether anything changed.
    pub fn set_memo_pinned(&mut self, id: &str, pinned: bool) -> bool {
        if pinned == self.memo_pinned(id) {
            return false;
        }
        if pinned {
            self.pinned_memos.push(id.to_string());
        } else {
            self.pinned_memos.retain(|m| m != id);
        }
        true
    }

    /// Whether an update check is due: enabled, and not already done today.
    pub fn update_check_due(&self) -> bool {
        self.update_check && now_millis() - self.last_update_check >= UPDATE_CHECK_INTERVAL_MS
    }

    /// Resolves `"auto"` against the system locale.
    pub fn effective_lang(&self) -> Lang {
        Lang::parse(&self.lang).unwrap_or_else(ymemo_i18n::system_lang)
    }
}

// ---------------------------------------------------------------------------
// Stay-unlocked session
// ---------------------------------------------------------------------------

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

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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
    let path = session_path(dir);
    if let Err(e) = fs::write(&path, bytes) {
        diag!("could not save the session: {e}");
        return;
    }
    restrict_permissions(&path);
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

/// Owner-only permissions. Meaningful on unix; Windows relies on the user profile's ACL.
#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
        diag!("could not set the session file permissions: {e}");
    }
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

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
        let dir = std::env::temp_dir().join(format!("ymemo-settings-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn settings_roundtrip_and_defaults() {
        let dir = temp_dir();
        let s = Settings {
            lang: "en".into(),
            unlock_days: 7,
            ..Settings::default()
        };
        s.save(&dir);
        assert_eq!(Settings::load(&dir), s);
        fs::remove_file(dir.join(SETTINGS_FILE)).ok();
        // No file means defaults.
        assert_eq!(Settings::load(&dir), Settings::default());
    }

    #[test]
    fn sanitize_clamps_everything() {
        let mut s = Settings {
            lang: "klingon".into(),
            unlock_days: 99_999,
            idle_lock_minutes: -5,
            default_color: "chartreuse".into(),
            default_opacity: 1,
            merge_seconds: 0,
            // Both out of range on the low side, where clamping matters most: a zero-second
            // watch delay would have Syncthing shipping half-written logs.
            watch_delay_seconds: 0,
            rescan_seconds: 1,
            keep_versions_days: 9_999,
            update_check: true,
            // A timestamp from the future, as a clock that went backwards would leave.
            last_update_check: i64::MAX,
            // The same memo twice, as two runs racing to save the same pin would leave.
            pinned_memos: vec!["b".into(), "a".into(), "b".into()],
        };
        s.sanitize();
        assert_eq!(s.pinned_memos, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(s.lang, "auto");
        assert_eq!(s.unlock_days, UNLOCK_DAYS_MAX);
        assert_eq!(s.idle_lock_minutes, 0);
        assert_eq!(s.default_color, "yellow");
        // A future timestamp is dropped, so the next check is not postponed forever.
        assert_eq!(s.last_update_check, 0);
        assert!(s.update_check_due());
        assert!(s.default_opacity >= 20); // raised to the core's MIN_OPACITY
        assert_eq!(s.merge_seconds, MERGE_SECONDS_RANGE.0);
        assert_eq!(s.watch_delay_seconds, WATCH_DELAY_RANGE.0);
        assert_eq!(s.rescan_seconds, RESCAN_SECONDS_RANGE.0);
        assert_eq!(s.keep_versions_days, KEEP_VERSIONS_DAYS_MAX);
    }

    // A note sits with the ordinary windows until someone pins it, and only the one that
    // was pinned is: pinning is per memo, not a mode the whole desk is put into.
    #[test]
    fn memos_stay_out_of_the_way_until_they_are_pinned() {
        let mut s = Settings::default();
        assert!(!s.memo_pinned("m1"));

        assert!(s.set_memo_pinned("m1", true));
        assert!(s.memo_pinned("m1"));
        assert!(!s.memo_pinned("m2"));
        // Already pinned: nothing to write, so nothing to save.
        assert!(!s.set_memo_pinned("m1", true));

        assert!(s.set_memo_pinned("m1", false));
        assert!(!s.memo_pinned("m1"));
        assert!(s.pinned_memos.is_empty());
        assert!(!s.set_memo_pinned("m1", false));
    }

    #[test]
    fn explicit_lang_wins_over_system_locale() {
        let mut s = Settings {
            lang: "en".into(),
            ..Settings::default()
        };
        assert_eq!(s.effective_lang(), Lang::En);
        s.lang = "ko".into();
        assert_eq!(s.effective_lang(), Lang::Ko);
        // "auto" follows the system locale, so only "it does not panic" is assertable.
        s.lang = "auto".into();
        let _ = s.effective_lang();
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
