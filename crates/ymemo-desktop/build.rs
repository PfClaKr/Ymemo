use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 번역 카탈로그 (저장소 루트 기준).
const CATALOG: &str = "../../i18n/ko.json";
/// Slint 전역으로 내보낼 키의 접두사. 나머지(core./msg./tray.)는 Rust 에서만 쓴다.
const UI_PREFIX: &str = "ui.";

fn main() {
    generate_i18n();

    // ui/app.slint 을 컴파일해 `slint::include_modules!()` 로 노출한다.
    // 생성된 문구 전역은 라이브러리 경로로 넘겨 `import { Strings } from "@i18n";` 이 되게 한다
    // (소스 트리를 건드리지 않고 OUT_DIR 에만 쓰기 위함).
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR 없음"));
    let config = slint_build::CompilerConfiguration::new()
        .with_library_paths(HashMap::from([("i18n".to_string(), out.join("i18n.slint"))]));
    slint_build::compile_with_config("ui/app.slint", config).expect("Slint UI 컴파일 실패");

    // Windows: 실행 파일에 아이콘 리소스를 박아 Explorer·작업표시줄·인스톨러에서
    // 아이콘이 보이게 한다. (다른 OS 에선 아무 일도 하지 않는다.)
    // build.rs 는 호스트에서 도므로, Windows 타깃은 windows-latest 러너(호스트=windows)에서
    // 빌드된다 → cfg(windows) 로 분기해도 정상 동작한다.
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../packaging/assets/ymemo.ico");
        if let Err(e) = res.compile() {
            // 아이콘 리소스가 없어도 빌드 자체는 막지 않는다(경고만).
            println!("cargo:warning=Windows 아이콘 리소스 컴파일 실패: {e}");
        }
    }
}

/// 카탈로그의 `ui.*` 키에서 두 파일을 만든다.
///
/// - `i18n.slint` — 키마다 프로퍼티 하나인 `Strings` 전역 (기본값은 정본 한국어).
/// - `i18n_apply.rs` — 현재 언어의 문구를 그 전역에 넣는 `apply_strings`.
///
/// 손으로 두 벌을 관리하면 반드시 어긋나므로 카탈로그 하나에서 뽑는다. 키를 JSON 에
/// 추가하면 프로퍼티와 setter 가 저절로 따라온다.
fn generate_i18n() {
    println!("cargo:rerun-if-changed={CATALOG}");
    println!("cargo:rerun-if-changed=../../i18n/en.json");

    let json = std::fs::read_to_string(CATALOG)
        .unwrap_or_else(|e| panic!("{CATALOG} 를 읽지 못함: {e}"));
    let value: serde_json::Value = serde_json::from_str(&json).expect("ko.json 파싱 실패");
    let obj = value.as_object().expect("ko.json 은 객체여야 한다");

    let mut keys: Vec<&String> = obj.keys().filter(|k| k.starts_with(UI_PREFIX)).collect();
    keys.sort();
    assert!(!keys.is_empty(), "카탈로그에 {UI_PREFIX}* 키가 없다");

    let mut slint = String::from(
        "// 자동 생성 파일 — 고치지 말 것. 원본은 저장소 루트의 i18n/*.json 이고,\n\
         // 생성기는 crates/ymemo-desktop/build.rs 다.\n\
         // 기본값은 정본(한국어) — Rust 가 apply_strings 를 부르기 전에도 화면이 비지 않는다.\n\
         export global Strings {\n",
    );
    let mut rust = String::from(
        "// 자동 생성 파일 — 고치지 말 것 (crates/ymemo-desktop/build.rs).\n\
         /// 현재 언어의 문구를 이 창의 `Strings` 전역에 채워 넣는다.\n\
         ///\n\
         /// Slint 전역은 창(컴포넌트 인스턴스)마다 따로 있으므로 창마다 한 번씩 불러야 한다.\n\
         fn apply_strings(g: &Strings) {\n",
    );

    for key in keys {
        let ident = key.replace('.', "_");
        let default = obj[key].as_str().unwrap_or_else(|| panic!("{key} 값이 문자열이 아님"));
        slint.push_str(&format!(
            "    in-out property <string> {ident}: \"{}\";\n",
            escape_slint(default)
        ));
        rust.push_str(&format!(
            "    g.set_{ident}(ymemo_i18n::t!(\"{key}\").into());\n"
        ));
    }
    slint.push_str("}\n");
    rust.push_str("}\n");

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR 없음"));
    write_if_changed(&out.join("i18n.slint"), &slint);
    write_if_changed(&out.join("i18n_apply.rs"), &rust);
}

/// 내용이 같으면 건드리지 않는다 (mtime 이 바뀌면 slint 가 매번 다시 컴파일된다).
fn write_if_changed(path: &Path, content: &str) {
    if std::fs::read_to_string(path).is_ok_and(|old| old == content) {
        return;
    }
    std::fs::write(path, content).unwrap_or_else(|e| panic!("{} 쓰기 실패: {e}", path.display()));
}

/// Slint 문자열 리터럴용 이스케이프.
fn escape_slint(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('{', "\\{")
}
