use automerge::{transaction::Transactable, AutoCommit, ObjType, ReadDoc, Value, ROOT};
use std::fs;
use std::path::{Path, PathBuf};

use crate::changelog::ChangeLog;
use crate::crypto::{generate_salt, MasterKey, Salt};
use crate::history::{Entity, RevisionKind};
use crate::{Group, Memo, Store};

use super::*;
use super::doc::put_text_if_changed;
use super::header::{from_hex, read_header, to_hex, VaultHeader};
use super::photos::{close_gap, open_gap};

/// A fresh temporary vault directory.
fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ymemo-vault-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Ids in the order the list draws them.
fn order(v: &Vault) -> Vec<String> {
    v.store().list().unwrap().into_iter().map(|m| m.title).collect()
}

/// A folder nobody has arranged is still the newest-first list it always was, and a new
/// memo lands at the top of it without anything being written to say so.
#[test]
fn an_unarranged_folder_is_still_newest_first() {
    let dir = temp_dir();
    let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    for title in ["one", "two", "three"] {
        let mut m = Memo::new(title, "");
        m.updated_at = crate::now_millis() + order(&v).len() as i64;
        v.upsert(&m).unwrap();
    }
    assert_eq!(order(&v), ["three", "two", "one"]);
    assert!(v.store().list().unwrap().iter().all(|m| m.order_key.is_empty()));
}

/// The first drag stamps the folder with the order it was already being shown in, so
/// nothing jumps except the memo being moved.
#[test]
fn arranging_keeps_what_was_on_screen_and_moves_only_the_one() {
    let dir = temp_dir();
    let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let mut ids = Vec::new();
    for (i, title) in ["a", "b", "c", "d"].iter().enumerate() {
        let mut m = Memo::new(*title, "");
        m.updated_at = 1_000 + i as i64;
        v.upsert(&m).unwrap();
        ids.push(m.id);
    }
    // Newest first: d c b a
    assert_eq!(order(&v), ["d", "c", "b", "a"]);

    // Drag "a" to the very top.
    let a = ids[0].clone();
    v.move_memo(&a, "", None, Some(&ids[3])).unwrap();
    assert_eq!(order(&v), ["a", "d", "c", "b"]);
    // Everything now has a key, so the arrangement survives a later edit.
    assert!(v.store().list().unwrap().iter().all(|m| !m.order_key.is_empty()));
}

/// Rearranging is not editing: it must not restamp `updated_at`, or the folder it is
/// arranging reorders underneath it.
#[test]
fn arranging_leaves_the_timestamps_alone() {
    let dir = temp_dir();
    let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let mut m = Memo::new("only", "");
    m.updated_at = 4_242;
    v.upsert(&m).unwrap();
    let other = Memo::new("other", "");
    v.upsert(&other).unwrap();

    v.move_memo(&m.id, "", Some(&other.id), None).unwrap();
    assert_eq!(v.store().get(&m.id).unwrap().unwrap().updated_at, 4_242);
}

/// An arrangement is a memo field like any other, so the logs alone must restore it.
#[test]
fn the_arrangement_survives_a_rebuild_from_logs() {
    let dir = temp_dir();
    let ids: Vec<String>;
    {
        let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let mut made = Vec::new();
        for (i, title) in ["a", "b", "c"].iter().enumerate() {
            let mut m = Memo::new(*title, "");
            m.updated_at = 1_000 + i as i64;
            v.upsert(&m).unwrap();
            made.push(m.id);
        }
        // "a" to the top. Both neighbours given: `None` on both sides is not "the
        // bottom", it is the middle of an empty folder.
        v.move_memo(&made[0], "", None, Some(&made[2])).unwrap();
        assert_eq!(order(&v), ["a", "c", "b"]);
        ids = made;
    }
    let v = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    assert_eq!(order(&v), ["a", "c", "b"]);
    assert!(!v.store().get(&ids[0]).unwrap().unwrap().order_key.is_empty());
}

/// Placing into an arranged folder puts a new memo on top, where a new memo belongs.
#[test]
fn a_new_memo_lands_on_top_of_an_arranged_folder() {
    let dir = temp_dir();
    let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let first = Memo::new("first", "");
    v.upsert(&first).unwrap();
    let second = Memo::new("second", "");
    v.upsert(&second).unwrap();
    v.move_memo(&first.id, "", Some(&second.id), None).unwrap();
    assert_eq!(order(&v), ["second", "first"]);

    v.upsert(&Memo::new("newest", "")).unwrap();
    assert_eq!(order(&v), ["newest", "second", "first"]);
}

/// The case a position number cannot survive, and the reason the order is a fractional
/// index: two devices rearrange the same folder without seeing each other.
///
/// Neither arrangement may be thrown away, and both devices must end up drawing the
/// folder the same way round — which is what a renumbering scheme cannot promise, because
/// each device would write a new number for memos it never touched.
#[test]
fn two_devices_rearranging_at_once_agree_on_the_result() {
    let dir = temp_dir();

    let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let mut ids = Vec::new();
    for (i, title) in ["a", "b", "c", "d"].iter().enumerate() {
        let mut m = Memo::new(*title, "");
        m.updated_at = 1_000 + i as i64;
        a.upsert(&m).unwrap();
        ids.push(m.id);
    }
    // Arrange it once so both devices start from the same keys, not from timestamps.
    a.move_memo(&ids[0], "", None, Some(&ids[3])).unwrap();
    assert_eq!(order(&a), ["a", "d", "c", "b"]);

    let mut b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    assert_eq!(order(&b), ["a", "d", "c", "b"]);

    // Neither can see the other: A drops "c" at the top, B drops "b" at the top.
    a.move_memo(&ids[2], "", None, Some(&ids[0])).unwrap();
    b.move_memo(&ids[1], "", None, Some(&ids[0])).unwrap();

    a.rebuild().unwrap();
    b.rebuild().unwrap();

    // Both moves survived — neither device's drag was silently undone by the other's.
    let merged = order(&a);
    assert_eq!(merged, order(&b), "the two devices disagree about the folder");
    assert!(
        merged.iter().position(|t| t == "c").unwrap() < merged.iter().position(|t| t == "a").unwrap(),
        "A's move was lost: {merged:?}",
    );
    assert!(
        merged.iter().position(|t| t == "b").unwrap() < merged.iter().position(|t| t == "a").unwrap(),
        "B's move was lost: {merged:?}",
    );
}

/// Moving a memo to another folder puts it at the top of that folder, not at whatever
/// position its old key happens to name among memos it has never been beside.
#[test]
fn a_memo_moved_to_another_folder_lands_on_top_of_it() {
    let dir = temp_dir();
    let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let folder = crate::Group::new("folder");
    v.upsert_group(&folder).unwrap();

    // Two memos in the folder, arranged, so it has real keys to land among.
    let mut inside = Vec::new();
    for (i, title) in ["in-one", "in-two"].iter().enumerate() {
        let mut m = Memo::new(*title, "");
        m.group_id = folder.id.clone();
        m.updated_at = 1_000 + i as i64;
        v.upsert(&m).unwrap();
        inside.push(m.id);
    }
    v.move_memo(&inside[0], &folder.id, Some(&inside[1]), None).unwrap();

    // One at the top level, arranged to the bottom so its key is a large one.
    let mut outside = Memo::new("outsider", "");
    outside.updated_at = 5_000;
    v.upsert(&outside).unwrap();
    let filler = Memo::new("filler", "");
    v.upsert(&filler).unwrap();
    v.move_memo(&outside.id, "", Some(&filler.id), None).unwrap();

    // Now move it into the folder the ordinary way — as the list and the phone both do.
    let mut moved = v.store().get(&outside.id).unwrap().unwrap();
    moved.group_id = folder.id.clone();
    v.upsert(&moved).unwrap();

    let in_folder: Vec<String> = v
        .store()
        .in_group(&folder.id)
        .unwrap()
        .into_iter()
        .map(|m| m.title)
        .collect();
    assert_eq!(in_folder[0], "outsider", "landed at {in_folder:?}");
}

/// Blob to a file, record to the log — and the logs alone must restore both.
#[test]
fn attachment_survives_a_rebuild_from_logs() {
    let dir = temp_dir();
    let photo = b"\x89PNG fake bytes".repeat(50);
    let (memo_id, att_id);
    {
        let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let memo = Memo::new("memo with a photo", "");
        v.upsert(&memo).unwrap();
        let a = v.attach(&memo.id, &photo, "photo.png", "image/png", 4000, 3000).unwrap();
        memo_id = memo.id;
        att_id = a.id;
    }

    // Reopening with an empty cache replays the logs.
    let v = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let list = v.store().attachments_of(&memo_id).unwrap();
    assert_eq!(list.len(), 1);
    let a = &list[0];
    assert_eq!(a.id, att_id);
    assert_eq!(a.name, "photo.png");
    assert_eq!((a.width_px, a.height_px), (4000, 3000));
    assert_eq!(a.width_em_milli, crate::DEFAULT_WIDTH_EM_MILLI);
    // The bytes must come back too.
    assert!(v.has_blob(&a.hash));
    assert_eq!(v.attachment_bytes(&a.hash).unwrap(), photo);

    fs::remove_dir_all(&dir).ok();
}

