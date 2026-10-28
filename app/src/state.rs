use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use chrono::{Local, NaiveDate, TimeZone, Utc};
use ml_core::{sessions, summary, time, Bookmark, Categorizer, Category, DayInput, DaySummary, Download, SearchHit, WeekDay};
use ml_storage::Storage;
use serde::Serialize;

use crate::log;

#[derive(Serialize)]
pub struct Status {
    pub paused: bool,
    pub last_sample_age_secs: Option<i64>,
}

/// Где мы сейчас при листании буфера горячими клавишами.
pub struct ClipNav {
    pub sel: usize,
    pub last: Option<Instant>,
    /// Растёт при каждом нажатии: по нему плашка понимает, что скрывать её пока рано.
    pub generation: u64,
}

/// Общее состояние приложения: база, правила категорий, флаг паузы.
pub struct AppState {
    pub storage: Mutex<Storage>,
    pub categorizer: Categorizer,
    pub paused: AtomicBool,
    /// Номер изменения буфера, сделанного листанием: сборщик его не записывает.
    pub clip_ignore: AtomicU32,
    pub clip_nav: Mutex<ClipNav>,
    /// Последнее содержимое плашки: только что созданная плашка забирает его сама.
    pub clip_payload: Mutex<Option<serde_json::Value>>,
    downloads_dir: Option<PathBuf>,
    db_path: PathBuf,
}

fn parse_date(date: Option<&str>) -> Result<NaiveDate, String> {
    match date {
        Some(d) => d.parse().map_err(|e| format!("неверная дата: {e}")),
        None => Ok(time::today()),
    }
}

