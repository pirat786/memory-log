use std::collections::HashMap;

use chrono::{Datelike, DateTime, Duration, Local, NaiveDate, NaiveTime, TimeZone};
use ml_core::{Category, Goal, Session};
use serde::Serialize;

use crate::state::AppState;

#[derive(Default, Serialize)]
pub struct WeekTotals {
    from: String,
    through: String,
    active_secs: i64,
    recorded_days: u32,
    categories: HashMap<Category, i64>,
    apps: HashMap<String, i64>,
    goals_met: HashMap<Category, u32>,
}

#[derive(Serialize)]
pub struct WeekComparison {
    current: WeekTotals,
    previous: WeekTotals,
    goals: Vec<Goal>,
}

fn timestamp(date: NaiveDate, time: NaiveTime) -> i64 {
    let local = date.and_time(time);
    Local.from_local_datetime(&local).earliest()
        .unwrap_or_else(|| local.and_utc().with_timezone(&Local)).timestamp()
}

fn aggregate(
    start: NaiveDate,
    now: DateTime<Local>,
    hours: Option<(NaiveTime, NaiveTime)>,
    goals: &[Goal],
    load_day: impl Fn(NaiveDate) -> Result<Vec<Session>, String>,
) -> Result<WeekTotals, String> {
    let offset = now.weekday().num_days_from_monday();
    let through = start + Duration::days(offset.into());
    let mut totals = WeekTotals { from: start.to_string(), through: through.to_string(), ..Default::default() };
    for index in 0..=offset {
        if index >= 5 && hours.is_some() {
            break;
        }
        let date = start + Duration::days(index.into());
        let (mut from, mut to) = ml_core::time::day_bounds(date);
        if let Some((start, end)) = hours {
            from = timestamp(date, start);
            to = timestamp(date, end);
        }
        if index == offset {
            to = to.min(timestamp(date, now.time()));
        }
        if to <= from {
            continue;
        }
        let mut daily = HashMap::new();
        for session in load_day(date)? {
            let seconds = session.end.min(to) - session.start.max(from);
            if seconds > 0 {
                totals.active_secs += seconds;
                *totals.apps.entry(session.app).or_default() += seconds;
                *totals.categories.entry(session.category).or_default() += seconds;
                *daily.entry(session.category).or_insert(0i64) += seconds;
            }
        }
        if !daily.is_empty() {
            totals.recorded_days += 1;
            for goal in goals {
                if daily.get(&goal.category).copied().unwrap_or(0) <= i64::from(goal.limit_mins) * 60 {
                    *totals.goals_met.entry(goal.category).or_default() += 1;
                }
            }
        }
    }
    Ok(totals)
}

pub fn compare(state: &AppState, work_hours: Option<[u32; 2]>) -> Result<WeekComparison, String> {
    let hours = match work_hours {
        Some([start, end]) if start < end && end < 1440 => Some((
            NaiveTime::from_num_seconds_from_midnight_opt(start * 60, 0).unwrap(),
            NaiveTime::from_num_seconds_from_midnight_opt(end * 60, 0).unwrap(),
        )),
        Some(_) => return Err(crate::i18n::t(&state.lang(), "compare.invalid_hours")),
        None => None,
    };
    let now = Local::now();
    let today = now.date_naive();
    let offset = today.weekday().num_days_from_monday();
    let monday = today - Duration::days(offset.into());
    let st = state.storage.lock().map_err(|e| e.to_string())?;
    let goals = st.goals().map_err(|e| e.to_string())?;
    let load = |date| state.sessions_for_day(&st, date).map_err(|e| e.to_string());
    Ok(WeekComparison {
        current: aggregate(monday, now, hours, &goals, load)?,
        previous: aggregate(monday - Duration::days(7), now, hours, &goals, load)?,
        goals,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparable_work_hours_clip_sessions_and_count_goal_days() {
        let now = Local.with_ymd_and_hms(2026, 11, 18, 10, 0, 0).single().unwrap();
        let monday = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        let workday = |hour| NaiveTime::from_hms_opt(hour, 0, 0).unwrap();
        let goals = [Goal { category: Category::Work, limit_mins: 120 }];
        let totals = aggregate(monday, now, Some((workday(9), workday(18))), &goals, |date| {
            if date.weekday() == chrono::Weekday::Tue {
                return Ok(vec![]);
            }
            Ok(vec![Session {
                start: timestamp(date, workday(8)), end: timestamp(date, workday(12)),
                app: "editor".into(), title: "Document".into(), category: Category::Work,
            }])
        }).unwrap();
        assert_eq!(totals.through, "2026-11-11");
        assert_eq!(totals.active_secs, 4 * 3600);
        assert_eq!(totals.recorded_days, 2);
        assert_eq!(totals.goals_met[&Category::Work], 1);
    }
}
