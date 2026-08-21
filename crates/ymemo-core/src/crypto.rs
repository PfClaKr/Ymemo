//! E2E encryption primitives (pure Rust, RustCrypto).
//!
//! - Key derivation: master password + salt -> Argon2id -> 32-byte symmetric key.
//! - Cipher: XChaCha20-Poly1305 (24-byte nonce, so random nonces never collide).
//!
//! One user with many devices means exactly one symmetric key. The salt is not secret,
//! but it must be kept to re-derive that key from the password.

use anyhow::{anyhow, Result};
use ymemo_i18n::t;
use argon2::Argon2;
use chacha20poly1305::{
    aead::Aead, KeyInit, XChaCha20Poly1305, XNonce,
};
use rand::{rngs::OsRng, RngCore};

pub const SALT_LEN: usize = 16;
pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 24;

/// Argon2id salt. Not secret, but must be stored next to the vault.
pub type Salt = [u8; SALT_LEN];

pub fn generate_salt() -> Salt {
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    salt
}

/// A fresh random 32-byte key, for the vault's data key (see [`crate::vault`]).
///
/// Unlike [`MasterKey::derive`] this has no password behind it: the data key is what
/// actually encrypts logs and blobs, and it is stored wrapped under a derived key. That
/// indirection is what lets the master password change without re-encrypting anything.
pub fn generate_key() -> [u8; KEY_LEN] {
    let mut key = [0u8; KEY_LEN];
    OsRng.fill_bytes(&mut key);
    key
}

/// Symmetric key derived from the master password.
///
/// `Clone` so one key can back every per-device log in a vault.
#[derive(Clone)]
pub struct MasterKey {
    /// Raw derived key. Only leaves through [`Self::to_bytes`]; read its docs first.
    raw: [u8; KEY_LEN],
    cipher: XChaCha20Poly1305,
}

impl MasterKey {
    /// Derives the key from the master password; the same (password, salt) always
    /// reproduces the same key.
    pub fn derive(password: &[u8], salt: &Salt) -> Result<Self> {
        let mut key = [0u8; KEY_LEN];
        Argon2::default()
            .hash_password_into(password, salt, &mut key)
            .map_err(|e| anyhow!(t!("core.argon2_failed", error = e)))?;
        Self::from_bytes(&key)
    }

    /// Restores a key from raw bytes (the unlock-without-password path).
    pub fn from_bytes(key: &[u8; KEY_LEN]) -> Result<Self> {
        let cipher = XChaCha20Poly1305::new_from_slice(key)
            .map_err(|e| anyhow!(t!("core.cipher_init_failed", error = e)))?;
        Ok(Self { raw: *key, cipher })
    }

    /// Raw derived key bytes.
    ///
    /// **Writing this to disk defeats the vault's at-rest encryption** — it opens every
    /// memo without the master password. Only for the opt-in "stay unlocked" cache.
    pub fn to_bytes(&self) -> [u8; KEY_LEN] {
        self.raw
    }

    /// Encrypts to `nonce(24B) || ciphertext+tag` with a fresh random nonce per call.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = XNonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| anyhow!(t!("core.encrypt_failed", error = e)))?;

        let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Encrypts with a caller-chosen nonce; same layout as [`Self::encrypt`].
    ///
    /// **Not for general data.** Reusing a nonce for a different plaintext under the same
    /// key repeats the XChaCha20 keystream. Only for content-hashed blobs
    /// ([`crate::blob`]), where the nonce is derived from the plaintext. In exchange the
    /// encryption is convergent: the same photo yields the same bytes on every device,
    /// so syncing it never conflicts.
    pub fn encrypt_with_nonce(&self, plaintext: &[u8], nonce_bytes: &[u8; NONCE_LEN]) -> Result<Vec<u8>> {
        let nonce = XNonce::from_slice(nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| anyhow!(t!("core.encrypt_failed", error = e)))?;

        let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        out.extend_from_slice(nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Decrypts `nonce || ciphertext`. Fails if the key is wrong or the data was tampered
    /// with (Poly1305 authentication).
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>> {
        if data.len() < NONCE_LEN {
            return Err(anyhow!(t!("core.ciphertext_too_short", len = data.len(), min = NONCE_LEN)));
        }
        let (nonce_bytes, ciphertext) = data.split_at(NONCE_LEN);
        let nonce = XNonce::from_slice(nonce_bytes);
        self.cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| anyhow!(t!("core.decrypt_failed", error = e)))
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
        // Same key: each must decrypt the other's ciphertext.
        let ct = a.encrypt(b"hello").unwrap();
        assert_eq!(b.decrypt(&ct).unwrap(), b"hello");
    }

    #[test]
    fn roundtrip() {
        let salt = generate_salt();
        let key = MasterKey::derive(b"pw", &salt).unwrap();
        let msg = "한글 메모 내용 🦀".as_bytes();
        let ct = key.encrypt(msg).unwrap();
        assert_ne!(&ct[NONCE_LEN..], msg); // actually encrypted
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
        ct[last] ^= 0x01; // corrupt the tag
        assert!(key.decrypt(&ct).is_err());
    }
}