/// A display size set on one device carries to the other: it is em, not pixels, so it
/// stays "so many characters wide" whatever the font size.
#[test]
fn display_width_syncs_between_devices() {
    let dir = temp_dir();
    let mut phone = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let memo = Memo::new("photo", "");
    phone.upsert(&memo).unwrap();
    let a = phone.attach(&memo.id, b"jpeg", "p.jpg", "image/jpeg", 1000, 500).unwrap();

    // Shrink to 8em on the phone.
    phone.set_attachment_layout(&a.id, a.x_permille, a.y_permille, 8_000).unwrap();

    // The desktop (another cache, another device) merges and sees the same value.
    let desktop = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let got = desktop.store().get_attachment(&a.id).unwrap().unwrap();
    assert_eq!(got.width_em_milli, 8_000);

    // Same 8em, different base fonts: different pixels, same ratio.
    assert_eq!(got.display_size(16.0), (128.0, 64.0));
    assert_eq!(got.display_size(20.0), (160.0, 80.0));

    fs::remove_dir_all(&dir).ok();
}

/// Where a photo was dropped on the note carries to the other device too, and as a
/// fraction of the note it lands on the same part of a phone screen and a small sticky.
#[test]
fn photo_placement_syncs_between_devices() {
    let dir = temp_dir();
    let mut phone = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let memo = Memo::new("photo", "");
    phone.upsert(&memo).unwrap();
    let a = phone.attach(&memo.id, b"jpeg", "p.jpg", "image/jpeg", 1000, 500).unwrap();

    // Drag it to the middle and shrink it, in one write.
    phone.set_attachment_layout(&a.id, 500, 250, 10_000).unwrap();

    let desktop = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let got = desktop.store().get_attachment(&a.id).unwrap().unwrap();
    assert_eq!((got.x_permille, got.y_permille), (500, 250));
    assert_eq!(got.width_em_milli, 10_000);

    fs::remove_dir_all(&dir).ok();
}

/// The float/flow choice travels, and a photo from before the choice existed is a float.
#[test]
fn photo_mode_syncs_and_defaults_to_floating() {
    let dir = temp_dir();
    let mut phone = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let memo = Memo::new("photo", "");
    phone.upsert(&memo).unwrap();
    let a = phone.attach(&memo.id, b"jpeg", "p.jpg", "image/jpeg", 1000, 500).unwrap();

    // Every photo starts where photos have always been: lying on top of the note.
    assert_eq!(a.mode(), crate::PhotoMode::Float);
    assert!(a.mode.is_empty(), "a float stores nothing, so old memos need no migration");

    phone.take_attachment_out_of_writing(&a.id, crate::PhotoMode::Flow).unwrap();

    let mut desktop = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    assert_eq!(
        desktop.store().get_attachment(&a.id).unwrap().unwrap().mode(),
        crate::PhotoMode::Flow
    );

    // And back again.
    desktop.take_attachment_out_of_writing(&a.id, crate::PhotoMode::Float).unwrap();
    let back = desktop.store().get_attachment(&a.id).unwrap().unwrap();
    assert_eq!(back.mode(), crate::PhotoMode::Float);
    assert!(back.mode.is_empty());

    fs::remove_dir_all(&dir).ok();
}

/// A mode written by a version this one has never met is drawn as a float rather than
/// dropping the photo.
#[test]
fn an_unknown_photo_mode_reads_as_floating() {
    assert_eq!(crate::PhotoMode::parse("wrapped-around"), crate::PhotoMode::Float);
    assert_eq!(crate::PhotoMode::parse(""), crate::PhotoMode::Float);
    assert_eq!(crate::PhotoMode::parse("flow"), crate::PhotoMode::Flow);
}

/// Two photos on one memo do not land on the same spot, or the lower one could not be
/// picked up again.
#[test]
fn photos_cascade_instead_of_stacking() {
    let dir = temp_dir();
    let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let memo = Memo::new("photos", "");
    v.upsert(&memo).unwrap();
    let a = v.attach(&memo.id, b"one", "1.jpg", "image/jpeg", 10, 10).unwrap();
    let b = v.attach(&memo.id, b"two", "2.jpg", "image/jpeg", 10, 10).unwrap();
    assert_ne!((a.x_permille, a.y_permille), (b.x_permille, b.y_permille));

    fs::remove_dir_all(&dir).ok();
}

/// Detaching keeps the blob file — no GC, another device may still reference it.
#[test]
fn detach_keeps_the_blob_file() {
    let dir = temp_dir();
    let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let memo = Memo::new("memo", "");
    v.upsert(&memo).unwrap();
    let a = v.attach(&memo.id, b"bytes", "x.png", "image/png", 10, 10).unwrap();

    v.detach(&a.id).unwrap();
    assert!(v.store().attachments_of(&memo.id).unwrap().is_empty());
    assert!(v.has_blob(&a.hash), "the blob file must survive");

    // The detach must survive a replay, i.e. it reached the log.
    let v2 = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    assert!(v2.store().attachments_of(&memo.id).unwrap().is_empty());

    fs::remove_dir_all(&dir).ok();
}

