//! Vault: 암호화 change 로그(automerge) + 로컬 SQLite 캐시를 묶는 상위 계층.
//!
//! 동기화 디렉터리(이후 Syncthing 공유 폴더) 레이아웃:
//! ```text
//! <vault_dir>/
//!   vault.json            ← 헤더: salt + key_check. 생성 시 1회 기록, 이후 불변 → 동기화 충돌 없음.
//!   logs/<device_id>.ymlog ← 기기별 append-only 암호화 로그. 각 기기는 자기 파일에만 쓴다.
//! ```
//!
//! 로그 레코드 = **automerge change 바이너리** (암호화됨). 문서 구조:
//! `ROOT.memos: Map<memo_id, {title, body, created_at, updated_at}>`.
//! 병합은 automerge 가 순서 무관으로 처리한다 — 서로 다른 기기가 같은 메모의 다른
//! 필드를 고치면 둘 다 살아남고(필드 단위), 같은 필드 충돌은 결정론적으로 수렴한다.
//! actor id = device_id 이므로 자기 로그의 change 만 자기 actor 를 갖는다.

use anyhow::{anyhow, bail, Context, Result};
use automerge::{
    transaction::Transactable, ActorId, AutoCommit, Change, ObjId, ObjType, ReadDoc, ScalarValue,
    Value, ROOT,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::changelog::ChangeLog;
use crate::crypto::{generate_salt, MasterKey, Salt, SALT_LEN};
use crate::{Memo, Store};

const HEADER_FILE: &str = "vault.json";
const LOGS_DIR: &str = "logs";
const LOG_EXT: &str = "ymlog";
/// 틀린 암호 조기 감지용 카나리. 이 평문을 암호화해 헤더에 둔다.
const KEY_CHECK: &[u8] = b"ymemo-key-check-v1";

/// `vault.json` 내용. salt 는 비밀이 아니다.
#[derive(Serialize, Deserialize)]
struct VaultHeader {
    version: u32,
    /// hex 인코딩된 Argon2id salt.
    salt: String,
    /// hex 인코딩된 `encrypt(KEY_CHECK)`. 복호화 성공 = 암호 일치.
    key_check: String,
}

pub struct Vault {
    dir: PathBuf,
    store: Store,
    key: MasterKey,
    device_id: String,
    own_log: ChangeLog,
    /// 모든 기기 로그를 병합한 automerge 문서 (진실의 원천의 메모리 표현).
    doc: AutoCommit,
}

impl Vault {
    /// 새 vault 생성 (salt 생성 + 헤더 기록). 이미 있으면 에러.
    pub fn create(dir: impl AsRef<Path>, password: &[u8], store: Store) -> Result<Self> {
        let dir = dir.as_ref();
        let header_path = dir.join(HEADER_FILE);
        if header_path.exists() {
            bail!("vault 가 이미 존재함: {}", header_path.display());
        }
        fs::create_dir_all(dir)?;

        let salt = generate_salt();
        let key = MasterKey::derive(password, &salt)?;
        let header = VaultHeader {
            version: 1,
            salt: to_hex(&salt),
            key_check: to_hex(&key.encrypt(KEY_CHECK)?),
        };
        fs::write(&header_path, serde_json::to_vec_pretty(&header)?)?;

        Self::open(dir, password, store)
    }

    /// 기존 vault 열기. 헤더의 key_check 로 암호를 검증하고,
    /// 모든 기기 로그를 병합해 automerge 문서와 로컬 캐시를 재구성한다.
    pub fn open(dir: impl AsRef<Path>, password: &[u8], store: Store) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        let header_path = dir.join(HEADER_FILE);
        let header: VaultHeader = serde_json::from_slice(
            &fs::read(&header_path)
                .with_context(|| format!("vault 헤더 없음: {}", header_path.display()))?,
        )?;

        let salt_vec = from_hex(&header.salt)?;
        let salt: Salt = salt_vec
            .try_into()
            .map_err(|_| anyhow!("헤더의 salt 길이가 {SALT_LEN}B 가 아님"))?;
        let key = MasterKey::derive(password, &salt)?;

        // 암호 검증: 카나리 복호화가 실패하면 틀린 암호.
        let check = key
            .decrypt(&from_hex(&header.key_check)?)
            .map_err(|_| anyhow!("암호가 틀렸습니다, 다시 입력해주세요"))?;
        if check != KEY_CHECK {
            bail!("key_check 불일치 (헤더 손상?)");
        }

        let device_id = store.device_id()?;
        fs::create_dir_all(dir.join(LOGS_DIR))?;
        let own_log = ChangeLog::open(
            dir.join(LOGS_DIR).join(format!("{device_id}.{LOG_EXT}")),
            key.clone(),
        );

        let mut vault = Self {
            doc: AutoCommit::new(),
            dir,
            store,
            key,
            device_id,
            own_log,
        };
        vault.rebuild()?;
        Ok(vault)
    }

    /// 헤더가 있으면 열고, 없으면 새로 만든다.
    pub fn open_or_create(dir: impl AsRef<Path>, password: &[u8], store: Store) -> Result<Self> {
        if dir.as_ref().join(HEADER_FILE).exists() {
            Self::open(dir, password, store)
        } else {
            Self::create(dir, password, store)
        }
    }

    /// 메모 삽입/갱신. 바뀐 필드만 automerge 에 기록해 필드 단위 병합을 살린다.
    pub fn upsert(&mut self, memo: &Memo) -> Result<()> {
        let memos = self.memos_obj()?;
        let obj = match self.doc.get(&memos, &memo.id)? {
            Some((Value::Object(ObjType::Map), id)) => id,
            _ => self.doc.put_object(&memos, &memo.id, ObjType::Map)?,
        };
        put_str_if_changed(&mut self.doc, &obj, "title", &memo.title)?;
        put_str_if_changed(&mut self.doc, &obj, "body", &memo.body)?;
        put_str_if_changed(&mut self.doc, &obj, "color", &memo.color)?;
        put_i64_if_changed(&mut self.doc, &obj, "opacity", crate::clamp_opacity(memo.opacity))?;
        put_i64_if_changed(&mut self.doc, &obj, "created_at", memo.created_at)?;
        put_i64_if_changed(&mut self.doc, &obj, "updated_at", memo.updated_at)?;

        self.append_local_change()?;
        self.store.upsert(memo)
    }

    /// 메모 삭제.
    pub fn delete(&mut self, id: &str) -> Result<()> {
        let memos = self.memos_obj()?;
        if self.doc.get(&memos, id)?.is_some() {
            self.doc.delete(&memos, id)?;
        }
        self.append_local_change()?;
        self.store.delete(id)
    }

    /// `logs/` 의 모든 기기 로그를 복호화해 새 automerge 문서로 병합하고,
    /// 로컬 SQLite 캐시를 처음부터 재구성한다.
    ///
    /// Syncthing 이 다른 기기의 로그 파일을 가져다 놓으면, 이 호출 한 번으로 반영된다.
    pub fn rebuild(&mut self) -> Result<()> {
        let mut doc = AutoCommit::new();
        doc.apply_changes(self.read_all_changes()?)?;
        // actor = device_id: 이후의 로컬 변경이 자기 로그의 actor 로 이어진다.
        // (자기 옛 change 들을 이미 적용했으므로 actor seq 도 이어진다.)
        doc.set_actor(ActorId::from(self.device_id.as_bytes()));
        self.doc = doc;
        self.materialize()
    }

    /// 읽기용 로컬 캐시 접근 (list/get 등).
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// 이 기기의 식별자.
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// 동기화 대상 디렉터리 (Syncthing 공유 폴더로 지정할 경로).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 모든 `.ymlog` 를 복호화해 automerge change 로 파싱한다.
    fn read_all_changes(&self) -> Result<Vec<Change>> {
        let logs_dir = self.dir.join(LOGS_DIR);
        let mut changes = Vec::new();
        if logs_dir.exists() {
            for entry in fs::read_dir(&logs_dir)? {
                let path = entry?.path();
                if path.extension().and_then(|e| e.to_str()) == Some(LOG_EXT) {
                    for record in ChangeLog::open(&path, self.key.clone()).read_all()? {
                        changes.push(
                            Change::from_bytes(record)
                                .with_context(|| format!("automerge change 파싱 실패: {}", path.display()))?,
                        );
                    }
                }
            }
        }
        Ok(changes)
    }

    /// 방금의 로컬 변경을 커밋해 자기 로그에 암호화 append 한다.
    /// (변경이 실제로 없었으면 아무것도 쓰지 않는다.)
    fn append_local_change(&mut self) -> Result<()> {
        if self.doc.commit().is_some() {
            let change = self
                .doc
                .get_last_local_change()
                .context("커밋 직후인데 로컬 change 가 없음")?;
            self.own_log.append(change.raw_bytes())?;
        }
        Ok(())
    }

    /// automerge 문서를 SQLite 캐시로 실체화한다.
    fn materialize(&mut self) -> Result<()> {
        self.store.clear_memos()?;
        let Some((Value::Object(ObjType::Map), memos)) = self.doc.get(ROOT, "memos")? else {
            return Ok(()); // 아직 메모 없음
        };
        let ids: Vec<String> = self.doc.keys(&memos).collect();
        for id in ids {
            let Some((Value::Object(ObjType::Map), obj)) = self.doc.get(&memos, &id)? else {
                continue;
            };
            let memo = Memo {
                id: id.clone(),
                title: get_str(&self.doc, &obj, "title")?,
                body: get_str(&self.doc, &obj, "body")?,
                // color/opacity 는 이후에 추가된 필드라 옛 change 엔 없을 수 있다 → 기본값.
                color: get_str_or(&self.doc, &obj, "color", crate::DEFAULT_COLOR),
                opacity: crate::clamp_opacity(get_i64_or(
                    &self.doc,
                    &obj,
                    "opacity",
                    crate::DEFAULT_OPACITY,
                )),
                created_at: get_i64(&self.doc, &obj, "created_at")?,
                updated_at: get_i64(&self.doc, &obj, "updated_at")?,
            };
            self.store.upsert(&memo)?;
        }
        Ok(())
    }

    /// `ROOT.memos` 맵을 얻는다 (없으면 생성).
    fn memos_obj(&mut self) -> Result<ObjId> {
        Ok(match self.doc.get(ROOT, "memos")? {
            Some((Value::Object(ObjType::Map), id)) => id,
            _ => self.doc.put_object(ROOT, "memos", ObjType::Map)?,
        })
    }
}

