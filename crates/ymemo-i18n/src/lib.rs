//! Translation catalog.
//!
//! Strings live in JSON under [`i18n/`](../../../i18n), not in code; this crate bakes them
//! in with `include_str!` and picks a language at runtime. The point is that core, desktop
//! and mobile all share **one** catalog.
//!
//! ```ignore
//! use ymemo_i18n::t;
//! bail!(t!("core.wrong_password"));
//! bail!(t!("core.vault_exists", path = header_path.display()));
//! ```
//!
//! A missing key falls back to the source language (Korean), then to the key itself —
//! better than a panic or a blank label, and a key on screen makes the gap obvious.
//!
//! The language is process-global and kept in an atomic, since Dart (over FFI) and the tray
//! call in from other threads.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

/// Source language, used whenever a translation is missing.
const FALLBACK: Lang = Lang::Ko;

const KO_JSON: &str = include_str!("../../../i18n/ko.json");
const EN_JSON: &str = include_str!("../../../i18n/en.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Ko,
    En,
}

impl Lang {
    /// Parses a BCP-47-ish string (`"ko"`, `"ko-KR"`, `"en_US"`); `None` if unknown.
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

/// Every language the app offers; the settings screen lists these.
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

/// Guesses the language from the system locale, falling back to the source language.
pub fn system_lang() -> Lang {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find_map(|v| Lang::parse(&v))
        .unwrap_or(FALLBACK)
}

/// Translates a key and fills its `{name}` placeholders; usually called via [`t!`].
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

/// Looks up the current language, then the source language, then the key itself.
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
    // Unknown key: showing it beats showing nothing.
    Box::leak(key.to_string().into_boxed_str())
}

/// Every key in the source catalog; used by code generation and the checks below.
pub fn keys() -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = FALLBACK.catalog().keys().copied().collect();
    keys.sort_unstable();
    keys
}

/// Raw string for one language, ignoring the global setting; used by code generation.
pub fn raw(lang: Lang, key: &str) -> Option<&'static str> {
    lang.catalog().get(key).copied()
}

/// The JSON must be one flat `{"key": "string"}` layer. A malformed catalog panics on first
/// use rather than at build time, hence the catalog tests below.
fn parse_catalog(json: &str, name: &str) -> HashMap<&'static str, &'static str> {
    let value: serde_json::Value =
        serde_json::from_str(json).unwrap_or_else(|e| panic!("i18n/{name}.json failed to parse: {e}"));
    let obj = value
        .as_object()
        .unwrap_or_else(|| panic!("i18n/{name}.json must be an object"));
    obj.iter()
        .map(|(k, v)| {
            let s = v
                .as_str()
                .unwrap_or_else(|| panic!("i18n/{name}.json: '{k}' is not a string"));
            // The catalog lives as long as the program, so leaking to 'static is fine.
            (
                Box::leak(k.clone().into_boxed_str()) as &'static str,
                Box::leak(s.to_string().into_boxed_str()) as &'static str,
            )
        })
        .collect()
}

/// Pulls a string out of the catalog.
///
/// - `t!("key")`
/// - `t!("key", name = value, ...)` fills the string's `{name}` placeholders.
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

    /// Safety net for missing or misspelled keys: both catalogs must hold the same set.
    #[test]
    fn catalogs_have_identical_keys() {
        let ko: HashSet<_> = Lang::Ko.catalog().keys().copied().collect();
        let en: HashSet<_> = Lang::En.catalog().keys().copied().collect();
        let only_ko: Vec<_> = ko.difference(&en).collect();
        let only_en: Vec<_> = en.difference(&ko).collect();
        assert!(
            only_ko.is_empty() && only_en.is_empty(),
            "catalog key mismatch — ko only: {only_ko:?}, en only: {only_en:?}"
        );
    }

    /// Placeholders must match across languages, or one language silently drops a value.
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
            assert_eq!(ko, en, "placeholders differ between languages for '{key}'");
        }
    }

    /// An empty string usually means a forgotten translation.
    #[test]
    fn no_empty_strings() {
        for lang in ALL {
            for key in keys() {
                assert!(
                    !raw(*lang, key).unwrap_or("").trim().is_empty(),
                    "{}/{key} is empty",
                    lang.code()
                );
            }
        }
    }

    /// Every `t!("...")` key in the sources must exist in the catalog; otherwise the key
    /// just shows up on screen at runtime and nobody notices.
    #[test]
    fn every_key_used_in_code_exists() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root")
            .join("crates");

        let mut sources = Vec::new();
        collect_rs(&root, &mut sources);
        // Skip this crate: its docs and macro use `t!("key")` as an example.
        sources.retain(|p| !p.components().any(|c| c.as_os_str() == "ymemo-i18n"));
        assert!(!sources.is_empty(), "no .rs files scanned — wrong path");

        let known: HashSet<&str> = keys().into_iter().collect();
        let mut missing = Vec::new();
        for path in &sources {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            for (i, _) in text.match_indices("t!(\"") {
                // Must be a real `t!`, not the tail of another identifier.
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
                // The missing keys used by the tests below are missing on purpose.
                if key.starts_with("no.such") {
                    continue;
                }
                if !known.contains(key) {
                    missing.push(format!("{}: {key}", path.display()));
                }
            }
        }
        assert!(missing.is_empty(), "keys missing from the catalog: {missing:#?}");
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
        // Use a real key with a placeholder, so rewording the string does not break this.
        let out = t!("core.hex_parse_failed", error = "bad digit");
        assert!(out.contains("bad digit"), "actual output: {out}");
        assert!(!out.contains("{error}"), "placeholder left in place: {out}");
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
