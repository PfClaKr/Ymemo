//! 기기별 append-only 암호화 레코드 로그.
//!
//! 데이터 모델의 핵심(재론 금지 결정): 각 기기는 자기 로그 파일 끝에만 덧붙인다.
//! 파일을 되쓰지 않으므로 동기화 시 파일 충돌이 0 이다. 각 레코드는 XChaCha20-Poly1305
//! 로 개별 암호화된다.
//!
//! 레코드 내용은 이 계층에선 불투명한 바이트다 — 현재는 automerge change 바이너리가
//! 들어간다(actor·seq·타임스탬프·의존성은 automerge change 에 내장). 해석은 vault 몫.
//!
//! 온디스크 포맷(레코드 반복):
//! ```text
//! [u32 LE 레코드 길이][nonce(24B) || ciphertext+tag] ...
//! ```

use anyhow::{anyhow, Result};
use ymemo_i18n::t;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use crate::crypto::MasterKey;

/// 암호화된 append-only 레코드 로그 파일.
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

    /// 레코드(평문 바이트) 하나를 암호화해 파일 끝에 덧붙인다.
    pub fn append(&self, plaintext: &[u8]) -> Result<()> {
        let record = self.key.encrypt(plaintext)?;
        let len = u32::try_from(record.len())
            .map_err(|_| anyhow!(t!("core.record_too_large", len = record.len())))?;

        let mut f = OpenOptions::new().create(true).append(true).open(&self.path)?;
        f.write_all(&len.to_le_bytes())?;
        f.write_all(&record)?;
        Ok(())
    }

    /// 로그를 처음부터 끝까지 읽어 복호화한 레코드들을 순서대로 반환.
    ///
    /// 파일이 없으면 빈 벡터.
    pub fn read_all(&self) -> Result<Vec<Vec<u8>>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let mut reader = BufReader::new(File::open(&self.path)?);
        let mut records = Vec::new();

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
            records.push(self.key.decrypt(&record)?);
        }
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{generate_salt, MasterKey};

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("ymemo-log-{}.bin", uuid::Uuid::new_v4()))
    }

    #[test]
    fn append_read_roundtrip() {
        let path = temp_path();
        let salt = generate_salt();
        let log = ChangeLog::open(&path, MasterKey::derive(b"pw", &salt).unwrap());

        log.append(b"record-1").unwrap();
        log.append("두 번째 🦀".as_bytes()).unwrap();

        // 파일엔 평문이 없어야 한다.
        let raw = std::fs::read(&path).unwrap();
        assert!(!raw.windows(8).any(|w| w == b"record-1"));

        // 별도 인스턴스(같은 키)로 읽어도 순서대로 복원.
        let log2 = ChangeLog::open(&path, MasterKey::derive(b"pw", &salt).unwrap());
        let records = log2.read_all().unwrap();
        assert_eq!(records, vec![b"record-1".to_vec(), "두 번째 🦀".as_bytes().to_vec()]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn wrong_key_fails() {
        let path = temp_path();
        let salt = generate_salt();
        ChangeLog::open(&path, MasterKey::derive(b"pw", &salt).unwrap())
            .append(b"secret")
            .unwrap();
        let wrong = ChangeLog::open(&path, MasterKey::derive(b"nope", &salt).unwrap());
        assert!(wrong.read_all().is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn missing_file_is_empty() {
        let salt = generate_salt();
        let log = ChangeLog::open(temp_path(), MasterKey::derive(b"pw", &salt).unwrap());
        assert!(log.read_all().unwrap().is_empty());
    }
}