fn put_str_if_changed(doc: &mut AutoCommit, obj: &ObjId, key: &str, val: &str) -> Result<()> {
    let same = matches!(
        doc.get(obj, key)?,
        Some((Value::Scalar(ref s), _)) if matches!(s.as_ref(), ScalarValue::Str(cur) if cur.as_str() == val)
    );
    if !same {
        doc.put(obj, key, val)?;
    }
    Ok(())
}

fn put_i64_if_changed(doc: &mut AutoCommit, obj: &ObjId, key: &str, val: i64) -> Result<()> {
    let same = matches!(
        doc.get(obj, key)?,
        Some((Value::Scalar(ref s), _)) if matches!(s.as_ref(), ScalarValue::Int(cur) if *cur == val)
    );
    if !same {
        doc.put(obj, key, val)?;
    }
    Ok(())
}

fn get_str(doc: &AutoCommit, obj: &ObjId, key: &str) -> Result<String> {
    match doc.get(obj, key)? {
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::Str(v) => Ok(v.to_string()),
            other => bail!("필드 {key} 가 문자열이 아님: {other:?}"),
        },
        _ => bail!("필드 없음: {key}"),
    }
}

/// 문자열 필드를 읽되, 없거나 문자열이 아니면 기본값을 돌려준다.
fn get_str_or(doc: &AutoCommit, obj: &ObjId, key: &str, default: &str) -> String {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Str(v) => v.to_string(),
            _ => default.to_string(),
        },
        _ => default.to_string(),
    }
}

