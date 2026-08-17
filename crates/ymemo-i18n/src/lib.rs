//! 번역 카탈로그.
//!
//! 문구는 코드가 아니라 저장소 루트의 [`i18n/`](../../../i18n) 폴더에 있는 JSON 에 있고,
//! 이 크레이트가 그걸 컴파일 타임에 박아 넣어(`include_str!`) 런타임에 골라 준다.
//! 코어·데스크탑·(장차) 모바일이 **같은 카탈로그 하나**를 쓰는 것이 요점이다.
//!
//! ```ignore
//! use ymemo_i18n::t;
//! bail!(t!("core.wrong_password"));
//! bail!(t!("core.vault_exists", path = header_path.display()));
//! ```
//!
//! 키가 없으면 정본(한국어) → 그래도 없으면 키 자체를 돌려준다. 앱이 죽거나 빈 문자열이
//! 뜨는 것보다 낫고, 화면에 키가 보이면 빠진 번역이 바로 눈에 띈다.
//!
//! 언어는 프로세스 전역 상태다 — Dart(FFI)나 트레이처럼 다른 스레드에서도 부르므로
//! 원자 값으로 들고 있다.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

/// 정본 언어. 번역이 빠졌을 때 여기로 떨어진다.
const FALLBACK: Lang = Lang::Ko;

const KO_JSON: &str = include_str!("../../../i18n/ko.json");
const EN_JSON: &str = include_str!("../../../i18n/en.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Ko,
    En,
}

impl Lang {
    /// BCP-47 스러운 문자열에서 (`"ko"`, `"ko-KR"`, `"en_US"` …). 모르는 값은 `None`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().split(['-', '_', '.']).next()? {
            "ko" => Some(Lang::Ko),
            "en" => Some(Lang::En),
            _ => None,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Lang::Ko => "ko",
            Lang::En => "en",
        }
    }

    fn catalog(self) -> &'static HashMap<&'static str, &'static str> {
        static KO: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
        static EN: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
        match self {
            Lang::Ko => KO.get_or_init(|| parse_catalog(KO_JSON, "ko")),
            Lang::En => EN.get_or_init(|| parse_catalog(EN_JSON, "en")),
        }
    }
}

/// 이 앱이 제공하는 모든 언어. 설정 화면이 목록을 만들 때 쓴다.
pub const ALL: &[Lang] = &[Lang::Ko, Lang::En];

static CURRENT: AtomicU8 = AtomicU8::new(0); // 0 = Ko, 1 = En

pub fn set_lang(lang: Lang) {
    CURRENT.store(if lang == Lang::En { 1 } else { 0 }, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    if CURRENT.load(Ordering::Relaxed) == 1 {
        Lang::En
    } else {
        Lang::Ko
    }
}

/// 시스템 로캘에서 언어를 추정한다. 모르면 정본.
pub fn system_lang() -> Lang {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find_map(|v| Lang::parse(&v))
        .unwrap_or(FALLBACK)
}

/// 키를 현재 언어로 옮기고 `{이름}` 자리를 채운다. 보통은 [`t!`] 매크로로 부른다.
pub fn translate(key: &str, args: &[(&str, String)]) -> String {
    let template = lookup(key);
    if args.is_empty() {
        return template.to_string();
    }
    let mut out = template.to_string();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

/// 현재 언어 → 정본 → 키 자체 순으로 찾는다.
fn lookup(key: &str) -> &'static str {
    let current = lang();
    if let Some(v) = current.catalog().get(key) {
        return v;
    }
    if current != FALLBACK {
        if let Some(v) = FALLBACK.catalog().get(key) {
            return v;
        }
    }
    // 카탈로그에 없는 키. 빈 문자열보다는 키가 보이는 편이 고치기 쉽다.
    Box::leak(key.to_string().into_boxed_str())
}

/// 어떤 언어에든 들어 있는 모든 키 (정본 기준). 코드 생성·검증용.
pub fn keys() -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = FALLBACK.catalog().keys().copied().collect();
    keys.sort_unstable();
    keys
}

/// 특정 언어의 원문 그대로 (언어 전역 상태와 무관). 코드 생성용.
pub fn raw(lang: Lang, key: &str) -> Option<&'static str> {
    lang.catalog().get(key).copied()
}

/// JSON 은 평평한 `{"키": "문구"}` 한 겹이어야 한다. 형식이 틀리면 빌드가 아니라
/// 첫 사용 시점에 터지므로, 카탈로그 자체를 검사하는 테스트를 아래에 두었다.
fn parse_catalog(json: &str, name: &str) -> HashMap<&'static str, &'static str> {
    let value: serde_json::Value =
        serde_json::from_str(json).unwrap_or_else(|e| panic!("i18n/{name}.json 파싱 실패: {e}"));
    let obj = value
        .as_object()
        .unwrap_or_else(|| panic!("i18n/{name}.json 은 객체여야 한다"));
    obj.iter()
        .map(|(k, v)| {
            let s = v
                .as_str()
                .unwrap_or_else(|| panic!("i18n/{name}.json 의 '{k}' 값이 문자열이 아니다"));
            // 카탈로그는 프로그램이 사는 동안 그대로 있으므로 'static 으로 새어도 무방하다.
            (
                Box::leak(k.clone().into_boxed_str()) as &'static str,
                Box::leak(s.to_string().into_boxed_str()) as &'static str,
            )
        })
        .collect()
}

