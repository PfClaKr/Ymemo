//! One app per user, plus a small control channel for the outside world.
//!
//! Two instances would be worse than useless — they would run two sync daemons over the same
//! syncthing home directory (the second fails on the locked database, so that copy silently
//! loses sync) and write the same SQLite cache from two processes.
//!
//! Two pieces, deliberately separate:
//!
//!  - [`acquire`] is the authority: a **named mutex** on Windows, an `flock` on a file in the
//!    data directory on unix. Both are released by the kernel when the process dies, however
//!    it dies, so a crash never leaves a stale lock behind.
//!  - [`serve`] listens on loopback for two one-word commands:
//!      * `SHOW` — a second launch arrived; raise the window. Without this a second launch
//!        would just vanish, and to the user a tray-resident app looks broken.
//!      * `QUIT` — someone wants us gone politely. This is what `ymemo --quit` sends, and it
//!        is how the Windows installer and uninstaller get the app (and with it the sync
//!        daemon) out of the way without terminating it and losing the last edits.
//!
//! The socket takes an **ephemeral port, written to `instance.port` in the data directory**,
//! rather than a fixed one. The data directory is per-user, so two users on one machine each
//! get their own channel instead of racing for the same port.

use std::net::{Ipv4Addr, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Raise the window (a second launch).
const MSG_SHOW: &[u8] = b"YMEMO-SHOW";
/// Save and quit. Anything else on the socket is ignored, so a stray datagram from an
/// unrelated program can do nothing.
const MSG_QUIT: &[u8] = b"YMEMO-QUIT";

/// How long [`quit_running`] waits for the app to actually go away.
const QUIT_TIMEOUT: Duration = Duration::from_secs(8);

/// Held for the life of the process; dropping it lets the next instance start. Nothing reads
/// it — it is the holding that matters.
pub(crate) struct InstanceGuard {
    _inner: imp::Guard,
}

/// Takes the single-instance lock. `None` means another instance already holds it.
pub(crate) fn acquire(dir: &Path) -> Option<InstanceGuard> {
    imp::acquire(dir).map(|_inner| InstanceGuard { _inner })
}

fn port_file(dir: &Path) -> PathBuf {
    dir.join("instance.port")
}

/// Starts the loopback command channel. Call it only from the instance holding the guard.
///
/// Bound to 127.0.0.1 only: nothing off the machine can reach it, and on Windows a loopback
/// bind raises no firewall prompt. The thread lives as long as the app; failing to bind is
/// not an error, the app just loses the second-launch and `--quit` conveniences.
pub(crate) fn serve(dir: &Path) {
    let sock = match UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("the control channel could not be opened, continuing without it: {e}");
            return;
        }
    };
    match sock.local_addr() {
        Ok(addr) => {
            if let Err(e) = std::fs::write(port_file(dir), addr.port().to_string()) {
                eprintln!("could not record the control port: {e}");
                return;
            }
        }
        Err(e) => {
            eprintln!("could not read the control port: {e}");
            return;
        }
    }
    std::thread::spawn(move || {
        let mut buf = [0u8; 32];
        loop {
            match sock.recv_from(&mut buf) {
                Ok((n, _)) if &buf[..n] == MSG_SHOW => crate::tray::request_show(),
                // request_quit saves pending edits first, then ends the event loop, which
                // shuts the sync daemon down on the way out.
                Ok((n, _)) if &buf[..n] == MSG_QUIT => crate::tray::request_quit(),
                Ok(_) => {}
                Err(e) => {
                    eprintln!("the control channel stopped: {e}");
                    return;
                }
            }
        }
    });
}

/// Sends one command to the running instance. Best effort — a missing or stale port file just
/// means nobody is listening.
fn send(dir: &Path, msg: &[u8]) {
    let Ok(text) = std::fs::read_to_string(port_file(dir)) else { return };
    let Ok(port) = text.trim().parse::<u16>() else { return };
    let Ok(sock) = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)) else { return };
    let _ = sock.send_to(msg, (Ipv4Addr::LOCALHOST, port));
}

