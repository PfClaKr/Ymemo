//! 내용해시(content-addressed) blob 저장소 — 사진 첨부의 실제 바이트가 사는 곳.
//!
//! 레이아웃: `<vault_dir>/blobs/<sha256(평문) hex>.ymblob`
//!
//! 세 가지가 이 설계의 요점이다.
//!
//! 1. **이름이 곧 내용해시라 불변이다.** 한 번 쓰인 파일은 절대 바뀌지 않으므로,
//!    기기별 append-only 로그와 같은 이유로 동기화 충돌이 0이다. 같은 사진을 두 기기가
//!    붙여도 파일 하나로 합쳐진다.
//! 2. **암호화가 convergent 하다.** nonce 를 평문 해시에서 유도해, 같은 평문이면 어느
//!    기기에서 암호화해도 바이트가 완전히 같다. 랜덤 nonce 를 쓰면 파일 이름은 같은데
//!    내용이 달라져 Syncthing 이 충돌 파일을 만든다. 평문이 다르면 해시가 달라 nonce 도
//!    달라지므로 nonce 재사용 문제는 생기지 않는다.
//! 3. **지우지 않는다(GC 없음).** 메모에서 사진을 떼어도 blob 파일은 남는다. 어떤 기기가
//!    아직 그 blob 을 참조하는지는 append-only 모델에서 확실히 알 수 없고, 잘못 지우면
//!    다른 기기의 메모에서 사진이 사라지기 때문이다. 저장 공간과 안전을 맞바꾼 선택이다.

use anyhow::Result;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

use crate::crypto::{MasterKey, NONCE_LEN};

const BLOBS_DIR: &str = "blobs";
const BLOB_EXT: &str = "ymblob";
/// nonce 유도용 도메인 구분자. 다른 용도의 해시와 값이 겹치지 않게 한다.
const NONCE_DOMAIN: &[u8] = b"ymemo-blob-nonce-v1";

/// vault 안의 blob 디렉터리. 키를 소유하며 읽고 쓸 때 암·복호화한다.
pub struct BlobStore {
    dir: PathBuf,
    key: MasterKey,
}

impl BlobStore {
    /// `<vault_dir>/blobs` 를 연다 (디렉터리는 첫 쓰기 때 만든다).
    pub fn open(vault_dir: impl AsRef<Path>, key: MasterKey) -> Self {
        Self {
            dir: vault_dir.as_ref().join(BLOBS_DIR),
            key,
        }
    }

    /// 평문 바이트를 저장하고 내용해시(hex)를 돌려준다.
    ///
    /// 같은 내용이 이미 있으면 다시 쓰지 않는다 — 파일이 불변이라 덮어쓸 이유가 없고,
    /// 같은 사진을 여러 메모에 붙여도 저장은 한 번뿐이다.
    pub fn put(&self, plaintext: &[u8]) -> Result<String> {
        let hash = content_hash(plaintext);
        let path = self.path(&hash);
        if path.exists() {
            return Ok(hash);
        }
        fs::create_dir_all(&self.dir)?;
        let sealed = self.key.encrypt_with_nonce(plaintext, &nonce_for(&hash))?;
        // 같은 디렉터리에 임시로 쓴 뒤 rename — 반쯤 쓰인 파일이 동기화되지 않게 한다.
        let tmp = path.with_extension("part");
        fs::write(&tmp, &sealed)?;
        fs::rename(&tmp, &path)?;
        Ok(hash)
    }

    /// 해시로 평문 바이트를 읽는다. 파일이 없거나(아직 동기화 전) 키가 다르면 에러.
    pub fn get(&self, hash: &str) -> Result<Vec<u8>> {
        let data = fs::read(self.path(hash))?;
        self.key.decrypt(&data)
    }

    /// 이 기기에 blob 파일이 이미 도착해 있는가. (없으면 UI 는 자리표시자를 보여주면 된다)
    pub fn has(&self, hash: &str) -> bool {
        self.path(hash).exists()
    }

    /// 저장 경로. 해시는 hex 라 경로 조작에 쓰일 수 없다.
    pub fn path(&self, hash: &str) -> PathBuf {
        self.dir.join(format!("{hash}.{BLOB_EXT}"))
    }
}

/// 평문의 sha256 을 hex 로. blob 이름이자 첨부가 가리키는 식별자다.
pub fn content_hash(plaintext: &[u8]) -> String {
    let digest = Sha256::digest(plaintext);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// 내용해시에서 nonce 를 유도한다(convergent 암호화의 핵심).
fn nonce_for(hash: &str) -> [u8; NONCE_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(NONCE_DOMAIN);
    hasher.update(hash.as_bytes());
    let digest = hasher.finalize();
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&digest[..NONCE_LEN]);
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::generate_salt;

    fn store(dir: &Path) -> BlobStore {
        let key = MasterKey::derive(b"pw", &generate_salt()).unwrap();
        BlobStore::open(dir, key)
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ymemo-blob-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trips_and_dedupes() {
        let dir = tmp_dir("round");
        let s = store(&dir);

        let data = b"pretend this is a photo".repeat(100);
        let hash = s.put(&data).unwrap();
        assert!(s.has(&hash));
        assert_eq!(s.get(&hash).unwrap(), data);

        // 같은 내용을 또 넣어도 같은 해시 하나뿐.
        assert_eq!(s.put(&data).unwrap(), hash);
        let files: Vec<_> = fs::read_dir(dir.join(BLOBS_DIR)).unwrap().collect();
        assert_eq!(files.len(), 1);
    }

    /// 같은 키·같은 평문이면 **바이트까지 같아야** 한다. 이게 깨지면 두 기기가 같은
    /// 파일 이름에 다른 내용을 써서 동기화 충돌이 난다.
    #[test]
    fn encryption_is_convergent() {
        let dir_a = tmp_dir("conv-a");
        let dir_b = tmp_dir("conv-b");
        let salt = generate_salt();
        let key_a = MasterKey::derive(b"pw", &salt).unwrap();
        let key_b = MasterKey::derive(b"pw", &salt).unwrap();
        let (a, b) = (BlobStore::open(&dir_a, key_a), BlobStore::open(&dir_b, key_b));

        let data = b"same photo on two devices";
        let hash_a = a.put(data).unwrap();
        let hash_b = b.put(data).unwrap();
        assert_eq!(hash_a, hash_b);
        assert_eq!(fs::read(a.path(&hash_a)).unwrap(), fs::read(b.path(&hash_b)).unwrap());
    }

    /// 평문이 다르면 nonce 도 달라야 한다 (같으면 키스트림이 겹쳐 치명적).
    #[test]
    fn different_content_gets_different_nonce() {
        assert_ne!(nonce_for(&content_hash(b"a")), nonce_for(&content_hash(b"b")));
    }

    #[test]
    fn missing_blob_is_an_error_not_a_panic() {
        let dir = tmp_dir("missing");
        let s = store(&dir);
        assert!(!s.has("00ff"));
        assert!(s.get("00ff").is_err());
    }
}
