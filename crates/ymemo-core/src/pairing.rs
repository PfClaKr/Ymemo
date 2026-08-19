//! Device pairing codes.
//!
//! Adding a device, both ways round:
//! 1. Device A shows its Syncthing device ID as a pairing code (string or QR).
//! 2. Device B reads it and registers A as a peer; B registers its own code with A.
//! 3. Once Syncthing carries the vault over, B derives the same key from the synced
//!    `vault.json` salt plus the master password — no key is ever transmitted.
//!
//! The code is a plain versioned string, which makes it easy to render as a QR.

use anyhow::{bail, Result};
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

#[cfg(test)]
mod tests {
    use super::*;

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
