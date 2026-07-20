//! Ymemo 공유 코어.
//!
//! 데스크탑(Slint)과 모바일(Flutter, 향후 FFI)이 함께 쓰는 순수 Rust 라이브러리.
//! 현재는 데이터 모델 + 로컬 SQLite 저장소만 담는다.
//! 이후 단계에서 CRDT 병합, E2E 암호화, Syncthing 기반 동기화가 이 crate 에 추가된다.

pub mod changelog;
pub mod crypto;
pub mod pairing;
pub mod sync;
pub mod vault;

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

/// 스티커 기본 색상 키. 팔레트 키는 UI 가 실제 색으로 매핑한다(코어는 문자열만 저장).
pub const DEFAULT_COLOR: &str = "yellow";

/// 메모 한 건. (사진 첨부 등은 이후 단계에서 확장)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Memo {
    pub id: String,
    pub title: String,
    pub body: String,
    /// 스티커 색상 팔레트 키 ("yellow"/"pink"/"green"/"blue"/"purple").
    /// 값 해석은 UI 몫이고 코어는 불투명 문자열로 저장·동기화만 한다.
    pub color: String,
    /// 생성 시각 (Unix epoch millis)
    pub created_at: i64,
    /// 마지막 수정 시각 (Unix epoch millis)
    pub updated_at: i64,
}

impl Memo {
    /// 새 메모 생성 (UUID v4 id 부여, 생성/수정 시각 = 현재, 기본 색상).
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        let now = now_millis();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: title.into(),
            body: body.into(),
            color: DEFAULT_COLOR.to_string(),
            created_at: now,
            updated_at: now,
        }
    }
}

/// 로컬 메모 저장소 (SQLite).
///
/// 주의: `rusqlite::Connection` 은 단일 스레드용이다. 데스크탑 UI 스레드에서
/// `Rc<RefCell<Store>>` 로 쓰는 것을 전제로 한다.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// 파일 경로로 저장소 열기 (없으면 생성).
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let store = Self {
            conn: Connection::open(path)?,
        };
        store.init()?;
        Ok(store)
    }

    /// 인메모리 저장소 (테스트용).
    pub fn open_in_memory() -> Result<Self> {
        let store = Self {
            conn: Connection::open_in_memory()?,
        };
        store.init()?;
        Ok(store)
    }

    fn init(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS memos (
                id         TEXT PRIMARY KEY,
                title      TEXT NOT NULL,
                body       TEXT NOT NULL,
                color      TEXT NOT NULL DEFAULT 'yellow',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            -- 기기 로컬 메타데이터 (device_id 등). 동기화되지 않는 기기별 값.
            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )?;
        // 마이그레이션: color 없이 만들어진 기존 캐시에 컬럼을 더한다.
        // (캐시는 로그에서 재구성 가능하지만, 재빌드 없이도 열리도록 무해하게 보강)
        let has_color = self
            .conn
            .prepare("SELECT 1 FROM pragma_table_info('memos') WHERE name = 'color'")?
            .exists([])?;
        if !has_color {
            self.conn
                .execute("ALTER TABLE memos ADD COLUMN color TEXT NOT NULL DEFAULT 'yellow'", [])?;
        }
        Ok(())
    }

    /// 이 저장소(=이 기기)의 고유 식별자. 없으면 생성해 meta 에 영속화한다.
    ///
    /// SQLite 캐시는 기기별 자산이므로 device_id 를 여기 두면 동기화 대상에서
    /// 자연히 제외된다. (캐시를 지우면 새 id 가 나오지만, 옛 로그는 남고
    /// 새 append 가 새 로그 파일로 갈 뿐이라 무해하다.)
    pub fn device_id(&self) -> Result<String> {
        if let Some(id) = self.meta_get("device_id")? {
            return Ok(id);
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.meta_set("device_id", &id)?;
        Ok(id)
    }

    fn meta_get(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare("SELECT value FROM meta WHERE key = ?1")?;
        let mut rows = stmt.query_map([key], |row| row.get(0))?;
        Ok(rows.next().transpose()?)
    }

    fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, value],
        )?;
        Ok(())
    }

    /// 메모 테이블 전체 비우기 (로그 재생 전 초기화용; meta 는 유지).
    pub fn clear_memos(&self) -> Result<()> {
        self.conn.execute("DELETE FROM memos", [])?;
        Ok(())
    }

    /// 삽입 또는 갱신 (id 기준).
    pub fn upsert(&self, memo: &Memo) -> Result<()> {
        self.conn.execute(
            "INSERT INTO memos (id, title, body, color, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET title = ?2, body = ?3, color = ?4, updated_at = ?6",
            params![memo.id, memo.title, memo.body, memo.color, memo.created_at, memo.updated_at],
        )?;
        Ok(())
    }

    /// 최근 수정순 전체 목록.
    pub fn list(&self) -> Result<Vec<Memo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, body, color, created_at, updated_at
             FROM memos ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_memo)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// id 로 한 건 조회.
    pub fn get(&self, id: &str) -> Result<Option<Memo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, body, color, created_at, updated_at FROM memos WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map([id], row_to_memo)?;
        Ok(rows.next().transpose()?)
    }

    /// id 로 삭제.
    pub fn delete(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM memos WHERE id = ?1", [id])?;
        Ok(())
    }
}

