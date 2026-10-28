use std::path::Path;
use std::{env, fs};

/// Языковые файлы лежат в ui/i18n/ и общие для интерфейса и Rust.
/// Здесь они встраиваются в программу: новый язык добавляется файлом, код менять не нужно.
fn main() {
    println!("cargo:rerun-if-changed=../ui/i18n");

    let mut langs: Vec<(String, String)> = Vec::new();
    for entry in fs::read_dir("../ui/i18n").expect("нет папки ui/i18n") {
        let path = entry.expect("папка ui/i18n").path();
        let is_json = path.extension().and_then(|e| e.to_str()) == Some("json");
        let code = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        if is_json && code != "languages" {
            let full = fs::canonicalize(&path).expect("путь к языковому файлу");
            // Windows возвращает путь вида \\?\C:\..., include_str! его не любит.
            let full = full.display().to_string();
            langs.push((code, full.trim_start_matches(r"\\?\").to_string()));
        }
    }
    langs.sort();

    let items: String = langs.iter().map(|(code, path)| format!("    ({code:?}, include_str!({path:?})),\n")).collect();
    let generated = format!("pub const LANGS: &[(&str, &str)] = &[\n{items}];\n");
    fs::write(Path::new(&env::var("OUT_DIR").expect("OUT_DIR")).join("i18n_gen.rs"), generated).expect("i18n_gen.rs");

    tauri_build::build()
}
