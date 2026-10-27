use crate::categorize::Categorizer;
use crate::model::{Sample, Session};

/// Если между снимками больше этого времени, считаем, что программа не работала.
pub const MAX_GAP_SECS: i64 = 30;
/// Сколько длится последний снимок, у которого нет следующего.
pub const TAIL_SECS: i64 = 3;

/// Склеивает снимки в непрерывные сессии. Простой в сессии не попадает.
/// `samples` должны быть отсортированы по времени.
pub fn sessionize(samples: &[Sample], categorizer: &Categorizer) -> Vec<Session> {
    let mut out: Vec<Session> = Vec::new();
    for (i, s) in samples.iter().enumerate() {
        let dur = match samples.get(i + 1) {
            Some(next) => (next.ts - s.ts).clamp(0, MAX_GAP_SECS),
            None => TAIL_SECS,
        };
        if s.idle || dur == 0 {
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

    fn sample(ts: i64, app: &str, title: &str, idle: bool) -> Sample {
        Sample { ts, app: app.into(), title: title.into(), idle }
    }

    #[test]
    fn merges_same_window_and_skips_idle() {
        let c = Categorizer::with_defaults();
        let samples = vec![
            sample(0, "winword", "a", false),
            sample(3, "winword", "a", false),
            sample(6, "winword", "a", true),
            sample(9, "chrome", "b", false),
        ];
        let s = sessionize(&samples, &c);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].start, s[0].end), (0, 6));
        assert_eq!(s[1].app, "chrome");
    }

    #[test]
    fn long_gap_breaks_session() {
        let c = Categorizer::with_defaults();
        let samples = vec![sample(0, "a", "t", false), sample(600, "a", "t", false)];
        assert_eq!(sessionize(&samples, &c).len(), 2);
    }
}
