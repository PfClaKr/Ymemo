//! Vault: 암호화 change 로그 + 로컬 SQLite 캐시를 묶는 상위 계층.
//!
//! 동기화 디렉터리(이후 Syncthing 공유 폴더) 레이아웃:
//! ```text
//! <vault_dir>/
//!   vault.json            ← 헤더: salt + key_check. 생성 시 1회 기록, 이후 불변 → 동기화 충돌 없음.
//!   logs/<device_id>.ymlog ← 기기별 append-only 암호화 로그. 각 기기는 자기 파일에만 쓴다.
//! ```
//! 로그가 진실의 원천(source of truth)이고 SQLite 는 재구성 가능한 로컬 캐시다.
//! 모든 쓰기는 Vault 를 거쳐 store 적용 + 로그 append 가 함께 일어난다.
//!
//! 다기기 병합: 모든 로그의 change 를 `(timestamp, device_id, seq)` 로 정렬해 재생하는
//! 임시 LWW. 순서 무관 병합은 이후 CRDT(Automerge) 단계에서 대체된다.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::changelog::{Change, ChangeLog, ChangeOp};
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
    next_seq: u64,
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
    /// 모든 기기 로그를 병합 재생해 로컬 캐시를 최신화한다.
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
            .map_err(|_| anyhow!("마스터 암호가 틀렸다"))?;
        if check != KEY_CHECK {
            bail!("key_check 불일치 (헤더 손상?)");
        }

        let device_id = store.device_id()?;
        fs::create_dir_all(dir.join(LOGS_DIR))?;
        let own_log = ChangeLog::open(
            dir.join(LOGS_DIR).join(format!("{device_id}.{LOG_EXT}")),
            key.clone(),
        );
        // 자기 로그의 마지막 seq 다음부터 이어 쓴다.
        let next_seq = own_log.read_all()?.last().map_or(0, |c| c.seq + 1);

        let vault = Self { dir, store, key, device_id, own_log, next_seq };
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

    /// 메모 삽입/갱신: 캐시 적용 + 자기 로그에 append.
    pub fn upsert(&mut self, memo: &Memo) -> Result<()> {
        self.record(ChangeOp::Upsert(memo.clone()))
    }

    /// 메모 삭제: 캐시 적용 + 자기 로그에 append.
    pub fn delete(&mut self, id: &str) -> Result<()> {
        self.record(ChangeOp::Delete { id: id.to_string() })
    }

    fn record(&mut self, op: ChangeOp) -> Result<()> {
        self.store.apply(&op)?;
        self.own_log
            .append(&Change::new(&self.device_id, self.next_seq, op))?;
        self.next_seq += 1;
        Ok(())
    }

    /// `logs/` 의 모든 기기 로그를 복호화·병합해 로컬 캐시를 처음부터 재구성한다.
    ///
    /// Syncthing 이 다른 기기의 로그 파일을 가져다 놓으면, 이 호출 한 번으로 반영된다.
    pub fn rebuild(&self) -> Result<()> {
        let logs_dir = self.dir.join(LOGS_DIR);
        let mut changes: Vec<Change> = Vec::new();
        if logs_dir.exists() {
            for entry in fs::read_dir(&logs_dir)? {
                let path = entry?.path();
                if path.extension().and_then(|e| e.to_str()) == Some(LOG_EXT) {
                    changes.extend(ChangeLog::open(&path, self.key.clone()).read_all()?);
                }
            }
        }
        // 임시 전순서: 시각 → 기기 → 기기 내 순서. (CRDT 도입 전 LWW)
        changes.sort_by(|a, b| {
            (a.timestamp, &a.device_id, a.seq).cmp(&(b.timestamp, &b.device_id, b.seq))
        });

        self.store.clear_memos()?;
        for change in &changes {
            self.store.apply(&change.op)?;
        }
        Ok(())
    }

    /// 읽기용 로컬 캐시 접근 (list/get 등).
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// 이 기기의 식별자.
    pub fn device_id(&self) -> &str {
        &self.device_id
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

        let m1;
        {
            let mut vault =
                Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
            m1 = Memo::new("살아남을 메모", "본문");
            vault.upsert(&m1).unwrap();
            let dead = Memo::new("지워질 메모", "");
            vault.upsert(&dead).unwrap();
            vault.delete(&dead.id).unwrap();
        } // vault 와 인메모리 캐시 소멸 — 로그 파일만 남는다.

        // 새(빈) 캐시로 다시 열면 로그만으로 상태가 복원돼야 한다.
        let vault = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let all = vault.store().list().unwrap();
        assert_eq!(all, vec![m1]);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn wrong_password_rejected_by_key_check() {
        let dir = temp_dir();
        Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        // 로그가 비어 있어도 헤더 카나리만으로 즉시 거부돼야 한다.
        let err = Vault::open(&dir, b"wrong", Store::open_in_memory().unwrap());
        assert!(err.is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn seq_continues_after_reopen() {
        let dir = temp_dir();
        let db = std::env::temp_dir().join(format!("ymemo-cache-{}.db", uuid::Uuid::new_v4()));

        {
            let mut vault = Vault::create(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
            vault.upsert(&Memo::new("a", "")).unwrap();
            vault.upsert(&Memo::new("b", "")).unwrap();
        }
        {
            // 같은 캐시(=같은 device_id)로 다시 열면 seq 2 부터 이어 써야 한다.
            let mut vault = Vault::open(&dir, b"pw", Store::open(&db).unwrap()).unwrap();
            assert_eq!(vault.next_seq, 2);
            vault.upsert(&Memo::new("c", "")).unwrap();
            assert_eq!(vault.next_seq, 3);
            assert_eq!(vault.store().list().unwrap().len(), 3);
        }

        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&db).ok();
    }

    /// 두 기기가 같은 vault 디렉터리를 공유하는 상황(=Syncthing 동기화 후)을 흉내낸다.
    #[test]
    fn two_devices_merge_last_write_wins() {
        let dir = temp_dir();
        let sleep = || std::thread::sleep(std::time::Duration::from_millis(5)); // timestamp 순서 보장

        // 기기 A: 메모 생성.
        let mut a = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let mut memo = Memo::new("원본 제목", "");
        a.upsert(&memo).unwrap();
        sleep();

        // 기기 B: (별도 캐시 → 별도 device_id) 열면 A 의 메모가 보이고, 제목을 고친다.
        let mut b = Vault::open(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        assert_ne!(a.device_id(), b.device_id());
        assert_eq!(b.store().list().unwrap().len(), 1);
        memo.title = "B가 고친 제목".into();
        b.upsert(&memo).unwrap();
        sleep();

        // 기기 A: rebuild 로 B 의 나중 변경이 이겨야 한다 (LWW).
        a.rebuild().unwrap();
        assert_eq!(a.store().get(&memo.id).unwrap().unwrap().title, "B가 고친 제목");

        // 로그 파일은 기기당 하나씩 두 개.
        let logs = fs::read_dir(dir.join(LOGS_DIR)).unwrap().count();
        assert_eq!(logs, 2);

        fs::remove_dir_all(&dir).ok();
    }
}
