use crate::categorize::Categorizer;
use crate::model::{Category, Sample, Session};

/// Если между снимками больше этого времени, считаем, что программа не работала.
/// Снимки идут раз в 3 секунды, но Windows может притормаживать фоновую программу,
/// поэтому запас большой: иначе каждая задержка «съедает» время.
pub const MAX_GAP_SECS: i64 = 90;
/// Сколько длится последний снимок, у которого нет следующего.
pub const TAIL_SECS: i64 = 3;
/// Без нажатий и движений мыши дольше этого человек считается отошедшим.
pub const IDLE_SECS: u32 = 10 * 60;
/// Видео и музыку смотрят без нажатий, поэтому для развлечений порог больше.
pub const MEDIA_IDLE_SECS: u32 = 45 * 60;

/// Склеивает снимки в непрерывные сессии. Время, когда человека не было, в сессии не попадает.
/// `samples` должны быть отсортированы по времени.
pub fn sessionize(samples: &[Sample], categorizer: &Categorizer) -> Vec<Session> {
    sessionize_with_away(samples, categorizer).0
}

/// То же, что `sessionize`, и ещё сколько секунд человека не было за компьютером.
/// Первые `IDLE_SECS` без ввода считаются активными (человек мог читать), отсчёт «не было» идёт после них.
pub fn sessionize_with_away(samples: &[Sample], categorizer: &Categorizer) -> (Vec<Session>, i64) {
    let mut out: Vec<Session> = Vec::new();
    let mut away = 0i64;
    for (i, s) in samples.iter().enumerate() {
        let dur = match samples.get(i + 1) {
            Some(next) => (next.ts - s.ts).clamp(0, MAX_GAP_SECS),
            None => TAIL_SECS,
        };
        if dur == 0 {
            continue;
        }
        let category = categorizer.categorize(&s.app, &s.title);
        let limit = if category == Category::Entertainment { MEDIA_IDLE_SECS } else { IDLE_SECS };
        if s.idle_secs >= limit {
            away += dur;
            continue;
        }
        if let Some(last) = out.last_mut() {
            if last.app == s.app && last.title == s.title && s.ts <= last.end {
                last.end = s.ts + dur;
                continue;
            }
        }
        out.push(Session { start: s.ts, end: s.ts + dur, app: s.app.clone(), title: s.title.clone(), category });
    }
    (out, away)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts: i64, app: &str, title: &str) -> Sample {
        Sample { ts, app: app.into(), title: title.into(), idle_secs: 0 }
    }

    fn idle_sample(ts: i64, app: &str, idle_secs: u32) -> Sample {
        Sample { ts, app: app.into(), title: "t".into(), idle_secs }
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

    #[test]
    fn idle_time_becomes_away_and_splits_the_session() {
        let c = Categorizer::with_defaults();
        let samples = vec![
            idle_sample(0, "winword", 5),
            idle_sample(3, "winword", 8),
            idle_sample(6, "winword", IDLE_SECS),
            idle_sample(9, "winword", IDLE_SECS + 3),
            idle_sample(12, "winword", 1),
            idle_sample(15, "winword", 4),
        ];
        let (s, away) = sessionize_with_away(&samples, &c);
        assert_eq!(away, 6);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].start, s[0].end), (0, 6));
        assert_eq!((s[1].start, s[1].end), (12, 18));
    }

    #[test]
    fn video_gets_a_longer_leash_than_documents() {
        let c = Categorizer::with_defaults();
        let quiet = IDLE_SECS + 60;
        // Без ввода 11 минут: документ — отошёл, видео на YouTube — смотрит.
        let doc = vec![idle_sample(0, "winword", quiet), idle_sample(3, "winword", quiet)];
        assert_eq!(sessionize_with_away(&doc, &c).1, 6);

        let mut video = vec![sample(0, "chrome", "Cats - YouTube"), sample(3, "chrome", "Cats - YouTube")];
        for s in &mut video {
            s.idle_secs = quiet;
        }
        let (s, away) = sessionize_with_away(&video, &c);
        assert_eq!(away, 0);
        assert_eq!(s.len(), 1);

        for s in &mut video {
            s.idle_secs = MEDIA_IDLE_SECS + 1;
        }
        assert_eq!(sessionize_with_away(&video, &c).1, 6);
    }
}