/// A log written under a foreign key must not fail the rebuild; the readable logs still
/// apply.
#[test]
fn rebuild_skips_undecryptable_foreign_log() {
    let dir = temp_dir();
    let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let memo = Memo::new("my memo", "");
    a.upsert(&memo).unwrap();

    // Plant a log encrypted with a different key.
    let foreign = ChangeLog::open(
        dir.join(LOGS_DIR).join("ffffffff.ymlog"),
        MasterKey::derive(b"other password", &generate_salt()).unwrap(),
    );
    foreign.append(b"not an automerge change").unwrap();

    // The merge still succeeds and our memo survives.
    a.rebuild().unwrap();
    assert_eq!(a.store().get(&memo.id).unwrap().unwrap().title, "my memo");

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn create_reopen_rebuilds_cache() {
    let dir = temp_dir();
    let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

    let m1;
    {
        let mut vault = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
        m1 = Memo::new("keeper", "body");
        vault.upsert(&m1).unwrap();
        let dead = Memo::new("doomed", "");
        vault.upsert(&dead).unwrap();
        vault.delete(&dead.id).unwrap();
    } // vault dropped; the log files are the source of truth

    // Reopen with the same cache (same device id, same actor): the seq must continue.
    let m2;
    {
        let mut vault = Vault::open(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
        assert_eq!(vault.store().list().unwrap(), vec![m1.clone()]);
        m2 = Memo::new("second session", "");
        vault.upsert(&m2).unwrap();
    }

    // A brand-new cache restores everything from the logs alone.
    let vault = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let mut titles: Vec<String> =
        vault.store().list().unwrap().into_iter().map(|m| m.title).collect();
    titles.sort();
    assert_eq!(titles, vec!["keeper", "second session"]);

    fs::remove_dir_all(&dir).ok();
    fs::remove_file(&db).ok();
}

/// Writes a header wrapping `data_key` under `password`, to imitate Syncthing's conflict
/// resolution: the winner lands in vault.json while the loser stays as
/// `vault.sync-conflict-*.json`.
fn write_test_header(dir: &Path, name: &str, password: &[u8], salt: &Salt, data_key: &MasterKey) {
    let password_key = MasterKey::derive(password, salt).unwrap();
    let header = VaultHeader {
        version: HEADER_VERSION,
        salt: to_hex(salt),
        key_check: to_hex(&data_key.encrypt(KEY_CHECK).unwrap()),
        wrapped_key: to_hex(&password_key.encrypt(&data_key.to_bytes()).unwrap()),
        recovery_salt: String::new(),
        recovery_key: String::new(),
    };
    fs::write(dir.join(name), serde_json::to_vec_pretty(&header).unwrap()).unwrap();
}

/// A header in the original unwrapped format, where the password key is the data key.
fn write_legacy_header(dir: &Path, password: &[u8], salt: &Salt) {
    let key = MasterKey::derive(password, salt).unwrap();
    let header = serde_json::json!({
        "version": 1,
        "salt": to_hex(salt),
        "key_check": to_hex(&key.encrypt(KEY_CHECK).unwrap()),
    });
    fs::write(dir.join(HEADER_FILE), serde_json::to_vec_pretty(&header).unwrap()).unwrap();
}

/// With our log under the old salt and vault.json converged on the canonical one, open
/// must re-encrypt the log and bring the memos back.
#[test]
fn heals_divergent_vault_key_on_open() {
    let dir = temp_dir();
    let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

    // This device creates the vault under the old salt and writes a memo.
    let memo;
    {
        let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
        memo = Memo::new("must survive", "body");
        v.upsert(&memo).unwrap();
    }
    let old_salt: Salt = from_hex(
        &serde_json::from_slice::<VaultHeader>(&fs::read(dir.join(HEADER_FILE)).unwrap())
            .unwrap()
            .salt,
    )
    .unwrap()
    .try_into()
    .unwrap();

    // Imitate the conflict resolution: loser to conflict, canonical salt to vault.json.
    fs::rename(
        dir.join(HEADER_FILE),
        dir.join("vault.sync-conflict-20260101-120000-AAAAAAA.json"),
    )
    .unwrap();
    let canonical_salt = generate_salt();
    assert_ne!(canonical_salt, old_salt);
    // The winning device made its own data key, so healing has to re-encrypt the log
    // rather than merely re-derive from another salt.
    let canonical_key = MasterKey::from_bytes(&crate::crypto::generate_key()).unwrap();
    write_test_header(&dir, HEADER_FILE, b"pw", &canonical_salt, &canonical_key);

    // Reopening with the same password heals the log and the memo is back.
    let v = Vault::open(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
    assert_eq!(v.store().get(&memo.id).unwrap().unwrap().title, "must survive");

    // Our log now opens under the canonical key directly.
    let device_id = Store::open(&db).unwrap().device_id().unwrap();
    let own_log = ChangeLog::open(
        dir.join(LOGS_DIR).join(format!("{device_id}.{LOG_EXT}")),
        canonical_key,
    );
    assert!(own_log.read_all().is_ok());

    fs::remove_dir_all(&dir).ok();
    fs::remove_file(&db).ok();
}

/// A crash mid-append left half a record at the end of our log. Reopening must still
/// show every memo before it, and what is written next must be readable — before, the
/// whole log was skipped and every later append landed behind the torn bytes.
#[test]
fn a_torn_own_log_still_opens_and_stays_writable() {
    let dir = temp_dir();
    let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));
    let kept = Memo::new("kept", "body");
    let device_id = {
        let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
        v.upsert(&kept).unwrap();
        v.device_id.clone()
    };
    let log = dir.join(LOGS_DIR).join(format!("{device_id}.{LOG_EXT}"));
    let mut raw = fs::read(&log).unwrap();
    raw.extend_from_slice(&200u32.to_le_bytes());
    raw.extend_from_slice(b"half a record");
    fs::write(&log, &raw).unwrap();

    let later = Memo::new("later", "");
    {
        let mut v = Vault::open(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
        assert_eq!(v.store().get(&kept.id).unwrap().unwrap().title, "kept");
        v.upsert(&later).unwrap();
    }
    let v = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    assert!(v.store().get(&kept.id).unwrap().is_some());
    assert!(v.store().get(&later.id).unwrap().is_some());

    fs::remove_dir_all(&dir).ok();
    fs::remove_file(&db).ok();
}

/// A password change must rewrap the same data key: instant, and invisible to the logs.
#[test]
fn change_password_keeps_the_data_key_and_the_memos() {
    let dir = temp_dir();
    let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

    let memo = Memo::new("survives the change", "body");
    let key_before = {
        let mut v = Vault::create(&dir, b"old-pw", Store::open(&db).unwrap()).unwrap();
        v.upsert(&memo).unwrap();
        v.change_password(b"old-pw", b"new-pw").unwrap();
        v.key_bytes()
    };

    // The old password is gone, the new one opens, and the memo never moved.
    assert!(Vault::open(&dir, b"old-pw", Store::open_in_memory().unwrap()).is_err());
    let v = Vault::open(&dir, b"new-pw", Store::open(&db).unwrap()).unwrap();
    assert_eq!(v.store().get(&memo.id).unwrap().unwrap().title, "survives the change");
    // Same data key, so no log or blob had to be rewritten.
    assert_eq!(v.key_bytes(), key_before);

    fs::remove_dir_all(&dir).ok();
    fs::remove_file(&db).ok();
}

#[test]
fn change_password_rejects_a_wrong_current_password() {
    let dir = temp_dir();
    let v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    assert!(v.change_password(b"not-the-password", b"new-pw").is_err());
    assert!(v.change_password(b"pw", b"").is_err(), "an empty new password is rejected");
    // Still the original password.
    assert!(Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).is_ok());
    fs::remove_dir_all(&dir).ok();
}

/// The recovery code opens a vault whose password is lost, without touching the data.
#[test]
fn recovery_code_sets_a_new_password() {
    let dir = temp_dir();
    let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

    let memo = Memo::new("behind a forgotten password", "body");
    let code = {
        let mut v = Vault::create(&dir, b"forgotten", Store::open(&db).unwrap()).unwrap();
        v.upsert(&memo).unwrap();
        assert!(!v.has_recovery_code());
        let code = v.issue_recovery_code().unwrap();
        assert!(v.has_recovery_code());
        code
    };
    assert!(recovery_code_exists(&dir));

    assert!(reset_password_with_recovery(&dir, "WRONG-C0DE-0000-0000-0000-0000-0000-0000", b"x").is_err());
    // Formatting is not part of the secret: lower case and no dashes still works.
    let typed = code.to_lowercase().replace('-', " ");
    reset_password_with_recovery(&dir, &typed, b"brand-new").unwrap();

    assert!(Vault::open(&dir, b"forgotten", Store::open_in_memory().unwrap()).is_err());
    let v = Vault::open(&dir, b"brand-new", Store::open(&db).unwrap()).unwrap();
    assert_eq!(v.store().get(&memo.id).unwrap().unwrap().title, "behind a forgotten password");
    // The code is not spent by using it.
    assert!(v.has_recovery_code());

    fs::remove_dir_all(&dir).ok();
    fs::remove_file(&db).ok();
}

#[test]
fn recovery_is_refused_when_no_code_was_issued() {
    let dir = temp_dir();
    Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    assert!(!recovery_code_exists(&dir));
    assert!(reset_password_with_recovery(&dir, &crate::recovery::generate(), b"new").is_err());
    fs::remove_dir_all(&dir).ok();
}

/// Vaults written before key wrapping must keep opening, and be upgraded in place by a
/// password change without losing a single record.
#[test]
fn legacy_unwrapped_header_opens_and_upgrades() {
    let dir = temp_dir();
    let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

    // Build a vault the old way: the password key encrypts the log directly.
    let salt = generate_salt();
    write_legacy_header(&dir, b"pw", &salt);
    let memo = Memo::new("written before wrapping", "body");
    {
        let mut v = Vault::open(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
        // The data key is the password key while the header stays unwrapped.
        assert_eq!(v.key_bytes(), MasterKey::derive(b"pw", &salt).unwrap().to_bytes());
        v.upsert(&memo).unwrap();
        v.change_password(b"pw", b"pw2").unwrap();
    }

    let v = Vault::open(&dir, b"pw2", Store::open(&db).unwrap()).unwrap();
    assert_eq!(v.store().get(&memo.id).unwrap().unwrap().title, "written before wrapping");
    // Upgraded in place, and the data key still is the original one, so the log written
    // under it is readable without re-encryption.
    assert_eq!(v.key_bytes(), MasterKey::derive(b"pw", &salt).unwrap().to_bytes());
    assert_eq!(read_header(&dir).unwrap().version, HEADER_VERSION);

    fs::remove_dir_all(&dir).ok();
    fs::remove_file(&db).ok();
}

#[test]
fn wipe_empties_a_vault_but_refuses_anything_else() {
    let dir = temp_dir();
    Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    assert!(dir.join(HEADER_FILE).exists());

    let not_a_vault = temp_dir();
    fs::write(not_a_vault.join("holiday-photo.jpg"), b"not ours").unwrap();
    assert!(wipe(&not_a_vault).is_err());
    assert!(not_a_vault.join("holiday-photo.jpg").exists());

    wipe(&dir).unwrap();
    assert!(!dir.join(HEADER_FILE).exists());
    assert!(!dir.join(LOGS_DIR).exists());
    // A wiped directory is a first run again.
    Vault::create(&dir, b"fresh", Store::open_in_memory().unwrap()).unwrap();

    fs::remove_dir_all(&dir).ok();
    fs::remove_dir_all(&not_a_vault).ok();
}

/// A folder's colour is part of the document, so it reaches the other devices the same
/// way its name does.
#[test]
fn group_colour_syncs_across_devices() {
    let dir = temp_dir();
    let db_a = std::env::temp_dir().join(format!("ymemo-a-{}.db", uuid::Uuid::new_v4()));
    let db_b = std::env::temp_dir().join(format!("ymemo-b-{}.db", uuid::Uuid::new_v4()));

    let mut group = Group::new("shared folder");
    {
        let mut a = Vault::create(&dir, b"pw", Store::open(&db_a).unwrap()).unwrap();
        a.upsert_group(&group).unwrap();
        group.color = "blue".into();
        a.upsert_group(&group).unwrap();
    }

    // A second device merges the same logs and sees the colour, not the default.
    let b = Vault::open(&dir, b"pw", Store::open(&db_b).unwrap()).unwrap();
    let seen = b.store().get_group(&group.id).unwrap().unwrap();
    assert_eq!(seen.color, "blue");
    assert_eq!(seen.name, "shared folder");

    fs::remove_dir_all(&dir).ok();
    fs::remove_file(&db_a).ok();
    fs::remove_file(&db_b).ok();
}

/// The vault's name travels in the logs, so a paired device shows the same one.
#[test]
fn vault_name_syncs_across_devices() {
    let dir = temp_dir();
    let db_a = std::env::temp_dir().join(format!("ymemo-a-{}.db", uuid::Uuid::new_v4()));
    let db_b = std::env::temp_dir().join(format!("ymemo-b-{}.db", uuid::Uuid::new_v4()));

    {
        let mut a = Vault::create(&dir, b"pw", Store::open(&db_a).unwrap()).unwrap();
        assert_eq!(a.name(), "", "a new vault has no name until one is given");
        a.set_name("  집 메모  ").unwrap();
        assert_eq!(a.name(), "집 메모", "surrounding space is not part of the name");
    }

    let b = Vault::open(&dir, b"pw", Store::open(&db_b).unwrap()).unwrap();
    assert_eq!(b.name(), "집 메모");

    fs::remove_dir_all(&dir).ok();
    fs::remove_file(&db_a).ok();
    fs::remove_file(&db_b).ok();
}

/// A rename is a document change like any other, so it survives the cache being thrown
/// away and rebuilt from the logs.
#[test]
fn vault_name_survives_a_rebuild() {
    let dir = temp_dir();
    let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));
    let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();

    v.set_name("work").unwrap();
    v.set_name("work notes").unwrap();
    v.rebuild().unwrap();
    assert_eq!(v.name(), "work notes");

    // Longer than a heading can hold; cut by characters, so Korean stays whole.
    let long = "가".repeat(crate::VAULT_NAME_MAX + 20);
    v.set_name(&long).unwrap();
    assert_eq!(v.name().chars().count(), crate::VAULT_NAME_MAX);

    fs::remove_dir_all(&dir).ok();
    fs::remove_file(&db).ok();
}

