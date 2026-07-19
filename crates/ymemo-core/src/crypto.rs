//! E2E 암호화 기본 요소 (순수 Rust RustCrypto).
//!
//! - 키 유도: 마스터 암호 + salt → **Argon2id** → 32바이트 대칭키.
//! - 암호화: **XChaCha20-Poly1305** (24바이트 nonce → 랜덤 nonce 충돌 걱정 없음).
//!
//! 단일 사용자·다기기 모델이므로 대칭키는 하나다(이후 QR 로 기기 간 전달).
//! salt 는 비밀이 아니며, 마스터 암호에서 같은 키를 재현하려면 반드시 함께 보관해야 한다.

use anyhow::{anyhow, Result};
use argon2::Argon2;
use chacha20poly1305::{
    aead::Aead, KeyInit, XChaCha20Poly1305, XNonce,
};
use rand::{rngs::OsRng, RngCore};

pub const SALT_LEN: usize = 16;
pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 24;

/// Argon2id salt (비밀 아님, 키와 함께 보관).
pub type Salt = [u8; SALT_LEN];

/// 암호학적 난수 salt 생성.
pub fn generate_salt() -> Salt {
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    salt
}

/// 마스터 암호에서 유도한 대칭 암호화 키.
///
/// 원시 키 바이트는 밖으로 노출하지 않고, 준비된 AEAD cipher 만 들고 있는다.
/// Clone 은 같은 키를 여러 로그(vault 의 기기별 로그들)에서 쓰기 위함.
#[derive(Clone)]
pub struct MasterKey {
    cipher: XChaCha20Poly1305,
}

impl MasterKey {
    /// 마스터 암호 + salt → Argon2id → 대칭키.
    ///
    /// 같은 (암호, salt) 조합은 항상 같은 키를 재현한다.
    pub fn derive(password: &[u8], salt: &Salt) -> Result<Self> {
        let mut key = [0u8; KEY_LEN];
        Argon2::default()
            .hash_password_into(password, salt, &mut key)
            .map_err(|e| anyhow!("Argon2id 키 유도 실패: {e}"))?;
        let cipher = XChaCha20Poly1305::new_from_slice(&key)
            .map_err(|e| anyhow!("cipher 초기화 실패: {e}"))?;
        Ok(Self { cipher })
    }

    /// 평문을 암호화. 반환 형식: `nonce(24B) || ciphertext+tag`.
    ///
    /// nonce 는 매 호출마다 랜덤 생성되므로 같은 평문도 매번 다른 암호문이 된다.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = XNonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| anyhow!("암호화 실패: {e}"))?;

        let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// `encrypt` 가 만든 `nonce || ciphertext` 를 복호화.
    ///
    /// 키가 다르거나 데이터가 변조되면 Poly1305 인증이 실패해 에러가 난다.
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>> {
        if data.len() < NONCE_LEN {
            return Err(anyhow!("암호문이 너무 짧다 ({}B < {NONCE_LEN}B)", data.len()));
        }
        let (nonce_bytes, ciphertext) = data.split_at(NONCE_LEN);
        let nonce = XNonce::from_slice(nonce_bytes);
        self.cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| anyhow!("복호화 실패 (키 불일치 또는 변조): {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_is_deterministic() {
        let salt = generate_salt();
        let a = MasterKey::derive(b"correct horse", &salt).unwrap();
        let b = MasterKey::derive(b"correct horse", &salt).unwrap();
        // 같은 키면 서로의 암호문을 복호화할 수 있어야 한다.
        let ct = a.encrypt(b"hello").unwrap();
        assert_eq!(b.decrypt(&ct).unwrap(), b"hello");
    }

    #[test]
    fn roundtrip() {
        let salt = generate_salt();
        let key = MasterKey::derive(b"pw", &salt).unwrap();
        let msg = "한글 메모 내용 🦀".as_bytes();
        let ct = key.encrypt(msg).unwrap();
        assert_ne!(&ct[NONCE_LEN..], msg); // 실제로 암호화됨
        assert_eq!(key.decrypt(&ct).unwrap(), msg);
    }

    #[test]
    fn wrong_password_fails() {
        let salt = generate_salt();
        let ct = MasterKey::derive(b"pw", &salt).unwrap().encrypt(b"secret").unwrap();
        let wrong = MasterKey::derive(b"nope", &salt).unwrap();
        assert!(wrong.decrypt(&ct).is_err());
    }

    #[test]
    fn tamper_fails() {
        let salt = generate_salt();
        let key = MasterKey::derive(b"pw", &salt).unwrap();
        let mut ct = key.encrypt(b"secret").unwrap();
        let last = ct.len() - 1;
        ct[last] ^= 0x01; // tag 변조
        assert!(key.decrypt(&ct).is_err());
    }
}
