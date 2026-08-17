//! Flutter 에 노출하는 API.
//!
//! flutter_rust_bridge v2 는 이 모듈의 공개 함수/구조체에서 Dart 코드를 생성한다.
//! Dart 쪽은 스레드를 넘나들며 호출하므로 vault 는 전역 Mutex 로 감싼다
//! (rusqlite Connection 은 Sync 가 아님).
//!
//! 동기화(Syncthing)는 모바일에선 gomobile `.aar` 번들로 별도 처리 예정이라
//! 여기엔 아직 없다. 로그 병합(rebuild)은 이미 코어에 있으므로, 파일만 도착하면
//! `sync_rebuild` 로 반영된다.

use std::sync::Mutex;

use anyhow::{anyhow, Result};
use ymemo_core::{now_millis, pairing::PairingCode, vault::Vault, Attachment, Group, Memo, Store};
use ymemo_i18n::t;

/// 열린 vault (앱 프로세스당 하나).
static VAULT: Mutex<Option<Vault>> = Mutex::new(None);

fn with_vault<T>(f: impl FnOnce(&mut Vault) -> Result<T>) -> Result<T> {
    let mut guard = VAULT.lock().map_err(|_| anyhow!(t!("core.vault_lock_poisoned")))?;
    let vault = guard.as_mut().ok_or_else(|| anyhow!(t!("core.vault_not_open")))?;
    f(vault)
}

/// Dart 로 넘기는 메모 표현. (코어 `Memo` 와 동일 필드; frb 가 Dart 클래스로 변환)
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

