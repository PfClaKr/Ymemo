//! 기기 페어링 코드.
//!
//! 새 기기 추가 절차 (양방향):
//! 1. 기기 A 가 자기 Syncthing 기기 ID 를 페어링 코드(QR/문자열)로 보여준다.
//! 2. 기기 B 가 코드를 읽어 A 를 peer 로 등록하고, B 도 자기 코드를 A 에 등록한다.
//! 3. Syncthing 이 vault 디렉터리를 전파하면 B 는 동기화된 `vault.json` 의 salt +
//!    마스터 암호 입력만으로 같은 키를 유도한다 → 별도 키 전송 불필요.
//!
//! 코드는 버전 프리픽스가 붙은 단순 문자열이라 QR 로 렌더링하기 좋다.

use anyhow::{bail, Result};
use ymemo_i18n::t;

/// 페어링 코드 포맷 버전 프리픽스.
const PREFIX: &str = "YMEMO1:";

/// 페어링 코드 한 장 — 현재는 Syncthing 기기 ID 만 담는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCode {
    pub syncthing_device_id: String,
}

impl PairingCode {
    pub fn new(syncthing_device_id: impl Into<String>) -> Self {
        Self { syncthing_device_id: syncthing_device_id.into() }
    }

    /// QR/전송용 문자열. 예: `YMEMO1:ABCDEFG-...`
    pub fn encode(&self) -> String {
        format!("{PREFIX}{}", self.syncthing_device_id)
    }

    /// 문자열 파싱. 프리픽스 없이 기기 ID 만 붙여넣은 경우도 허용한다.
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
