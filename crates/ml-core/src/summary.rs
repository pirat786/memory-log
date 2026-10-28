use std::collections::HashMap;

use serde::Serialize;

use crate::model::{Bookmark, Category, ClipEntry, Download, Goal, ProcSample, Session, SystemSample, Timestamp};
use crate::stats::{self, Health, LoadStats, NetStats, ProcLoad, Rhythm};

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
pub struct GoalProgress {
    pub category: Category,
    pub limit_mins: u32,
    pub used_mins: u32,
}

/// Всё, что нужно для сводки одного дня.
pub struct DayInput<'a> {
    pub date: &'a str,
    pub sessions: &'a [Session],
    pub bookmarks: Vec<Bookmark>,
    pub system: &'a [SystemSample],
    pub procs: &'a [ProcSample],
    pub downloads: Vec<Download>,
    pub clips: Vec<ClipEntry>,
    pub goals: &'a [Goal],
}

#[derive(Debug, Clone, Serialize)]
pub struct DaySummary {
    pub date: String,
    pub active_secs: i64,
    pub by_category: Vec<CategoryTime>,
    pub top_apps: Vec<AppTime>,
    pub focus_blocks: Vec<FocusBlock>,
    /// Хронология дня: отрезки в одном окне, от новых к старым.
    pub timeline: Vec<Session>,
    pub bookmarks: Vec<Bookmark>,
    pub rhythm: Option<Rhythm>,
    pub load: Option<LoadStats>,
    pub net: Option<NetStats>,
    pub procs: Vec<ProcLoad>,
    pub health: Option<Health>,
    pub downloads: Vec<Download>,
    /// Скопированное за день, от новых к старым.
    pub clips: Vec<ClipEntry>,
    pub goals: Vec<GoalProgress>,
}

pub fn build(input: DayInput) -> DaySummary {
    let sessions = input.sessions;
    let mut cats: HashMap<Category, i64> = HashMap::new();
    let mut apps: HashMap<&str, i64> = HashMap::new();
    for s in sessions {
        *cats.entry(s.category).or_default() += s.secs();
        *apps.entry(s.app.as_str()).or_default() += s.secs();
    }

    let goals = input
        .goals
        .iter()
        .map(|g| GoalProgress {
            category: g.category,
            limit_mins: g.limit_mins,
            used_mins: (cats.get(&g.category).copied().unwrap_or(0) / 60) as u32,
        })
        .collect();

    let mut by_category: Vec<_> = cats.into_iter().map(|(category, secs)| CategoryTime { category, secs }).collect();
    by_category.sort_by(|a, b| b.secs.cmp(&a.secs));

    let mut top_apps: Vec<_> = apps.into_iter().map(|(app, secs)| AppTime { app: app.to_string(), secs }).collect();
    top_apps.sort_by(|a, b| b.secs.cmp(&a.secs));
    top_apps.truncate(8);

    DaySummary {
        date: input.date.to_string(),
        active_secs: sessions.iter().map(Session::secs).sum(),
        by_category,
        top_apps,
        focus_blocks: focus_blocks(sessions),
        timeline: timeline(sessions),
        bookmarks: input.bookmarks,
        rhythm: stats::rhythm(sessions),
        load: stats::load_stats(input.system),
        net: stats::net_stats(input.system),
        procs: stats::proc_loads(input.procs, 6),
        health: stats::health(input.system),
        downloads: input.downloads,
        clips: input.clips.into_iter().rev().collect(),
        goals,
    }
}

/// Подряд идущие отрезки в одной программе (заголовок окна не важен).
pub(crate) fn merge_blocks(sessions: &[Session]) -> Vec<FocusBlock> {
    let mut blocks: Vec<FocusBlock> = Vec::new();
    for s in sessions {
        match blocks.last_mut() {
            Some(b) if b.app == s.app && s.start - b.end <= FOCUS_GAP_SECS => b.end = s.end,
            _ => blocks.push(FocusBlock { app: s.app.clone(), start: s.start, end: s.end, category: s.category }),
        }
    }
    blocks
}

/// Отрезки короче этого (случайные переключения окон) в хронологию не попадают.
pub const TIMELINE_MIN_SECS: i64 = 15;

fn timeline(sessions: &[Session]) -> Vec<Session> {
    sessions.iter().rev().filter(|s| s.secs() >= TIMELINE_MIN_SECS).cloned().collect()
}

fn focus_blocks(sessions: &[Session]) -> Vec<FocusBlock> {
    let mut blocks = merge_blocks(sessions);
    blocks.retain(|b| b.end - b.start >= FOCUS_MIN_SECS);
    blocks.sort_by(|a, b| (b.end - b.start).cmp(&(a.end - a.start)));
    blocks.truncate(5);
    blocks
}

#[derive(Debug, Clone, Serialize)]
pub struct WeekDay {
    pub date: String,
    pub active_secs: i64,
    pub top_category: Option<Category>,
}

/// Краткая строка для недельного обзора.
pub fn week_day(date: &str, sessions: &[Session]) -> WeekDay {
    let mut cats: HashMap<Category, i64> = HashMap::new();
    for s in sessions {
        *cats.entry(s.category).or_default() += s.secs();
    }
    WeekDay {
        date: date.to_string(),
        active_secs: sessions.iter().map(Session::secs).sum(),
        top_category: cats.into_iter().max_by_key(|(_, secs)| *secs).map(|(c, _)| c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(start: i64, end: i64, app: &str, title: &str) -> Session {
        Session { start, end, app: app.into(), title: title.into(), category: Category::Work }
    }

    #[test]
    fn totals_focus_and_goals() {
        let sessions = vec![
            session(0, 600, "winword", "a"),
            session(610, 1500, "winword", "b"),
            session(1500, 1560, "chrome", "c"),
        ];
        let goals = [Goal { category: Category::Work, limit_mins: 20 }];
        let s = build(DayInput {
            date: "2026-10-27",
            sessions: &sessions,
            bookmarks: vec![],
            system: &[],
            procs: &[],
            downloads: vec![],
            clips: vec![],
            goals: &goals,
        });
        assert_eq!(s.active_secs, 1550);
        assert_eq!(s.top_apps[0].app, "winword");
        assert_eq!(s.focus_blocks.len(), 1);
        assert_eq!(s.focus_blocks[0].end, 1500);
        assert_eq!(s.goals[0].used_mins, 25);
        assert!(s.load.is_none());
        assert_eq!(s.timeline.len(), 3);
        assert_eq!(s.timeline[0].app, "chrome");
    }
}
