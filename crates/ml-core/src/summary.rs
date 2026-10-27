use std::collections::HashMap;

use serde::Serialize;

use crate::model::{Bookmark, Category, Session, Timestamp};

/// Минимальная длина «фокуса», чтобы попасть в сводку.
pub const FOCUS_MIN_SECS: i64 = 20 * 60;
/// Перерыв внутри одного фокуса, который не считается выходом из него.
pub const FOCUS_GAP_SECS: i64 = 60;

#[derive(Debug, Clone, Serialize)]
pub struct CategoryTime {
    pub category: Category,
    pub secs: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppTime {
    pub app: String,
    pub secs: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FocusBlock {
    pub app: String,
    pub start: Timestamp,
    pub end: Timestamp,
    pub category: Category,
}

#[derive(Debug, Clone, Serialize)]
pub struct DaySummary {
    pub date: String,
    pub active_secs: i64,
    pub by_category: Vec<CategoryTime>,
    pub top_apps: Vec<AppTime>,
    pub focus_blocks: Vec<FocusBlock>,
    pub bookmarks: Vec<Bookmark>,
}

pub fn build(date: &str, sessions: &[Session], bookmarks: Vec<Bookmark>) -> DaySummary {
    let mut cats: HashMap<Category, i64> = HashMap::new();
    let mut apps: HashMap<&str, i64> = HashMap::new();
    for s in sessions {
        *cats.entry(s.category).or_default() += s.secs();
        *apps.entry(s.app.as_str()).or_default() += s.secs();
    }

    let mut by_category: Vec<_> = cats.into_iter().map(|(category, secs)| CategoryTime { category, secs }).collect();
    by_category.sort_by(|a, b| b.secs.cmp(&a.secs));

    let mut top_apps: Vec<_> = apps.into_iter().map(|(app, secs)| AppTime { app: app.to_string(), secs }).collect();
    top_apps.sort_by(|a, b| b.secs.cmp(&a.secs));
    top_apps.truncate(8);

    DaySummary {
        date: date.to_string(),
        active_secs: sessions.iter().map(Session::secs).sum(),
        by_category,
        top_apps,
        focus_blocks: focus_blocks(sessions),
        bookmarks,
    }
}

/// Длинные подряд идущие отрезки в одной программе (заголовок окна не важен).
fn focus_blocks(sessions: &[Session]) -> Vec<FocusBlock> {
    let mut blocks: Vec<FocusBlock> = Vec::new();
    for s in sessions {
        match blocks.last_mut() {
            Some(b) if b.app == s.app && s.start - b.end <= FOCUS_GAP_SECS => b.end = s.end,
            _ => blocks.push(FocusBlock { app: s.app.clone(), start: s.start, end: s.end, category: s.category }),
        }
    }
    blocks.retain(|b| b.end - b.start >= FOCUS_MIN_SECS);
    blocks.sort_by(|a, b| (b.end - b.start).cmp(&(a.end - a.start)));
    blocks.truncate(5);
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(start: i64, end: i64, app: &str, title: &str) -> Session {
        Session { start, end, app: app.into(), title: title.into(), category: Category::Work }
    }

    #[test]
    fn totals_and_focus() {
        let sessions = vec![
            session(0, 600, "winword", "a"),
            session(610, 1500, "winword", "b"),
            session(1500, 1560, "chrome", "c"),
        ];
        let s = build("2026-10-27", &sessions, vec![]);
        assert_eq!(s.active_secs, 1560);
        assert_eq!(s.top_apps[0].app, "winword");
        assert_eq!(s.focus_blocks.len(), 1);
        assert_eq!(s.focus_blocks[0].end, 1500);
    }
}