/// Folders written before they had colours must still open, at the default.
#[test]
fn group_without_colour_falls_back_to_the_default() {
    let dir = temp_dir();
    let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

    let id = {
        let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
        let group = Group::new("no colour here");
        v.upsert_group(&group).unwrap();
        // Imitate the older document shape by dropping the field again.
        let groups = v.groups_obj().unwrap();
        let (_, obj) = v.doc.get(&groups, &group.id).unwrap().unwrap();
        v.doc.delete(&obj, "color").unwrap();
        v.append_local_change().unwrap();
        group.id
    };

    let v = Vault::open(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
    assert_eq!(v.store().get_group(&id).unwrap().unwrap().color, crate::DEFAULT_COLOR);

    fs::remove_dir_all(&dir).ok();
    fs::remove_file(&db).ok();
}

/// Every edit leaves a revision, in order, with the values it had at the time.
#[test]
fn memo_history_records_each_edit() {
    let dir = temp_dir();
    let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

    let mut memo = Memo::new("first", "one");
    {
        let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
        v.upsert(&memo).unwrap();
        memo.body = "one two".into();
        v.upsert(&memo).unwrap();
        memo.title = "second".into();
        memo.color = "blue".into();
        v.upsert(&memo).unwrap();
    }

    let mut v = Vault::open(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
    let hist = v.history(Entity::Memo, &memo.id).unwrap();
    assert_eq!(hist.len(), 3, "creation plus two edits");
    assert_eq!(hist[0].kind, RevisionKind::Created);
    assert_eq!(hist[0].field("title"), "first");
    assert_eq!(hist[0].field("body"), "one");

    assert_eq!(hist[1].kind, RevisionKind::Edited);
    assert_eq!(hist[1].field("body"), "one two");
    assert_eq!(hist[1].changed, vec!["body".to_string()]);

    // The last revision reports both fields it moved, in the order fields() lists them.
    assert_eq!(hist[2].changed, vec!["title".to_string(), "color".to_string()]);
    // Every revision names the device that wrote it.
    let device = Store::open(&db).unwrap().device_id().unwrap();
    assert!(hist.iter().all(|r| r.device == device));

    fs::remove_dir_all(&dir).ok();
    fs::remove_file(&db).ok();
}

/// A restore puts back **every** field of that version, and it survives the log.
///
/// `restoring_appends_rather_than_rewrites` above covers the body and the appending; what
/// is left untested is the rest of the memo (a restore that put the old text back under
/// today's colour would look like a bug to the person who clicked it), the trip through
/// the log another device reads, and stepping forward again afterwards.
#[test]
fn a_restore_puts_the_whole_version_back_and_can_be_stepped_forward_again() {
    let dir = temp_dir();
    let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();

    let mut memo = Memo::new("shopping", "milk\n");
    v.upsert(&memo).unwrap();
    memo.body = "milk\nbread\n".into();
    memo.title = "groceries".into();
    memo.color = "blue".into();
    memo.opacity = 60;
    v.upsert(&memo).unwrap();

    let hist = v.history(Entity::Memo, &memo.id).unwrap();
    v.restore(Entity::Memo, &memo.id, &hist[0]).unwrap();

    let back = v.store().get(&memo.id).unwrap().unwrap();
    assert_eq!(back.title, "shopping");
    assert_eq!(back.body, "milk\n");
    assert_eq!(back.color, crate::DEFAULT_COLOR, "the colour of that version, not today's");
    assert_eq!(back.opacity, crate::DEFAULT_OPACITY);

    // What another device will read is the log, not the cache.
    v.rebuild().unwrap();
    assert_eq!(v.store().get(&memo.id).unwrap().unwrap().body, "milk\n");

    // And the version that was stepped over is still there to step back to.
    let hist = v.history(Entity::Memo, &memo.id).unwrap();
    v.restore(Entity::Memo, &memo.id, &hist[1]).unwrap();
    let forward = v.store().get(&memo.id).unwrap().unwrap();
    assert_eq!(forward.body, "milk\nbread\n");
    assert_eq!(forward.color, "blue");

    fs::remove_dir_all(&dir).ok();
}

/// A folder can be restored too, and its name takes the other write path — a label rather
/// than prose. Nothing covered this side of `restore` at all.
#[test]
fn restoring_a_folder_puts_its_old_name_and_colour_back() {
    let dir = temp_dir();
    let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();

    let mut g = Group::new("Inbox");
    g.id = "g".into();
    v.upsert_group(&g).unwrap();
    g.name = "Work".into();
    g.color = "blue".into();
    v.upsert_group(&g).unwrap();

    let hist = v.history(Entity::Group, "g").unwrap();
    v.restore(Entity::Group, "g", &hist[0]).unwrap();

    let back = v.store().get_group("g").unwrap().unwrap();
    assert_eq!(back.name, "Inbox");
    assert_eq!(back.color, crate::DEFAULT_COLOR);

    v.rebuild().unwrap();
    assert_eq!(v.store().get_group("g").unwrap().unwrap().name, "Inbox");

    fs::remove_dir_all(&dir).ok();
}

/// Revisions must be dated. Automerge's default commit leaves the time at zero, which
/// showed every version as 1970 until the vault started stamping its own.
#[test]
fn revisions_carry_the_time_they_were_written() {
    let dir = temp_dir();
    let before = crate::now_millis();

    let memo = Memo::new("dated", "body");
    let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    v.upsert(&memo).unwrap();

    let rev = &v.history(Entity::Memo, &memo.id).unwrap()[0];
    // Automerge keeps seconds, so the millis it comes back as are rounded down.
    assert!(rev.at >= before - 1000, "revision dated before the write: {}", rev.at);
    assert!(rev.at <= crate::now_millis() + 1000, "revision dated in the future: {}", rev.at);

    fs::remove_dir_all(&dir).ok();
}

/// Restoring is a new edit, not a rewrite: the versions it stepped over stay readable.
#[test]
fn restoring_appends_rather_than_rewrites() {
    let dir = temp_dir();
    let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

    let mut memo = Memo::new("keep", "original");
    let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
    v.upsert(&memo).unwrap();
    memo.body = "ruined".into();
    v.upsert(&memo).unwrap();
    assert_eq!(v.store().get(&memo.id).unwrap().unwrap().body, "ruined");

    let first = v.history(Entity::Memo, &memo.id).unwrap()[0].clone();
    v.restore(Entity::Memo, &memo.id, &first).unwrap();
    assert_eq!(v.store().get(&memo.id).unwrap().unwrap().body, "original");

    // Three revisions now: the two edits and the restore. Nothing was removed.
    let hist = v.history(Entity::Memo, &memo.id).unwrap();
    assert_eq!(hist.len(), 3);
    assert_eq!(hist[1].field("body"), "ruined", "the bad version is still there");
    assert_eq!(hist[2].field("body"), "original");

    fs::remove_dir_all(&dir).ok();
    fs::remove_file(&db).ok();
}

/// A deleted memo keeps its history, and can be brought back from it.
#[test]
fn deletion_is_a_revision_and_can_be_undone() {
    let dir = temp_dir();
    let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

    let memo = Memo::new("gone", "body");
    let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
    v.upsert(&memo).unwrap();
    v.delete(&memo.id).unwrap();
    assert!(v.store().get(&memo.id).unwrap().is_none());

    let hist = v.history(Entity::Memo, &memo.id).unwrap();
    assert_eq!(hist.last().unwrap().kind, RevisionKind::Deleted);
    // The deletion itself is not a thing to restore; the version before it is.
    assert!(v.restore(Entity::Memo, &memo.id, hist.last().unwrap()).is_err());

    v.restore(Entity::Memo, &memo.id, &hist[0]).unwrap();
    let back = v.store().get(&memo.id).unwrap().unwrap();
    assert_eq!(back.title, "gone");
    assert_eq!(back.created_at, memo.created_at, "the original creation time comes back");

    fs::remove_dir_all(&dir).ok();
    fs::remove_file(&db).ok();
}

/// What a delete hands back is enough to put the memo, and its photos, straight back.
#[test]
fn undelete_restores_the_memo_it_removed() {
    let dir = temp_dir();
    let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

    let mut memo = Memo::new("shopping", "milk");
    memo.color = "pink".into();
    let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
    v.upsert(&memo).unwrap();
    let photo = v.attach(&memo.id, b"pretend-jpeg", "photo.jpg", "image/jpeg", 4, 3).unwrap();

    let removed = v.delete(&memo.id).unwrap().expect("the memo was there");
    assert!(v.store().get(&memo.id).unwrap().is_none());

    v.undelete(&removed).unwrap();
    let back = v.store().get(&memo.id).unwrap().unwrap();
    assert_eq!(back.title, "shopping");
    assert_eq!(back.body, "milk");
    assert_eq!(back.color, "pink", "everything about it comes back, not just the text");
    assert_eq!(back.created_at, memo.created_at);
    // Attachments point at the memo, not the other way round, so they were never touched.
    let photos = v.store().attachments_of(&memo.id).unwrap();
    assert_eq!(photos.len(), 1);
    assert_eq!(photos[0].id, photo.id);

    // Deleting something that is not there is not an error, and offers nothing to undo.
    assert!(v.delete("no-such-memo").unwrap().is_none());

    fs::remove_dir_all(&dir).ok();
    fs::remove_file(&db).ok();
}

/// Undoing a folder's deletion gathers its contents back up.
#[test]
fn undelete_puts_a_folders_contents_back_into_it() {
    let dir = temp_dir();
    let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

    let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
    let folder = Group::new("work");
    v.upsert_group(&folder).unwrap();
    let mut inside = Memo::new("report", "");
    inside.group_id = folder.id.clone();
    v.upsert(&inside).unwrap();
    let mut moved_away = Memo::new("elsewhere", "");
    moved_away.group_id = folder.id.clone();
    v.upsert(&moved_away).unwrap();

    let removed = v.delete_group(&folder.id).unwrap().expect("the folder was there");
    assert_eq!(v.store().get(&inside.id).unwrap().unwrap().group_id, "", "lifted out");

    // One of them is deliberately put somewhere else before the undo, standing in for a
    // user who reorganised in the meantime — an undo must not drag it back.
    let other = Group::new("home");
    v.upsert_group(&other).unwrap();
    let mut moved_away = v.store().get(&moved_away.id).unwrap().unwrap();
    moved_away.group_id = other.id.clone();
    v.upsert(&moved_away).unwrap();

    v.undelete(&removed).unwrap();
    assert_eq!(v.store().get_group(&folder.id).unwrap().unwrap().name, "work");
    assert_eq!(v.store().get(&inside.id).unwrap().unwrap().group_id, folder.id);
    assert_eq!(
        v.store().get(&moved_away.id).unwrap().unwrap().group_id,
        other.id,
        "a memo moved since the deletion stays where the user put it"
    );

    fs::remove_dir_all(&dir).ok();
    fs::remove_file(&db).ok();
}

/// A removal is a fact about the vault, so it survives a rebuild and can be taken back.
#[test]
fn a_removed_device_is_remembered_and_can_be_taken_back() {
    let dir = temp_dir();
    let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));
    let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();

    assert!(v.revoked_devices().unwrap().is_empty());
    v.revoke_device("GONE-DEVICE").unwrap();
    let revoked = v.revoked_devices().unwrap();
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0].device_id, "GONE-DEVICE");
    assert_eq!(revoked[0].by, v.device_id(), "the deciding device is recorded");
    assert!(revoked[0].at > 0);

    // Saying it twice is not a second change; the callers poll.
    v.revoke_device("GONE-DEVICE").unwrap();
    assert_eq!(v.revoked_devices().unwrap().len(), 1);

    // It comes out of the logs, not out of the cache, so a rebuild keeps it.
    v.rebuild().unwrap();
    assert_eq!(v.revoked_devices().unwrap().len(), 1);
    assert!(!v.is_revoked_here().unwrap(), "this device removed another, not itself");

    // Pairing with it again lifts the removal, and that survives a rebuild too.
    v.unrevoke_device("GONE-DEVICE").unwrap();
    assert!(v.revoked_devices().unwrap().is_empty());
    v.rebuild().unwrap();
    assert!(v.revoked_devices().unwrap().is_empty());

    // A device cannot remove itself; that is what wiping the vault is for.
    let me = v.device_id().to_string();
    assert!(v.revoke_device(&me).is_err());

    fs::remove_dir_all(&dir).ok();
    fs::remove_file(&db).ok();
}

