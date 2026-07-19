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
use ymemo_core::{now_millis, pairing::PairingCode, vault::Vault, Memo, Store};

/// 열린 vault (앱 프로세스당 하나).
static VAULT: Mutex<Option<Vault>> = Mutex::new(None);

fn with_vault<T>(f: impl FnOnce(&mut Vault) -> Result<T>) -> Result<T> {
    let mut guard = VAULT.lock().map_err(|_| anyhow!("vault 잠금 오염"))?;
    let vault = guard.as_mut().ok_or_else(|| anyhow!("vault 가 열려 있지 않음"))?;
    f(vault)
}

/// Dart 로 넘기는 메모 표현. (코어 `Memo` 와 동일 필드; frb 가 Dart 클래스로 변환)
pub struct FfiMemo {
    pub id: String,
    pub title: String,
    pub body: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<Memo> for FfiMemo {
    fn from(m: Memo) -> Self {
        Self {
            id: m.id,
            title: m.title,
            body: m.body,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

/// vault 열기(없으면 생성). `vault_dir` 는 동기화 대상 디렉터리,
/// `cache_db_path` 는 기기 로컬 SQLite 파일 경로.
pub fn vault_open(vault_dir: String, cache_db_path: String, password: String) -> Result<()> {
    let store = Store::open(&cache_db_path)?;
    let vault = Vault::open_or_create(&vault_dir, password.as_bytes(), store)?;
    *VAULT.lock().map_err(|_| anyhow!("vault 잠금 오염"))? = Some(vault);
    Ok(())
}

/// vault 닫기 (로그아웃).
pub fn vault_close() -> Result<()> {
    *VAULT.lock().map_err(|_| anyhow!("vault 잠금 오염"))? = None;
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
                    .ok_or_else(|| anyhow!("메모 없음: {id}"))?;
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

    /// FFI 표면 왕복: open → upsert → list → update → delete → close.
    /// (전역 VAULT 를 쓰므로 시나리오를 한 테스트에 몰아넣는다)
    #[test]
    fn ffi_surface_roundtrip() {
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
        assert!(memo_list().is_err()); // 닫힌 뒤엔 에러

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