/// 정수 필드를 읽되, 없거나 정수가 아니면 기본값을 돌려준다.
fn get_i64_or(doc: &AutoCommit, obj: &ObjId, key: &str, default: i64) -> i64 {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Int(v) => *v,
            _ => default,
        },
        _ => default,
    }
}

fn get_i64(doc: &AutoCommit, obj: &ObjId, key: &str) -> Result<i64> {
    match doc.get(obj, key)? {
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::Int(v) => Ok(*v),
            other => bail!("필드 {key} 가 정수가 아님: {other:?}"),
        },
        _ => bail!("필드 없음: {key}"),
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Result<Vec<u8>> {
    if s.len() % 2 != 0 {
        bail!("hex 길이가 홀수");
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| anyhow!("hex 파싱 실패: {e}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 테스트별 임시 vault 디렉터리.
    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ymemo-vault-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn create_reopen_rebuilds_cache() {
        let dir = temp_dir();
        let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

        let m1;
        {
            let mut vault = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
            m1 = Memo::new("살아남을 메모", "본문");
            vault.upsert(&m1).unwrap();
            let dead = Memo::new("지워질 메모", "");
            vault.upsert(&dead).unwrap();
            vault.delete(&dead.id).unwrap();
        } // vault 소멸 — 로그 파일이 진실의 원천으로 남는다.

        // 같은 캐시(=같은 device_id=actor)로 재오픈: actor seq 가 이어져야 한다.
        let m2;
        {
            let mut vault = Vault::open(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
            assert_eq!(vault.store().list().unwrap(), vec![m1.clone()]);
            m2 = Memo::new("두 번째 세션", "");
            vault.upsert(&m2).unwrap();
        }

        // 완전히 새(빈) 캐시로도 로그만으로 전체 복원.
        let vault = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let mut titles: Vec<String> =
            vault.store().list().unwrap().into_iter().map(|m| m.title).collect();
        titles.sort();
        assert_eq!(titles, vec!["두 번째 세션", "살아남을 메모"]);

        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&db).ok();
    }

    #[test]
    fn wrong_password_rejected_by_key_check() {
        let dir = temp_dir();
        Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        // 로그가 비어 있어도 헤더 카나리만으로 즉시 거부돼야 한다.
        assert!(Vault::open(&dir, b"wrong", Store::open_in_memory().unwrap()).is_err());
        fs::remove_dir_all(&dir).ok();
    }

    /// automerge 의 핵심 가치: 두 기기가 같은 메모의 **다른 필드**를 동시에 고치면
    /// 둘 다 살아남아야 한다. (구 LWW 는 메모 단위라 한쪽이 통째로 사라졌다.)
    #[test]
    fn concurrent_field_edits_both_survive() {
        let dir = temp_dir();

        // 기기 A: 메모 생성.
        let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let base = Memo::new("원본 제목", "원본 본문");
        a.upsert(&base).unwrap();

        // 기기 B: 열어서 같은 상태에서 출발.
        let mut b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        assert_ne!(a.device_id(), b.device_id());
        assert_eq!(b.store().list().unwrap().len(), 1);

        // 동시 편집: A 는 제목만, B 는 본문만 고친다.
        let mut a_edit = base.clone();
        a_edit.title = "A가 고친 제목".into();
        a.upsert(&a_edit).unwrap();

        let mut b_edit = base.clone();
        b_edit.body = "B가 고친 본문".into();
        b.upsert(&b_edit).unwrap();

        // 양쪽 모두 rebuild 후 같은 병합 결과로 수렴해야 한다.
        a.rebuild().unwrap();
        b.rebuild().unwrap();
        for v in [&a, &b] {
            let merged = v.store().get(&base.id).unwrap().unwrap();
            assert_eq!(merged.title, "A가 고친 제목");
            assert_eq!(merged.body, "B가 고친 본문");
        }

        // 로그 파일은 기기당 하나씩 두 개.
        assert_eq!(fs::read_dir(dir.join(LOGS_DIR)).unwrap().count(), 2);

        fs::remove_dir_all(&dir).ok();
    }

    /// 같은 필드 충돌은 양쪽이 같은 값으로 결정론적으로 수렴해야 한다.
    #[test]
    fn same_field_conflict_converges() {
        let dir = temp_dir();

        let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let base = Memo::new("t", "");
        a.upsert(&base).unwrap();

        let mut b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();

        let mut a_edit = base.clone();
        a_edit.title = "A안".into();
        a.upsert(&a_edit).unwrap();
        let mut b_edit = base.clone();
        b_edit.title = "B안".into();
        b.upsert(&b_edit).unwrap();

        a.rebuild().unwrap();
        b.rebuild().unwrap();
        let ta = a.store().get(&base.id).unwrap().unwrap().title;
        let tb = b.store().get(&base.id).unwrap().unwrap().title;
        assert_eq!(ta, tb); // 어느 쪽이 이기든 양쪽이 같아야 한다
        assert!(ta == "A안" || ta == "B안");

        fs::remove_dir_all(&dir).ok();
    }
}