/// Folders have a history too, colour included.
#[test]
fn group_history_follows_renames_and_colours() {
    let dir = temp_dir();
    let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

    let mut group = Group::new("Inbox");
    let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
    v.upsert_group(&group).unwrap();
    group.name = "Archive".into();
    group.color = "green".into();
    v.upsert_group(&group).unwrap();

    let hist = v.history(Entity::Group, &group.id).unwrap();
    assert_eq!(hist.len(), 2);
    assert_eq!(hist[0].field("name"), "Inbox");
    assert_eq!(hist[1].changed, vec!["name".to_string(), "color".to_string()]);

    v.restore(Entity::Group, &group.id, &hist[0]).unwrap();
    let back = v.store().get_group(&group.id).unwrap().unwrap();
    assert_eq!(back.name, "Inbox");
    assert_eq!(back.color, crate::DEFAULT_COLOR);

    fs::remove_dir_all(&dir).ok();
    fs::remove_file(&db).ok();
}

/// A memo's history must not pick up edits that belong to other memos.
#[test]
fn history_ignores_other_memos() {
    let dir = temp_dir();
    let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

    let mine = Memo::new("mine", "");
    let mut other = Memo::new("other", "");
    let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
    v.upsert(&mine).unwrap();
    for i in 0..5 {
        other.body = format!("edit {i}");
        v.upsert(&other).unwrap();
    }

    assert_eq!(v.history(Entity::Memo, &mine.id).unwrap().len(), 1);
    // The first pass through the loop creates it, the other four edit it.
    assert_eq!(v.history(Entity::Memo, &other.id).unwrap().len(), 5);

    fs::remove_dir_all(&dir).ok();
    fs::remove_file(&db).ok();
}

#[test]
fn wrong_password_rejected_by_key_check() {
    let dir = temp_dir();
    Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    // The header canary alone rejects it, even with an empty log.
    assert!(Vault::open(&dir, b"wrong", Store::open_in_memory().unwrap()).is_err());
    fs::remove_dir_all(&dir).ok();
}

/// "Stay unlocked": the cached raw key alone opens the same vault.
#[test]
fn cached_key_opens_vault_without_password() {
    let dir = temp_dir();
    let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

    let memo = Memo::new("seen while unlocked", "body");
    let key_bytes = {
        let mut vault = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
        vault.upsert(&memo).unwrap();
        vault.key_bytes()
    };

    let key = MasterKey::from_bytes(&key_bytes).unwrap();
    let vault = Vault::open_with_key(&dir, key, Store::open(&db).unwrap()).unwrap();
    assert_eq!(vault.store().list().unwrap(), vec![memo]);

    // A bogus key is caught by the header canary.
    let bogus = MasterKey::from_bytes(&[7u8; crate::crypto::KEY_LEN]).unwrap();
    assert!(Vault::open_with_key(&dir, bogus, Store::open_in_memory().unwrap()).is_err());

    fs::remove_dir_all(&dir).ok();
    fs::remove_file(&db).ok();
}

