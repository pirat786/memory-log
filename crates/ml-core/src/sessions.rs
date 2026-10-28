use crate::categorize::Categorizer;
use crate::model::{Sample, Session};

/// Если между снимками больше этого времени, считаем, что программа не работала.
/// Снимки идут раз в 3 секунды, но Windows может притормаживать фоновую программу,
/// поэтому запас большой: иначе каждая задержка «съедает» время.
pub const MAX_GAP_SECS: i64 = 90;
/// Сколько длится последний снимок, у которого нет следующего.
pub const TAIL_SECS: i64 = 3;

/// Склеивает снимки в непрерывные сессии. Время идёт непрерывно, пока работает программа.
/// `samples` должны быть отсортированы по времени.
pub fn sessionize(samples: &[Sample], categorizer: &Categorizer) -> Vec<Session> {
    let mut out: Vec<Session> = Vec::new();
    for (i, s) in samples.iter().enumerate() {
        let dur = match samples.get(i + 1) {
            Some(next) => (next.ts - s.ts).clamp(0, MAX_GAP_SECS),
            None => TAIL_SECS,
        };
        // Время идёт всё время, пока ПК включён и программа работает; простой не вычитается.
        if dur == 0 {
            continue;
        }
        if let Some(last) = out.last_mut() {
            if last.app == s.app && last.title == s.title && s.ts <= last.end {
                last.end = s.ts + dur;
                continue;
            }
        }
        out.push(Session {
            start: s.ts,
            end: s.ts + dur,
            app: s.app.clone(),
            title: s.title.clone(),
            category: categorizer.categorize(&s.app, &s.title),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts: i64, app: &str, title: &str) -> Sample {
        Sample { ts, app: app.into(), title: title.into() }
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
        let s = sessionize(&samples, &c);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].start, s[0].end), (0, 9));
        assert_eq!(s[1].app, "chrome");
    }

    #[test]
    fn delayed_snapshots_still_count() {
        let c = Categorizer::with_defaults();
        let samples = vec![sample(0, "a", "t"), sample(60, "a", "t"), sample(120, "a", "t")];
        let s = sessionize(&samples, &c);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].end - s[0].start, 123);
    }

    #[test]
    fn long_gap_breaks_session() {
        let c = Categorizer::with_defaults();
        let samples = vec![sample(0, "a", "t"), sample(600, "a", "t")];
        assert_eq!(sessionize(&samples, &c).len(), 2);
    }
}
