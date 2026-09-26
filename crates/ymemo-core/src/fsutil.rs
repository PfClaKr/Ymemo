//! Writing a small file so that a crash leaves either the old contents or the new ones.
//!
//! `fs::write` truncates first and writes second, so a crash or a power cut in between leaves
//! an empty or half-written file behind. For the files read back at startup that meant the
//! whole of it silently fell back to defaults — the desk, the pinned notes, the timings.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Replaces `path` with `bytes` through a sibling temporary file and a rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_via_tmp(path, bytes, false)
}

/// [`write_atomic`] for a file only its owner may read (mode 0600 on Unix). The mode is set
/// when the temporary file is **created**, so the contents are never readable by anyone
/// else, not even for the moment between writing and restricting.
pub fn write_atomic_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_via_tmp(path, bytes, true)
}

fn write_via_tmp(path: &Path, bytes: &[u8], private: bool) -> io::Result<()> {
    let tmp = tmp_path(path);
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let result = (|| {
        let mut f = options.open(&tmp)?;
        f.write_all(bytes)?;
        // The rename must not reach the disk before the data it points at does.
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// `name.tmp` beside `name`. Appended rather than `with_extension`, which would eat one.
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".tmp");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_file_and_leaves_nothing_beside_it() {
        let dir = std::env::temp_dir().join(format!("ymemo-fsutil-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_private_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ymemo-fsutil-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.json");
        write_atomic_private(&path, b"key").unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        fs::remove_dir_all(&dir).ok();
    }
}
