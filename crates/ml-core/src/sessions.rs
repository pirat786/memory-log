use crate::categorize::Categorizer;
use crate::model::{ActivityEvent, Session, Timestamp};

/// Если между снимками больше этого времени, считаем, что программа не работала.
/// Снимки идут раз в 3 секунды, но Windows может притормаживать фоновую программу,
/// поэтому запас большой: иначе каждая задержка «съедает» время.
pub const MAX_GAP_SECS: i64 = 90;
/// Сколько длится последний снимок, у которого нет следующего.
pub const TAIL_SECS: i64 = 3;

/// Склеивает снимки в пределах дня. События должны быть отсортированы по времени.
pub fn sessionize(
    events: &[ActivityEvent],
    categorizer: &Categorizer,
    (from, to): (Timestamp, Timestamp),
) -> Vec<Session> {
    let mut out: Vec<Session> = Vec::new();
    let mut can_merge = false;
    for (i, event) in events.iter().enumerate() {
        let ActivityEvent::Window(s) = event else {
            can_merge = false;
            continue;
        };
        let dur = match events.get(i + 1) {
            Some(ActivityEvent::Break(ts)) => (ts - s.ts).min(TAIL_SECS),
            Some(ActivityEvent::Window(next)) if next.ts - s.ts <= MAX_GAP_SECS => next.ts - s.ts,
            _ => TAIL_SECS,
        };
        let start = s.ts.max(from);
        let end = (s.ts + dur).min(to);
        if end <= start {
            continue;
        }
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
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Sample;

    fn sample(ts: i64, app: &str, title: &str) -> ActivityEvent {
        ActivityEvent::Window(Sample { ts, app: app.into(), title: title.into() })
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
}
