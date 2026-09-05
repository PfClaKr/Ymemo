//! The one place a user can point at when something goes wrong.
//!
//! Everything here used to be an `eprintln!`, which is fine while a developer is watching a
//! terminal and useless everywhere else. A release build on Windows has
//! `windows_subsystem = "windows"` and therefore no console at all, and on Android the NDK
//! sends a process's stderr to `/dev/null`. So on **both platforms a user runs**, every
//! message explaining why syncing did not start, or why a log would not decrypt, went
//! nowhere — and a bug report could carry nothing but a sentence.
//!
//! So the same messages also go to a file next to the vault:
//!
//! ```text
//! <data_dir>/ymemo.log     the current one
//! <data_dir>/ymemo.log.1   the one before it
//! ```
//!
//! Two files of [`MAX_BYTES`] each, so the thing meant to help diagnose a full disk can never
//! cause one. Nothing here returns an error or panics: a diagnostic that can break the app it
//! is diagnosing is worse than no diagnostic.
//!
//! **It holds no memo text.** The call sites write what failed and why — paths, error
//! strings, states — never a title or a body, so the file can be attached to a bug report
//! without reading it first. Keep it that way when adding one.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Rotation threshold. Small on purpose: what a bug report needs is the last few minutes, and
/// the file has to stay pasteable.
const MAX_BYTES: u64 = 256 * 1024;

/// Where to write, once the app knows its data directory. Unset in tests and in any library
/// use, where writing files nobody asked for would be the wrong default.
static PATH: OnceLock<PathBuf> = OnceLock::new();
/// Serializes writers within the process; two devices never share a file.
static LOCK: Mutex<()> = Mutex::new(());

/// Points the log at a directory. Safe to call more than once; the first call wins.
pub fn init(data_dir: &Path) {
    let _ = PATH.set(data_dir.join("ymemo.log"));
}

/// The current log's path, once [`init`] has run.
pub fn path() -> Option<&'static Path> {
    PATH.get().map(|p| p.as_path())
}

/// Appends one line. Silently does nothing before [`init`], and on any I/O failure.
pub fn write(message: &str) {
    let Some(path) = PATH.get() else { return };
    let Ok(_guard) = LOCK.lock() else { return };
    append(path, MAX_BYTES, message);
}

/// The write itself, with the path and the cap passed in so the rotation is testable without
/// the process-wide [`PATH`], which a test can only ever set once.
fn append(path: &Path, max_bytes: u64, message: &str) {
    // Rotate first, so a single oversized line cannot push the file past twice the cap.
    if fs::metadata(path).map(|m| m.len() >= max_bytes).unwrap_or(false) {
        let _ = fs::rename(path, rotated(path));
    }
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{stamp}  {message}");
    }
}

/// The previous log's path. `with_extension` would eat an existing one, so this appends.
fn rotated(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".1");
    PathBuf::from(name)
}

/// The tail of the log, newest content last, capped at `max_bytes`.
///
/// For the phone, which has no file manager worth sending someone to: the settings screen
/// shows this and offers to copy it. Reads the rotated file too, so a report is not empty
/// just because the log turned over a moment ago.
pub fn tail(max_bytes: usize) -> String {
    match PATH.get() {
        Some(path) => tail_at(path, max_bytes),
        None => String::new(),
    }
}

/// [`tail`] against an explicit path, for the same reason [`append`] takes one.
fn tail_at(path: &Path, max_bytes: usize) -> String {
    let mut text = fs::read_to_string(rotated(path)).unwrap_or_default();
    text.push_str(&fs::read_to_string(path).unwrap_or_default());
    if text.len() > max_bytes {
        // Start at a line boundary, so the first line is not half a message.
        let cut = text.len() - max_bytes;
        let start = text[cut..].find('\n').map(|i| cut + i + 1).unwrap_or(cut);
        text = text[start..].to_string();
    }
    text
}

/// Logs to stderr **and** to the file, with `println!`-style formatting.
///
/// Every `eprintln!` that reports a failure should be this instead; stderr alone is invisible
/// on the two platforms users actually run.
#[macro_export]
macro_rules! diag {
    ($($arg:tt)*) => {{
        let message = format!($($arg)*);
        eprintln!("{message}");
        $crate::diag::write(&message);
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of our own, since these tests write real files.
    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ymemo-diag-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn uninitialised_reads_and_writes_do_nothing() {
        // The library must not create files in a process that never asked for a log; the
        // desktop and the FFI both call `init` explicitly, and nothing else may.
        assert_eq!(tail(1024), "");
        write("this goes nowhere");
    }

    #[test]
    fn rotating_keeps_the_previous_file() {
        let dir = scratch();
        let log = dir.join("ymemo.log");
        // A cap small enough that the second line trips it.
        append(&log, 20, "first");
        assert!(!rotated(&log).exists(), "rotated too early");
        append(&log, 20, "second");

        // The old content moved aside rather than being dropped, and the new file holds only
        // what came after — which is what bounds the pair at two caps rather than growing.
        assert!(fs::read_to_string(rotated(&log)).unwrap().contains("first"));
        let current = fs::read_to_string(&log).unwrap();
        assert!(current.contains("second"));
        assert!(!current.contains("first"));

        // Reading spans both, oldest first, so a report is not empty just after a rotation.
        let all = tail_at(&log, 4096);
        assert!(all.find("first") < all.find("second"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tail_is_capped_at_whole_lines() {
        let dir = scratch();
        let log = dir.join("ymemo.log");
        for line in ["aaaa", "bbbb", "cccc"] {
            append(&log, MAX_BYTES, line);
        }
        // Every line here is the same length, so a cap of two lines' worth must drop exactly
        // the first — and must not leave half of it behind.
        let one_line = fs::read_to_string(&log).unwrap().len() / 3;
        let text = tail_at(&log, one_line * 2 + 1);
        assert!(!text.contains("aaaa"), "kept a line the cap excluded: {text:?}");
        assert!(text.contains("bbbb") && text.contains("cccc"));
        assert!(text.starts_with(char::is_numeric), "cut mid-line: {text:?}");
        let _ = fs::remove_dir_all(&dir);
    }
}
