use crate::categorize::Categorizer;
use crate::model::{ActivityEvent, Session, Timestamp};

/// При более длинном промежутке считаем только обычную длительность первого снимка.
pub const MAX_GAP_SECS: i64 = 90;
pub const TAIL_SECS: i64 = 3;
/// Порог для старых записей, в которых сохранено только время без ввода.
pub const LEGACY_IDLE_SECS: u32 = 60 * 60;

pub fn sessionize(
    events: &[ActivityEvent],
    categorizer: &Categorizer,
    bounds: (Timestamp, Timestamp),
) -> Vec<Session> {
    sessionize_with_away(events, categorizer, bounds).0
}

/// Склеивает отсортированные снимки в пределах дня и считает подтверждённый простой.
pub fn sessionize_with_away(
    events: &[ActivityEvent],
    categorizer: &Categorizer,
    (from, to): (Timestamp, Timestamp),
) -> (Vec<Session>, i64) {
    let mut out: Vec<Session> = Vec::new();
    let mut away = 0;
    let mut can_merge = false;
    for (i, event) in events.iter().enumerate() {
        if matches!(event, ActivityEvent::Break(_)) {
            can_merge = false;
            continue;
        }
        let ts = event.timestamp();
        let duration = match events.get(i + 1) {
            Some(ActivityEvent::Break(end)) => (end - ts).min(TAIL_SECS),
            Some(next) if next.timestamp() - ts <= MAX_GAP_SECS => next.timestamp() - ts,
            _ => TAIL_SECS,
        };
        let start = ts.max(from);
        let end = (ts + duration).min(to);
        if end <= start {
            continue;
        }
        let s = match event {
            ActivityEvent::Break(_) => continue,
            ActivityEvent::Idle(_) => {
                away += end - start;
                continue;
            }
            ActivityEvent::Window(s) if s.idle_secs >= LEGACY_IDLE_SECS => {
                away += end - start;
                continue;
            }
            ActivityEvent::Window(s) => s,
        };
        if let Some(last) = out.last_mut() {
            if can_merge && last.app == s.app && last.title == s.title && start == last.end {
                last.end = end;
                continue;
            }
        }
        out.push(Session {
            start,
            end,
            app: s.app.clone(),
            title: s.title.clone(),
            category: categorizer.categorize(&s.app, &s.title),
        });
        can_merge = true;
    }
    (out, away)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Sample;

    fn sample(ts: i64, app: &str, title: &str) -> ActivityEvent {
        ActivityEvent::Window(Sample { ts, app: app.into(), title: title.into(), idle_secs: 0 })
    }

    #[test]
    fn merges_same_window_and_counts_everything() {
        let c = Categorizer::with_defaults();
        let samples = vec![
            sample(0, "winword", "a"),
            sample(3, "winword", "a"),
            sample(6, "winword", "a"),
            sample(9, "chrome", "b"),
        ];
        let s = sessionize(&samples, &c, (0, 3600));
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].start, s[0].end), (0, 9));
        assert_eq!(s[1].app, "chrome");
    }

    #[test]
    fn delayed_snapshots_still_count() {
        let c = Categorizer::with_defaults();
        let samples = vec![sample(0, "a", "t"), sample(60, "a", "t"), sample(120, "a", "t")];
        let s = sessionize(&samples, &c, (0, 3600));
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].end - s[0].start, 123);
    }

    #[test]
    fn long_gap_breaks_session() {
        let c = Categorizer::with_defaults();
        let samples = vec![sample(0, "a", "t"), sample(600, "a", "t")];
        let s = sessionize(&samples, &c, (0, 3600));
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].end, TAIL_SECS);
    }

    #[test]
    fn breaks_stop_activity_and_sessions_respect_day_bounds() {
        let c = Categorizer::with_defaults();
        let events = [sample(8, "a", "t"), ActivityEvent::Break(11), sample(30, "a", "t"), sample(33, "a", "t")];
        let s = sessionize(&events, &c, (10, 32));
        assert_eq!(s.iter().map(|s| (s.start, s.end)).collect::<Vec<_>>(), [(10, 11), (30, 32)]);
    }

    #[test]
    fn break_in_the_same_second_stops_previous_window() {
        let c = Categorizer::with_defaults();
        let events = [sample(10, "a", "t"), ActivityEvent::Break(10), sample(10, "b", "t")];
        let s = sessionize(&events, &c, (0, 3600));
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].app, "b");
    }

    #[test]
    fn idle_time_becomes_away_and_splits_the_session() {
        let c = Categorizer::with_defaults();
        let samples = vec![
            sample(0, "winword", "t"),
            sample(3, "winword", "t"),
            ActivityEvent::Idle(6),
            ActivityEvent::Idle(9),
            sample(12, "winword", "t"),
            sample(15, "winword", "t"),
        ];
        let (s, away) = sessionize_with_away(&samples, &c, (0, 3600));
        assert_eq!(away, 6);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].start, s[0].end), (0, 6));
        assert_eq!((s[1].start, s[1].end), (12, 18));
    }

    #[test]
    fn historical_idle_respects_the_previous_one_hour_threshold() {
        let c = Categorizer::with_defaults();
        let events = [3599, 3600].map(|idle_secs| ActivityEvent::Window(Sample {
            ts: (idle_secs as i64 - 3599) * TAIL_SECS,
            app: "editor".into(),
            title: "t".into(),
            idle_secs,
        }));
        let (sessions, away) = sessionize_with_away(&events, &c, (1, 5));
        assert_eq!(sessions.iter().map(Session::secs).sum::<i64>(), 2);
        assert_eq!(away, 2);
    }
}
