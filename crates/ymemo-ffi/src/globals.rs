//! The process-wide state the FFI keeps between calls — the open vault, the running daemon,
//! the LAN listener, the refusals and the last delete — and the locks around it.
//!
//! Dart calls in from several threads, so each lives behind a `Mutex`. None of this is part
//! of the generated API; `api` is what flutter_rust_bridge scans.

use std::collections::HashSet;
use std::sync::{Mutex, MutexGuard};

use anyhow::{anyhow, Result};
use ymemo_core::{diag, lan_pair, sync::Syncthing, vault::Vault};
use ymemo_i18n::t;

/// The open vault; one per app process.
pub(crate) static VAULT: Mutex<Option<Vault>> = Mutex::new(None);

/// The last thing a delete removed, for as long as the UI is offering to put it back.
///
/// One slot, like the desktop's: what this stands in for is a confirmation dialog, and the
/// question a confirmation answers is only ever about the delete that just happened. It is
/// cleared by [`vault_close`], so a locked vault leaves nothing here — the value holds a
/// memo's text, and that must not outlive the session that could read it.
pub(crate) static LAST_DELETE: Mutex<Option<ymemo_core::vault::Deleted>> = Mutex::new(None);

/// Records what a delete removed, so [`memo_undelete`] can offer it back.
pub(crate) fn remember_delete(removed: Option<ymemo_core::vault::Deleted>) {
    *relock(&LAST_DELETE) = removed;
}

/// Locks one of this file's globals, taking it back if a panic poisoned it.
///
/// flutter_rust_bridge turns a panic into a Dart exception and the app carries on, but a
/// `Mutex` held at that moment stays poisoned — and every call after it failed, for the rest
/// of the process, until the user thought to restart. None of these hold anything a panic
/// can leave half-true except the vault, which [`vault_lock`] re-reads.
pub(crate) fn relock<T>(m: &'static Mutex<T>) -> MutexGuard<'static, T> {
    m.lock().unwrap_or_else(|poisoned| {
        diag!("a lock was poisoned by a panic; carrying on with it");
        m.clear_poison();
        poisoned.into_inner()
    })
}

/// [`relock`] for the vault, which on the way back is re-read from its logs: a panic in the
/// middle of an edit can leave the document in memory holding half of it.
pub(crate) fn vault_lock() -> MutexGuard<'static, Option<Vault>> {
    VAULT.lock().unwrap_or_else(|poisoned| {
        diag!("the vault lock was poisoned by a panic; re-reading the vault");
        VAULT.clear_poison();
        let mut guard = poisoned.into_inner();
        if let Some(v) = guard.as_mut() {
            if let Err(e) = v.reload() {
                diag!("could not re-read the vault after a panic: {e}");
            }
        }
        guard
    })
}

pub(crate) fn with_vault<T>(f: impl FnOnce(&mut Vault) -> Result<T>) -> Result<T> {
    let mut guard = vault_lock();
    let vault = guard.as_mut().ok_or_else(|| anyhow!(t!("core.vault_not_open")))?;
    f(vault)
}

/// The running daemon, one per app process, like [`VAULT`].
pub(crate) static SYNC: Mutex<Option<Syncthing>> = Mutex::new(None);

pub(crate) fn sync_lock() -> MutexGuard<'static, Option<Syncthing>> {
    relock(&SYNC)
}

pub(crate) fn with_sync<T>(f: impl FnOnce(&Syncthing) -> Result<T>) -> Result<T> {
    let guard = sync_lock();
    let st = guard.as_ref().ok_or_else(|| anyhow!(t!("core.sync_not_running")))?;
    f(st)
}

/// Requests refused during this run of the app.
///
/// Syncthing does not remember a refusal — the caller keeps retrying and is filed again —
/// so the answer is kept here instead. Deliberately in memory and deliberately **not** in
/// the `Syncthing` handle: mobile stops the daemon every time the app is backgrounded, and
/// a refusal that expired on the walk back to the app would be no refusal at all. Starting
/// the app again clears it, so a mis-tapped "reject" is never permanent.
pub(crate) static REJECTED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

pub(crate) fn rejected_lock() -> MutexGuard<'static, Option<HashSet<String>>> {
    relock(&REJECTED)
}

/// The pairing-mode listener, alive only while the screen is open.
pub(crate) static LAN: Mutex<Option<lan_pair::PairListener>> = Mutex::new(None);

pub(crate) fn lan_lock() -> MutexGuard<'static, Option<lan_pair::PairListener>> {
    relock(&LAN)
}