/// The point of automerge: concurrent edits to **different fields** of one memo both
/// survive. The old last-write-wins model dropped one side wholesale.
#[test]
fn concurrent_field_edits_both_survive() {
    let dir = temp_dir();

    // Device A creates the memo.
    let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let base = Memo::new("original title", "original body");
    a.upsert(&base).unwrap();

    // Device B starts from the same state.
    let mut b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    assert_ne!(a.device_id(), b.device_id());
    assert_eq!(b.store().list().unwrap().len(), 1);

    // Concurrent edits: A the title, B the body.
    let mut a_edit = base.clone();
    a_edit.title = "title from A".into();
    a.upsert(&a_edit).unwrap();

    let mut b_edit = base.clone();
    b_edit.body = "body from B".into();
    b.upsert(&b_edit).unwrap();

    // Both sides must converge on the same merge.
    a.rebuild().unwrap();
    b.rebuild().unwrap();
    for v in [&a, &b] {
        let merged = v.store().get(&base.id).unwrap().unwrap();
        assert_eq!(merged.title, "title from A");
        assert_eq!(merged.body, "body from B");
    }

    // One log file per device.
    assert_eq!(fs::read_dir(dir.join(LOGS_DIR)).unwrap().count(), 2);

    fs::remove_dir_all(&dir).ok();
}

/// A rebuild with nothing new is skipped, and one with something new is never skipped.
///
/// The skip is what keeps the merge timer off the UI thread when no change has arrived,
/// and getting it wrong is silent: the vault simply stops seeing the other device. The
/// case that matters is a **local write while another device's change is waiting** —
/// marking the whole log directory merged there would call that change read.
#[test]
fn a_rebuild_skips_nothing_it_has_not_already_read() {
    let dir = std::env::temp_dir().join(format!("ymemo-skip-{}", uuid::Uuid::new_v4()));
    let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let base = Memo::new("from A", "body");
    a.upsert(&base).unwrap();

    let mut b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();

    // Nothing has happened since B read the logs, so this rebuild has nothing to do —
    // and must leave the document it already has alone.
    b.rebuild().unwrap();
    assert_eq!(b.store().list().unwrap().len(), 1);

    // A writes. B has not read it yet.
    let mut edit = base.clone();
    edit.title = "A wrote again".into();
    a.upsert(&edit).unwrap();

    // B writes something of its own *first*. Its own log growing is not news; A's is.
    let mut own = Memo::new("from B", "body");
    own.id = "b-memo".into();
    b.upsert(&own).unwrap();

    b.rebuild().unwrap();
    let merged = b.store().get(&base.id).unwrap().unwrap();
    assert_eq!(merged.title, "A wrote again", "A's change must survive B's own write");
    assert!(b.store().get("b-memo").unwrap().is_some());

    fs::remove_dir_all(&dir).ok();
}

/// A revision is the memo as it stood *after* that change, merges included.
///
/// Two devices editing different fields at once produce two changes that neither has
/// seen the other make. Reading either one on its own shows that device's branch — and
/// the newest revision would then be missing the other's edit, which `restore` writes
/// back field by field: putting the latest version back would silently undo it.
#[test]
fn a_revision_after_a_concurrent_edit_carries_the_merge() {
    let dir = std::env::temp_dir().join(format!("ymemo-conc-{}", uuid::Uuid::new_v4()));
    let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let mut b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();

    let m = Memo::new("start", "body");
    a.upsert(&m).unwrap();
    b.rebuild().unwrap();

    // Neither has seen the other's edit when it makes its own.
    let mut a_edit = m.clone();
    a_edit.title = "A title".into();
    a.upsert(&a_edit).unwrap();
    let mut b_edit = m.clone();
    b_edit.body = "B body".into();
    b.upsert(&b_edit).unwrap();
    a.rebuild().unwrap();
    b.rebuild().unwrap();

    for v in [&mut a, &mut b] {
        let hist = v.history(Entity::Memo, &m.id).unwrap();
        let last = hist.last().unwrap();
        assert_eq!(last.field("title"), "A title", "the newest revision holds A's edit");
        assert_eq!(last.field("body"), "B body", "and B's");
        // It names only what that one change moved. *Which* of the two is last is up to
        // the order the changes merge in and differs between the devices — the field
        // values above are what both agree on, and what `restore` writes back.
        assert_eq!(last.changed.len(), 1);
        assert!(["title", "body"].contains(&last.changed[0].as_str()));
    }

    fs::remove_dir_all(&dir).ok();
}

/// Groups propagate through the logs, and so does a memo's membership.
#[test]
fn groups_sync_across_devices() {
    let dir = temp_dir();

    let group;
    let memo;
    {
        let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        group = Group::new("Work");
        a.upsert_group(&group).unwrap();
        memo = {
            let mut m = Memo::new("report", "");
            m.group_id = group.id.clone();
            m
        };
        a.upsert(&memo).unwrap();
    }

    // Another device with an empty cache restores from the logs.
    let b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let groups = b.store().list_groups().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "Work");
    assert_eq!(b.store().get(&memo.id).unwrap().unwrap().group_id, group.id);

    fs::remove_dir_all(&dir).ok();
}

/// Deleting a group lifts its memos and subgroups instead of destroying them.
#[test]
fn deleting_group_lifts_children_instead_of_destroying() {
    let dir = temp_dir();
    let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();

    let outer = Group::new("outer");
    v.upsert_group(&outer).unwrap();
    let mut inner = Group::new("inner");
    inner.parent_id = outer.id.clone();
    v.upsert_group(&inner).unwrap();
    let mut memo = Memo::new("memo inside", "");
    memo.group_id = outer.id.clone();
    v.upsert(&memo).unwrap();

    v.delete_group(&outer.id).unwrap();

    // Only the outer group is gone; the rest moved to the top level.
    let groups = v.store().list_groups().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].id, inner.id);
    assert_eq!(groups[0].parent_id, "");
    let survived = v.store().get(&memo.id).unwrap().unwrap();
    assert_eq!(survived.group_id, "");

    fs::remove_dir_all(&dir).ok();
}

/// Two devices editing the **same field** keep both edits and agree on the result.
///
/// This is what a memo being `Text` rather than a string buys. A plain string is
/// last-write-wins: measured on two real devices, typing into the same memo inside the
/// twenty seconds it takes to sync made one version simply become the memo, and the other
/// was only findable in the change history. Nothing said so.
///
/// Replacing the *whole* field on both sides, as here, is the worst case and reads oddly —
/// the two versions end up run together. It is still the right trade: odd beats gone, and
/// editing different parts of a real note (the test below) merges cleanly.
#[test]
fn same_field_conflict_converges() {
    let dir = temp_dir();

    let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let base = Memo::new("t", "");
    a.upsert(&base).unwrap();

    let mut b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();

    let mut a_edit = base.clone();
    a_edit.body = "from A".into();
    a.upsert(&a_edit).unwrap();
    let mut b_edit = base.clone();
    b_edit.body = "from B".into();
    b.upsert(&b_edit).unwrap();

    a.rebuild().unwrap();
    b.rebuild().unwrap();
    let ta = a.store().get(&base.id).unwrap().unwrap().body;
    let tb = b.store().get(&base.id).unwrap().unwrap().body;
    assert_eq!(ta, tb, "both devices must agree");
    assert!(ta.contains("from A"), "A's edit survived: {ta:?}");
    assert!(tb.contains("from B"), "B's edit survived: {tb:?}");

    fs::remove_dir_all(&dir).ok();
}
/// Two devices adding a line each to the same note end up with both lines.
///
/// The everyday shape of the case above: nobody replaces a whole note, they add to it.
#[test]
fn edits_in_different_places_merge_cleanly() {
    let dir = temp_dir();
    let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let base = Memo::new("list", "milk\nbread\n");
    a.upsert(&base).unwrap();
    let mut b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();

    // A adds to the end, B to the front; neither has seen the other.
    let mut a_edit = base.clone();
    a_edit.body = "milk\nbread\napples\n".into();
    a.upsert(&a_edit).unwrap();
    let mut b_edit = base.clone();
    b_edit.body = "eggs\nmilk\nbread\n".into();
    b.upsert(&b_edit).unwrap();

    a.rebuild().unwrap();
    b.rebuild().unwrap();
    let ba = a.store().get(&base.id).unwrap().unwrap().body;
    let bb = b.store().get(&base.id).unwrap().unwrap().body;
    assert_eq!(ba, bb, "both devices must agree");
    for line in ["milk", "bread", "apples", "eggs"] {
        assert!(ba.contains(line), "{line:?} survived the merge: {ba:?}");
    }

    fs::remove_dir_all(&dir).ok();
}

/// A **label** edited on two devices at once settles on one somebody chose.
///
/// The other half of the rule in `put_text_if_changed`: a folder's name and a memo's
/// title are replaced whole, not added to, so they stay last-write-wins. Merging them
/// character by character turned "Work" and "Home" into **"WHorkme"** — both edits
/// technically kept, and a folder nobody named that the user now has to repair. The one
/// that lost is still in the change history.
#[test]
fn two_devices_renaming_one_folder_settle_on_a_real_name() {
    let dir = temp_dir();
    let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let mut g = Group::new("Inbox");
    g.id = "g".into();
    a.upsert_group(&g).unwrap();
    let mut b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();

    let mut ga = g.clone();
    ga.name = "Work".into();
    a.upsert_group(&ga).unwrap();
    let mut gb = g.clone();
    gb.name = "Home".into();
    b.upsert_group(&gb).unwrap();

    a.rebuild().unwrap();
    b.rebuild().unwrap();
    let na = a.store().get_group("g").unwrap().unwrap().name;
    let nb = b.store().get_group("g").unwrap().unwrap().name;
    assert_eq!(na, nb, "both devices must agree");
    assert!(na == "Work" || na == "Home", "a name somebody chose, not a merge: {na:?}");

    fs::remove_dir_all(&dir).ok();
}

