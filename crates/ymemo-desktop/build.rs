use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Translation catalog, relative to this crate.
const CATALOG: &str = "../../i18n/ko.json";
/// Keys exported to the Slint global; the rest (core./msg./tray.) stay in Rust.
const UI_PREFIX: &str = "ui.";

fn main() {
    generate_i18n();

    // Compile ui/app.slint for `slint::include_modules!()`. The generated string global is
    // passed as a library path so `.slint` files can `import { Strings } from "@i18n";`
    // while everything generated stays in OUT_DIR.
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let config = slint_build::CompilerConfiguration::new()
        .with_library_paths(HashMap::from([("i18n".to_string(), out.join("i18n.slint"))]));
    slint_build::compile_with_config("ui/app.slint", config).expect("slint compile failed");

    // Windows: embed the icon resource so Explorer, the taskbar and the installer show it.
    // build.rs runs on the host, and Windows targets are built on a Windows runner, so
    // cfg(windows) is the right switch here.
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../packaging/assets/ymemo.ico");
        if let Err(e) = res.compile() {
            // A missing icon resource warns but never fails the build.
            println!("cargo:warning=failed to compile the Windows icon resource: {e}");
        }
    }
}

/// Generates two files from the catalog's `ui.*` keys:
///
/// - `i18n.slint` — a `Strings` global with one property per key, defaulting to Korean.
/// - `i18n_apply.rs` — `apply_strings`, which fills that global for the current language.
///
/// Both come from the one catalog, since two hand-kept copies always drift. Adding a key to
/// the JSON is enough to get the property and its setter.
fn generate_i18n() {
    println!("cargo:rerun-if-changed={CATALOG}");
    println!("cargo:rerun-if-changed=../../i18n/en.json");

    let json = std::fs::read_to_string(CATALOG)
        .unwrap_or_else(|e| panic!("cannot read {CATALOG}: {e}"));
    let value: serde_json::Value = serde_json::from_str(&json).expect("ko.json failed to parse");
    let obj = value.as_object().expect("ko.json must be an object");

    let mut keys: Vec<&String> = obj.keys().filter(|k| k.starts_with(UI_PREFIX)).collect();
    keys.sort();
    assert!(!keys.is_empty(), "no {UI_PREFIX}* keys in the catalog");

    let mut slint = String::from(
        "// Generated file — do not edit. Source: i18n/*.json, generator:\n\
         // crates/ymemo-desktop/build.rs.\n\
         // Defaults are the Korean originals, so screens are never blank before Rust calls\n\
         // apply_strings.\n\
         export global Strings {\n",
    );
    let mut rust = String::from(
        "// Generated file — do not edit (crates/ymemo-desktop/build.rs).\n\
         /// Fills this window's `Strings` global for the current language.\n\
         ///\n\
         /// A Slint global is per component instance, so call this once per window.\n\
         fn apply_strings(g: &Strings) {\n",
    );

    for key in keys {
        let ident = key.replace('.', "_");
        let default = obj[key].as_str().unwrap_or_else(|| panic!("{key} is not a string"));
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

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    write_if_changed(&out.join("i18n.slint"), &slint);
    write_if_changed(&out.join("i18n_apply.rs"), &rust);
}

/// Leaves the file alone when unchanged; a new mtime makes slint recompile everything.
fn write_if_changed(path: &Path, content: &str) {
    if std::fs::read_to_string(path).is_ok_and(|old| old == content) {
        return;
    }
    std::fs::write(path, content).unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
}

/// Escapes a string for a Slint literal.
fn escape_slint(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('{', "\\{")
}
