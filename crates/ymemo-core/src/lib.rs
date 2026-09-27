//! Ymemo shared core: a pure Rust library used by the Slint desktop app and, through
//! `ymemo-ffi`, by the Flutter mobile app.
//!
//! This file holds the data model ([`Memo`], [`Group`]) and the local SQLite cache
//! ([`Store`]). The layers above live in sibling modules: [`crypto`] (key derivation and
//! AEAD), [`changelog`] (encrypted append-only log), [`vault`] (automerge merge and cache
//! rebuild), [`sync`] (Syncthing control), [`pairing`] and [`lan_pair`] (device linking).
//! [`mod@diag`] is the log file every one of them reports failures to.

pub mod blob;
pub mod changelog;
pub mod crypto;
pub mod diag;
pub mod export;
pub mod fsutil;
mod groups;
pub mod history;
pub mod lan_pair;
mod model;
pub mod order;
pub mod pairing;
pub mod recovery;
mod store;
pub mod sync;
pub mod update;
pub mod vault;

pub use groups::{group_children, is_descendant};
pub use model::*;
pub use store::Store;

/// Current time in Unix epoch millis; public so FFI callers share the same clock.
pub fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
