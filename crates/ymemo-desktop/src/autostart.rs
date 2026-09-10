//! Starting with the session, on the two desktops that have somewhere to say so.
//!
//! Both mechanisms are **per user and need no privileges**: a file in the XDG autostart
//! directory on Linux, a value under `HKCU\...\Run` on Windows. Neither touches anything
//! system-wide, so turning this on never asks for a password and turning it off leaves
//! nothing behind.
//!
//! The command written down carries `--hidden`, which is what keeps a machine that has just
//! booted from being handed a password prompt on top of whatever the user actually opened. A
//! hidden start is only honoured where a tray icon registered — see `main`, where an app with
//! no way back to a window shows one regardless.
//!
//! Nothing is remembered in `settings.json`: the desktop **is** the record. A file somebody
//! deleted by hand, or a Run value some other tool cleared, has to read back as off, and a
//! second copy of the answer in our own settings would only be a second thing to disagree.

use anyhow::Result;

/// The flag the autostarted copy is launched with.
pub(crate) const HIDDEN_FLAG: &str = "--hidden";

/// Whether this launch was started by the session rather than by the user.
pub(crate) fn launched_hidden() -> bool {
    std::env::args().skip(1).any(|a| a == HIDDEN_FLAG)
}

/// Whether the app is set to start with the session.
pub(crate) fn enabled() -> bool {
    imp::enabled()
}

/// Turns starting with the session on or off.
pub(crate) fn set(on: bool) -> Result<()> {
    imp::set(on)
}

/// Whether this build has anywhere to put the setting at all; the UI hides the row otherwise.
pub(crate) fn supported() -> bool {
    cfg!(any(target_os = "linux", target_os = "windows"))
}

// ---------------------------------------------------------------------------
// Linux: an XDG autostart entry, which every desktop that has a session reads.
// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
mod imp {
    use super::HIDDEN_FLAG;
    use anyhow::{Context, Result};
    use std::path::PathBuf;

    /// `~/.config/autostart/ymemo.desktop`, per the XDG Desktop Application Autostart spec.
    fn entry() -> Option<PathBuf> {
        directories::BaseDirs::new()
            .map(|d| d.config_dir().join("autostart").join("ymemo.desktop"))
    }

    pub(super) fn enabled() -> bool {
        entry().is_some_and(|p| p.exists())
    }