/// memos 테이블 한 행 → Memo. (list/get 공용)
fn row_to_memo(row: &rusqlite::Row) -> rusqlite::Result<Memo> {
    Ok(Memo {
        id: row.get(0)?,
        title: row.get(1)?,
        body: row.get(2)?,
        color: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

/// 현재 시각 (Unix epoch millis). FFI 등 코어 밖에서도 같은 시계를 쓰도록 공개.
pub fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crud_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.list().unwrap().len(), 0);

        let memo = Memo::new("제목", "내용");
        store.upsert(&memo).unwrap();

        let all = store.list().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].title, "제목");
        assert_eq!(store.get(&memo.id).unwrap().unwrap(), memo);

        store.delete(&memo.id).unwrap();
        assert_eq!(store.list().unwrap().len(), 0);
    }

    #[test]
    fn upsert_updates_existing() {
        let store = Store::open_in_memory().unwrap();
        let mut memo = Memo::new("v1", "");
        store.upsert(&memo).unwrap();
        memo.title = "v2".into();
        memo.updated_at += 1000;
        store.upsert(&memo).unwrap();

        let all = store.list().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].title, "v2");
    }

    /// color 없이 만들어진 구버전 캐시를 열면 컬럼이 추가되고 기존 행은 기본색을 갖는다.
    #[test]
    fn migrates_pre_color_cache() {
        let path = std::env::temp_dir().join(format!("ymemo-mig-{}.db", uuid::Uuid::new_v4()));
        // 구버전 스키마(color 없음)로 직접 만들고 한 행을 넣는다.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE memos (
                    id TEXT PRIMARY KEY, title TEXT NOT NULL, body TEXT NOT NULL,
                    created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
                );
                INSERT INTO memos VALUES ('old1', '옛 메모', '본문', 1, 2);",
            )
            .unwrap();
        }
        // 새 Store 로 열면 init() 마이그레이션이 color 컬럼을 더한다.
        let store = Store::open(&path).unwrap();
        let m = store.get("old1").unwrap().unwrap();
        assert_eq!(m.title, "옛 메모");
        assert_eq!(m.color, DEFAULT_COLOR);

        // 색 갱신도 정상 저장된다.
        let mut m2 = m.clone();
        m2.color = "blue".into();
        store.upsert(&m2).unwrap();
        assert_eq!(store.get("old1").unwrap().unwrap().color, "blue");

        std::fs::remove_file(&path).ok();
    }
}
