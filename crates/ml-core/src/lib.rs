//! Чистая логика Memory Log: модели, категории, сессии, сводка.
//! Здесь нет работы с ОС, диском и интерфейсом, поэтому всё легко тестируется.

pub mod categorize;
pub mod model;
pub mod privacy;
pub mod sessions;
pub mod stats;
pub mod summary;
pub mod time;

pub use categorize::Categorizer;
pub use model::{Bookmark, Category, ClipEntry, Download, Goal, ProcSample, Sample, SearchHit, Session, SystemSample, Timestamp};
pub use privacy::PrivacyFilter;
pub use summary::{DayInput, DaySummary, WeekDay};
