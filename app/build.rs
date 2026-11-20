use std::path::Path;
use std::{env, fs};

const RELEASE_GOOGLE_CLIENT_ID: &str =
    "905394446925-55ermvpejet4lbn6qcud6390h75jkhqs.apps.googleusercontent.com";

/// Языковые файлы лежат в ui/i18n/ и общие для интерфейса и Rust.
/// Здесь они встраиваются в программу: новый язык добавляется файлом, код менять не нужно.
fn main() {
    println!("cargo:rerun-if-changed=../ui");
    println!("cargo:rerun-if-env-changed=MEMORY_LOG_GOOGLE_CLIENT_ID");
    println!("cargo:rerun-if-env-changed=MEMORY_LOG_GOOGLE_CLIENT_SECRET");
    assert!(
        !cfg!(feature = "google-sync-release")
            || env::var("MEMORY_LOG_GOOGLE_CLIENT_SECRET").is_ok_and(|value| !value.trim().is_empty()),
        "Set MEMORY_LOG_GOOGLE_CLIENT_SECRET from the Google Desktop OAuth client configuration to build with google-sync-release."
    );
    let configured_id = env::var("MEMORY_LOG_GOOGLE_CLIENT_ID").ok().filter(|id| !id.is_empty());
    let client_id = configured_id.as_deref()
        .or(cfg!(feature = "google-sync-release").then_some(RELEASE_GOOGLE_CLIENT_ID));
    if let Some(client_id) = client_id {
        assert!(
            client_id.strip_suffix(".apps.googleusercontent.com").is_some_and(|prefix| !prefix.is_empty())
                && client_id.len() <= 255
                && client_id.bytes().all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte)),
            "MEMORY_LOG_GOOGLE_CLIENT_ID must be a Google Desktop OAuth client ID."
        );
        println!("cargo:rustc-env=MEMORY_LOG_GOOGLE_CLIENT_ID={client_id}");
    }

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
