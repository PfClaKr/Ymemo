//! Device pairing codes and the check digits that go with them.
//!
//! Adding a device takes one scan and one tap:
//! 1. Device A shows its Syncthing device id as a pairing code (string or QR).
//! 2. Device B reads it, registers A as a peer and shares the vault folder with it. B then
//!    starts trying to connect, and A — which has never heard of B — turns that into a
//!    pending request ([`crate::sync::Syncthing::pending_devices`]).
//! 3. A shows the request and, if the user allows it, shares the folder back. That is the
//!    other half of the link, and nobody has to go and scan a second code.
//! 4. Once Syncthing carries the vault over, B derives the same key from the synced
//!    `vault.json` salt plus the master password — no key is ever transmitted.
//!
//! **Step 3 is the only thing standing between a stranger and the vault directory.** A's
//! pairing code is not a secret: it is shown on a screen, and a device id is public by
//! design. Anyone who copies it can make a request appear on A. What they cannot do is make
//! A's user press "allow" — and to give that press something to check,
//! [`verification_code`] turns the two device ids into eight characters that both screens
//! show and the user compares, the same numeric-comparison step Bluetooth pairing uses.
//!
//! The code is a plain versioned string, which makes it easy to render as a QR.

use anyhow::{bail, Result};
use sha2::{Digest, Sha256};
use ymemo_i18n::t;

/// Version prefix of the code format.
const PREFIX: &str = "YMEMO1:";

/// A pairing code; currently just a Syncthing device ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCode {
    pub syncthing_device_id: String,
}

impl PairingCode {
    pub fn new(syncthing_device_id: impl Into<String>) -> Self {
        Self { syncthing_device_id: syncthing_device_id.into() }
    }

    /// Encodes for QR or copy-paste, e.g. `YMEMO1:ABCDEFG-...`
    pub fn encode(&self) -> String {
        format!("{PREFIX}{}", self.syncthing_device_id)
    }

    /// Parses a code; a bare device ID without the prefix is accepted too.
    pub fn decode(s: &str) -> Result<Self> {
        let id = s.trim().strip_prefix(PREFIX).unwrap_or(s.trim()).trim();
        if id.len() < 7 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            bail!(t!("core.bad_pairing_code"));
        }
        Ok(Self::new(id.to_ascii_uppercase()))
    }
}

/// Domain separator, so this hash can never be confused with another use of the same ids.
const VERIFY_DOMAIN: &[u8] = b"YMEMO-PAIR-VERIFY-1";
/// Characters in a verification code, before the dash is inserted.
const VERIFY_LEN: usize = 8;

/// Eight characters both devices derive from the pair of device ids, e.g. `7QK2-M4XB`.
///
/// Shown on the screen that asks for a code and on the screen that approves the request, so
/// the user can see the two match before allowing anything. It carries no secret — both ids
/// are public — and is not a credential: it proves the two screens are talking about *the
/// same pair of devices*, which is exactly what someone replaying a copied pairing code
/// cannot arrange.
///
/// Order-independent, since each side learns the ids in the opposite order, and insensitive
/// to case and dashes so a hand-typed id verifies the same as a scanned one.
pub fn verification_code(one_device_id: &str, other_device_id: &str) -> String {
    let mut ids = [canonical_id(one_device_id), canonical_id(other_device_id)];
    ids.sort();

    let mut hasher = Sha256::new();
    hasher.update(VERIFY_DOMAIN);
    for id in &ids {
        // Length-prefixed, so two different splits of the same characters cannot collide.
        hasher.update((id.len() as u32).to_le_bytes());
        hasher.update(id.as_bytes());
    }
    let digest = hasher.finalize();

    let chars: String = digest
        .iter()
        .take(VERIFY_LEN)
        .map(|b| crate::recovery::ALPHABET[(b & 31) as usize] as char)
        .collect();
    format!("{}-{}", &chars[..VERIFY_LEN / 2], &chars[VERIFY_LEN / 2..])
}

/// A device id with the dashes dropped and the case folded, for hashing and comparing.
fn canonical_id(id: &str) -> String {
    id.chars().filter(|c| *c != '-').flat_map(char::to_uppercase).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID_A: &str = "MFZWI3D-BONSGYC-YLTMRWG-C43ENRQ-XGEZLTO-RUW4Z3V-KMXGG33-NMVGG2A";
    const ID_B: &str = "5W64LZH-OG2P24D-JPGNCW6-6RW26TH-W76GYJ7-4ZC4IHP-KBG2ZH3-3FMFDQI";

    #[test]
    fn verification_code_is_the_same_from_either_side() {
        assert_eq!(verification_code(ID_A, ID_B), verification_code(ID_B, ID_A));
    }

    #[test]
    fn verification_code_ignores_case_and_dashes() {
        let plain = ID_A.replace('-', "").to_ascii_lowercase();
        assert_eq!(verification_code(ID_A, ID_B), verification_code(&plain, ID_B));
    }

    #[test]
    fn verification_code_is_well_formed() {
        let code = verification_code(ID_A, ID_B);
        assert_eq!(code.len(), VERIFY_LEN + 1, "8 characters and one dash");
        assert_eq!(code.matches('-').count(), 1);
        assert!(
            code.chars().filter(|c| *c != '-').all(|c| crate::recovery::ALPHABET.contains(&(c as u8))),
            "{code} must avoid the confusable characters"
        );
    }

    #[test]
    fn a_different_peer_gives_a_different_code() {
        let other = "AAAAAAA-BBBBBBB-CCCCCCC-DDDDDDD-EEEEEEE-FFFFFFF-GGGGGGG-HHHHHHH";
        assert_ne!(verification_code(ID_A, ID_B), verification_code(ID_A, other));
    }

    #[test]
    fn encode_decode_roundtrip() {
        let code = PairingCode::new("MFZWI3D-BONSGYC-YLTMRWG-C43ENRQ-XGEZLTO-RUW4Z3V-KMXGG33-NMVGG2A");
        let s = code.encode();
        assert!(s.starts_with("YMEMO1:"));
        assert_eq!(PairingCode::decode(&s).unwrap(), code);
    }

    #[test]
    fn decode_accepts_raw_device_id() {
        let decoded = PairingCode::decode("  mfzwi3d-bonsgyc  ").unwrap();
        assert_eq!(decoded.syncthing_device_id, "MFZWI3D-BONSGYC");
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(PairingCode::decode("").is_err());
        assert!(PairingCode::decode("YMEMO1:").is_err());
        assert!(PairingCode::decode("hello world!").is_err());
    }
}
