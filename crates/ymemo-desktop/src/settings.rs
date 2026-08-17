//! 기기 로컬 환경설정과 "자동 잠금 해제" 세션.
//!
//! 둘 다 **동기화하지 않는다** — vault 디렉터리가 아니라 앱 데이터 디렉터리에 둔다.
//! 언어나 자동 잠금 시간은 기기마다 다른 게 자연스럽고, 세션 키는 애초에 다른 기기로
//! 나가면 안 되는 값이다.
//!
//! ```text
//! <data_dir>/settings.json   ← 환경설정 (평범한 설정 파일)
//! <data_dir>/session.json    ← 자동 해제용 키 캐시 (unix 0600)
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use ymemo_core::crypto::KEY_LEN;

const SETTINGS_FILE: &str = "settings.json";
const SESSION_FILE: &str = "session.json";

/// 동기화 주기의 허용 범위(초). 너무 짧으면 디스크를 계속 읽고, 너무 길면 안 붙는 느낌이 난다.
pub const MERGE_SECONDS_RANGE: (i32, i32) = (3, 3600);
/// 자동 해제 유지 일수의 상한. 0 = 매번 암호.
pub const UNLOCK_DAYS_MAX: i32 = 365;
/// 자리 비움 자동 잠금(분)의 상한. 0 = 끔.
pub const IDLE_MINUTES_MAX: i32 = 1440;

/// 기기 로컬 환경설정. 모든 필드가 `#[serde(default)]` 라 옛 설정 파일도 그대로 열린다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// UI 언어. `"auto"`(시스템 로캘) / `"ko"` / `"en"`.
    pub lang: String,
    /// 한 번 잠금을 푼 뒤 암호 없이 열리는 기간(일). 0 = 매번 암호를 묻는다.
    pub unlock_days: i32,
    /// 이 시간(분) 동안 앱을 건드리지 않으면 자동으로 잠근다. 0 = 끔.
    pub idle_lock_minutes: i32,
    /// 새 메모의 기본 색 (팔레트 키).
    pub default_color: String,
    /// 새 메모의 기본 불투명도(%).
    pub default_opacity: i32,
    /// 다른 기기의 변경을 가져오는 주기(초).
    pub merge_seconds: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            lang: "auto".into(),
            unlock_days: 30,
            idle_lock_minutes: 0,
            default_color: "yellow".into(),
            default_opacity: 100,
            merge_seconds: 15,
        }
    }
}

impl Settings {
    /// 설정 파일을 읽는다. 없거나 깨졌으면 기본값으로 시작한다(설정 때문에 앱이 안 뜨면 곤란).
    pub fn load(dir: &Path) -> Self {
        match fs::read(dir.join(SETTINGS_FILE)) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                eprintln!("설정 파일을 읽지 못해 기본값으로 시작합니다: {e}");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, dir: &Path) {
        match serde_json::to_vec_pretty(self) {
            Ok(bytes) => {
                if let Err(e) = fs::write(dir.join(SETTINGS_FILE), bytes) {
                    eprintln!("설정 저장 실패: {e}");
                }
            }
            Err(e) => eprintln!("설정 직렬화 실패: {e}"),
        }
    }

    /// 사용자가 입력한 값을 쓸 수 있는 범위로 다듬는다 (UI 에서 무엇을 넣든 안전하도록).
    pub fn sanitize(&mut self) {
        if !matches!(self.lang.as_str(), "auto" | "ko" | "en") {
            self.lang = "auto".into();
        }
        self.unlock_days = self.unlock_days.clamp(0, UNLOCK_DAYS_MAX);
        self.idle_lock_minutes = self.idle_lock_minutes.clamp(0, IDLE_MINUTES_MAX);
        if !matches!(
            self.default_color.as_str(),
            "yellow" | "pink" | "green" | "blue" | "purple"
        ) {
            self.default_color = "yellow".into();
        }
        self.default_opacity = ymemo_core::clamp_opacity(self.default_opacity as i64) as i32;
        self.merge_seconds = self
            .merge_seconds
            .clamp(MERGE_SECONDS_RANGE.0, MERGE_SECONDS_RANGE.1);
    }

    /// `"auto"` 를 시스템 로캘로 풀어 실제로 쓸 언어(`"ko"` / `"en"`)를 정한다.
    pub fn effective_lang(&self) -> &'static str {
        match self.lang.as_str() {
            "ko" => "ko",
            "en" => "en",
            _ if system_is_korean() => "ko",
            _ => "en",
        }
    }
}

/// 시스템 로캘이 한국어인가. (unix 는 LC_ALL/LC_MESSAGES/LANG, windows 는 GetUserDefaultLocaleName
/// 대신 동일 환경변수를 쓰는 셸이 드물어 결국 영어로 떨어지는데, 그 편이 안전한 기본값이다.)
fn system_is_korean() -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .any(|v| v.to_ascii_lowercase().starts_with("ko"))
}

// ---------------------------------------------------------------------------
// 자동 잠금 해제 세션
// ---------------------------------------------------------------------------

