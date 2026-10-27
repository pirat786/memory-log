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

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Work => "work",
            Category::Communication => "communication",
            Category::Social => "social",
            Category::Entertainment => "entertainment",
            Category::Shopping => "shopping",
            Category::Study => "study",
            Category::Games => "games",
            Category::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "work" => Category::Work,
            "communication" => Category::Communication,
            "social" => Category::Social,
            "entertainment" => Category::Entertainment,
            "shopping" => Category::Shopping,
            "study" => Category::Study,
            "games" => Category::Games,
            "other" => Category::Other,
            _ => return None,
        })
    }
}

/// Снимок состояния ПК. Трафик (`net_rx`, `net_tx`) — байты за интервал с прошлого снимка.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemSample {
    pub ts: Timestamp,
    pub cpu_pct: f32,
    pub mem_pct: f32,
    pub mem_used_mb: u32,
    pub net_rx: u64,
    pub net_tx: u64,
    pub disk_free_gb: Option<f32>,
    pub battery_pct: Option<u8>,
    pub on_battery: Option<bool>,
    pub temp_c: Option<f32>,
    pub uptime_s: u64,
}

/// Нагрузка одной программы (все её процессы вместе) в момент снимка.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcSample {
    pub ts: Timestamp,
    pub name: String,
    pub cpu_pct: f32,
    pub mem_mb: u32,
}

/// Дневная цель: не больше N минут в категории.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    pub category: Category,
    pub limit_mins: u32,
}

/// Файл, появившийся в папке загрузок.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Download {
    pub name: String,
    pub size: u64,
    pub ts: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub app: String,
    pub title: String,
    pub first_ts: Timestamp,
    pub last_ts: Timestamp,
    pub count: u32,
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
