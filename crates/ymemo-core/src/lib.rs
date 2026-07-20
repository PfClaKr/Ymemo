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
use std::collections::{HashMap, HashSet};

/// 스티커 기본 색상 키. 팔레트 키는 UI 가 실제 색으로 매핑한다(코어는 문자열만 저장).
pub const DEFAULT_COLOR: &str = "yellow";

/// 스티커 기본 불투명도(%). 100 = 완전 불투명.
pub const DEFAULT_OPACITY: i64 = 100;
/// 불투명도 하한(%). 너무 투명해져 창을 못 찾는 일이 없도록 막는다.
pub const MIN_OPACITY: i64 = 20;

/// 메모 한 건. (사진 첨부 등은 이후 단계에서 확장)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Memo {
    pub id: String,
    pub title: String,
    pub body: String,
    /// 스티커 색상 팔레트 키 ("yellow"/"pink"/"green"/"blue"/"purple").
    /// 값 해석은 UI 몫이고 코어는 불투명 문자열로 저장·동기화만 한다.
    pub color: String,
    /// 스티커 창 불투명도 (백분율, [`MIN_OPACITY`]~100).
    pub opacity: i64,
    /// 속한 그룹(폴더) id. 빈 문자열이면 최상위.
    pub group_id: String,
    /// 생성 시각 (Unix epoch millis)
    pub created_at: i64,
    /// 마지막 수정 시각 (Unix epoch millis)
    pub updated_at: i64,
}

impl Memo {
    /// 새 메모 생성 (UUID v4 id 부여, 생성/수정 시각 = 현재, 기본 색상·불투명도).
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        let now = now_millis();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: title.into(),
            body: body.into(),
            color: DEFAULT_COLOR.to_string(),
            opacity: DEFAULT_OPACITY,
            group_id: String::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

/// 메모를 담는 그룹(폴더). `parent_id` 로 폴더 안의 폴더를 표현한다.
///
/// 동시 편집으로 부모 관계가 순환할 수 있으므로(A→B, B→A), 트리를 구성하는 쪽에서
/// 순환을 감지해 끊어야 한다 — [`Store::group_tree_roots`] 참고.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Group {
    pub id: String,
    pub name: String,
    /// 상위 그룹 id. 빈 문자열이면 최상위.
    pub parent_id: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Group {
    /// 새 그룹 생성 (최상위, 이름 지정).
    pub fn new(name: impl Into<String>) -> Self {
        let now = now_millis();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            parent_id: String::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

/// 불투명도를 유효 범위로 자른다. 저장 전 항상 통과시킨다
/// (다른 기기/버전이 이상한 값을 써 넣어도 UI 가 깨지지 않도록).
pub fn clamp_opacity(v: i64) -> i64 {
    v.clamp(MIN_OPACITY, 100)
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
                opacity    INTEGER NOT NULL DEFAULT 100,
                group_id   TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            -- 그룹(폴더). parent_id 로 중첩을 표현한다 (빈 문자열 = 최상위).
            CREATE TABLE IF NOT EXISTS groups (
                id         TEXT PRIMARY KEY,
                name       TEXT NOT NULL,
                parent_id  TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            -- 기기 로컬 메타데이터 (device_id 등). 동기화되지 않는 기기별 값.
            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )?;
        // 마이그레이션: 초기 스키마 이후에 추가된 컬럼을 기존 캐시에 채워 넣는다.
        // (캐시는 로그에서 재구성 가능하지만, 재빌드 없이도 열리도록 무해하게 보강)
        // 새 컬럼을 추가할 땐 위 CREATE TABLE 과 이 목록 양쪽에 넣는다.
        for (name, ddl) in [
            ("color", "ALTER TABLE memos ADD COLUMN color TEXT NOT NULL DEFAULT 'yellow'"),
            ("opacity", "ALTER TABLE memos ADD COLUMN opacity INTEGER NOT NULL DEFAULT 100"),
            ("group_id", "ALTER TABLE memos ADD COLUMN group_id TEXT NOT NULL DEFAULT ''"),
        ] {
            let exists = self
                .conn
                .prepare("SELECT 1 FROM pragma_table_info('memos') WHERE name = ?1")?
                .exists([name])?;
            if !exists {
                self.conn.execute(ddl, [])?;
            }
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

    /// 메모/그룹 테이블 비우기 (로그 재생 전 초기화용; meta 는 유지).
    pub fn clear_memos(&self) -> Result<()> {
        self.conn.execute("DELETE FROM memos", [])?;
        self.conn.execute("DELETE FROM groups", [])?;
        Ok(())
    }

    /// 삽입 또는 갱신 (id 기준).
    pub fn upsert(&self, memo: &Memo) -> Result<()> {
        self.conn.execute(
            "INSERT INTO memos (id, title, body, color, opacity, group_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                 title = ?2, body = ?3, color = ?4, opacity = ?5, group_id = ?6, updated_at = ?8",
            params![
                memo.id,
                memo.title,
                memo.body,
                memo.color,
                clamp_opacity(memo.opacity),
                memo.group_id,
                memo.created_at,
                memo.updated_at
            ],
        )?;
        Ok(())
    }

    /// 최근 수정순 전체 목록.
    pub fn list(&self) -> Result<Vec<Memo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, body, color, opacity, group_id, created_at, updated_at
             FROM memos ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_memo)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// id 로 한 건 조회.
    pub fn get(&self, id: &str) -> Result<Option<Memo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, body, color, opacity, group_id, created_at, updated_at
             FROM memos WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map([id], row_to_memo)?;
        Ok(rows.next().transpose()?)
    }

    // ---- 그룹 ----

    /// 그룹 삽입 또는 갱신 (id 기준).
    pub fn upsert_group(&self, group: &Group) -> Result<()> {
        self.conn.execute(
            "INSERT INTO groups (id, name, parent_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET name = ?2, parent_id = ?3, updated_at = ?5",
            params![group.id, group.name, group.parent_id, group.created_at, group.updated_at],
        )?;
        Ok(())
    }

    /// 이름순 전체 그룹 목록.
    pub fn list_groups(&self) -> Result<Vec<Group>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, parent_id, created_at, updated_at FROM groups ORDER BY name",
        )?;
        let rows = stmt.query_map([], row_to_group)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// id 로 그룹 한 건 조회.
    pub fn get_group(&self, id: &str) -> Result<Option<Group>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, parent_id, created_at, updated_at FROM groups WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map([id], row_to_group)?;
        Ok(rows.next().transpose()?)
    }

