use std::error::Error;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use chrono::Utc;
use ml_core::{sessions, summary, time, Bookmark, Categorizer, DaySummary};
use ml_storage::Storage;

/// Общее состояние приложения: база, правила категорий, флаг паузы.
pub struct AppState {
    pub storage: Mutex<Storage>,
    pub categorizer: Categorizer,
    pub paused: AtomicBool,
}

impl AppState {
    pub fn open(db_path: &Path) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            storage: Mutex::new(Storage::open(db_path)?),
            categorizer: Categorizer::with_defaults(),
            paused: AtomicBool::new(false),
        })
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn set_paused(&self, value: bool) {
        self.paused.store(value, Ordering::Relaxed);
    }

    /// Сводка за день в формате `YYYY-MM-DD`; без даты — за сегодня.
    pub fn day_summary(&self, date: Option<&str>) -> Result<DaySummary, String> {
        let date = match date {
            Some(d) => d.parse().map_err(|e| format!("неверная дата: {e}"))?,
            None => time::today(),
        };
        let (from, to) = time::day_bounds(date);
        let storage = self.storage.lock().map_err(|e| e.to_string())?;
        let samples = storage.samples_between(from, to).map_err(|e| e.to_string())?;
        let bookmarks = storage.bookmarks_between(from, to).map_err(|e| e.to_string())?;
        let sessions = sessions::sessionize(&samples, &self.categorizer);
        Ok(summary::build(&date.to_string(), &sessions, bookmarks))
    }

    /// «Запомнить это»: отметка привязывается к последнему известному окну.
    pub fn add_bookmark(&self, note: &str) -> Result<(), String> {
        let storage = self.storage.lock().map_err(|e| e.to_string())?;
        let last = storage.last_sample().map_err(|e| e.to_string())?;
        let (app, title) = last.map(|s| (s.app, s.title)).unwrap_or_default();
        storage
            .add_bookmark(&Bookmark { ts: Utc::now().timestamp(), app, title, note: note.to_string() })
            .map_err(|e| e.to_string())
    }
}