/// Dart 로 넘기는 첨부 사진 표현.
///
/// 바이트는 들어 있지 않다 — 사진은 수 MB 라 목록마다 실어 나르면 낭비다.
/// 그릴 때 [`attachment_bytes`] 로 해시를 주고 따로 받는다.
pub struct FfiAttachment {
    pub id: String,
    pub memo_id: String,
    pub hash: String,
    pub name: String,
    pub mime: String,
    /// 원본 픽셀 크기 (비율 계산용, 모르면 0).
    pub width_px: i64,
    pub height_px: i64,
    /// 표시 너비 (em 의 1/1000). 실제 픽셀 = 이 값/1000 × 이 플랫폼 기본 폰트 px.
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

/// Dart 로 넘기는 그룹(폴더) 표현.
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

/// 코어가 돌려주는 에러 메시지의 언어를 정한다 (`"ko"` / `"en"`, `"ko-KR"` 같은 로캘도 됨).
///
/// 모르는 값이면 시스템 로캘로 추정한다. Dart 쪽 UI 문구는 Flutter 가 따로 관리하지만,
/// 코어 에러는 이 함수로 맞춰야 화면에서 언어가 섞이지 않는다. 다른 API 를 부르기 전에
/// 한 번 호출하면 되고, 언어를 바꿀 때마다 다시 부르면 된다.
pub fn set_language(code: String) {
    let lang = ymemo_i18n::Lang::parse(&code).unwrap_or_else(ymemo_i18n::system_lang);
    ymemo_i18n::set_lang(lang);
}

/// 지금 쓰이는 코어 메시지 언어 코드.
pub fn language() -> String {
    ymemo_i18n::lang().code().to_string()
}

/// 모바일 UI 문구 한 벌 (현재 언어). Dart 가 시작할 때 한 번 받아 들고 쓴다.
///
/// 문구를 Dart 에 따로 두면 언어가 갈라진다 — 코어 에러는 카탈로그, 화면 문구는
/// 하드코딩이 되어 한쪽만 번역된다. 키를 **Rust 에서** 읽어 넘기므로 `ymemo-i18n` 의
/// "코드가 쓰는 키가 카탈로그에 있는지" 테스트가 모바일 문구까지 함께 지켜 준다.
/// (언어를 바꾼 뒤엔 [`set_language`] 를 부르고 이걸 다시 받으면 된다.)
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

/// 현재 언어의 모바일 문구를 모아 돌려준다.
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

/// vault 열기(없으면 생성). `vault_dir` 는 동기화 대상 디렉터리,
/// `cache_db_path` 는 기기 로컬 SQLite 파일 경로.
pub fn vault_open(vault_dir: String, cache_db_path: String, password: String) -> Result<()> {
    let store = Store::open(&cache_db_path)?;
    let vault = Vault::open_or_create(&vault_dir, password.as_bytes(), store)?;
    *VAULT.lock().map_err(|_| anyhow!(t!("core.vault_lock_poisoned")))? = Some(vault);
    Ok(())
}

/// vault 닫기 (로그아웃).
pub fn vault_close() -> Result<()> {
    *VAULT.lock().map_err(|_| anyhow!(t!("core.vault_lock_poisoned")))? = None;
    Ok(())
}

/// 최근 수정순 메모 목록.
pub fn memo_list() -> Result<Vec<FfiMemo>> {
    with_vault(|v| Ok(v.store().list()?.into_iter().map(FfiMemo::from).collect()))
}

/// 메모 생성(id=None) 또는 수정(id=Some). 생성된/수정된 메모의 id 를 돌려준다.
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

/// 메모 삭제.
pub fn memo_delete(id: String) -> Result<()> {
    with_vault(|v| v.delete(&id))
}

/// 스티커 색상 팔레트 키 변경.
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

/// 스티커 불투명도(%) 변경. 범위를 벗어나면 코어가 잘라낸다.
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

/// 메모를 그룹으로 옮긴다. `group_id` 가 빈 문자열이면 최상위로 뺀다.
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

/// 한 메모에 붙은 사진 목록 (붙인 순서).
pub fn attachment_list(memo_id: String) -> Result<Vec<FfiAttachment>> {
    with_vault(|v| {
        Ok(v.store()
            .attachments_of(&memo_id)?
            .into_iter()
            .map(FfiAttachment::from)
            .collect())
    })
}

/// 사진을 메모에 붙인다. `data` 는 **원본 파일 바이트 그대로**.
///
/// `width_px`/`height_px` 는 Dart 가 디코딩해서 넘긴다(코어에 이미지 디코더를 두지
/// 않는다). 모르면 0 — 그 경우 표시 비율이 1:1 로 취급된다.
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

/// 사진 바이트(평문). 아직 동기화가 안 됐으면 에러 — UI 는 자리표시자를 그리면 된다.
pub fn attachment_bytes(hash: String) -> Result<Vec<u8>> {
    with_vault(|v| v.attachment_bytes(&hash))
}

/// 이 기기에 사진 파일이 도착했는가 (없으면 바이트를 청하지 말 것).
pub fn attachment_has_blob(hash: String) -> Result<bool> {
    with_vault(|v| Ok(v.has_blob(&hash)))
}

/// 표시 너비 변경 (em 의 1/1000). 다른 기기에도 같은 비율로 반영된다.
pub fn attachment_set_width(id: String, width_em_milli: i64) -> Result<()> {
    with_vault(|v| v.set_attachment_width(&id, width_em_milli))
}

/// 메모에서 사진을 뗀다. blob 파일은 남는다(GC 없음).
pub fn attachment_remove(id: String) -> Result<()> {
    with_vault(|v| v.detach(&id))
}

/// 전체 그룹 목록 (이름순).
pub fn group_list() -> Result<Vec<FfiGroup>> {
    with_vault(|v| Ok(v.store().list_groups()?.into_iter().map(FfiGroup::from).collect()))
}

/// 그룹 생성. 만들어진 그룹 id 를 돌려준다.
pub fn group_create(name: String, parent_id: String) -> Result<String> {
    with_vault(|v| {
        let mut group = Group::new(name);
        group.parent_id = parent_id;
        v.upsert_group(&group)?;
        Ok(group.id)
    })
}

/// 그룹 이름 변경.
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

/// 그룹을 다른 그룹 밑으로 옮긴다. 자기 자손 밑으로는 옮길 수 없다(순환 방지).
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

/// 그룹 삭제. 안의 메모/하위 그룹은 지워지지 않고 상위로 올라온다.
pub fn group_delete(id: String) -> Result<()> {
    with_vault(|v| v.delete_group(&id))
}

/// 다른 기기의 로그를 병합해 로컬 상태를 갱신한다.
/// (전송 계층이 vault 디렉터리에 새 로그를 가져다 놓은 뒤 호출)
pub fn sync_rebuild() -> Result<()> {
    with_vault(|v| v.rebuild())
}

/// QR 스캔으로 받은 페어링 코드 검증 + 기기 ID 추출.
/// (모바일 Syncthing 연동 전까지는 검증/표시에만 쓴다.)
pub fn pairing_decode(code: String) -> Result<String> {
    Ok(PairingCode::decode(&code)?.syncthing_device_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 이 crate 의 vault 는 **프로세스 전역**이라, 각자 vault 를 여는 테스트들이
    /// 병렬로 돌면 서로의 상태를 덮어쓴다. 그래서 vault 를 쓰는 테스트는 이 잠금을 잡는다.
    /// (실패로 poison 돼도 그대로 이어 쓴다 — 테스트 격리에만 쓰는 잠금이다.)
    static VAULT_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock_vault_for_test() -> std::sync::MutexGuard<'static, ()> {
        VAULT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// FFI 표면 왕복: open → upsert → list → update → delete → close.
    /// (전역 VAULT 를 쓰므로 시나리오를 한 테스트에 몰아넣는다)
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

        let id = memo_upsert(None, "모바일 메모".into(), "본문".into()).unwrap();
        assert_eq!(memo_list().unwrap().len(), 1);

        memo_upsert(Some(id.clone()), "수정됨".into(), "본문".into()).unwrap();
        let all = memo_list().unwrap();
        assert_eq!(all[0].title, "수정됨");
        assert_eq!(all[0].id, id);

        sync_rebuild().unwrap();
        assert_eq!(memo_list().unwrap().len(), 1);

        memo_delete(id).unwrap();
        assert!(memo_list().unwrap().is_empty());

        vault_close().unwrap();
        // 닫힌 뒤엔 에러 — 그 문구가 카탈로그를 타는지도 함께 본다
        // (하드코딩으로 되돌아가면 여기서 걸린다. 전역 VAULT 를 쓰므로 이 테스트 안에서 확인).
        for code in ["en", "ko"] {
            set_language(code.into());
            let lang = ymemo_i18n::Lang::parse(code).unwrap();
            let Err(e) = memo_list() else {
                panic!("닫힌 vault 인데 목록이 나왔다");
            };
            assert_eq!(e.to_string(), ymemo_i18n::raw(lang, "core.vault_not_open").unwrap());
        }

        // 재오픈: 로그에서 복원 (삭제까지 반영된 빈 상태)
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

    /// 첨부 FFI 표면: 붙이기 → 목록 → 크기 변경 → 바이트 → 떼기.
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

        let memo_id = memo_upsert(None, "사진 메모".into(), "".into()).unwrap();
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