/// A delete on one device beats an edit on the other, and both agree it is gone.
///
/// Not an accident of the merge — worth pinning, because the alternative (an edit
/// resurrecting a memo somebody deleted) is the more surprising of the two. What was
/// typed is still in the change history, and the delete itself is what the undo bar
/// hands back.
#[test]
fn a_delete_beats_an_edit_made_at_the_same_time() {
    let dir = temp_dir();
    let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let m = Memo::new("keep", "line one\n");
    a.upsert(&m).unwrap();
    let mut b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();

    a.delete(&m.id).unwrap();
    let mut edit = m.clone();
    edit.body = "line one\nline two\n".into();
    b.upsert(&edit).unwrap();

    a.rebuild().unwrap();
    b.rebuild().unwrap();
    assert!(a.store().get(&m.id).unwrap().is_none());
    assert!(b.store().get(&m.id).unwrap().is_none(), "both devices agree it is gone");

    fs::remove_dir_all(&dir).ok();
}

/// Every shape of edit the writing can take must land on exactly the text handed over.
///
/// `splice_changed_span` writes a span rather than the whole body, so a wrong offset here
/// does not fail loudly — it quietly corrupts a memo. Korean is in the list because the
/// span is measured in characters and scanned in bytes, and a syllable is three bytes.
#[test]
fn every_kind_of_edit_lands_on_the_text_it_was_given() {
    let cases: &[(&str, &str)] = &[
        ("", "hello"),
        ("hello", ""),
        ("hello", "hello world"),
        ("hello", "say hello"),
        ("hello", "heXllo"),
        ("hello world", "hello"),
        ("hello world", "world"),
        ("hello", "goodbye"),
        ("aaaa", "aaaaa"),
        ("aaaaa", "aaaa"),
        ("abcabc", "abcXabc"),
        ("line one\nline two\n", "line one\nline two\nline three\n"),
        ("line one\nline two\n", "line one\n"),
        // Multi-byte, including an edit inside a run of identical syllables.
        ("장보기", "장보기\n우유"),
        ("장보기\n우유", "장보기\n계란\n우유"),
        ("가가가", "가가나가"),
        ("한글", "한"),
        ("한", "한글"),
        ("émoji 🎉", "émoji 🎉🎉"),
        ("🎉🎉", "🎉"),
        ("a🎉b", "ab"),
        // The ends match but the middle is unrelated.
        ("start MIDDLE end", "start OTHER end"),
    ];

    for (from, to) in cases {
        let mut doc = AutoCommit::new();
        let obj = doc.put_object(&ROOT, "m", ObjType::Map).unwrap();
        put_text_if_changed(&mut doc, &obj, "body", from).unwrap();
        put_text_if_changed(&mut doc, &obj, "body", to).unwrap();
        let got = match doc.get(&obj, "body").unwrap() {
            Some((Value::Object(ObjType::Text), t)) => doc.text(&t).unwrap(),
            other => panic!("{from:?} -> {to:?}: not a text object: {other:?}"),
        };
        assert_eq!(&got.as_str(), to, "{from:?} -> {to:?}");
    }
}

/// The same, on text nobody would type, so an off-by-one has somewhere to show itself.
#[test]
fn a_thousand_random_edits_land_on_the_text_they_were_given() {
    let alphabet: Vec<char> = "ab가나🎉\n".chars().collect();
    // A cheap deterministic generator: this needs to be reproducible when it fails.
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };

    let mut doc = AutoCommit::new();
    let obj = doc.put_object(&ROOT, "m", ObjType::Map).unwrap();
    let mut body = String::new();
    put_text_if_changed(&mut doc, &obj, "body", &body).unwrap();

    for _ in 0..1000 {
        let chars: Vec<char> = body.chars().collect();
        let at = if chars.is_empty() { 0 } else { next(chars.len() + 1) };
        let mut edited: String = chars[..at].iter().collect();
        if next(3) == 0 && at < chars.len() {
            // Delete a stretch.
            let len = 1 + next(chars.len() - at);
            edited.extend(&chars[at + len..]);
        } else {
            // Insert a stretch.
            for _ in 0..1 + next(5) {
                edited.push(alphabet[next(alphabet.len())]);
            }
            edited.extend(&chars[at..]);
        }

        put_text_if_changed(&mut doc, &obj, "body", &edited).unwrap();
        let got = match doc.get(&obj, "body").unwrap() {
            Some((Value::Object(ObjType::Text), t)) => doc.text(&t).unwrap(),
            other => panic!("not a text object: {other:?}"),
        };
        assert_eq!(got, edited, "after editing {body:?}");
        body = edited;
    }
}

/// Opening and closing the room a photo stands in, on every shape of note.
#[test]
fn a_gap_opens_and_closes_where_it_was_asked_to() {
    // In the middle.
    assert_eq!(open_gap("a\nb\nc", 1, 2), "a\n\n\nb\nc");
    assert_eq!(close_gap("a\n\n\nb\nc", 1), "a\nb\nc");
    // At the very top.
    assert_eq!(open_gap("a\nb", 0, 1), "\na\nb");
    assert_eq!(close_gap("\na\nb", 0), "a\nb");
    // At the end, where the body has no newline of its own to sit after.
    assert_eq!(open_gap("a\nb", 2, 2), "a\nb\n\n\n");
    assert_eq!(open_gap("a\nb\n", 2, 2), "a\nb\n\n\n");
    // An empty note.
    assert_eq!(open_gap("", 0, 2), "\n\n");
    // Closing takes only the blank run, never the writing under it.
    assert_eq!(close_gap("a\n\n\nb", 1), "a\nb");
    assert_eq!(close_gap("a\nb\nc", 1), "a\nb\nc", "nothing blank there to take");
    // Written in since: what was typed stays, the blank lines around it go.
    assert_eq!(close_gap("a\n\nhello\n\nb", 1), "a\nhello\n\nb");
    // Round trip is exact anywhere inside the writing.
    let body = "one\ntwo\nthree";
    for at in 0..3 {
        assert_eq!(close_gap(&open_gap(body, at, 3), at), body, "at line {at}");
    }
    // Past the last line it is exact but for the line break the gap needed to sit after,
    // which is left behind as an empty last line. Worth knowing rather than hiding: it is
    // one keystroke to the user and the alternative is guessing which newlines were ours.
    assert_eq!(close_gap(&open_gap(body, 3, 3), 3), "one\ntwo\nthree\n");
}

/// A photo standing in the writing stays next to the same words when the note is edited.
#[test]
fn a_photo_in_the_writing_moves_with_the_words_above_it() {
    let dir = temp_dir();
    let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let mut memo = Memo::new("list", "one\ntwo\nthree\n");
    v.upsert(&memo).unwrap();
    let a = v.attach(&memo.id, b"pretend-jpeg", "p.jpg", "image/jpeg", 4, 3).unwrap();

    // Into the writing, after two lines.
    v.place_attachment_in_writing(&a.id, 2, 3).unwrap();
    let body = v.store().get(&memo.id).unwrap().unwrap().body;
    assert_eq!(body, "one\ntwo\n\n\n\nthree\n", "three blank lines make the room");
    assert_eq!(v.store().get_attachment(&a.id).unwrap().unwrap().anchor_line, 2);

    // Writing a line **above** it moves the photo down with the words.
    memo.body = format!("zero\n{body}");
    v.upsert(&memo).unwrap();
    assert_eq!(v.store().get_attachment(&a.id).unwrap().unwrap().anchor_line, 3);

    // Writing **below** it leaves it alone.
    let cur = v.store().get(&memo.id).unwrap().unwrap().body;
    memo.body = format!("{cur}four\n");
    v.upsert(&memo).unwrap();
    assert_eq!(v.store().get_attachment(&a.id).unwrap().unwrap().anchor_line, 3);

    // Taking a line away above it brings it back up.
    let cur = v.store().get(&memo.id).unwrap().unwrap().body;
    memo.body = cur.strip_prefix("zero\n").unwrap().to_string();
    v.upsert(&memo).unwrap();
    assert_eq!(v.store().get_attachment(&a.id).unwrap().unwrap().anchor_line, 2);

    // And out again: the room closes behind it.
    v.take_attachment_out_of_writing(&a.id, crate::PhotoMode::Float).unwrap();
    assert_eq!(v.store().get(&memo.id).unwrap().unwrap().body, "one\ntwo\nthree\nfour\n");
    assert_eq!(v.store().get_attachment(&a.id).unwrap().unwrap().mode(), crate::PhotoMode::Float);

    fs::remove_dir_all(&dir).ok();
}

