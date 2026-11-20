//! Лента дня для людей: сессии склеиваются в блоки по программам, чтобы не было сотен повторов.

use std::collections::HashMap;

use serde::Serialize;

use crate::model::{Category, Session, Timestamp};

/// Переключения короче этого (случайный Alt+Tab) в ленту не попадают и не разрывают блок.
pub const BLIP_SECS: i64 = 20;
/// Перерыв между двумя кусками одной программы, внутри которого блок не разрывается.
pub const BLOCK_GAP_SECS: i64 = 120;
/// Блоки с меньшим активным временем в ленте не показываются.
pub const MIN_BLOCK_SECS: i64 = 30;
/// Сколько названий окон внутри блока отдаём в интерфейс.
pub const MAX_TITLES: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TitleTime {
    pub title: String,
    pub secs: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TimelineBlock {
    pub app: String,
    pub start: Timestamp,
    pub end: Timestamp,
    /// Время, которое программа была на экране внутри блока (без вкраплений других программ).
    pub active_secs: i64,
    pub category: Category,
    /// Названия окон по убыванию времени, до `MAX_TITLES` штук.
    pub titles: Vec<TitleTime>,
    /// Сколько всего разных названий было в блоке (может быть больше `titles.len()`).
    pub title_count: u32,
}

const BROWSER_SUFFIXES: &[&str] = &[
    "Google Chrome",
    "Mozilla Firefox",
    "Microsoft Edge",
    "Microsoft\u{200b} Edge",
    "Opera",
    "Brave",
    "Vivaldi",
    "Яндекс Браузер",
    "Yandex",
];

/// Убирает из названия окна то, что только мешает: счётчик непрочитанного `(7) ` в начале
/// и имя браузера в конце. Для разных вкладок остаётся только их собственное название.
pub fn clean_title(title: &str) -> String {
    let mut t = title.trim();
    if let Some(rest) = t.strip_prefix('(') {
        if let Some(end) = rest.find(')') {
            if end > 0 && rest[..end].chars().all(|c| c.is_ascii_digit()) {
                t = rest[end + 1..].trim_start();
            }
        }
    }
    for sep in [" - ", " — ", " – "] {
        for browser in BROWSER_SUFFIXES {
            if let Some(stripped) = t.strip_suffix(&format!("{sep}{browser}")) {
                return stripped.trim().to_string();
            }
        }
    }
    t.to_string()
}

struct Acc {
    app: String,
    start: Timestamp,
    end: Timestamp,
    category: Category,
    titles: HashMap<String, i64>,
    active_secs: i64,
}

impl Acc {
    fn finish(self) -> TimelineBlock {
        let title_count = self.titles.len() as u32;
        let mut titles: Vec<TitleTime> =
            self.titles.into_iter().map(|(title, secs)| TitleTime { title, secs }).collect();
        titles.sort_by(|a, b| b.secs.cmp(&a.secs).then_with(|| a.title.cmp(&b.title)));
        titles.truncate(MAX_TITLES);
        TimelineBlock {
            app: self.app,
            start: self.start,
            end: self.end,
            active_secs: self.active_secs,
            category: self.category,
            titles,
            title_count,
        }
    }
}

/// Блоки дня от новых к старым.
pub fn build(sessions: &[Session]) -> Vec<TimelineBlock> {
    let mut blocks: Vec<Acc> = Vec::new();
    for s in sessions.iter().filter(|s| s.secs() >= BLIP_SECS) {
        let title = clean_title(&s.title);
        match blocks.last_mut() {
            Some(b) if b.app == s.app && s.start - b.end <= BLOCK_GAP_SECS => {
                b.end = s.end;
                b.active_secs += s.secs();
                *b.titles.entry(title).or_default() += s.secs();
            }
            _ => blocks.push(Acc {
                app: s.app.clone(),
                start: s.start,
                end: s.end,
                category: s.category,
                active_secs: s.secs(),
                titles: HashMap::from([(title, s.secs())]),
            }),
        }
    }
    blocks.into_iter().rev().filter(|b| b.active_secs >= MIN_BLOCK_SECS).map(Acc::finish).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(start: i64, end: i64, app: &str, title: &str) -> Session {
        Session { start, end, app: app.into(), title: title.into(), category: Category::Other }
    }

    #[test]
    fn short_detour_does_not_split_a_block() {
        let s = vec![
            session(0, 300, "chrome", "Doc - Google Chrome"),
            session(300, 310, "claude", "Claude"),
            session(310, 600, "chrome", "Doc - Google Chrome"),
        ];
        let t = build(&s);
        assert_eq!(t.len(), 1);
        assert_eq!((t[0].start, t[0].end), (0, 600));
        assert_eq!(t[0].active_secs, 590);
        assert_eq!(t[0].titles, vec![TitleTime { title: "Doc".into(), secs: 590 }]);
    }

    #[test]
    fn titles_of_one_app_are_grouped_and_sorted() {
        let s = vec![
            session(0, 100, "chrome", "A - Google Chrome"),
            session(100, 400, "chrome", "(3) B - Google Chrome"),
            session(400, 450, "chrome", "A - Google Chrome"),
        ];
        let t = build(&s);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].title_count, 2);
        assert_eq!(t[0].titles[0].title, "B");
        assert_eq!(t[0].titles[0].secs, 300);
        assert_eq!(t[0].titles[1], TitleTime { title: "A".into(), secs: 150 });
    }

    #[test]
    fn different_apps_stay_separate_and_newest_first() {
        let s = vec![session(0, 600, "winword", "x"), session(600, 900, "chrome", "y")];
        let t = build(&s);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].app, "chrome");
    }

    #[test]
    fn long_pause_splits_and_tiny_blocks_are_hidden() {
        let s = vec![
            session(0, 100, "a", "t"),
            session(1000, 1100, "a", "t"),
            session(2000, 2025, "b", "t"),
        ];
        let t = build(&s);
        assert_eq!(t.len(), 2);
        assert!(t.iter().all(|b| b.app == "a"));
    }

    #[test]
    fn cleans_browser_noise() {
        assert_eq!(clean_title("(7) Discord | @x - Google Chrome"), "Discord | @x");
        assert_eq!(clean_title("YouTube - Microsoft\u{200b} Edge"), "YouTube");
        assert_eq!(clean_title("Notes"), "Notes");
        assert_eq!(clean_title("(draft) Notes"), "(draft) Notes");
    }
}