    /// id 로 그룹 삭제. (자식을 상위로 올리는 건 `Vault::delete_group` 이 한다)
    pub fn delete_group(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM groups WHERE id = ?1", [id])?;
        Ok(())
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
        opacity: row.get(4)?,
        group_id: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

/// groups 테이블 한 행 → Group.
fn row_to_group(row: &rusqlite::Row) -> rusqlite::Result<Group> {
    Ok(Group {
        id: row.get(0)?,
        name: row.get(1)?,
        parent_id: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

/// 순환하거나 부모가 사라진 그룹을 최상위로 끌어올려, 안전한 부모→자식 맵을 만든다.
/// 키는 부모 id (`""` = 최상위), 값은 이름순 자식 그룹.
///
/// 두 기기가 동시에 부모를 바꾸면 A→B, B→A 같은 **순환**이 생길 수 있다(CRDT 는 둘 다
/// 살린다). 그대로 트리를 그리면 무한 재귀가 되므로, 조상 사슬이 최상위에 닿지 못하는
/// 그룹은 최상위로 올려 항상 그릴 수 있는 트리를 보장한다.
pub fn group_children(groups: &[Group]) -> HashMap<String, Vec<Group>> {
    let by_id: HashMap<&str, &Group> = groups.iter().map(|g| (g.id.as_str(), g)).collect();
    let mut out: HashMap<String, Vec<Group>> = HashMap::new();
    for g in groups {
        let parent = if reaches_root(&by_id, g) {
            g.parent_id.clone()
        } else {
            String::new()
        };
        out.entry(parent).or_default().push(g.clone());
    }
    for children in out.values_mut() {
        children.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    }
    out
}

/// 조상 사슬을 따라가 최상위에 도달하면 true. 순환이거나 부모가 없으면 false.
fn reaches_root(by_id: &HashMap<&str, &Group>, g: &Group) -> bool {
    let mut seen: HashSet<&str> = HashSet::from([g.id.as_str()]);
    let mut cur = g.parent_id.as_str();
    while !cur.is_empty() {
        if !seen.insert(cur) {
            return false; // 순환
        }
        match by_id.get(cur) {
            Some(parent) => cur = parent.parent_id.as_str(),
            None => return false, // 부모가 사라짐
        }
    }
    true
}

/// `id` 가 `ancestor` 자신이거나 그 자손인가. 그룹을 자기 자손 밑으로 넣어
/// 순환을 만드는 드롭을 막는 데 쓴다.
pub fn is_descendant(groups: &[Group], id: &str, ancestor: &str) -> bool {
    if id == ancestor {
        return true;
    }
    let by_id: HashMap<&str, &Group> = groups.iter().map(|g| (g.id.as_str(), g)).collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut cur = id;
    while let Some(g) = by_id.get(cur) {
        if !seen.insert(cur) {
            return false; // 순환 — 더 볼 것 없음
        }
        if g.parent_id == ancestor {
            return true;
        }
        if g.parent_id.is_empty() {
            return false;
        }
        cur = g.parent_id.as_str();
    }
    false
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

    /// 불투명도는 항상 유효 범위로 잘려 저장된다 (다른 기기가 이상한 값을 써도 안전).
    #[test]
    fn opacity_is_clamped_on_store() {
        assert_eq!(clamp_opacity(0), MIN_OPACITY);
        assert_eq!(clamp_opacity(1000), 100);
        assert_eq!(clamp_opacity(55), 55);

        let store = Store::open_in_memory().unwrap();
        let mut memo = Memo::new("t", "");
        assert_eq!(memo.opacity, DEFAULT_OPACITY);
        memo.opacity = 5; // 하한 미만
        store.upsert(&memo).unwrap();
        assert_eq!(store.get(&memo.id).unwrap().unwrap().opacity, MIN_OPACITY);
    }

    fn group_at(id: &str, name: &str, parent: &str) -> Group {
        Group {
            id: id.into(),
            name: name.into(),
            parent_id: parent.into(),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn group_children_nests_and_sorts() {
        let groups = vec![
            group_at("b", "일", ""),
            group_at("a", "가", ""),
            group_at("c", "안쪽", "a"),
        ];
        let tree = group_children(&groups);
        let roots: Vec<&str> = tree[""].iter().map(|g| g.id.as_str()).collect();
        assert_eq!(roots, vec!["a", "b"]); // 이름순 (가 < 일)
        assert_eq!(tree["a"].len(), 1);
        assert_eq!(tree["a"][0].id, "c");
    }

    /// 두 기기가 서로를 부모로 지정하면 순환이 생긴다. 그리기가 불가능해지지 않도록
    /// 순환에 걸린 그룹은 최상위로 끌어올려야 한다.
    #[test]
    fn group_children_breaks_parent_cycles() {
        let groups = vec![group_at("a", "A", "b"), group_at("b", "B", "a")];
        let tree = group_children(&groups);
        let mut roots: Vec<&str> = tree[""].iter().map(|g| g.id.as_str()).collect();
        roots.sort();
        assert_eq!(roots, vec!["a", "b"]); // 둘 다 최상위로 구제
    }

    /// 부모가 (다른 기기에서) 삭제된 그룹도 사라지지 않고 최상위로 올라온다.
    #[test]
    fn group_children_rescues_orphans() {
        let groups = vec![group_at("a", "A", "없는부모")];
        let tree = group_children(&groups);
        assert_eq!(tree[""].len(), 1);
        assert_eq!(tree[""][0].id, "a");
    }

    #[test]
    fn is_descendant_detects_self_and_nested() {
        let groups = vec![group_at("a", "A", ""), group_at("b", "B", "a"), group_at("c", "C", "b")];
        assert!(is_descendant(&groups, "a", "a")); // 자기 자신
        assert!(is_descendant(&groups, "c", "a")); // 손자
        assert!(!is_descendant(&groups, "a", "c")); // 반대 방향은 아님
    }

    #[test]
    fn group_crud_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        let g = Group::new("업무");
        store.upsert_group(&g).unwrap();
        assert_eq!(store.get_group(&g.id).unwrap().unwrap(), g);
        assert_eq!(store.list_groups().unwrap().len(), 1);
        store.delete_group(&g.id).unwrap();
        assert!(store.list_groups().unwrap().is_empty());
    }

    /// color/opacity 없이 만들어진 구버전 캐시를 열면 컬럼이 추가되고 기본값을 갖는다.
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
        assert_eq!(m.opacity, DEFAULT_OPACITY);

        // 색 갱신도 정상 저장된다.
        let mut m2 = m.clone();
        m2.color = "blue".into();
        store.upsert(&m2).unwrap();
        assert_eq!(store.get("old1").unwrap().unwrap().color, "blue");

        std::fs::remove_file(&path).ok();
    }
}
