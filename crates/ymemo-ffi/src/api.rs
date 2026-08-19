//! The API exposed to Flutter.
//!
//! flutter_rust_bridge v2 generates Dart from the public functions and structs here. Dart
//! calls in from several threads, so the vault sits behind a global Mutex (a rusqlite
//! Connection is not Sync).
//!
//! Transport (Syncthing) is missing: on mobile it will arrive as a gomobile `.aar`. Merging
//! already lives in the core, so once the files land, `sync_rebuild` picks them up.

use std::sync::Mutex;

use anyhow::{anyhow, Result};
use ymemo_core::{now_millis, pairing::PairingCode, vault::Vault, Attachment, Group, Memo, Store};
use ymemo_i18n::t;

/// The open vault; one per app process.
static VAULT: Mutex<Option<Vault>> = Mutex::new(None);

fn with_vault<T>(f: impl FnOnce(&mut Vault) -> Result<T>) -> Result<T> {
    let mut guard = VAULT.lock().map_err(|_| anyhow!(t!("core.vault_lock_poisoned")))?;
    let vault = guard.as_mut().ok_or_else(|| anyhow!(t!("core.vault_not_open")))?;
    f(vault)
}

/// A memo as handed to Dart; same fields as the core `Memo`.
pub struct FfiMemo {
    pub id: String,
    pub title: String,
    pub body: String,
    pub color: String,
    pub opacity: i64,
    pub group_id: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<Memo> for FfiMemo {
    fn from(m: Memo) -> Self {
        Self {
            id: m.id,
            title: m.title,
            body: m.body,
            color: m.color,
            opacity: m.opacity,
            group_id: m.group_id,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

/// A photo attachment as handed to Dart.
///
/// The bytes are not included: photos run to megabytes and carrying them in every list
/// would be waste. Fetch them by hash with [`attachment_bytes`] when drawing.
pub struct FfiAttachment {
    pub id: String,
    pub memo_id: String,
    pub hash: String,
    pub name: String,
    pub mime: String,
    /// Original pixel size for the aspect ratio; 0 when unknown.
    pub width_px: i64,
    pub height_px: i64,
    /// Display width in 1/1000 em; pixels = value / 1000 * the platform's base font px.
    pub width_em_milli: i64,
    pub created_at: i64,
}

impl From<Attachment> for FfiAttachment {
    fn from(a: Attachment) -> Self {
        Self {
            id: a.id,
            memo_id: a.memo_id,
            hash: a.hash,
            name: a.name,
            mime: a.mime,
            width_px: a.width_px,
            height_px: a.height_px,
            width_em_milli: a.width_em_milli,
            created_at: a.created_at,
        }
    }
}

/// A group (folder) as handed to Dart.
pub struct FfiGroup {
    pub id: String,
    pub name: String,
    pub parent_id: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<Group> for FfiGroup {
    fn from(g: Group) -> Self {
        Self {
            id: g.id,
            name: g.name,
            parent_id: g.parent_id,
            created_at: g.created_at,
            updated_at: g.updated_at,
        }
    }
}

/// Sets the language of core error messages (`"ko"`, `"en"`, or a locale like `"ko-KR"`).
///
/// An unknown value falls back to the system locale. Call it once before the other APIs,
/// and again whenever the language changes, or the screen ends up mixing languages.
pub fn set_language(code: String) {
    let lang = ymemo_i18n::Lang::parse(&code).unwrap_or_else(ymemo_i18n::system_lang);
    ymemo_i18n::set_lang(lang);
}

/// Language code currently used for core messages.
pub fn language() -> String {
    ymemo_i18n::lang().code().to_string()
}

/// The mobile UI strings in the current language; Dart fetches these once at startup.
///
/// Keeping them in Dart instead would split the languages — catalog for core errors,
/// hardcoded for the screens. Reading the keys **in Rust** also puts mobile strings under
/// `ymemo-i18n`'s "every key used in code exists in the catalog" test. After
/// [`set_language`], fetch this again.
pub struct FfiStrings {
    pub add_photo: String,
    pub body_hint: String,
    pub camera_error: String,
    pub close: String,
    pub list_title: String,
    pub master_password: String,
    pub new_memo: String,
    pub opening: String,
    pub photo_camera: String,
    pub photo_gallery: String,
    pub photo_missing: String,
    pub photo_remove: String,
    pub photo_size: String,
    pub save: String,
    pub scan_hint: String,
    pub scan_pairing_unavailable: String,
    pub scan_qr: String,
    pub scan_result: String,
    pub sync_now: String,
    pub title_hint: String,
    pub unlock: String,
}

/// Collects the mobile strings for the current language.
pub fn mobile_strings() -> FfiStrings {
    FfiStrings {
        add_photo: t!("mobile.add_photo"),
        body_hint: t!("mobile.body_hint"),
        camera_error: t!("mobile.camera_error"),
        close: t!("mobile.close"),
        list_title: t!("mobile.list_title"),
        master_password: t!("mobile.master_password"),
        new_memo: t!("mobile.new_memo"),
        opening: t!("mobile.opening"),
        photo_camera: t!("mobile.photo_camera"),
        photo_gallery: t!("mobile.photo_gallery"),
        photo_missing: t!("mobile.photo_missing"),
        photo_remove: t!("mobile.photo_remove"),
        photo_size: t!("mobile.photo_size"),
        save: t!("mobile.save"),
        scan_hint: t!("mobile.scan_hint"),
        scan_pairing_unavailable: t!("mobile.scan_pairing_unavailable"),
        scan_qr: t!("mobile.scan_qr"),
        scan_result: t!("mobile.scan_result"),
        sync_now: t!("mobile.sync_now"),
        title_hint: t!("mobile.title_hint"),
        unlock: t!("mobile.unlock"),
    }
}

/// Opens the vault, creating it if needed. `vault_dir` is the synced directory,
/// `cache_db_path` the device-local SQLite file.
pub fn vault_open(vault_dir: String, cache_db_path: String, password: String) -> Result<()> {
    let store = Store::open(&cache_db_path)?;
    let vault = Vault::open_or_create(&vault_dir, password.as_bytes(), store)?;
    *VAULT.lock().map_err(|_| anyhow!(t!("core.vault_lock_poisoned")))? = Some(vault);
    Ok(())
}

/// Closes the vault (log out).
pub fn vault_close() -> Result<()> {
    *VAULT.lock().map_err(|_| anyhow!(t!("core.vault_lock_poisoned")))? = None;
    Ok(())
}

/// Memos, most recently updated first.
pub fn memo_list() -> Result<Vec<FfiMemo>> {
    with_vault(|v| Ok(v.store().list()?.into_iter().map(FfiMemo::from).collect()))
}

/// Creates (`id` = None) or updates (`id` = Some) a memo and returns its id.
pub fn memo_upsert(id: Option<String>, title: String, body: String) -> Result<String> {
    with_vault(|v| {
        let memo = match id {
            Some(id) => {
                let mut memo = v
                    .store()
                    .get(&id)?
                    .ok_or_else(|| anyhow!(t!("core.memo_not_found", id = id)))?;
                memo.title = title;
                memo.body = body;
                memo.updated_at = now_millis();
                memo
            }
            None => Memo::new(title, body),
        };
        v.upsert(&memo)?;
        Ok(memo.id)
    })
}

/// Deletes a memo.
pub fn memo_delete(id: String) -> Result<()> {
    with_vault(|v| v.delete(&id))
}

/// Sets the palette key.
pub fn memo_set_color(id: String, color: String) -> Result<()> {
    with_vault(|v| {
        let mut memo = v
            .store()
            .get(&id)?
            .ok_or_else(|| anyhow!(t!("core.memo_not_found", id = id)))?;
        memo.color = color;
        memo.updated_at = now_millis();
        v.upsert(&memo)
    })
}

/// Sets the opacity in percent; the core clamps out-of-range values.
pub fn memo_set_opacity(id: String, opacity: i64) -> Result<()> {
    with_vault(|v| {
        let mut memo = v
            .store()
            .get(&id)?
            .ok_or_else(|| anyhow!(t!("core.memo_not_found", id = id)))?;
        memo.opacity = opacity;
        memo.updated_at = now_millis();
        v.upsert(&memo)
    })
}

/// Moves a memo into a group; an empty `group_id` moves it to the top level.
pub fn memo_set_group(id: String, group_id: String) -> Result<()> {
    with_vault(|v| {
        let mut memo = v
            .store()
            .get(&id)?
            .ok_or_else(|| anyhow!(t!("core.memo_not_found", id = id)))?;
        memo.group_id = group_id;
        memo.updated_at = now_millis();
        v.upsert(&memo)
    })
}

/// Photos on one memo, in the order they were added.
pub fn attachment_list(memo_id: String) -> Result<Vec<FfiAttachment>> {
    with_vault(|v| {
        Ok(v.store()
            .attachments_of(&memo_id)?
            .into_iter()
            .map(FfiAttachment::from)
            .collect())
    })
}

/// Attaches a photo; `data` is the original file bytes.
///
/// Dart decodes and passes `width_px`/`height_px`, so the core needs no image decoder. Pass
/// 0 when unknown and the aspect ratio falls back to 1:1.
pub fn attachment_add(
    memo_id: String,
    data: Vec<u8>,
    name: String,
    mime: String,
    width_px: i64,
    height_px: i64,
) -> Result<FfiAttachment> {
    with_vault(|v| {
        Ok(v.attach(&memo_id, &data, &name, &mime, width_px, height_px)?
            .into())
    })
}

/// Photo bytes. Errors while the blob has not synced yet; draw a placeholder instead.
pub fn attachment_bytes(hash: String) -> Result<Vec<u8>> {
    with_vault(|v| v.attachment_bytes(&hash))
}

/// Whether the photo has arrived on this device; do not ask for bytes if it has not.
pub fn attachment_has_blob(hash: String) -> Result<bool> {
    with_vault(|v| Ok(v.has_blob(&hash)))
}

/// Sets the display width in 1/1000 em; other devices see the same proportion.
pub fn attachment_set_width(id: String, width_em_milli: i64) -> Result<()> {
    with_vault(|v| v.set_attachment_width(&id, width_em_milli))
}

/// Detaches a photo; the blob file stays (no GC).
pub fn attachment_remove(id: String) -> Result<()> {
    with_vault(|v| v.detach(&id))
}

/// All groups, sorted by name.
pub fn group_list() -> Result<Vec<FfiGroup>> {
    with_vault(|v| Ok(v.store().list_groups()?.into_iter().map(FfiGroup::from).collect()))
}

/// Creates a group and returns its id.
pub fn group_create(name: String, parent_id: String) -> Result<String> {
    with_vault(|v| {
        let mut group = Group::new(name);
        group.parent_id = parent_id;
        v.upsert_group(&group)?;
        Ok(group.id)
    })
}

/// Renames a group.
pub fn group_rename(id: String, name: String) -> Result<()> {
    with_vault(|v| {
        let mut group = v
            .store()
            .get_group(&id)?
            .ok_or_else(|| anyhow!(t!("core.group_not_found", id = id)))?;
        group.name = name;
        group.updated_at = now_millis();
        v.upsert_group(&group)
    })
}

/// Moves a group under another; moving it into its own subtree is rejected.
pub fn group_move(id: String, parent_id: String) -> Result<()> {
    with_vault(|v| {
        let groups = v.store().list_groups()?;
        if !parent_id.is_empty() && ymemo_core::is_descendant(&groups, &parent_id, &id) {
            return Err(anyhow!(t!("core.group_cycle")));
        }
        let mut group = v
            .store()
            .get_group(&id)?
            .ok_or_else(|| anyhow!(t!("core.group_not_found", id = id)))?;
        group.parent_id = parent_id;
        group.updated_at = now_millis();
        v.upsert_group(&group)
    })
}

/// Deletes a group; its memos and subgroups move up instead of being deleted.
pub fn group_delete(id: String) -> Result<()> {
    with_vault(|v| v.delete_group(&id))
}

/// Merges the other devices' logs into the local state; call it after the transport has
/// delivered new logs.
pub fn sync_rebuild() -> Result<()> {
    with_vault(|v| v.rebuild())
}

/// Validates a scanned pairing code and extracts the device id. Until mobile Syncthing
/// lands, this is only used to check and display it.
pub fn pairing_decode(code: String) -> Result<String> {
    Ok(PairingCode::decode(&code)?.syncthing_device_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vault is process-global, so tests that open one would clobber each other in
    /// parallel; they take this lock instead. A poisoned lock is reused as-is, since it only
    /// isolates tests.
    static VAULT_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock_vault_for_test() -> std::sync::MutexGuard<'static, ()> {
        VAULT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Round-trip over the FFI surface: open, upsert, list, update, delete, close. It is one
    /// test because the vault is global.
    #[test]
    fn ffi_surface_roundtrip() {
        let _guard = lock_vault_for_test();
        let base = std::env::temp_dir().join(format!("ymemo-ffi-{}", uuid_like()));
        std::fs::create_dir_all(&base).unwrap();
        let vault_dir = base.join("vault");
        let db = base.join("cache.db");

        vault_open(
            vault_dir.to_string_lossy().into(),
            db.to_string_lossy().into(),
            "pw".into(),
        )
        .unwrap();

        let id = memo_upsert(None, "mobile memo".into(), "body".into()).unwrap();
        assert_eq!(memo_list().unwrap().len(), 1);

        memo_upsert(Some(id.clone()), "edited".into(), "body".into()).unwrap();
        let all = memo_list().unwrap();
        assert_eq!(all[0].title, "edited");
        assert_eq!(all[0].id, id);

        sync_rebuild().unwrap();
        assert_eq!(memo_list().unwrap().len(), 1);

        memo_delete(id).unwrap();
        assert!(memo_list().unwrap().is_empty());

        vault_close().unwrap();
        // Once closed, calls must fail with the catalog's message — a hardcoded string would
        // be caught here.
        for code in ["en", "ko"] {
            set_language(code.into());
            let lang = ymemo_i18n::Lang::parse(code).unwrap();
            let Err(e) = memo_list() else {
                panic!("closed vault still listed memos");
            };
            assert_eq!(e.to_string(), ymemo_i18n::raw(lang, "core.vault_not_open").unwrap());
        }

        // Reopen: restored from the logs, empty because the delete is in there too.
        vault_open(
            vault_dir.to_string_lossy().into(),
            db.to_string_lossy().into(),
            "pw".into(),
        )
        .unwrap();
        assert!(memo_list().unwrap().is_empty());
        vault_close().unwrap();

        std::fs::remove_dir_all(&base).ok();
    }

    /// Attachment surface: add, list, resize, read bytes, remove.
    #[test]
    fn attachment_surface_roundtrip() {
        let _guard = lock_vault_for_test();
        let base = std::env::temp_dir().join(format!("ymemo-ffi-att-{}", uuid_like()));
        std::fs::create_dir_all(&base).unwrap();
        vault_open(
            base.join("vault").to_string_lossy().into(),
            base.join("cache.db").to_string_lossy().into(),
            "pw".into(),
        )
        .unwrap();

        let memo_id = memo_upsert(None, "photo memo".into(), "".into()).unwrap();
        let photo = b"fake jpeg bytes".repeat(20);
        let a = attachment_add(
            memo_id.clone(),
            photo.clone(),
            "p.jpg".into(),
            "image/jpeg".into(),
            1200,
            800,
        )
        .unwrap();
        assert!(attachment_has_blob(a.hash.clone()).unwrap());
        assert_eq!(attachment_bytes(a.hash.clone()).unwrap(), photo);

        let listed = attachment_list(memo_id.clone()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].width_em_milli, ymemo_core::DEFAULT_WIDTH_EM_MILLI);

        attachment_set_width(a.id.clone(), 6_000).unwrap();
        assert_eq!(attachment_list(memo_id.clone()).unwrap()[0].width_em_milli, 6_000);

        attachment_remove(a.id).unwrap();
        assert!(attachment_list(memo_id).unwrap().is_empty());

        vault_close().unwrap();
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn pairing_decode_works() {
        assert_eq!(
            pairing_decode("YMEMO1:ABC-DEF-1234".into()).unwrap(),
            "ABC-DEF-1234"
        );
        assert!(pairing_decode("!!".into()).is_err());
    }

    fn uuid_like() -> String {
        format!("{}-{}", std::process::id(), now_millis())
    }
}
