use serde::{Deserialize, Serialize};

/// Секунды Unix (UTC).
pub type Timestamp = i64;

/// Один «снимок» активности: что было на экране в момент `ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sample {
    pub ts: Timestamp,
    pub app: String,
    pub title: String,
    /// Пользователь не трогал мышь и клавиатуру дольше порога.
    pub idle: bool,
}

/// Отметка «запомнить это», поставленная пользователем.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub ts: Timestamp,
    pub app: String,
    pub title: String,
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Work,
    Communication,
    Social,
    Entertainment,
    Shopping,
    Study,
    Games,
    Other,
}

/// Непрерывный отрезок времени в одном окне.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub start: Timestamp,
    pub end: Timestamp,
    pub app: String,
    pub title: String,
    pub category: Category,
}

impl Session {
    pub fn secs(&self) -> i64 {
        self.end - self.start
    }
}