/// An edit that splits a line above a photo still counts as being above it.
///
/// The off-by-one that hid here is the difference between counting lines and counting the
/// breaks between them: pressing return in the middle of a line above the picture left the
/// picture where it was while the writing moved down a line past it.
#[test]
fn opening_a_line_above_a_photo_moves_it_down() {
    let dir = temp_dir();
    let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let mut memo = Memo::new("list", "one\ntwo\nthree\n");
    v.upsert(&memo).unwrap();
    let a = v.attach(&memo.id, b"pretend-jpeg", "p.jpg", "image/jpeg", 4, 3).unwrap();
    v.place_attachment_in_writing(&a.id, 2, 2).unwrap();
    assert_eq!(v.store().get_attachment(&a.id).unwrap().unwrap().anchor_line, 2);

    // Return pressed in the middle of "two", which is the line right above the picture.
    let body = v.store().get(&memo.id).unwrap().unwrap().body;
    memo.body = body.replacen("two", "tw\no", 1);
    v.upsert(&memo).unwrap();
    assert_eq!(v.store().get_attachment(&a.id).unwrap().unwrap().anchor_line, 3);

    fs::remove_dir_all(&dir).ok();
}

/// Korean above a photo must not panic the re-anchoring.
///
/// The edit is compared byte by byte, and where two strings part company is as likely to
/// be inside a syllable as between two — slicing there is a panic, so nothing slices.
#[test]
fn an_edit_that_parts_inside_a_syllable_is_safe() {
    let dir = temp_dir();
    let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let mut memo = Memo::new("목록", "장보기\n우유\n계란\n");
    v.upsert(&memo).unwrap();
    let a = v.attach(&memo.id, b"pretend-jpeg", "p.jpg", "image/jpeg", 4, 3).unwrap();
    v.place_attachment_in_writing(&a.id, 1, 2).unwrap();

    // "장보기" -> "장바구니": the two part company inside the second syllable.
    let body = v.store().get(&memo.id).unwrap().unwrap().body;
    memo.body = body.replacen("장보기", "장바구니\n더", 1);
    v.upsert(&memo).unwrap();
    assert_eq!(v.store().get_attachment(&a.id).unwrap().unwrap().anchor_line, 2);

    fs::remove_dir_all(&dir).ok();
}

/// Moving a photo that is already in the writing must not leave its old room behind.
#[test]
fn moving_a_photo_within_the_writing_takes_its_room_with_it() {
    let dir = temp_dir();
    let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let memo = Memo::new("list", "one\ntwo\nthree\n");
    v.upsert(&memo).unwrap();
    let a = v.attach(&memo.id, b"pretend-jpeg", "p.jpg", "image/jpeg", 4, 3).unwrap();

    v.place_attachment_in_writing(&a.id, 1, 2).unwrap();
    v.place_attachment_in_writing(&a.id, 2, 2).unwrap();
    let body = v.store().get(&memo.id).unwrap().unwrap().body;
    assert_eq!(body, "one\ntwo\n\n\nthree\n", "one room, not two");

    fs::remove_dir_all(&dir).ok();
}

/// A label a build from the middle of this change wrote as a text object still reads, and
/// turns back into a plain string the next time it is saved.
///
/// The body became a text object and the title and folder name briefly went with it, before
/// the folder rename above showed why they should not have. So a real vault can hold labels
/// in either shape, and it will keep holding them until every one of them is next edited —
/// there is no migration pass, and there should not be one, since an unasked-for write to
/// every memo is a sync conflict looking for somewhere to happen.
#[test]
fn a_label_left_as_a_text_object_reads_and_converts_on_the_next_save() {
    let dir = temp_dir();
    let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let m = Memo::new("Groceries", "milk\n");
    a.upsert(&m).unwrap();
    let mut g = Group::new("Work");
    g.id = "g".into();
    a.upsert_group(&g).unwrap();

    // Rewrite both labels in the shape that build left behind.
    let memos = a.memos_obj().unwrap();
    let memo_obj = match a.doc.get(&memos, &m.id).unwrap() {
        Some((Value::Object(ObjType::Map), id)) => id,
        other => panic!("no memo map: {other:?}"),
    };
    put_text_if_changed(&mut a.doc, &memo_obj, "title", "Groceries").unwrap();
    let groups = a.groups_obj().unwrap();
    let group_obj = match a.doc.get(&groups, "g").unwrap() {
        Some((Value::Object(ObjType::Map), id)) => id,
        other => panic!("no group map: {other:?}"),
    };
    put_text_if_changed(&mut a.doc, &group_obj, "name", "Work").unwrap();
    a.append_local_change().unwrap();
    a.rebuild().unwrap();

    assert_eq!(a.store().get(&m.id).unwrap().unwrap().title, "Groceries");
    assert_eq!(a.store().get_group("g").unwrap().unwrap().name, "Work");

    // And a device that has only ever seen the log reads them the same way.
    let b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    assert_eq!(b.store().get(&m.id).unwrap().unwrap().title, "Groceries");
    assert_eq!(b.store().get_group("g").unwrap().unwrap().name, "Work");

    // Saving over it puts the label back to a plain string, so the next rename is decided
    // by who wrote last rather than blended.
    let mut renamed = m.clone();
    renamed.title = "Shopping".into();
    a.upsert(&renamed).unwrap();
    let mut g2 = g.clone();
    g2.name = "Home".into();
    a.upsert_group(&g2).unwrap();
    assert!(
        matches!(a.doc.get(&memo_obj, "title").unwrap(), Some((Value::Scalar(_), _))),
        "a saved title is a plain string again"
    );
    assert!(
        matches!(a.doc.get(&group_obj, "name").unwrap(), Some((Value::Scalar(_), _))),
        "a saved folder name is a plain string again"
    );
    assert_eq!(a.store().get(&m.id).unwrap().unwrap().title, "Shopping");
    assert_eq!(a.store().get_group("g").unwrap().unwrap().name, "Home");

    fs::remove_dir_all(&dir).ok();
}

/// A group must survive the cache being rebuilt from the logs — the merge timer does that
/// every few seconds, so anything it drops disappears while the user is looking at it.
#[test]
fn rebuild_keeps_groups() {
    let dir = std::env::temp_dir().join(format!("ymemo-rebuild-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(dir.join("cache.db")).unwrap();
    let mut v = Vault::open_or_create(dir.join("vault"), b"pw", store).unwrap();

    let group = crate::Group::new("work");
    v.upsert_group(&group).unwrap();
    assert_eq!(v.store().list_groups().unwrap().len(), 1, "just created");

    v.rebuild().unwrap();
    let after = v.store().list_groups().unwrap();
    assert_eq!(after.len(), 1, "the group vanished when the cache was rebuilt");
    assert_eq!(after[0].name, "work");

    std::fs::remove_dir_all(&dir).ok();
}

/// Removing a photo hands it back, and putting it back restores it exactly — the room in the
/// writing included, which the removal closes rather than leaving as a blank gap.
#[test]
fn a_removed_photo_can_be_put_back_room_and_all() {
    let dir = temp_dir();
    let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
    let memo = Memo::new("list", "one\ntwo\nthree\n");
    v.upsert(&memo).unwrap();
    let a = v.attach(&memo.id, b"pretend-jpeg", "p.jpg", "image/jpeg", 4, 3).unwrap();
    let floating = v.attach(&memo.id, b"another", "q.jpg", "image/jpeg", 4, 3).unwrap();
    v.place_attachment_in_writing(&a.id, 2, 3).unwrap();
    let with_room = v.store().get(&memo.id).unwrap().unwrap().body;

    let removed = v.remove_attachment(&a.id).unwrap().expect("it was there");
    assert_eq!(removed.rows, 3);
    assert_eq!(v.store().get(&memo.id).unwrap().unwrap().body, "one\ntwo\nthree\n", "room closed");
    assert!(v.store().get_attachment(&a.id).unwrap().is_none());

    v.restore_attachment(&removed).unwrap();
    assert_eq!(v.store().get(&memo.id).unwrap().unwrap().body, with_room, "room reopened");
    let back = v.store().get_attachment(&a.id).unwrap().unwrap();
    assert_eq!(back.mode(), crate::PhotoMode::Inline);
    assert_eq!(back.anchor_line, 2);
    assert_eq!(back.hash, a.hash);

    // A floating photo has no room: removed and back, the body never moves.
    let removed = v.remove_attachment(&floating.id).unwrap().unwrap();
    assert_eq!(removed.rows, 0);
    v.restore_attachment(&removed).unwrap();
    assert_eq!(v.store().get(&memo.id).unwrap().unwrap().body, with_room);
    assert_eq!(v.store().get_attachment(&floating.id).unwrap().unwrap().x_permille, floating.x_permille);

    // Something already gone is nothing to hand back.
    v.detach(&floating.id).unwrap();
    assert!(v.remove_attachment(&floating.id).unwrap().is_none());
    fs::remove_dir_all(&dir).ok();
}
