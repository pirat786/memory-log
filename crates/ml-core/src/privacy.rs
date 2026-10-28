use crate::model::Sample;

pub const HIDDEN_APP: &str = "(скрыто)";

/// Прячет чувствительные окна ДО записи в базу: то, чего нет в базе, нельзя украсть.
pub struct PrivacyFilter {
    hidden_apps: Vec<String>,
    hidden_titles: Vec<String>,
}

impl Default for PrivacyFilter {
    fn default() -> Self {
        let lower = |items: &[&str]| items.iter().map(|s| s.to_lowercase()).collect();
        Self {
            hidden_apps: lower(&["keepass", "bitwarden", "1password", "lastpass"]),
            hidden_titles: lower(&[
                "password", "пароль", "банк", "bank", "incognito", "инкогнито", "private browsing",
            ]),
        }
    }
}

impl PrivacyFilter {
    pub fn new(hidden_apps: Vec<String>, hidden_titles: Vec<String>) -> Self {
        Self {
            hidden_apps: hidden_apps.into_iter().map(|s| s.to_lowercase()).collect(),
            hidden_titles: hidden_titles.into_iter().map(|s| s.to_lowercase()).collect(),
        }
    }

    pub fn apply(&self, mut sample: Sample) -> Sample {
        let app = sample.app.to_lowercase();
        let title = sample.title.to_lowercase();
        let hidden = self.hidden_apps.iter().any(|p| app.contains(p))
            || self.hidden_titles.iter().any(|p| title.contains(p));
        if hidden {
            sample.app = HIDDEN_APP.to_string();
            sample.title.clear();
        }
        sample
    }
}

/// Похоже ли скопированное на пароль или ключ: одно «слово» без пробелов, в котором перемешаны
/// строчные и заглавные буквы, цифры и знаки. Такие тексты в историю буфера не попадают.
/// Это эвристика: редкий пароль из одних букв она не поймает, а идентификатор вида `getUser2` пропустит.
pub fn looks_like_secret(text: &str) -> bool {
    let t = text.trim();
    let n = t.chars().count();
    if !(6..=128).contains(&n) || t.chars().any(char::is_whitespace) {
        return false;
    }
    let lower = t.to_lowercase();
    // Адреса, пути и почта на пароли не похожи.
    if lower.contains("://") || lower.starts_with("www.") || t.contains('/') || t.contains('\\') {
        return false;
    }
    if let Some((_, domain)) = t.split_once('@') {
        if domain.contains('.') && !domain.contains(|c: char| !c.is_alphanumeric() && c != '.' && c != '-') {
            return false;
        }
    }
    // «имя.расширение» — скорее файл или сайт.
    if let Some((_, ext)) = t.rsplit_once('.') {
        if (2..=5).contains(&ext.chars().count()) && ext.chars().all(|c| c.is_ascii_alphabetic()) {
            return false;
        }
    }
    let has = |f: fn(char) -> bool| t.chars().any(f);
    let classes = [
        has(char::is_lowercase),
        has(char::is_uppercase),
        has(|c| c.is_ascii_digit()),
        has(|c| !c.is_alphanumeric()),
    ]
    .iter()
    .filter(|&&b| b)
    .count();
    // Длинная строка из букв и цифр — вероятнее всего ключ или токен.
    classes >= 3 || (n >= 24 && classes >= 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_password_like_text() {
        for secret in ["P@ssw0rd!2024", "Xk9#mQ2$vL", "9f86d081884c7d659a2feaa0c55ad015", "Qwerty123!"] {
            assert!(looks_like_secret(secret), "{secret}");
        }
        for plain in [
            "привет", "Текст договора", "https://example.com/a?b=1", "report_final_2024.docx", "ivan@mail.ru",
            "C:\\Users\\a\\b.txt", "hello", "12345", "две строки\nтекста",
        ] {
            assert!(!looks_like_secret(plain), "{plain}");
        }
    }

    #[test]
    fn hides_sensitive_titles() {
        let f = PrivacyFilter::default();
        let s = Sample { ts: 0, app: "chrome".into(), title: "Мой банк — вход".into() };
        let out = f.apply(s);
        assert_eq!(out.app, HIDDEN_APP);
        assert!(out.title.is_empty());
    }
}