    pub(super) fn set(on: bool) -> Result<()> {
        let path = entry().context("no config directory to write an autostart entry into")?;
        if !on {
            if path.exists() {
                std::fs::remove_file(&path)?;
            }
            return Ok(());
        }
        let exe = std::env::current_exe()?;
        std::fs::create_dir_all(path.parent().expect("the entry has a directory"))?;
        // `Name` is not translated here: the file is read by the session, and the app's own
        // .desktop in the packages is where the translated one lives.
        std::fs::write(
            &path,
            format!(
                "[Desktop Entry]\n\
                 Type=Application\n\
                 Name=Ymemo\n\
                 Exec=\"{}\" {HIDDEN_FLAG}\n\
                 Icon=ymemo\n\
                 Terminal=false\n\
                 X-GNOME-Autostart-enabled=true\n",
                exe.display()
            ),
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Windows: a value under the per-user Run key.
// ---------------------------------------------------------------------------
#[cfg(target_os = "windows")]
mod imp {
    use super::HIDDEN_FLAG;
    use anyhow::{bail, Result};
    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ,
    };

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    /// The value's name, which is what the Task Manager's Startup tab shows.
    const VALUE: &str = "Ymemo";

    /// A NUL-terminated UTF-16 buffer, which is what every `*W` entry point wants.
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Opens the Run key with the given access. The key always exists on a live Windows.
    fn open(access: u32) -> Result<HKEY> {
        let mut key: HKEY = std::ptr::null_mut::<std::ffi::c_void>() as HKEY;
        // SAFETY: `wide` is NUL-terminated and outlives the call; `key` is written only on
        // success, which is the one path that goes on to use it.
        let rc = unsafe {
            RegOpenKeyExW(HKEY_CURRENT_USER, wide(RUN_KEY).as_ptr(), 0, access, &mut key)
        };
        if rc != ERROR_SUCCESS {
            bail!("could not open the Run key: error {rc}");
        }
        Ok(key)
    }

    pub(super) fn enabled() -> bool {
        let Ok(key) = open(KEY_READ) else { return false };
        let mut size: u32 = 0;
        // SAFETY: asking for the size only — every out pointer but `size` is null, which is
        // what the API documents for that question.
        let rc = unsafe {
            RegQueryValueExW(
                key,
                wide(VALUE).as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        };
        // SAFETY: `key` came from a successful open and is not used again.
        unsafe { RegCloseKey(key) };
        rc == ERROR_SUCCESS
    }

    pub(super) fn set(on: bool) -> Result<()> {
        let key = open(KEY_WRITE)?;
        let result = if on {
            let exe = std::env::current_exe()?;
            // Quoted, because a path with a space in it is otherwise read as a command and
            // its arguments — and the default install path has one.
            let command = wide(&format!("\"{}\" {HIDDEN_FLAG}", exe.display()));
            let bytes = std::mem::size_of_val(&command[..]) as u32;
            // SAFETY: `command` is NUL-terminated UTF-16 and outlives the call, and `bytes`
            // is its own length including that terminator, which is what `REG_SZ` wants.
            let rc = unsafe {
                RegSetValueExW(
                    key,
                    wide(VALUE).as_ptr(),
                    0,
                    REG_SZ,
                    command.as_ptr().cast::<u8>(),
                    bytes,
                )
            };
            if rc == ERROR_SUCCESS {
                Ok(())
            } else {
                bail_rc("could not write the Run value", rc)
            }
        } else {
            // SAFETY: same contract as above; a value that is not there is not an error worth
            // reporting, which is why `FILE_NOT_FOUND` is let through below.
            let rc = unsafe { RegDeleteValueW(key, wide(VALUE).as_ptr()) };
            const ERROR_FILE_NOT_FOUND: u32 = 2;
            if rc == ERROR_SUCCESS || rc == ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                bail_rc("could not remove the Run value", rc)
            }
        };
        // SAFETY: `key` came from a successful open and is not used after this.
        unsafe { RegCloseKey(key) };
        result
    }

    fn bail_rc(what: &str, rc: u32) -> Result<()> {
        bail!("{what}: error {rc}")
    }

    // Only to keep the unused-import warning honest on a build that never opens a HANDLE.
    const _: Option<HANDLE> = None;
}

#[cfg(test)]
mod tests {
    //! The Linux half is a file and needs no test to believe. The Windows half is raw
    //! registry FFI that nothing else in this workspace exercises, so it gets one — run it
    //! with `cargo test -p ymemo-desktop --target x86_64-pc-windows-gnullvm`, which WSL will
    //! execute on Windows for you.

    /// Turning it on is visible to `enabled`, turning it off is not, and whatever the machine
    /// had before is put back.
    #[test]
    #[cfg(target_os = "windows")]
    fn the_run_value_goes_in_and_comes_out() {
        let before = super::enabled();

        super::set(true).expect("writing the Run value");
        assert!(super::enabled(), "it should be there after being written");
        // Writing twice is what pressing Save twice does, and must not fail.
        super::set(true).expect("writing it again");
        assert!(super::enabled());

        super::set(false).expect("removing the Run value");
        assert!(!super::enabled(), "it should be gone after being removed");
        // Removing one that is not there is not an error.
        super::set(false).expect("removing it again");

        if before {
            super::set(true).expect("putting back what the machine had");
        }
    }
}

// ---------------------------------------------------------------------------
// Everywhere else: nowhere to write it, so it is off and cannot be turned on.
// ---------------------------------------------------------------------------
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod imp {
    use anyhow::Result;

    pub(super) fn enabled() -> bool {
        false
    }

    pub(super) fn set(_on: bool) -> Result<()> {
        Ok(())
    }
}
