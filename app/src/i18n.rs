//! Тексты программы на стороне Rust (трей, CSV, ошибки). Берутся из тех же файлов ui/i18n/*.json,
//! что и интерфейс; список языков собирает build.rs.

use std::collections::HashMap;
use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/i18n_gen.rs"));

const FALLBACK: &str = "en";

type Dict = HashMap<String, String>;

fn dicts() -> &'static HashMap<&'static str, Dict> {
    static DICTS: OnceLock<HashMap<&'static str, Dict>> = OnceLock::new();
    DICTS.get_or_init(|| {
        LANGS
            .iter()
            .map(|(code, json)| (*code, serde_json::from_str(json).unwrap_or_default()))
            .collect()
    })
}

pub fn is_supported(code: &str) -> bool {
    dicts().contains_key(code)
}

/// Строка на языке `lang`; нет перевода — английская, нет и её — сам ключ.
pub fn t(lang: &str, key: &str) -> String {
    let d = dicts();
    d.get(lang)
        .and_then(|m| m.get(key))
        .or_else(|| d.get(FALLBACK).and_then(|m| m.get(key)))
        .cloned()
        .unwrap_or_else(|| key.to_string())
}

/// То же, с подстановкой `{имя}`.
pub fn t_with(lang: &str, key: &str, params: &[(&str, &str)]) -> String {
    params.iter().fold(t(lang, key), |s, (k, v)| s.replace(&format!("{{{k}}}"), v))
}

/// Язык по умолчанию — язык интерфейса Windows, если он среди поддерживаемых.
pub fn detect() -> String {
    let locale = system_locale().unwrap_or_default().to_lowercase();
    let code = locale.split('-').next().unwrap_or("");
    if is_supported(code) { code.to_string() } else { FALLBACK.to_string() }
}

#[cfg(windows)]
fn system_locale() -> Option<String> {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buf = [0u16; 85];
    let n = unsafe { GetUserDefaultLocaleName(&mut buf) };
    (n > 1).then(|| String::from_utf16_lossy(&buf[..n as usize - 1]))
}

#[cfg(not(windows))]
fn system_locale() -> Option<String> {
    std::env::var("LANG").ok().map(|l| l.replace('_', "-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_language_has_the_same_keys_as_english() {
        let d = dicts();
        let en = &d[FALLBACK];
        for (code, dict) in d {
            for key in en.keys() {
                assert!(dict.contains_key(key), "в {code}.json нет ключа {key}");
            }
            for key in dict.keys() {
                assert!(en.contains_key(key), "в {code}.json лишний ключ {key}");
            }
        }
    }

    #[test]
    fn substitutes_params_and_falls_back() {
        assert_eq!(t_with("ru", "err.bad_date", &[("err", "x")]), "неверная дата: x");
        assert_eq!(t("xx", "tray.quit"), "Quit");
        assert_eq!(t("en", "no.such.key"), "no.such.key");
    }
}