fn csv_field(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn fmt_ts(ts: i64) -> String {
    Local.timestamp_opt(ts, 0).single().map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default()
}

impl AppState {
    pub fn open(db_path: &Path, downloads_dir: Option<PathBuf>) -> Result<Self, Box<dyn Error>> {
        let (storage, moved) = Storage::open_or_recover(db_path)?;
        if let Some(moved) = moved {
            log::info(&format!("база была повреждена, старый файл сохранён: {}", moved.display()));
        }
        Ok(Self {
            storage: Mutex::new(storage),
            categorizer: Categorizer::with_defaults(),
            paused: AtomicBool::new(false),
            clip_ignore: AtomicU32::new(0),
            clip_nav: Mutex::new(ClipNav { sel: 0, last: None, generation: 0 }),
            clip_payload: Mutex::new(None),
            downloads_dir,
            db_path: db_path.to_path_buf(),
        })
    }

    /// Вызывается, когда во время работы выяснилось, что база повреждена:
    /// откладывает файл и начинает новую базу, чтобы запись не останавливалась.
    pub fn recover_database(&self) {
        let Ok(mut storage) = self.storage.lock() else { return };
        // Windows не даёт переименовать файл, пока он открыт, поэтому сначала закрываем
        // старое соединение (подменяем его временной базой в памяти).
        if let Ok(placeholder) = Storage::open_in_memory() {
            drop(std::mem::replace(&mut *storage, placeholder));
        }
        match Storage::open_or_recover(&self.db_path) {
            Ok((fresh, moved)) => {
                if let Some(moved) = moved {
                    log::info(&format!("база повреждена во время работы, старый файл: {}", moved.display()));
                }
                *storage = fresh;
            }
            Err(e) => log::info(&format!("не удалось восстановить базу: {e}")),
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn set_paused(&self, value: bool) {
        self.paused.store(value, Ordering::Relaxed);
    }

    pub fn status(&self) -> Result<Status, String> {
        let last = self.storage.lock().map_err(|e| e.to_string())?.last_sample().map_err(|e| e.to_string())?;
        Ok(Status { paused: self.is_paused(), last_sample_age_secs: last.map(|s| Utc::now().timestamp() - s.ts) })
    }

    /// Сводка за день в формате `YYYY-MM-DD`; без даты — за сегодня.
    pub fn day_summary(&self, date: Option<&str>) -> Result<DaySummary, String> {
        let date = parse_date(date)?;
        let (from, to) = time::day_bounds(date);
        let (samples, bookmarks, system, procs, goals, clips) = {
            let st = self.storage.lock().map_err(|e| e.to_string())?;
            let e = |e: ml_storage::StorageError| e.to_string();
            (
                st.samples_between(from, to).map_err(e)?,
                st.bookmarks_between(from, to).map_err(e)?,
                st.system_between(from, to).map_err(e)?,
                st.procs_between(from, to).map_err(e)?,
                st.goals().map_err(e)?,
                st.clips_between(from, to).map_err(e)?,
            )
        };
        let sessions = sessions::sessionize(&samples, &self.categorizer);
        Ok(summary::build(DayInput {
            date: &date.to_string(),
            sessions: &sessions,
            bookmarks,
            system: &system,
            procs: &procs,
            downloads: self.downloads(from, to),
            clips,
            goals: &goals,
        }))
    }

    /// Файлы, изменённые в папке загрузок за период (без вложенных папок и недокачанных).
    fn downloads(&self, from: i64, to: i64) -> Vec<Download> {
        let Some(dir) = &self.downloads_dir else { return vec![] };
        let Ok(entries) = fs::read_dir(dir) else { return vec![] };
        let mut out: Vec<Download> = entries
            .flatten()
            .filter_map(|e| {
                let meta = e.metadata().ok()?;
                let name = e.file_name().to_string_lossy().to_string();
                if !meta.is_file() || name.ends_with(".crdownload") || name.ends_with(".tmp") || name.ends_with(".part") {
                    return None;
                }
                let ts = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
                (ts >= from && ts < to).then_some(Download { name, size: meta.len(), ts })
            })
            .collect();
        out.sort_by(|a, b| b.ts.cmp(&a.ts));
        out.truncate(20);
        out
    }

    /// Активное время за последние 7 дней (от старого к новому).
    pub fn week_overview(&self) -> Result<Vec<WeekDay>, String> {
        let today = time::today();
        let st = self.storage.lock().map_err(|e| e.to_string())?;
        (0..7)
            .rev()
            .map(|back| {
                let date = today - chrono::Duration::days(back);
                let (from, to) = time::day_bounds(date);
                let samples = st.samples_between(from, to).map_err(|e| e.to_string())?;
                let sessions = sessions::sessionize(&samples, &self.categorizer);
                Ok(summary::week_day(&date.to_string(), &sessions))
            })
            .collect()
    }

    /// «Запомнить это»: отметка привязывается к последнему известному окну.
    pub fn add_bookmark(&self, note: &str) -> Result<(), String> {
        let st = self.storage.lock().map_err(|e| e.to_string())?;
        let last = st.last_sample().map_err(|e| e.to_string())?;
        let (app, title) = last.map(|s| (s.app, s.title)).unwrap_or_default();
        st.add_bookmark(&Bookmark { ts: Utc::now().timestamp(), app, title, note: note.to_string() })
            .map_err(|e| e.to_string())
    }

    pub fn delete_clip(&self, text: &str) -> Result<(), String> {
        self.storage.lock().map_err(|e| e.to_string())?.delete_clip(text).map_err(|e| e.to_string())
    }

    pub fn clear_clips(&self) -> Result<(), String> {
        self.storage.lock().map_err(|e| e.to_string())?.clear_clips().map_err(|e| e.to_string())
    }

    pub fn search(&self, query: &str) -> Result<Vec<SearchHit>, String> {
        self.storage.lock().map_err(|e| e.to_string())?.search(query, 100).map_err(|e| e.to_string())
    }

    pub fn set_goal(&self, category: &str, limit_mins: u32) -> Result<(), String> {
        let category = Category::parse(category).ok_or("неизвестная категория")?;
        self.storage.lock().map_err(|e| e.to_string())?.set_goal(category, limit_mins).map_err(|e| e.to_string())
    }

    /// Забыть всё, что записано за последние `minutes` минут.
    pub fn forget_minutes(&self, minutes: u32) -> Result<(), String> {
        let now = Utc::now().timestamp();
        self.storage
            .lock()
            .map_err(|e| e.to_string())?
            .delete_between(now - minutes as i64 * 60, now + 1)
            .map_err(|e| e.to_string())
    }

    /// Сохраняет сессии дня в CSV (открывается в Excel) и возвращает путь к файлу.
    pub fn export_day_csv(&self, date: Option<&str>, dir: &Path) -> Result<String, String> {
        let date = parse_date(date)?;
        let (from, to) = time::day_bounds(date);
        let samples = self
            .storage
            .lock()
            .map_err(|e| e.to_string())?
            .samples_between(from, to)
            .map_err(|e| e.to_string())?;
        let sessions = sessions::sessionize(&samples, &self.categorizer);

        let mut csv = String::from("\u{feff}Начало;Конец;Секунд;Программа;Заголовок;Категория\r\n");
        for s in &sessions {
            csv.push_str(&format!(
                "{};{};{};{};{};{}\r\n",
                fmt_ts(s.start),
                fmt_ts(s.end),
                s.secs(),
                csv_field(&s.app),
                csv_field(&s.title),
                s.category.as_str()
            ));
        }
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("memory-log-{date}.csv"));
        fs::write(&path, csv).map_err(|e| e.to_string())?;
        Ok(path.to_string_lossy().to_string())
    }
}