/// 디스크에 캐시해 둔 vault 키와 만료 시각.
///
/// **이 파일이 있는 동안은 마스터 암호 없이 메모를 읽을 수 있다.** 그게 "자동 해제"의
/// 정의이자 대가로, 저장 시 암호화가 그 기간만큼 무력해진다. 그래서 unix 에선 0600 으로
/// 만들고, 수동 잠금·설정 변경·만료 어느 쪽으로든 곧바로 지운다.
#[derive(Serialize, Deserialize)]
struct Session {
    /// hex 로 인코딩한 32바이트 vault 키.
    key: String,
    /// 만료 시각 (unix epoch ms).
    expires_at: i64,
}

fn session_path(dir: &Path) -> PathBuf {
    dir.join(SESSION_FILE)
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 아직 유효한 세션 키를 읽는다. 만료됐거나 깨졌으면 파일을 지우고 `None`.
pub fn load_session(dir: &Path) -> Option<[u8; KEY_LEN]> {
    let bytes = fs::read(session_path(dir)).ok()?;
    let session: Session = match serde_json::from_slice(&bytes) {
        Ok(s) => s,
        Err(_) => {
            clear_session(dir);
            return None;
        }
    };
    if now_millis() >= session.expires_at {
        clear_session(dir);
        return None;
    }
    match from_hex(&session.key) {
        Some(key) => Some(key),
        None => {
            clear_session(dir);
            None
        }
    }
}

/// 잠금을 푼 직후 호출. `days` 가 0 이면 아무것도 남기지 않는다(= 매번 암호).
pub fn save_session(dir: &Path, key: &[u8; KEY_LEN], days: i32) {
    if days <= 0 {
        clear_session(dir);
        return;
    }
    let session = Session {
        key: to_hex(key),
        expires_at: now_millis() + days as i64 * 24 * 60 * 60 * 1000,
    };
    let Ok(bytes) = serde_json::to_vec(&session) else {
        return;
    };
    let path = session_path(dir);
    if let Err(e) = fs::write(&path, bytes) {
        eprintln!("자동 해제 세션 저장 실패: {e}");
        return;
    }
    restrict_permissions(&path);
}

/// 세션을 폐기한다 (수동 잠금, 기간 변경, 만료).
pub fn clear_session(dir: &Path) {
    let path = session_path(dir);
    if path.exists() {
        if let Err(e) = fs::remove_file(&path) {
            eprintln!("자동 해제 세션 삭제 실패: {e}");
        }
    }
}

/// 소유자만 읽을 수 있게 한다. unix 만 실효성이 있고, windows 는 사용자 프로필
/// 디렉터리의 ACL 에 기댄다(별도 조치 없음).
#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
        eprintln!("세션 파일 권한 설정 실패: {e}");
    }
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Option<[u8; KEY_LEN]> {
    if s.len() != KEY_LEN * 2 {
        return None;
    }
    let mut out = [0u8; KEY_LEN];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ymemo-settings-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn settings_roundtrip_and_defaults() {
        let dir = temp_dir();
        let mut s = Settings::default();
        s.lang = "en".into();
        s.unlock_days = 7;
        s.save(&dir);
        assert_eq!(Settings::load(&dir), s);
        fs::remove_file(dir.join(SETTINGS_FILE)).ok();
        // 파일이 없으면 기본값.
        assert_eq!(Settings::load(&dir), Settings::default());
    }

    #[test]
    fn sanitize_clamps_everything() {
        let mut s = Settings {
            lang: "klingon".into(),
            unlock_days: 99_999,
            idle_lock_minutes: -5,
            default_color: "chartreuse".into(),
            default_opacity: 1,
            merge_seconds: 0,
        };
        s.sanitize();
        assert_eq!(s.lang, "auto");
        assert_eq!(s.unlock_days, UNLOCK_DAYS_MAX);
        assert_eq!(s.idle_lock_minutes, 0);
        assert_eq!(s.default_color, "yellow");
        assert!(s.default_opacity >= 20); // 코어의 MIN_OPACITY 로 올라간다
        assert_eq!(s.merge_seconds, MERGE_SECONDS_RANGE.0);
    }

    #[test]
    fn session_survives_until_expiry_then_vanishes() {
        let dir = temp_dir();
        let key = [3u8; KEY_LEN];

        save_session(&dir, &key, 30);
        assert_eq!(load_session(&dir), Some(key));

        // 0일 = 저장하지 않는다.
        save_session(&dir, &key, 0);
        assert_eq!(load_session(&dir), None);

        // 이미 지난 만료 시각은 읽는 순간 파일째 사라진다.
        save_session(&dir, &key, 30);
        let past = Session {
            key: to_hex(&key),
            expires_at: now_millis() - 1,
        };
        fs::write(session_path(&dir), serde_json::to_vec(&past).unwrap()).unwrap();
        assert_eq!(load_session(&dir), None);
        assert!(!session_path(&dir).exists());
    }
}
