use chrono::{Duration, Local, NaiveDate, TimeZone};

use crate::model::Timestamp;

/// Границы локальных суток `[start, end)` в секундах Unix.
pub fn day_bounds(date: NaiveDate) -> (Timestamp, Timestamp) {
    (local_midnight(date), local_midnight(date + Duration::days(1)))
}

pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

fn local_midnight(date: NaiveDate) -> Timestamp {
    let naive = date.and_hms_opt(0, 0, 0).expect("полночь существует");
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|d| d.timestamp())
        .unwrap_or_else(|| naive.and_utc().timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_is_about_24_hours() {
        let d = NaiveDate::from_ymd_opt(2026, 10, 27).unwrap();
        let (a, b) = day_bounds(d);
        assert!((23 * 3600..=25 * 3600).contains(&(b - a)));
    }
}
