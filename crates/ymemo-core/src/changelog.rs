//! 기기별 append-only 암호화 change 로그.
//!
//! 데이터 모델의 핵심(재론 금지 결정): 각 기기는 자기 변경분만 로그 끝에 덧붙인다.
//! 파일을 되쓰지 않으므로 동기화 시 파일 충돌이 0 이다. 각 레코드는 XChaCha20-Poly1305
//! 로 개별 암호화되며, 복호화 후 순서대로 재생하면 로컬 SQLite 상태가 재구성된다.
//!
//! 온디스크 포맷(레코드 반복):
//! ```text
//! [u32 LE 레코드 길이][nonce(24B) || ciphertext+tag] ...
//! ```
//! 평문은 `Change` 의 JSON 직렬화다. (이후 CRDT=Automerge 로 교체 예정, 지금은 단순 저장.)

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use crate::crypto::MasterKey;
use crate::{now_millis, Memo, Store};

/// 한 change 가 담는 연산.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChangeOp {
    /// 메모 삽입 또는 갱신 (전체 스냅샷).
    Upsert(Memo),
    /// 메모 삭제.
    Delete { id: String },
}

/// 로그에 기록되는 변경 한 건.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Change {
    /// 기록한 기기 식별자.
    pub device_id: String,
    /// 기기 내 단조 증가 시퀀스 (기기별 순서 보장).
    pub seq: u64,
    /// 기록 시각 (Unix epoch millis).
    pub timestamp: i64,
    pub op: ChangeOp,
}

impl Change {
    /// 현재 시각으로 새 change 생성.
    pub fn new(device_id: impl Into<String>, seq: u64, op: ChangeOp) -> Self {
        Self {
            device_id: device_id.into(),
            seq,
            timestamp: now_millis(),
            op,
        }
    }
}

/// 암호화된 append-only change 로그 파일.
///
/// 키를 소유하며, append/read 시 자동으로 암·복호화한다.
pub struct ChangeLog {
    path: PathBuf,
    key: MasterKey,
}

impl ChangeLog {
    /// 경로에 로그를 연다 (파일은 첫 append 때 생성). 키는 이 로그가 소유한다.
    pub fn open(path: impl AsRef<Path>, key: MasterKey) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            key,
        }
    }

    /// change 하나를 암호화해 파일 끝에 덧붙인다.
    pub fn append(&self, change: &Change) -> Result<()> {
        let plaintext = serde_json::to_vec(change)?;
        let record = self.key.encrypt(&plaintext)?;
        let len = u32::try_from(record.len())
            .map_err(|_| anyhow!("레코드가 u32 범위를 넘음 ({}B)", record.len()))?;

        let mut f = OpenOptions::new().create(true).append(true).open(&self.path)?;
        f.write_all(&len.to_le_bytes())?;
        f.write_all(&record)?;
        Ok(())
    }

    /// 로그를 처음부터 끝까지 읽어 복호화한 change 들을 순서대로 반환.
    ///
    /// 파일이 없으면 빈 벡터.
    pub fn read_all(&self) -> Result<Vec<Change>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let mut reader = BufReader::new(File::open(&self.path)?);
        let mut changes = Vec::new();

        loop {
            let mut len_buf = [0u8; 4];
            match reader.read_exact(&mut len_buf) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
            let len = u32::from_le_bytes(len_buf) as usize;

            let mut record = vec![0u8; len];
            reader.read_exact(&mut record)?;
            let plaintext = self.key.decrypt(&record)?;
            changes.push(serde_json::from_slice(&plaintext)?);
        }
        Ok(changes)
    }

    /// 로그를 복호화해 전부 `store` 에 재생(replay)한다.
    ///
    /// 현재는 로그에 적힌 순서대로 적용한다(단일 기기 기준). 다기기 병합은
    /// 이후 CRDT 단계에서 순서 무관 병합으로 대체된다.
    pub fn rebuild_into(&self, store: &Store) -> Result<()> {
        for change in self.read_all()? {
            store.apply(&change.op)?;
        }
        Ok(())
    }
}
