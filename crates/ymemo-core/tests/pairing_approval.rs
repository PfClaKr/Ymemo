//! The approval handshake, against two real Syncthing daemons.
//!
//! `#[ignore]`d, and skipped outright without `YMEMO_SYNCTHING_BIN`: it spawns two daemons,
//! lets them find each other and waits on real timeouts, none of which belongs in the unit
//! suite or on a CI runner that ships no daemon. Run it by hand after touching anything in
//! `sync.rs` or `pairing.rs`:
//!
//! ```text
//! YMEMO_SYNCTHING_BIN=/path/to/syncthing cargo test -p ymemo-core --test pairing_approval -- --ignored --nocapture
//! ```
//!
//! What it pins down is the claim the whole flow rests on: a device that is dialled by a
//! stranger files the caller as pending rather than silently dropping it, and sharing the
//! folder back is all it takes to complete the link.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ymemo_core::pairing;
use ymemo_core::sync::{Syncthing, VAULT_FOLDER_ID};

/// How long to wait for one side to notice the other. Discovery plus a dial attempt.
const REACH_TIMEOUT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(500);

struct Device {
    st: Syncthing,
    id: String,
    home: PathBuf,
    vault: PathBuf,
}

impl Drop for Device {
    fn drop(&mut self) {
        // The daemon goes down with `st`; its home and the vault are ours to clean up.
        let _ = std::fs::remove_dir_all(&self.home);
        let _ = std::fs::remove_dir_all(&self.vault);
    }
}

fn temp_dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ymemo-{what}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn start(binary: &Path, label: &str) -> Device {
    let home = temp_dir("sthome");
    let vault = temp_dir("vault");
    let st = Syncthing::spawn(binary, &home).expect("spawn syncthing");
    let id = st.device_id().expect("device id");
    st.ensure_folder(VAULT_FOLDER_ID, "Ymemo Vault", &vault).expect("register the folder");
    println!("{label}: {id}");
    Device { st, id, home, vault }
}

/// Polls `f` until it returns true, or panics with `what` after [`REACH_TIMEOUT`].
fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + REACH_TIMEOUT;
    while Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(POLL);
    }
    panic!("timed out waiting for {what}");
}

#[test]
#[ignore = "spawns two syncthing daemons; needs YMEMO_SYNCTHING_BIN"]
fn a_scanned_device_shows_up_as_pending_and_approving_it_links_both_sides() {
    let Some(binary) = std::env::var_os("YMEMO_SYNCTHING_BIN").map(PathBuf::from) else {
        eprintln!("YMEMO_SYNCTHING_BIN not set; skipping");
        return;
    };

    // A is the device holding the vault and showing its pairing code.
    let a = start(&binary, "A (shows the code)");
    let b = start(&binary, "B (scans it)");

    // Something for B to receive, so "linked" can be checked by the data arriving rather
    // than by a status flag alone.
    std::fs::write(a.vault.join("vault.json"), br#"{"pretend":"header"}"#).unwrap();

    // Both sides derive the same verification code from the two ids, with no exchange.
    assert_eq!(
        pairing::verification_code(&a.id, &b.id),
        pairing::verification_code(&b.id, &a.id),
    );

    // --- B scans A's code. This is B's half and nothing more. ---
    b.st.share_folder_with(VAULT_FOLDER_ID, &a.id).unwrap();
    assert!(
        a.st.pending_devices().unwrap().is_empty(),
        "A cannot know about B before B has dialled it"
    );

    // --- A learns of the request without anyone scanning anything back. ---
    wait_for("B to appear as a pending device on A", || {
        a.st.pending_devices().unwrap().iter().any(|d| d.id == b.id)
    });

    // --- Refusing clears the entry; it is Syncthing's list, not a permanent block. ---
    a.st.dismiss_pending_device(&b.id).unwrap();
    assert!(!a.st.pending_devices().unwrap().iter().any(|d| d.id == b.id));

    // --- Allowing is just sharing the folder back. ---
    wait_for("B to ask again after being dismissed", || {
        a.st.pending_devices().unwrap().iter().any(|d| d.id == b.id)
    });
    a.st.share_folder_with(VAULT_FOLDER_ID, &b.id).unwrap();

    wait_for("the vault to reach B", || b.vault.join("vault.json").is_file());
    wait_for("Syncthing to drop the answered request", || {
        a.st.pending_devices().unwrap().is_empty()
    });

    assert!(
        a.st.shared_devices(VAULT_FOLDER_ID).unwrap().iter().any(|d| d.id == b.id),
        "A should list B once the link is up"
    );
}
