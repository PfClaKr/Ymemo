//! The API exposed to Flutter.
//!
//! flutter_rust_bridge v2 generates Dart from the public functions and structs here. Dart
//! calls in from several threads, so the vault sits behind a global Mutex (a rusqlite
//! Connection is not Sync).
//!
//! Transport (Syncthing) runs as the same bundled child process as on the desktop; see the
//! "Transport" section near the bottom for what mobile does differently.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use ymemo_core::{
    lan_pair, now_millis,
    pairing::{self, PairingCode},
    sync::{Syncthing, VAULT_FOLDER_ID},
    vault::Vault,
    Attachment, Group, Memo, Store,
};
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
    /// Top-left corner on the note, in per-mille of the note area (0..=1000 across and down).
    pub x_permille: i64,
    pub y_permille: i64,
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
            x_permille: a.x_permille,
            y_permille: a.y_permille,
            created_at: a.created_at,
        }
    }
}

/// A group (folder) as handed to Dart.
pub struct FfiGroup {
    pub id: String,
    pub name: String,
    pub parent_id: String,
    /// Palette key, the same set memos use; the core never turns it into a real color.
    pub color: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<Group> for FfiGroup {
    fn from(g: Group) -> Self {
        Self {
            id: g.id,
            name: g.name,
            parent_id: g.parent_id,
            color: g.color,
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
    pub connected: String,
    pub copied: String,
    pub copy: String,
    pub disconnected: String,
    pub lan_connect: String,
    pub lan_done: String,
    pub lan_enter_code: String,
    pub lan_my_code: String,
    pub lan_not_found: String,
    pub lan_pairing: String,
    pub lan_searching: String,
    pub days_unit: String,
    pub language: String,
    pub language_auto: String,
    pub lock_now: String,
    pub lock_on_background: String,
    pub lock_on_background_hint: String,
    pub lock_section: String,
    pub saved: String,
    pub settings: String,
    pub unlock_days: String,
    pub unlock_days_hint: String,
    pub update_available: String,
    pub update_check: String,
    pub update_check_hint: String,
    pub update_checking: String,
    pub update_latest: String,
    pub update_now: String,
    pub update_open: String,
    pub update_section: String,
    pub version: String,
    pub cancel: String,
    pub delete: String,
    pub delete_group_hint: String,
    pub empty_folder: String,
    pub folder_name: String,
    pub move_to: String,
    pub new_group: String,
    pub ok: String,
    pub rename: String,
    pub root_folder: String,
    pub list_title: String,
    pub master_password: String,
    pub my_code: String,
    pub new_memo: String,
    pub no_devices: String,
    pub opening: String,
    pub photo_camera: String,
    pub photo_gallery: String,
    pub photo_missing: String,
    pub photo_remove: String,
    pub photo_size: String,
    pub save: String,
    pub scan_hint: String,
    pub scan_qr: String,
    pub sync_devices: String,
    pub sync_now: String,
    pub sync_starting: String,
    pub sync_unavailable: String,
    pub title_hint: String,
    pub unlock: String,
    pub unpair: String,

    // Colors, and the master password / recovery code screens. The `msg.*` ones are shared
    // word for word with the desktop, which raises them from Rust.
    pub color: String,
    pub change_password: String,
    pub confirm_password: String,
    pub create_vault: String,
    pub current_password: String,
    pub forgot_password: String,
    pub issue_recovery: String,
    pub new_password: String,
    pub new_vault_hint: String,
    pub no_recovery: String,
    pub password_changed: String,
    pub password_hint: String,
    pub password_mismatch: String,
    pub recovery_absent: String,
    pub recovery_ack: String,
    pub recovery_code: String,
    pub recovery_hint: String,
    pub recovery_issued: String,
    pub recovery_present: String,
    pub recovery_prompt: String,
    pub recovery_warning: String,
    pub reissue_recovery: String,
    pub reset_done: String,
    pub reset_password: String,
    pub reset_vault: String,
    pub reset_vault_confirm: String,
    pub reset_vault_hint: String,
    pub security_section: String,

    // Incoming and outgoing pairing requests.
    pub allow: String,
    pub device_id: String,
    pub pair_cancel_wait: String,
    pub pair_connected: String,
    pub pair_request: String,
    pub pair_request_hint: String,
    pub pair_verification: String,
    pub pair_verify: String,
    pub pair_waiting: String,
    pub pair_waiting_hint: String,
    pub reject: String,
}

/// Collects the mobile strings for the current language.
pub fn mobile_strings() -> FfiStrings {
    FfiStrings {
        add_photo: t!("mobile.add_photo"),
        body_hint: t!("mobile.body_hint"),
        camera_error: t!("mobile.camera_error"),
        connected: t!("mobile.connected"),
        copied: t!("mobile.copied"),
        copy: t!("mobile.copy"),
        disconnected: t!("mobile.disconnected"),
        lan_connect: t!("mobile.lan_connect"),
        lan_done: t!("mobile.lan_done"),
        lan_enter_code: t!("mobile.lan_enter_code"),
        lan_my_code: t!("mobile.lan_my_code"),
        lan_not_found: t!("mobile.lan_not_found"),
        lan_pairing: t!("mobile.lan_pairing"),
        lan_searching: t!("mobile.lan_searching"),
        days_unit: t!("mobile.days_unit"),
        language: t!("mobile.language"),
        language_auto: t!("mobile.language_auto"),
        lock_now: t!("mobile.lock_now"),
        lock_on_background: t!("mobile.lock_on_background"),
        lock_on_background_hint: t!("mobile.lock_on_background_hint"),
        lock_section: t!("mobile.lock_section"),
        saved: t!("mobile.saved"),
        settings: t!("mobile.settings"),
        unlock_days: t!("mobile.unlock_days"),
        unlock_days_hint: t!("mobile.unlock_days_hint"),
        update_available: t!("mobile.update_available"),
        update_check: t!("mobile.update_check"),
        update_check_hint: t!("mobile.update_check_hint"),
        update_checking: t!("mobile.update_checking"),
        update_latest: t!("mobile.update_latest"),
        update_now: t!("mobile.update_now"),
        update_open: t!("mobile.update_open"),
        update_section: t!("mobile.update_section"),
        version: t!("mobile.version"),
        cancel: t!("mobile.cancel"),
        delete: t!("mobile.delete"),
        delete_group_hint: t!("mobile.delete_group_hint"),
        empty_folder: t!("mobile.empty_folder"),
        folder_name: t!("mobile.folder_name"),
        move_to: t!("mobile.move_to"),
        new_group: t!("mobile.new_group"),
        ok: t!("mobile.ok"),
        rename: t!("mobile.rename"),
        root_folder: t!("mobile.root_folder"),
        list_title: t!("mobile.list_title"),
        master_password: t!("mobile.master_password"),
        my_code: t!("mobile.my_code"),
        new_memo: t!("mobile.new_memo"),
        no_devices: t!("mobile.no_devices"),
        opening: t!("mobile.opening"),
        photo_camera: t!("mobile.photo_camera"),
        photo_gallery: t!("mobile.photo_gallery"),
        photo_missing: t!("mobile.photo_missing"),
        photo_remove: t!("mobile.photo_remove"),
        photo_size: t!("mobile.photo_size"),
        save: t!("mobile.save"),
        scan_hint: t!("mobile.scan_hint"),
        scan_qr: t!("mobile.scan_qr"),
        sync_devices: t!("mobile.sync_devices"),
        sync_now: t!("mobile.sync_now"),
        sync_starting: t!("mobile.sync_starting"),
        sync_unavailable: t!("mobile.sync_unavailable"),
        title_hint: t!("mobile.title_hint"),
        unlock: t!("mobile.unlock"),
        unpair: t!("mobile.unpair"),

        color: t!("mobile.color"),
        change_password: t!("mobile.change_password"),
        confirm_password: t!("mobile.confirm_password"),
        create_vault: t!("mobile.create_vault"),
        current_password: t!("mobile.current_password"),
        forgot_password: t!("mobile.forgot_password"),
        issue_recovery: t!("mobile.issue_recovery"),
        new_password: t!("mobile.new_password"),
        new_vault_hint: t!("mobile.new_vault_hint"),
        no_recovery: t!("mobile.no_recovery"),
        password_changed: t!("msg.password_changed"),
        password_hint: t!("mobile.password_hint"),
        password_mismatch: t!("mobile.password_mismatch"),
        recovery_absent: t!("mobile.recovery_absent"),
        recovery_ack: t!("mobile.recovery_ack"),
        recovery_code: t!("mobile.recovery_code"),
        recovery_hint: t!("mobile.recovery_hint"),
        recovery_issued: t!("msg.recovery_issued"),
        recovery_present: t!("mobile.recovery_present"),
        recovery_prompt: t!("mobile.recovery_prompt"),
        recovery_warning: t!("mobile.recovery_warning"),
        reissue_recovery: t!("mobile.reissue_recovery"),
        reset_done: t!("msg.reset_done"),
        reset_password: t!("mobile.reset_password"),
        reset_vault: t!("mobile.reset_vault"),
        reset_vault_confirm: t!("mobile.reset_vault_confirm"),
        reset_vault_hint: t!("mobile.reset_vault_hint"),
        security_section: t!("mobile.security_section"),

        allow: t!("mobile.allow"),
        device_id: t!("mobile.device_id"),
        pair_cancel_wait: t!("mobile.pair_cancel_wait"),
        pair_connected: t!("mobile.pair_connected"),
        pair_request: t!("mobile.pair_request"),
        pair_request_hint: t!("mobile.pair_request_hint"),
        pair_verification: t!("mobile.pair_verification"),
        pair_verify: t!("mobile.pair_verify"),
        pair_waiting: t!("mobile.pair_waiting"),
        pair_waiting_hint: t!("mobile.pair_waiting_hint"),
        reject: t!("mobile.reject"),
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

// ===========================================================================
// Master password and recovery code
// ===========================================================================
//
// The core does all of this by rewriting `vault.json`'s wrapper alone — no log and no blob
// is touched — so every call here is two Argon2id runs at worst and nothing to show progress
// for. See `ymemo_core::vault` for why that is safe.

/// Whether `vault_dir` already holds a vault.
///
/// The lock screen asks before anything is unlocked, to tell "set a password" from "enter
/// the password" — and to know that the vault it just created is the one to show a recovery
/// code for.
pub fn vault_exists(vault_dir: String) -> bool {
    Path::new(&vault_dir).join("vault.json").exists()
}

/// Whether the vault at `vault_dir` has a recovery code, without opening it.
pub fn vault_has_recovery_code(vault_dir: String) -> bool {
    ymemo_core::vault::recovery_code_exists(&vault_dir)
}

/// Replaces the master password of the open vault, after checking the current one.
pub fn vault_change_password(current: String, new_password: String) -> Result<()> {
    with_vault(|v| v.change_password(current.as_bytes(), new_password.as_bytes()))
}

/// Issues a fresh recovery code, retiring any earlier one, and returns it.
///
/// **The only time the code is readable.** Only its Argon2id wrapper is stored, so a code
/// that is not written down is gone.
pub fn vault_issue_recovery_code() -> Result<String> {
    with_vault(|v| v.issue_recovery_code())
}

/// Sets a new master password from the recovery code, for a vault nobody can unlock.
///
/// Takes the directory rather than an open vault, because the whole point is that it cannot
/// be opened. Unlock with the new password afterwards; the recovery code stays valid.
pub fn vault_reset_password_with_recovery(
    vault_dir: String,
    code: String,
    new_password: String,
) -> Result<()> {
    ymemo_core::vault::reset_password_with_recovery(&vault_dir, &code, new_password.as_bytes())
}

/// Deletes this device's vault and its cache: the way out of a forgotten password when there
/// is no recovery code either.
///
/// **Unsharing comes first and is not optional.** Syncthing propagates deletions, so emptying
/// a folder it still carries would delete the memos on every paired device too. If the folder
/// cannot be released, nothing is deleted at all. The daemon keeps running: it is still this
/// device's identity, and the vault created next registers a folder under the same id.
pub fn vault_reset(vault_dir: String, cache_db_path: String) -> Result<()> {
    // Scoped so the sync lock is released before the vault lock is taken; the two are
    // reached from different Dart threads and one consistent order is what keeps that safe.
    {
        let guard = sync_lock()?;
        if let Some(st) = guard.as_ref() {
            // Wrapped so the message says *nothing was deleted*: a bare REST error here
            // reads like the wipe half-happened, which is the one thing it never does.
            st.remove_folder(VAULT_FOLDER_ID)
                .map_err(|_| anyhow!(t!("msg.unshare_before_reset")))?;
        }
    }

    ymemo_core::vault::wipe(&vault_dir)?;
    // The cache is a plaintext copy of everything the vault held, so it goes with it.
    let db = Path::new(&cache_db_path);
    if db.exists() {
        std::fs::remove_file(db)?;
    }
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

/// Sets where the photo sits on the note and how wide it is, in one write.
///
/// The position is a fraction of the note (per-mille) rather than pixels, so a photo dropped
/// two thirds of the way down a phone screen is two thirds of the way down a desktop sticky
/// too. Both are clamped by the core.
pub fn attachment_set_layout(
    id: String,
    x_permille: i64,
    y_permille: i64,
    width_em_milli: i64,
) -> Result<()> {
    with_vault(|v| v.set_attachment_layout(&id, x_permille, y_permille, width_em_milli))
}

/// Detaches a photo; the blob file stays (no GC).
pub fn attachment_remove(id: String) -> Result<()> {
    with_vault(|v| v.detach(&id))
}

/// All groups, sorted by name.
pub fn group_list() -> Result<Vec<FfiGroup>> {
    with_vault(|v| Ok(v.store().list_groups()?.into_iter().map(FfiGroup::from).collect()))
}

/// The folders directly inside `parent_id` (`""` for the top level), name-sorted.
///
/// The resolution is the core's, not Dart's, so the same defence applies on both platforms:
/// a group whose ancestry loops — which CRDTs allow, since a concurrent move on two devices
/// keeps both parents — or whose parent is gone surfaces at the top level instead of being
/// unreachable. Dart walking `parent_id` itself could loop forever on exactly that case.
pub fn group_children(parent_id: String) -> Result<Vec<FfiGroup>> {
    with_vault(|v| {
        let groups = v.store().list_groups()?;
        Ok(ymemo_core::group_children(&groups)
            .remove(&parent_id)
            .unwrap_or_default()
            .into_iter()
            .map(FfiGroup::from)
            .collect())
    })
}

/// The memos in one folder, newest first.
///
/// A memo whose group has been deleted elsewhere is shown at the **top level** rather than
/// nowhere, which is what the desktop list does too. Anything else would read as data loss.
pub fn memos_in_group(group_id: String) -> Result<Vec<FfiMemo>> {
    with_vault(|v| {
        let known: std::collections::HashSet<String> =
            v.store().list_groups()?.into_iter().map(|g| g.id).collect();
        let at_root = group_id.is_empty();
        Ok(v.store()
            .list()?
            .into_iter()
            .filter(|m| {
                if known.contains(&m.group_id) {
                    m.group_id == group_id
                } else {
                    at_root // no group, or one that is gone
                }
            })
            .map(FfiMemo::from)
            .collect())
    })
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

/// Sets a group's palette key. Folders carry a color just like memos and sync it the same
/// way, so the two UIs agree on what a folder looks like.
pub fn group_set_color(id: String, color: String) -> Result<()> {
    with_vault(|v| {
        let mut group = v
            .store()
            .get_group(&id)?
            .ok_or_else(|| anyhow!(t!("core.group_not_found", id = id)))?;
        group.color = color;
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

// ===========================================================================
// Transport (Syncthing)
// ===========================================================================
//
// Same daemon as the desktop, same core code driving it (`ymemo_core::sync`); only the way
// it is started differs. Android may execute a binary **only** from the app's native library
// directory, so the app ships syncthing as `libsyncthing.so` in `jniLibs/` and hands the path
// down here — there is no `find_binary()` guessing on mobile, and none is wanted.
//
// **The daemon runs only while the app is in the foreground.** Dart starts it on resume and
// stops it on pause. Android freezes background processes anyway, so pretending otherwise
// would only cost battery and a permanent notification; a memo app can sync while it is
// open. (If the process is killed outright, `PR_SET_PDEATHSIG` in the core takes the daemon
// with it, so nothing is left running.)

/// The running daemon, one per app process, like [`VAULT`].
static SYNC: Mutex<Option<Syncthing>> = Mutex::new(None);

fn sync_lock() -> Result<std::sync::MutexGuard<'static, Option<Syncthing>>> {
    SYNC.lock().map_err(|_| anyhow!(t!("core.vault_lock_poisoned")))
}

fn with_sync<T>(f: impl FnOnce(&Syncthing) -> Result<T>) -> Result<T> {
    let guard = sync_lock()?;
    let st = guard.as_ref().ok_or_else(|| anyhow!(t!("core.sync_not_running")))?;
    f(st)
}

/// Another device sharing this vault.
pub struct FfiSharedDevice {
    /// Syncthing device id — also what unpairing takes.
    pub id: String,
    /// Name the peer announced; empty when it has none.
    pub name: String,
    pub connected: bool,
}

/// Starts the daemon and registers the vault directory, returning this device's pairing code.
///
/// Idempotent, which is what makes it safe to call on every resume. The first start generates
/// the device key and can take a few seconds; flutter_rust_bridge runs this off the UI thread
/// on its own.
pub fn sync_start(binary_path: String, home_dir: String, vault_dir: String) -> Result<String> {
    let mut guard = sync_lock()?;

    // Holding a handle is not the same as having a daemon: Android kills backgrounded child
    // processes under memory pressure, and nothing tells us when it does. Ask the daemon
    // something, and if it does not answer, drop it and start a fresh one — otherwise the app
    // would report sync as running until it was restarted by hand.
    if let Some(st) = guard.as_ref() {
        match st.device_id() {
            Ok(id) => return Ok(PairingCode::new(&id).encode()),
            Err(_) => *guard = None,
        }
    }

    // Sync starts before the vault is unlocked, so this may be a device where the directory
    // does not exist yet; syncthing is being pointed at it either way.
    std::fs::create_dir_all(&vault_dir)?;
    let st = Syncthing::spawn(Path::new(&binary_path), Path::new(&home_dir))?;
    st.ensure_folder(VAULT_FOLDER_ID, "Ymemo Vault", Path::new(&vault_dir))?;
    let id = st.device_id()?;
    *guard = Some(st);
    Ok(PairingCode::new(&id).encode())
}

/// Re-registers the vault directory with the running daemon.
///
/// [`sync_start`] does this on its first run and then short-circuits, so a vault created
/// after [`vault_reset`] — which removes the folder on purpose — would sit there unshared
/// until the app was next restarted. Doing nothing when the daemon is down is correct: the
/// next `sync_start` registers it anyway.
pub fn sync_ensure_folder(vault_dir: String) -> Result<()> {
    let guard = sync_lock()?;
    let Some(st) = guard.as_ref() else { return Ok(()) };
    st.ensure_folder(VAULT_FOLDER_ID, "Ymemo Vault", Path::new(&vault_dir))
}

/// Stops the daemon. Safe to call when it is not running.
pub fn sync_stop() -> Result<()> {
    // Dropping it shuts the daemon down over REST, then kills it if it will not go.
    *sync_lock()? = None;
    Ok(())
}

/// Whether the daemon is up. Cheap: it does not talk to it.
pub fn sync_running() -> bool {
    sync_lock().map(|g| g.is_some()).unwrap_or(false)
}

/// This device's pairing code (`YMEMO1:<device-id>`), for the other device to scan or type.
pub fn sync_pairing_code() -> Result<String> {
    with_sync(|st| Ok(PairingCode::new(&st.device_id()?).encode()))
}

/// Pairs with a scanned or typed code: registers the peer and shares the vault with it.
///
/// Returns the peer's device id, which the screen then waits on: this side is now dialling a
/// device that has never heard of it, so nothing syncs until the **other** device allows the
/// request in (`sync_pending_devices` there). It no longer has to be given this device's code
/// by hand — that was the second scan nobody remembered to do.
pub fn sync_pair_with(code: String) -> Result<String> {
    let peer = PairingCode::decode(&code)?.syncthing_device_id;
    with_sync(|st| st.share_folder_with(VAULT_FOLDER_ID, &peer))?;
    Ok(peer)
}

/// Devices this vault is shared with, ourselves excluded.
pub fn sync_devices() -> Result<Vec<FfiSharedDevice>> {
    with_sync(|st| {
        Ok(st
            .shared_devices(VAULT_FOLDER_ID)?
            .into_iter()
            .map(|d| FfiSharedDevice { id: d.id, name: d.name, connected: d.connected })
            .collect())
    })
}

/// Drops a peer. Only this side stops syncing; the other device keeps its own entry until it
/// unpairs too.
pub fn sync_unpair(device_id: String) -> Result<()> {
    with_sync(|st| st.unshare_folder_with(VAULT_FOLDER_ID, &device_id))
}

// ===========================================================================
// Incoming pairing requests
// ===========================================================================
//
// The half of pairing that used to be missing. A device that scans this one's code adds it
// and starts dialling; Syncthing here refuses the unknown caller and files it as a pending
// device, which is what turns up in `sync_pending_devices`. Allowing one is the same
// `share_folder_with` the scan side already did, in the other direction.

/// Requests refused during this run of the app.
///
/// Syncthing does not remember a refusal — the caller keeps retrying and is filed again —
/// so the answer is kept here instead. Deliberately in memory and deliberately **not** in
/// the `Syncthing` handle: mobile stops the daemon every time the app is backgrounded, and
/// a refusal that expired on the walk back to the app would be no refusal at all. Starting
/// the app again clears it, so a mis-tapped "reject" is never permanent.
static REJECTED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn rejected_lock() -> Result<std::sync::MutexGuard<'static, Option<HashSet<String>>>> {
    REJECTED.lock().map_err(|_| anyhow!(t!("core.vault_lock_poisoned")))
}

/// A device asking to be let in.
pub struct FfiPendingDevice {
    /// Syncthing device id; the handle for approving or rejecting.
    pub id: String,
    /// The name it announced. **It chose this itself**, so the UI must show it as a hint
    /// next to the id and never in place of one.
    pub name: String,
    /// The eight characters the other device is showing on its own screen right now. The
    /// user comparing the two is what makes an approval safe; see `ymemo_core::pairing`.
    pub verification_code: String,
}

/// Requests waiting for an answer, oldest first, minus the ones already rejected.
pub fn sync_pending_devices() -> Result<Vec<FfiPendingDevice>> {
    let rejected = rejected_lock()?.clone().unwrap_or_default();
    with_sync(|st| {
        let my_id = st.device_id()?;
        Ok(st
            .pending_devices()?
            .into_iter()
            .filter(|d| !rejected.contains(&d.id))
            .map(|d| FfiPendingDevice {
                verification_code: pairing::verification_code(&my_id, &d.id),
                id: d.id,
                name: d.name,
            })
            .collect())
    })
}

/// Allows a device in: shares the vault with it, completing the link.
///
/// Syncthing drops the pending entry as soon as the device is in the config, so there is
/// nothing to clear afterwards.
pub fn sync_approve_device(device_id: String) -> Result<()> {
    // An id that was rejected and then approved must not stay filtered out of the list.
    if let Some(set) = rejected_lock()?.as_mut() {
        set.remove(&device_id);
    }
    with_sync(|st| st.share_folder_with(VAULT_FOLDER_ID, &device_id))
}

/// Turns a device away and stops asking about it for the rest of this run.
pub fn sync_reject_device(device_id: String) -> Result<()> {
    rejected_lock()?.get_or_insert_with(HashSet::new).insert(device_id.clone());
    // Best effort: the local answer is what actually silences the prompt, and a daemon that
    // has already gone away has no list to clear.
    let _ = with_sync(|st| st.dismiss_pending_device(&device_id));
    Ok(())
}

/// The verification code to show while waiting for `peer_device_id` to allow this device in.
///
/// The same eight characters the other side sees on its approval prompt, derived from the
/// two device ids alone.
pub fn sync_verification_code(peer_device_id: String) -> Result<String> {
    let peer = PairingCode::decode(&peer_device_id)?.syncthing_device_id;
    with_sync(|st| Ok(pairing::verification_code(&st.device_id()?, &peer)))
}

// ===========================================================================
// LAN pairing (the 6-digit code)
// ===========================================================================
//
// Two devices on one network swap ids over a rotating 6-digit code instead of a scanned or
// typed 63-character one (`ymemo_core::lan_pair`). Unlike the QR path this finishes **both**
// halves: whichever side answers registers the other, so nobody has to go and do a second
// step on the other device.
//
// The listener runs only while the pairing screen is open. That is what the core intends by
// "pairing mode", and on a phone it also keeps a UDP socket and a wifi multicast lock from
// sitting there all day.

/// The pairing-mode listener, alive only while the screen is open.
static LAN: Mutex<Option<lan_pair::PairListener>> = Mutex::new(None);

fn lan_lock() -> Result<std::sync::MutexGuard<'static, Option<lan_pair::PairListener>>> {
    LAN.lock().map_err(|_| anyhow!(t!("core.vault_lock_poisoned")))
}

/// Registers a peer learnt over LAN with the running daemon.
///
/// Kept separate so the LAN lock is never held while the sync lock is taken — the two are
/// touched from a Dart timer and a Dart button at the same time, and one consistent order is
/// what keeps that from deadlocking.
fn share_with_peer(peer_id: &str) -> Result<()> {
    with_sync(|st| st.share_folder_with(VAULT_FOLDER_ID, peer_id))
}

/// Enters pairing mode and returns the code to show. Needs the daemon, since the code's whole
/// purpose is to hand over its device id.
pub fn lan_start() -> Result<String> {
    let device_id = with_sync(|st| st.device_id())?;
    let mut guard = lan_lock()?;
    if guard.is_none() {
        *guard = Some(lan_pair::PairListener::start(device_id)?);
    }
    Ok(guard.as_ref().expect("just started").code())
}

/// The code currently on offer. It rotates every minute, so the screen re-reads it.
pub fn lan_code() -> Result<Option<String>> {
    Ok(lan_lock()?.as_ref().map(|l| l.code()))
}

/// Leaves pairing mode: the socket closes and the code stops being answered.
pub fn lan_stop() -> Result<()> {
    *lan_lock()? = None;
    Ok(())
}

/// Picks up devices that paired with **us** (they typed our code) and shares the vault with
/// each. Returns their ids, for the screen to report. Poll it while pairing mode is on.
pub fn lan_poll_paired() -> Result<Vec<String>> {
    let peers: Vec<String> = {
        let guard = lan_lock()?;
        let Some(listener) = guard.as_ref() else { return Ok(Vec::new()) };
        std::iter::from_fn(|| listener.next_paired_peer()).collect()
    };
    for peer in &peers {
        share_with_peer(peer)?;
    }
    Ok(peers)
}

/// Pairs with the device showing `code`: broadcasts for it, and on an answer shares the vault
/// with it. `None` means nobody answered in time — a wrong or expired code, or a network that
/// drops broadcasts.
///
/// Blocks for up to `timeout_secs`; flutter_rust_bridge keeps that off the UI thread.
pub fn lan_join(code: String, timeout_secs: u64) -> Result<Option<String>> {
    let device_id = with_sync(|st| st.device_id())?;
    let found = lan_pair::join(&code, &device_id, std::time::Duration::from_secs(timeout_secs))?;
    if let Some(peer) = &found {
        share_with_peer(peer)?;
    }
    Ok(found)
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

        // Moving and resizing at once is one call, and comes back on the next list.
        attachment_set_layout(a.id.clone(), 250, 750, 12_000).unwrap();
        let placed = &attachment_list(memo_id.clone()).unwrap()[0];
        assert_eq!((placed.x_permille, placed.y_permille), (250, 750));
        assert_eq!(placed.width_em_milli, 12_000);

        attachment_remove(a.id).unwrap();
        assert!(attachment_list(memo_id).unwrap().is_empty());

        vault_close().unwrap();
        std::fs::remove_dir_all(&base).ok();
    }

    /// Rejecting is the local answer, so it must not depend on the daemon being up.
    ///
    /// It runs with no daemon here, which is exactly the case the "best effort" dismissal
    /// exists for: an app that was backgrounded between the prompt and the tap still has to
    /// record the refusal, or the request would come straight back.
    #[test]
    fn rejecting_works_without_a_running_daemon() {
        let id = format!("TESTDEV-{}", uuid_like());
        sync_reject_device(id.clone()).unwrap();
        assert!(rejected_lock().unwrap().as_ref().is_some_and(|s| s.contains(&id)));

        // Approving the same device has to lift the refusal, or a change of mind would leave
        // it filtered out of the list forever.
        let _ = sync_approve_device(id.clone());
        assert!(!rejected_lock().unwrap().as_ref().is_some_and(|s| s.contains(&id)));
    }

    fn uuid_like() -> String {
        format!("{}-{}", std::process::id(), now_millis())
    }
}

// ===========================================================================
// Device-local settings
// ===========================================================================
//
// The mobile counterpart of the desktop's `settings.json`, and the same rule applies: this is
// **device-local and never synced**. It lives in the app's private directory, not in the vault
// directory, because a synced preference would mean one device deciding another's language.
//
// Dart passes the path rather than the app deriving one: the platform directories are the
// Flutter side's business, exactly as they are for the vault.

/// Preferences as Dart sees them. Every field has a default, so a file written by an older
/// version still loads.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct FfiSettings {
    /// `"auto"` (system locale), `"ko"` or `"en"`.
    pub lang: String,
    /// Days the vault reopens without the password after one unlock; 0 asks every time.
    pub unlock_days: i32,
    /// Close the vault when the app leaves the foreground.
    pub lock_on_background: bool,
    /// Ask GitHub about newer releases. The app's only outbound request.
    pub update_check: bool,
    /// When that last happened (epoch millis), so it is not asked on every start.
    pub last_update_check: i64,
}

impl Default for FfiSettings {
    fn default() -> Self {
        Self {
            lang: "auto".into(),
            unlock_days: 0,
            lock_on_background: true,
            update_check: true,
            last_update_check: 0,
        }
    }
}

impl FfiSettings {
    /// Clamps everything on the way in and out, so a hand-edited file cannot put the app in a
    /// state its own UI could not produce.
    fn sanitize(&mut self) {
        if self.lang != "auto" && ymemo_i18n::Lang::parse(&self.lang).is_none() {
            self.lang = "auto".into();
        }
        self.unlock_days = self.unlock_days.clamp(0, UNLOCK_DAYS_MAX);
        if self.last_update_check < 0 || self.last_update_check > now_millis() {
            self.last_update_check = 0;
        }
    }
}

/// Longest stay-unlocked window, matching the desktop's.
pub const UNLOCK_DAYS_MAX: i32 = 365;

/// Reads the settings; a missing or damaged file gives the defaults rather than an error,
/// since preferences must never be what stops the app from starting.
pub fn settings_load(path: String) -> FfiSettings {
    let mut settings = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<FfiSettings>(&bytes).ok())
        .unwrap_or_default();
    settings.sanitize();
    settings
}

/// Writes the settings back, sanitized. Returns the values as stored, so the screen can show
/// what was actually kept.
pub fn settings_save(path: String, settings: FfiSettings) -> Result<FfiSettings> {
    let mut settings = settings;
    settings.sanitize();
    std::fs::write(&path, serde_json::to_vec_pretty(&settings)?)?;
    Ok(settings)
}

// ===========================================================================
// Staying unlocked
// ===========================================================================
//
// The key derived from the master password, handed out so the caller can keep it and reopen
// the vault without asking again. **While a copy of that key exists, the password buys
// nothing** — whoever can read it can read the memos. That is the inherent cost of "stay
// unlocked", the same one the desktop pays; on a phone the copy belongs in the platform's
// keystore, and `unlock_days = 0` (ask every time) stays a supported answer.

/// The open vault's key, for caching. Only meaningful right after an unlock.
pub fn vault_key() -> Result<Vec<u8>> {
    with_vault(|v| Ok(v.key_bytes().to_vec()))
}

/// Reopens the vault with a cached key instead of the password.
///
/// This **skips the divergent-key healing** that `vault_open` does, because healing needs the
/// password to re-derive old keys. A vault that has diverged therefore fails here; the caller
/// is expected to drop the cached key and ask for the password, which is exactly what the
/// desktop does.
pub fn vault_open_with_key(vault_dir: String, cache_db_path: String, key: Vec<u8>) -> Result<()> {
    let bytes: [u8; ymemo_core::crypto::KEY_LEN] = key
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!(t!("core.session_key_bad")))?;
    let store = Store::open(&cache_db_path)?;
    let vault = Vault::open_with_key(&vault_dir, ymemo_core::crypto::MasterKey::from_bytes(&bytes)?, store)?;
    *VAULT.lock().map_err(|_| anyhow!(t!("core.vault_lock_poisoned")))? = Some(vault);
    Ok(())
}

// ===========================================================================
// Update check
// ===========================================================================

/// A release newer than the running build.
pub struct FfiRelease {
    pub version: String,
    /// Where the update button goes: the apk built for this device's ABI when the release
    /// carries one, the release page otherwise. The app opens it and installs nothing itself.
    pub url: String,
    /// File name behind [`FfiRelease::url`], empty when it is the release page. Shown so the
    /// user can see it is the apk for their phone and not one of the other two.
    pub file: String,
}

/// The running build's version — the same number the release tag carries, since CI checks
/// the two against each other. Shown in settings, where it is what a bug report needs.
pub fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Asks GitHub whether there is a newer release. `None` means this build is current.
///
/// The **only** request the app makes to anyone's server, which is why it sits behind a
/// setting; see `ymemo_core::update` for what it does and does not send.
pub fn update_check() -> Result<Option<FfiRelease>> {
    Ok(
        ymemo_core::update::check(env!("CARGO_PKG_VERSION"))?.map(|r| {
            let url = r.download_url().to_string();
            FfiRelease { version: r.version, url, file: r.asset_name }
        }),
    )
}
