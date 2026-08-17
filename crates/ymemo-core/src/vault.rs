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
use ymemo_i18n::t;
use automerge::{
    transaction::Transactable, ActorId, AutoCommit, Change, ObjId, ObjType, ReadDoc, ScalarValue,
    Value, ROOT,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::changelog::ChangeLog;
use crate::crypto::{generate_salt, MasterKey, Salt, SALT_LEN};
use crate::{Group, Memo, Store};

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
            bail!(t!("core.vault_exists", path = header_path.display()));
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
        let header = read_header(&dir)?;

        let salt_vec = from_hex(&header.salt)?;
        let salt: Salt = salt_vec
            .try_into()
            .map_err(|_| anyhow!(t!("core.salt_length_bad", expected = SALT_LEN)))?;
        let key = MasterKey::derive(password, &salt)?;
        verify_key(&header, &key)?;

        let device_id = store.device_id()?;
        fs::create_dir_all(dir.join(LOGS_DIR))?;

        // 갈라진 키 자가 치유: 과거에 두 기기가 각자 vault.json(다른 salt)을 만들어
        // 키가 갈라졌다가, Syncthing 충돌 해소로 vault.json 이 하나(정본)로 수렴한
        // 상황을 복구한다. 내 로그가 정본 키로 열리지 않으면 sync-conflict 헤더의
        // salt 에서 옛 키를 찾아 로그를 정본 키로 재암호화한다.
        heal_divergent_log(&dir, &device_id, password, &key)?;

        Self::finish_open(dir, store, key, device_id)
    }

    /// 이미 유도해 둔 키로 열기 ("자동 잠금 해제" 경로 — 마스터 암호를 묻지 않는다).
    ///
    /// 암호가 없으므로 갈라진 키 자가 치유([`heal_divergent_log`])는 건너뛴다. 그 상황이면
    /// 여기서 로그가 열리지 않아 에러가 나고, 호출자는 잠금 화면으로 되돌리면 된다 —
    /// 복구는 사용자가 암호를 입력하는 [`Self::open`] 경로에서 일어난다.
    pub fn open_with_key(dir: impl AsRef<Path>, key: MasterKey, store: Store) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        let header = read_header(&dir)?;
        verify_key(&header, &key)?;

        let device_id = store.device_id()?;
        fs::create_dir_all(dir.join(LOGS_DIR))?;

        Self::finish_open(dir, store, key, device_id)
    }

    /// 키 검증까지 끝난 뒤의 공통 마무리: 내 로그를 열고 전체를 병합한다.
    fn finish_open(dir: PathBuf, store: Store, key: MasterKey, device_id: String) -> Result<Self> {
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

    /// 이 vault 를 여는 원시 키. "자동 잠금 해제" 캐시에 쓰라고 있는 것으로,
    /// 보안상의 의미는 [`MasterKey::to_bytes`] 문서를 볼 것.
    pub fn key_bytes(&self) -> [u8; crate::crypto::KEY_LEN] {
        self.key.to_bytes()
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
        put_str_if_changed(&mut self.doc, &obj, "group_id", &memo.group_id)?;
        put_i64_if_changed(&mut self.doc, &obj, "created_at", memo.created_at)?;
        put_i64_if_changed(&mut self.doc, &obj, "updated_at", memo.updated_at)?;

        self.append_local_change()?;
        self.store.upsert(memo)
    }

    /// 그룹 삽입/갱신 (이름 변경·부모 변경 모두 이 경로).
    pub fn upsert_group(&mut self, group: &Group) -> Result<()> {
        let groups = self.groups_obj()?;
        let obj = match self.doc.get(&groups, &group.id)? {
            Some((Value::Object(ObjType::Map), id)) => id,
            _ => self.doc.put_object(&groups, &group.id, ObjType::Map)?,
        };
        put_str_if_changed(&mut self.doc, &obj, "name", &group.name)?;
        put_str_if_changed(&mut self.doc, &obj, "parent_id", &group.parent_id)?;
        put_i64_if_changed(&mut self.doc, &obj, "created_at", group.created_at)?;
        put_i64_if_changed(&mut self.doc, &obj, "updated_at", group.updated_at)?;

        self.append_local_change()?;
        self.store.upsert_group(group)
    }

    /// 그룹 삭제. 안에 있던 그룹/메모는 지우지 않고 **상위로 끌어올린다**
    /// (폴더를 지웠다고 메모가 사라지면 곤란하므로).
    pub fn delete_group(&mut self, id: &str) -> Result<()> {
        let parent = self
            .store
            .get_group(id)?
            .map(|g| g.parent_id)
            .unwrap_or_default();

        // 자식 그룹을 상위로.
        let children: Vec<Group> = self
            .store
            .list_groups()?
            .into_iter()
            .filter(|g| g.parent_id == id)
            .collect();
        for mut child in children {
            child.parent_id = parent.clone();
            child.updated_at = crate::now_millis();
            self.upsert_group(&child)?;
        }
        // 속해 있던 메모를 상위로.
        let memos: Vec<Memo> = self
            .store
            .list()?
            .into_iter()
            .filter(|m| m.group_id == id)
            .collect();
        for mut memo in memos {
            memo.group_id = parent.clone();
            memo.updated_at = crate::now_millis();
            self.upsert(&memo)?;
        }

        let groups = self.groups_obj()?;
        if self.doc.get(&groups, id)?.is_some() {
            self.doc.delete(&groups, id)?;
        }
        self.append_local_change()?;
        self.store.delete_group(id)
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
    ///
    /// **한 로그가 실패해도 전체 병합을 막지 않는다** — 그 파일만 건너뛴다.
    /// 실패 원인: 다른 키로 쓰인 로그(기기별 vault 가 갈라진 경우)나 Syncthing 이
    /// 아직 다 옮기지 못한 부분 파일. 이런 게 하나 있다고 다른 기기의 정상 로그까지
    /// 못 읽으면 "동기화가 통째로 멈춘 것처럼" 보인다(특히 콘솔 없는 Windows 릴리스).
    fn read_all_changes(&self) -> Result<Vec<Change>> {
        let logs_dir = self.dir.join(LOGS_DIR);
        let mut changes = Vec::new();
        if !logs_dir.exists() {
            return Ok(changes);
        }
        for entry in fs::read_dir(&logs_dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some(LOG_EXT) {
                continue;
            }
            let records = match ChangeLog::open(&path, self.key.clone()).read_all() {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("로그 건너뜀(복호화 실패) {}: {e}", path.display());
                    continue;
                }
            };
            for record in records {
                match Change::from_bytes(record) {
                    Ok(c) => changes.push(c),
                    Err(e) => eprintln!("change 건너뜀(파싱 실패) {}: {e}", path.display()),
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
                .context(t!("core.no_local_change"))?;
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
                group_id: get_str_or(&self.doc, &obj, "group_id", ""),
                created_at: get_i64(&self.doc, &obj, "created_at")?,
                updated_at: get_i64(&self.doc, &obj, "updated_at")?,
            };
            self.store.upsert(&memo)?;
        }
        self.materialize_groups()
    }

    /// `ROOT.groups` 를 SQLite 캐시로 실체화한다.
    fn materialize_groups(&mut self) -> Result<()> {
        let Some((Value::Object(ObjType::Map), groups)) = self.doc.get(ROOT, "groups")? else {
            return Ok(()); // 아직 그룹 없음
        };
        let ids: Vec<String> = self.doc.keys(&groups).collect();
        for id in ids {
            let Some((Value::Object(ObjType::Map), obj)) = self.doc.get(&groups, &id)? else {
                continue;
            };
            let group = Group {
                id: id.clone(),
                name: get_str_or(&self.doc, &obj, "name", ""),
                parent_id: get_str_or(&self.doc, &obj, "parent_id", ""),
                created_at: get_i64_or(&self.doc, &obj, "created_at", 0),
                updated_at: get_i64_or(&self.doc, &obj, "updated_at", 0),
            };
            self.store.upsert_group(&group)?;
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

    /// `ROOT.groups` 맵을 얻는다 (없으면 생성).
    fn groups_obj(&mut self) -> Result<ObjId> {
        Ok(match self.doc.get(ROOT, "groups")? {
            Some((Value::Object(ObjType::Map), id)) => id,
            _ => self.doc.put_object(ROOT, "groups", ObjType::Map)?,
        })
    }
}

/// 갈라진 vault 키 자가 치유.
///
/// 배경: 예전엔 새 기기가 페어링으로 vault.json 을 받기 전에 암호를 입력하면 각자
/// 다른 salt 로 vault.json 을 만들어 키가 갈라졌다. Syncthing 은 두 vault.json 을
/// 충돌로 보고 하나를 정본(vault.json)으로 남기고 진 쪽을 `vault.sync-conflict-*.json`
/// 으로 이름을 바꾼다 → 모든 기기가 같은 정본 salt 로 수렴한다.
///
/// 이 함수는 정본 키(`canonical_key`, 이미 vault.json 의 key_check 로 검증됨)로 내
/// 로그가 열리는지 보고, 안 열리면 conflict 헤더들의 salt 로 옛 키를 찾아 내 로그를
/// 정본 키로 재암호화한다. 남의 로그는 건드리지 않는다(각 기기가 스스로 치유).
///
/// conflict 파일은 지우지 않는다 — 아직 치유하지 못한 다른 기기가 자기 옛 salt 를
/// 찾는 데 필요할 수 있고, 지우면 그 삭제가 동기화로 퍼져 복구를 막는다. 일단
/// 치유되면 내 로그가 정본 키로 바로 열리므로 이 탐색은 다시 돌지 않는다.
fn heal_divergent_log(
    dir: &Path,
    device_id: &str,
    password: &[u8],
    canonical_key: &MasterKey,
) -> Result<()> {
    let own_path = dir.join(LOGS_DIR).join(format!("{device_id}.{LOG_EXT}"));
    if !own_path.exists() {
        return Ok(()); // 로컬 로그 없음 → 치유할 것 없음
    }
    // 정본 키로 이미 열리면 정상(또는 이미 치유됨).
    if ChangeLog::open(&own_path, canonical_key.clone()).read_all().is_ok() {
        return Ok(());
    }
    // 내 로그를 여는 옛 키를 conflict 헤더들의 salt 에서 찾는다.
    for salt in conflict_salts(dir) {
        let old_key = MasterKey::derive(password, &salt)?;
        if ChangeLog::open(&own_path, old_key.clone()).read_all().is_ok() {
            reencrypt_log(&own_path, &old_key, canonical_key)?;
            eprintln!("갈라진 vault 키 감지 → 내 로그를 정본 키로 재암호화 완료");
            return Ok(());
        }
    }
    // 못 찾음: 그대로 두면 rebuild 가 이 로그만 건너뛴다(다른 기기 로그는 정상 병합).
    eprintln!(
        "경고: 내 로그를 여는 키를 찾지 못함 ({}) — vault.json 이 예기치 않게 바뀌었을 수 있음",
        own_path.display()
    );
    Ok(())
}

/// `vault.json` 헤더를 읽는다.
fn read_header(dir: &Path) -> Result<VaultHeader> {
    let path = dir.join(HEADER_FILE);
    let bytes =
        fs::read(&path).with_context(|| t!("core.header_missing", path = path.display()))?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// 헤더의 카나리(key_check)를 복호화해 키가 맞는지 본다. 실패 = 틀린 암호/키.
fn verify_key(header: &VaultHeader, key: &MasterKey) -> Result<()> {
    let check = key
        .decrypt(&from_hex(&header.key_check)?)
        .map_err(|_| anyhow!(t!("core.wrong_password")))?;
    if check != KEY_CHECK {
        bail!(t!("core.key_check_mismatch"));
    }
    Ok(())
}

/// `vault.sync-conflict-*.json` 들에서 salt 를 파싱해 반환. 읽기/파싱 실패는 조용히 건너뛴다.
fn conflict_salts(dir: &Path) -> Vec<Salt> {
    let mut salts = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return salts;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !(name.starts_with("vault.sync-conflict-") && name.ends_with(".json")) {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else { continue };
        let Ok(header) = serde_json::from_slice::<VaultHeader>(&bytes) else { continue };
        let Ok(salt_vec) = from_hex(&header.salt) else { continue };
        if let Ok(salt) = <Salt>::try_from(salt_vec) {
            salts.push(salt);
        }
    }
    salts
}

/// 로그를 `old_key` 로 복호화해 `new_key` 로 다시 쓰고 원자적으로 교체한다.
/// (레코드 순서는 유지되지만 automerge change 는 순서 무관이라 무해하다.)
fn reencrypt_log(path: &Path, old_key: &MasterKey, new_key: &MasterKey) -> Result<()> {
    let records = ChangeLog::open(path, old_key.clone()).read_all()?;
    let tmp = path.with_extension("ymlog.tmp");
    let _ = fs::remove_file(&tmp);
    let new_log = ChangeLog::open(&tmp, new_key.clone());
    for r in &records {
        new_log.append(r)?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
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
            other => bail!(t!("core.field_not_string", key = key, found = format!("{other:?}"))),
        },
        _ => bail!(t!("core.field_missing", key = key)),
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
            other => bail!(t!("core.field_not_int", key = key, found = format!("{other:?}"))),
        },
        _ => bail!(t!("core.field_missing", key = key)),
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Result<Vec<u8>> {
    if s.len() % 2 != 0 {
        bail!(t!("core.hex_odd_length"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| anyhow!(t!("core.hex_parse_failed", error = e))))
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

    /// 다른 키로 쓰인(=기기 vault 가 갈라진) 로그가 섞여 있어도, rebuild 는 실패하지
    /// 않고 복호화되는 로그만 반영해야 한다.
    #[test]
    fn rebuild_skips_undecryptable_foreign_log() {
        let dir = temp_dir();
        let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let memo = Memo::new("내 메모", "");
        a.upsert(&memo).unwrap();

        // logs/ 에 다른 키로 암호화된 이질 로그를 심는다.
        let foreign = ChangeLog::open(
            dir.join(LOGS_DIR).join("ffffffff.ymlog"),
            MasterKey::derive("다른 암호".as_bytes(), &generate_salt()).unwrap(),
        );
        foreign.append("automerge change 가 아닌 쓰레기".as_bytes()).unwrap();

        // 이질 로그가 있어도 병합은 성공하고 내 메모는 살아 있어야 한다.
        a.rebuild().unwrap();
        assert_eq!(a.store().get(&memo.id).unwrap().unwrap().title, "내 메모");

        fs::remove_dir_all(&dir).ok();
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

    /// 정본 salt 로 vault.json 을 덮어쓴다(테스트에서 Syncthing 충돌 해소를 흉내). 진
    /// 헤더는 `vault.sync-conflict-*.json` 으로 남겨, 옛 salt 를 찾을 수 있게 한다.
    fn write_header(dir: &Path, name: &str, password: &[u8], salt: &Salt) {
        let key = MasterKey::derive(password, salt).unwrap();
        let header = VaultHeader {
            version: 1,
            salt: to_hex(salt),
            key_check: to_hex(&key.encrypt(KEY_CHECK).unwrap()),
        };
        fs::write(dir.join(name), serde_json::to_vec_pretty(&header).unwrap()).unwrap();
    }

    /// 갈라진 키 자가 치유: 내 로그가 옛 salt 로 암호화돼 있고 vault.json 이 정본
    /// salt 로 수렴하면, open 이 내 로그를 정본 키로 재암호화해 메모를 되살려야 한다.
    #[test]
    fn heals_divergent_vault_key_on_open() {
        let dir = temp_dir();
        let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

        // 이 기기가 옛(진) salt 로 vault 를 만들고 메모를 쓴다.
        let memo;
        {
            let mut v = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
            memo = Memo::new("살아남아야 할 메모", "본문");
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

        // Syncthing 충돌 해소 흉내: 진 헤더는 conflict 로, vault.json 은 정본 salt 로.
        fs::rename(
            dir.join(HEADER_FILE),
            dir.join("vault.sync-conflict-20260101-120000-AAAAAAA.json"),
        )
        .unwrap();
        let canonical_salt = generate_salt();
        assert_ne!(canonical_salt, old_salt);
        write_header(&dir, HEADER_FILE, b"pw", &canonical_salt);

        // 같은 암호로 재오픈 → 치유 후 메모가 살아 있어야 한다.
        let v = Vault::open(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
        assert_eq!(v.store().get(&memo.id).unwrap().unwrap().title, "살아남아야 할 메모");

        // 내 로그가 이제 정본 키로 직접 열려야 한다(재암호화 확인).
        let device_id = Store::open(&db).unwrap().device_id().unwrap();
        let canonical_key = MasterKey::derive(b"pw", &canonical_salt).unwrap();
        let own_log = ChangeLog::open(
            dir.join(LOGS_DIR).join(format!("{device_id}.{LOG_EXT}")),
            canonical_key,
        );
        assert!(own_log.read_all().is_ok());

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

    /// "자동 잠금 해제": 캐시해 둔 원시 키만으로 암호 없이 같은 vault 가 열려야 한다.
    #[test]
    fn cached_key_opens_vault_without_password() {
        let dir = temp_dir();
        let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

        let memo = Memo::new("자동 해제로 볼 메모", "본문");
        let key_bytes = {
            let mut vault = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
            vault.upsert(&memo).unwrap();
            vault.key_bytes()
        };

        let key = MasterKey::from_bytes(&key_bytes).unwrap();
        let vault = Vault::open_with_key(&dir, key, Store::open(&db).unwrap()).unwrap();
        assert_eq!(vault.store().list().unwrap(), vec![memo]);

        // 엉뚱한 키는 헤더 카나리에서 걸러진다.
        let bogus = MasterKey::from_bytes(&[7u8; crate::crypto::KEY_LEN]).unwrap();
        assert!(Vault::open_with_key(&dir, bogus, Store::open_in_memory().unwrap()).is_err());

        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&db).ok();
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

    /// 그룹도 로그를 통해 다른 기기로 전파되고, 메모의 소속도 함께 넘어가야 한다.
    #[test]
    fn groups_sync_across_devices() {
        let dir = temp_dir();

        let group;
        let memo;
        {
            let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
            group = Group::new("업무");
            a.upsert_group(&group).unwrap();
            memo = {
                let mut m = Memo::new("보고서", "");
                m.group_id = group.id.clone();
                m
            };
            a.upsert(&memo).unwrap();
        }

        // 다른 기기(빈 캐시)에서 로그만으로 복원.
        let b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let groups = b.store().list_groups().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "업무");
        assert_eq!(b.store().get(&memo.id).unwrap().unwrap().group_id, group.id);

        fs::remove_dir_all(&dir).ok();
    }

    /// 그룹을 지워도 안에 있던 메모/하위 그룹은 사라지지 않고 상위로 올라와야 한다.
    #[test]
    fn deleting_group_lifts_children_instead_of_destroying() {
        let dir = temp_dir();
        let mut v = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();

        let outer = Group::new("상위");
        v.upsert_group(&outer).unwrap();
        let mut inner = Group::new("하위");
        inner.parent_id = outer.id.clone();
        v.upsert_group(&inner).unwrap();
        let mut memo = Memo::new("안에 있던 메모", "");
        memo.group_id = outer.id.clone();
        v.upsert(&memo).unwrap();

        v.delete_group(&outer.id).unwrap();

        // 상위 그룹만 사라지고, 하위 그룹과 메모는 최상위로.
        let groups = v.store().list_groups().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].id, inner.id);
        assert_eq!(groups[0].parent_id, "");
        let survived = v.store().get(&memo.id).unwrap().unwrap();
        assert_eq!(survived.group_id, "");

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
