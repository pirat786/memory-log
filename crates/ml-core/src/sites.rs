//! Сайты по заголовкам вкладок браузера. Адреса страниц мы не читаем, поэтому узнаём только известные сайты.

/// Программы, которые считаем браузерами (имя процесса без `.exe`, в нижнем регистре).
const BROWSERS: &[&str] = &["chrome", "msedge", "firefox", "opera", "brave", "vivaldi", "browser", "arc"];

/// Фрагмент заголовка (в нижнем регистре) и название сайта. Более частные записи идут раньше общих.
const SITES: &[(&str, &str)] = &[
    ("youtube", "YouTube"),
    ("discord", "Discord"),
    ("github", "GitHub"),
    ("stack overflow", "Stack Overflow"),
    ("stackoverflow", "Stack Overflow"),
    ("twitch", "Twitch"),
    ("chatgpt", "ChatGPT"),
    ("claude", "Claude"),
    ("gmail", "Gmail"),
    ("google docs", "Google Docs"),
    ("google sheets", "Google Sheets"),
    ("google таблицы", "Google Sheets"),
    ("google документы", "Google Docs"),
    ("telegram", "Telegram"),
    ("whatsapp", "WhatsApp"),
    ("вконтакте", "VK"),
    ("vk.com", "VK"),
    ("instagram", "Instagram"),
    ("facebook", "Facebook"),
    ("tiktok", "TikTok"),
    ("reddit", "Reddit"),
    ("twitter", "X"),
    ("wikipedia", "Wikipedia"),
    ("википедия", "Wikipedia"),
    ("netflix", "Netflix"),
    ("kinopoisk", "Kinopoisk"),
    ("кинопоиск", "Kinopoisk"),
    ("rutube", "Rutube"),
    ("avito", "Avito"),
    ("авито", "Avito"),
    ("ozon", "Ozon"),
    ("wildberries", "Wildberries"),
    ("aliexpress", "AliExpress"),
    ("amazon", "Amazon"),
    ("ebay", "eBay"),
    ("яндекс", "Yandex"),
    ("yandex", "Yandex"),
    ("google", "Google"),
];

pub fn is_browser(app: &str) -> bool {
    BROWSERS.contains(&app.to_lowercase().as_str())
}

/// `None` — программа не браузер; `Some("")` — браузер, но сайт не узнан; иначе название сайта.
pub fn site_of(app: &str, title: &str) -> Option<&'static str> {
    if !is_browser(app) {
        return None;
    }
    let title = title.to_lowercase();
    Some(SITES.iter().find(|(needle, _)| title.contains(needle)).map_or("", |(_, name)| name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_known_sites_in_browsers_only() {
        assert_eq!(site_of("chrome", "Cats - YouTube"), Some("YouTube"));
        assert_eq!(site_of("msedge", "(3) Discord | @x"), Some("Discord"));
        assert_eq!(site_of("chrome", "Мой личный сайт"), Some(""));
        assert_eq!(site_of("explorer", "YouTube downloads"), None);
    }

    #[test]
    fn specific_beats_general() {
        assert_eq!(site_of("chrome", "How to X - Stack Overflow - Google"), Some("Stack Overflow"));
    }
}