/// Asks the instance that beat us to it to show itself.
pub(crate) fn send_show(dir: &Path) {
    send(dir, MSG_SHOW);
}

/// `ymemo --quit`: asks a running instance to save and exit, and waits until it has.
///
/// This is what the Windows installer and uninstaller run before touching the files, so the
/// app closes its windows properly and the sync daemon releases `ymemo-sync.exe` instead of
/// being terminated under it. Returns `true` once no instance is running — including the case
/// where none was.
pub(crate) fn quit_running(dir: &Path) -> bool {
    send(dir, MSG_QUIT);
    let deadline = Instant::now() + QUIT_TIMEOUT;
    loop {
        // Taking the lock is the proof the other process is gone; drop it again right away.
        if acquire(dir).is_some() {
            let _ = std::fs::remove_file(port_file(dir));
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ===========================================================================
// Windows: a named mutex, which the installer can see too
// ===========================================================================
#[cfg(windows)]
mod imp {
    use std::path::Path;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    /// Session-local on purpose: creating a `Global\` object needs a privilege a standard user
    /// does not have. Anything running elevated in the same session — an installer wanting to
    /// know whether the app is up, through `AppMutex` — can still open a local one.
    const MUTEX_NAME: &str = "Ymemo";

    pub(super) struct Guard(HANDLE);

    // The handle is only closed on drop, from whichever thread owns the guard.
    unsafe impl Send for Guard {}

    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    pub(super) fn acquire(_dir: &Path) -> Option<Guard> {
        // UTF-16, NUL terminated, as CreateMutexW wants.
        let name: Vec<u16> = MUTEX_NAME.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            if handle.is_null() {
                // Cannot tell, so let the app start rather than refuse over a lock.
                return Some(Guard(std::ptr::null_mut()));
            }
            if GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(handle);
                return None;
            }
            Some(Guard(handle))
        }
    }
}

// ===========================================================================
// Unix: flock on a file in the data directory
// ===========================================================================
#[cfg(unix)]
mod imp {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::os::unix::io::AsRawFd;
    use std::path::Path;

    /// Holds the locked file open; closing it is what releases the lock.
    pub(super) struct Guard {
        _file: std::fs::File,
    }

    /// The lock file lives in the data directory, not the vault: it is device-local state and
    /// syncing it would be nonsense.
    pub(super) fn acquire(dir: &Path) -> Option<Guard> {
        let path = dir.join("instance.lock");
        let mut file = OpenOptions::new().create(true).write(true).truncate(false).open(path).ok()?;
        // LOCK_NB: fail instead of waiting for the other instance to quit. The kernel drops
        // the lock when the process ends, crash included, so it can never go stale.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return None;
        }
        // Purely for a human looking at the file; nothing reads it back.
        let _ = file.set_len(0);
        let _ = write!(file, "{}", std::process::id());
        Some(Guard { _file: file })
    }
}

// Any other OS (macOS is unix, so this covers the rest): no guard.
#[cfg(not(any(windows, unix)))]
mod imp {
    use std::path::Path;
    pub(super) struct Guard;
    pub(super) fn acquire(_dir: &Path) -> Option<Guard> {
        Some(Guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both backends refuse a second holder and release on drop. Windows named mutexes and
    /// unix `flock` both work per open handle, not per process, so one process can play both
    /// instances here.
    #[test]
    fn the_lock_admits_one_holder_at_a_time() {
        let dir = std::env::temp_dir().join(format!("ymemo-inst-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let first = acquire(&dir).expect("the first instance should get the lock");
        assert!(acquire(&dir).is_none(), "a second instance must be refused");
        drop(first);
        assert!(acquire(&dir).is_some(), "the lock should be free again");

        std::fs::remove_dir_all(&dir).ok();
    }
}