/// 카탈로그에서 문구를 꺼낸다.
///
/// - `t!("key")`
/// - `t!("key", name = value, …)` → 문구의 `{name}` 자리에 `value` 를 넣는다.
#[macro_export]
macro_rules! t {
    ($key:expr) => {
        $crate::translate($key, &[])
    };
    ($key:expr, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::translate($key, &[
            $((stringify!($name), ::std::string::ToString::to_string(&$value))),+
        ])
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// 번역 누락·오타를 잡는 그물. 두 카탈로그의 키 집합이 정확히 같아야 한다.
    #[test]
    fn catalogs_have_identical_keys() {
        let ko: HashSet<_> = Lang::Ko.catalog().keys().copied().collect();
        let en: HashSet<_> = Lang::En.catalog().keys().copied().collect();
        let only_ko: Vec<_> = ko.difference(&en).collect();
        let only_en: Vec<_> = en.difference(&ko).collect();
        assert!(
            only_ko.is_empty() && only_en.is_empty(),
            "카탈로그 키 불일치 — ko 에만: {only_ko:?}, en 에만: {only_en:?}"
        );
    }

    /// `{name}` 자리표시자도 언어마다 같아야 한다 (한쪽만 빠지면 값이 사라진다).
    #[test]
    fn placeholders_match_across_languages() {
        fn placeholders(s: &str) -> HashSet<String> {
            let mut out = HashSet::new();
            let mut rest = s;
            while let Some(open) = rest.find('{') {
                let Some(close) = rest[open..].find('}') else { break };
                out.insert(rest[open + 1..open + close].to_string());
                rest = &rest[open + close + 1..];
            }
            out
        }
        for key in keys() {
            let ko = placeholders(raw(Lang::Ko, key).unwrap());
            let en = placeholders(raw(Lang::En, key).unwrap_or(""));
            assert_eq!(ko, en, "'{key}' 의 자리표시자가 언어마다 다름");
        }
    }

    /// 빈 문구는 대개 번역을 깜빡한 것이다.
    #[test]
    fn no_empty_strings() {
        for lang in ALL {
            for key in keys() {
                assert!(
                    !raw(*lang, key).unwrap_or("").trim().is_empty(),
                    "{}/{key} 가 비어 있다",
                    lang.code()
                );
            }
        }
    }

    /// 소스에서 부르는 `t!("...")` 키가 전부 카탈로그에 있어야 한다.
    ///
    /// 없으면 런타임에 키 문자열이 그대로 화면에 뜰 뿐 아무도 모른다 — 오타를 여기서 잡는다.
    #[test]
    fn every_key_used_in_code_exists() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("워크스페이스 루트")
            .join("crates");

        let mut sources = Vec::new();
        collect_rs(&root, &mut sources);
        // 이 크레이트 자신은 제외 — 문서와 매크로 정의에 예시용 `t!("key")` 가 들어 있다.
        sources.retain(|p| !p.components().any(|c| c.as_os_str() == "ymemo-i18n"));
        assert!(!sources.is_empty(), "스캔한 .rs 파일이 없다 — 경로가 틀렸다");

        let known: HashSet<&str> = keys().into_iter().collect();
        let mut missing = Vec::new();
        for path in &sources {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            for (i, _) in text.match_indices("t!(\"") {
                // `format!("…")` 의 꼬리가 아니라 진짜 `t!` 여야 한다 (앞 글자가 식별자면 아님).
                let preceded_by_ident = text[..i]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_');
                if preceded_by_ident {
                    continue;
                }
                let rest = &text[i + 4..];
                let Some(end) = rest.find('"') else { continue };
                let key = &rest[..end];
                // 이 파일 자체의 테스트가 쓰는 "없는 키" 는 일부러 없는 것이다.
                if key.starts_with("no.such") {
                    continue;
                }
                if !known.contains(key) {
                    missing.push(format!("{}: {key}", path.display()));
                }
            }
        }
        assert!(missing.is_empty(), "카탈로그에 없는 키: {missing:#?}");
    }

    fn collect_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn interpolates_named_args() {
        set_lang(Lang::Ko);
        // 자리표시자가 있는 실제 키로 확인 (문구가 바뀌어도 값만 들어가면 통과).
        let out = t!("core.hex_parse_failed", error = "bad digit");
        assert!(out.contains("bad digit"), "실제 출력: {out}");
        assert!(!out.contains("{error}"), "자리표시자가 남았다: {out}");
    }

    #[test]
    fn falls_back_and_never_returns_empty() {
        set_lang(Lang::En);
        assert_eq!(t!("no.such.key.at.all"), "no.such.key.at.all");
        set_lang(Lang::Ko);
    }

    #[test]
    fn parses_locale_strings() {
        assert_eq!(Lang::parse("ko_KR.UTF-8"), Some(Lang::Ko));
        assert_eq!(Lang::parse("en-US"), Some(Lang::En));
        assert_eq!(Lang::parse("fr"), None);
    }
}
