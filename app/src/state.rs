use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use chrono::{Datelike, Days, Local, NaiveDate, TimeZone, Utc};
use ml_collector::LiveSystemProbe;
use ml_core::categorize::apply_overrides;
use ml_core::model::{CategoryOverride, OverrideScope, RecordingSettings, Session};
use ml_core::privacy::HIDDEN_APP;
use ml_core::{sessions, summary, time, Bookmark, Categorizer, Category, DayInput, DaySummary, Download, SearchHit, WeekDay};
use ml_storage::Storage;
use ml_storage::sync::{Device, PeerHit};
use serde::Serialize;

use crate::i18n;
use crate::log;
use crate::cloud::{self, SyncControl};
use crate::sys_tools::PowerGuard;

#[derive(Serialize)]
pub struct ClipboardSearchResult {
    pub clips: Vec<ml_core::ClipEntry>,
    pub images: Vec<crate::clip_images::ClipImageItem>,
}

#[derive(Serialize)]
pub struct DeviceDay {
    device: Device,
    summary: DaySummary,
}

#[derive(Serialize)]
pub struct Status {
    pub paused: bool,
    pub inactive: bool,
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
    pub sync: SyncControl,
    paused: AtomicBool,
    pub inactive: AtomicBool,
    pub idle_minutes: AtomicU32,
    /// Номер изменения буфера, сделанного листанием: сборщик его не записывает.
    pub clip_ignore: AtomicU32,
    pub clipboard_config_version: AtomicU32,
    pub clip_nav: Mutex<ClipNav>,
    /// Последнее содержимое плашки: только что созданная плашка забирает его сама.
    pub clip_payload: Mutex<Option<serde_json::Value>>,
    downloads_dir: Option<PathBuf>,
    db_path: PathBuf,
    language: Mutex<String>,
    /// Пункты меню трея: при смене языка их подписи обновляются.
    pub tray_items: Mutex<Vec<(&'static str, tauri::menu::MenuItem<tauri::Wry>)>>,
    pub live_system: Mutex<Option<LiveSystemProbe>>,
    pub power_guard: Mutex<PowerGuard>,
    recording_settings_cache: Mutex<RecordingSettings>,
}

fn parse_date(date: Option<&str>, lang: &str) -> Result<NaiveDate, String> {
    match date {
        Some(d) => d.parse().map_err(|e| i18n::t_with(lang, "err.bad_date", &[("err", &format!("{e}"))])),
        None => Ok(time::today()),
    }
}

fn csv_field(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn fmt_ts(ts: i64) -> String {
    Local.timestamp_opt(ts, 0).single().map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default()
}

/// Файл с выбранным языком лежит рядом с базой. Нет файла — берём язык Windows.
fn language_file(db_path: &Path) -> PathBuf {
    db_path.with_file_name("language.txt")
}

fn read_flag(db_path: &Path, name: &str) -> bool {
    read_flag_default(db_path, name, false)
}

fn read_flag_default(db_path: &Path, name: &str, default: bool) -> bool {
    match fs::read_to_string(db_path.with_file_name(name)) {
        Ok(s) => s.trim() == "1",
        Err(_) => default,
    }
}

fn write_flag(db_path: &Path, name: &str, enabled: bool) {
    if let Err(e) = fs::write(db_path.with_file_name(name), if enabled { "1" } else { "0" }) {
        log::info(&format!("не удалось сохранить {name}: {e}"));
    }
}

fn initial_language(db_path: &Path) -> String {
    fs::read_to_string(language_file(db_path))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|c| i18n::is_supported(c))
        .unwrap_or_else(i18n::detect)
}

impl AppState {
    pub fn lang(&self) -> String {
        self.language.lock().map(|l| l.clone()).unwrap_or_else(|_| "en".to_string())
    }

    pub fn set_language(&self, code: &str) -> Result<(), String> {
        if !i18n::is_supported(code) {
            return Err(format!("unsupported language: {code}"));
        }
        fs::write(language_file(&self.db_path), code).map_err(|e| e.to_string())?;
        *self.language.lock().map_err(|e| e.to_string())? = code.to_string();
        self.relabel_tray();
        Ok(())
    }

    pub fn widget_enabled(&self) -> bool {
        read_flag(&self.db_path, "widget.txt")
    }

    pub fn set_widget_enabled(&self, enabled: bool) {
        write_flag(&self.db_path, "widget.txt", enabled);
    }

    pub fn pet_enabled(&self) -> bool {
        read_flag(&self.db_path, "pet.txt")
    }

    pub fn set_pet_enabled(&self, enabled: bool) {
        write_flag(&self.db_path, "pet.txt", enabled);
    }

    pub fn layout_fix_enabled(&self) -> bool {
        read_flag_default(&self.db_path, "layout_fix.txt", true)
    }

    pub fn set_layout_fix_enabled(&self, enabled: bool) {
        write_flag(&self.db_path, "layout_fix.txt", enabled);
    }

    /// Подписи пунктов трея на текущем языке.
    pub fn relabel_tray(&self) {
        let lang = self.lang();
        let Ok(items) = self.tray_items.lock() else { return };
        for (key, item) in items.iter() {
            let _ = item.set_text(i18n::t(&lang, key));
        }
    }

    pub fn open(db_path: &Path, downloads_dir: Option<PathBuf>) -> Result<Self, Box<dyn Error>> {
        let (storage, moved) = Storage::open_or_recover(db_path)?;
        if let Some(moved) = moved {
            log::info(&format!("база была повреждена, старый файл сохранён: {}", moved.display()));
        }
        let idle_minutes = storage.idle_minutes()?;
        let initial_recording = storage.recording_settings()?;
        cloud::initialize(&storage)?;
        Ok(Self {
            storage: Mutex::new(storage),
            categorizer: Categorizer::with_defaults(),
            sync: SyncControl::default(),
            paused: AtomicBool::new(false),
            inactive: AtomicBool::new(false),
            idle_minutes: AtomicU32::new(idle_minutes),
            clip_ignore: AtomicU32::new(0),
            clipboard_config_version: AtomicU32::new(0),
            clip_nav: Mutex::new(ClipNav { sel: 0, last: None, generation: 0 }),
            clip_payload: Mutex::new(None),
            downloads_dir,
            db_path: db_path.to_path_buf(),
            language: Mutex::new(initial_language(db_path)),
            tray_items: Mutex::new(Vec::new()),
            live_system: Mutex::new(None),
            power_guard: Mutex::new(PowerGuard::default()),
            recording_settings_cache: Mutex::new(initial_recording),
        })
    }

    /// Вызывается, когда во время работы выяснилось, что база повреждена:
    /// откладывает файл и начинает новую базу, чтобы запись не останавливалась.
    pub fn recover_database(&self) {
        let Ok(mut storage) = self.storage.lock() else { return };
        // Windows не даёт переименовать файл, пока он открыт, поэтому сначала закрываем
        // старое соединение (подменяем его временной базой в памяти).
        if let Ok(placeholder) = Storage::open_in_memory().and_then(|st| { cloud::initialize(&st)?; Ok(st) }) {
            drop(std::mem::replace(&mut *storage, placeholder));
        }
        match Storage::open_or_recover(&self.db_path) {
            Ok((fresh, moved)) => {
                if let Some(moved) = moved {
                    log::info(&format!("база повреждена во время работы, старый файл: {}", moved.display()));
                }
                match cloud::initialize(&fresh) {
                    Ok(()) => *storage = fresh,
                    Err(e) => log::info(&format!("не удалось подготовить базу синхронизации: {e}")),
                }
            }
            Err(e) => log::info(&format!("не удалось восстановить базу: {e}")),
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn set_paused(&self, value: bool) -> Result<(), String> {
        let storage = self.storage.lock().map_err(|e| e.to_string())?;
        if self.paused.load(Ordering::Relaxed) != value {
            if value {
                storage.insert_activity_break(Utc::now().timestamp()).map_err(|e| e.to_string())?;
            }
            self.paused.store(value, Ordering::Relaxed);
            self.clipboard_config_version.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    pub fn set_idle_minutes(&self, minutes: u32) -> Result<(), String> {
        if !(1..=60).contains(&minutes) {
            return Err(i18n::t(&self.lang(), "idle.invalid"));
        }
        let storage = self.storage.lock().map_err(|e| e.to_string())?;
        storage.set_idle_minutes(minutes).map_err(|e| e.to_string())?;
        self.idle_minutes.store(minutes, Ordering::Relaxed);
        Ok(())
    }

    pub fn status(&self) -> Result<Status, String> {
        let last = self.storage.lock().map_err(|e| e.to_string())?.last_sample().map_err(|e| e.to_string())?;
        Ok(Status {
            paused: self.is_paused(),
            inactive: self.inactive.load(Ordering::Relaxed),
            last_sample_age_secs: last.map(|s| Utc::now().timestamp() - s.ts),
        })
    }

    /// Сводка за день в формате `YYYY-MM-DD`; без даты — за сегодня.
    pub fn day_summary(&self, date: Option<&str>) -> Result<DaySummary, String> {
        let date = parse_date(date, &self.lang())?;
        let (from, to) = time::day_bounds(date);
        let (samples, overrides, bookmarks, system, procs, goals, clips) = {
            let st = self.storage.lock().map_err(|e| e.to_string())?;
            let e = |e: ml_storage::StorageError| e.to_string();
            (
                st.activity_between(from - sessions::MAX_GAP_SECS, to + sessions::MAX_GAP_SECS).map_err(e)?,
                st.category_overrides(from, to).map_err(e)?,
                st.bookmarks_between(from, to).map_err(e)?,
                st.system_between(from, to).map_err(e)?,
                st.procs_between(from, to).map_err(e)?,
                st.goals().map_err(e)?,
                st.clips_between(from, to).map_err(e)?,
            )
        };
        let (mut sessions, away_secs) = sessions::sessionize_with_away(&samples, &self.categorizer, (from, to));
        apply_overrides(&mut sessions, &overrides);
        Ok(summary::build(DayInput {
            date: &date.to_string(),
            bounds: (from, to),
            sessions: &sessions,
            away_secs,
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
                let sessions = self.sessions_for_day(&st, date).map_err(|e| e.to_string())?;
                Ok(summary::week_day(&date.to_string(), &sessions))
            })
            .collect()
    }

    pub fn synced_days(&self, date: &str, device: Option<&str>) -> Result<Vec<DeviceDay>, String> {
        let date = parse_date(Some(date), &self.lang())?;
        let (from, to) = time::day_bounds(date);
        let st = self.storage.lock().map_err(|e| e.to_string())?;
        let mut days = Vec::new();
        for peer in st.sync_peers().map_err(|e| e.to_string())? {
            if device.is_some_and(|id| id != peer.id) { continue; }
            let e = |e: ml_storage::StorageError| e.to_string();
            let events = st.peer_activity(&peer.id, from - sessions::MAX_GAP_SECS, to + sessions::MAX_GAP_SECS).map_err(e)?;
            let (mut sessions, away_secs) = sessions::sessionize_with_away(&events, &self.categorizer, (from, to));
            apply_overrides(&mut sessions, &st.peer_rules(&peer.id, from, to).map_err(e)?);
            let day = summary::build(DayInput {
                date: &date.to_string(), bounds: (from, to), sessions: &sessions, away_secs,
                bookmarks: st.peer_bookmarks(&peer.id, from, to).map_err(e)?, system: &[], procs: &[],
                downloads: Vec::new(), clips: Vec::new(), goals: &[],
            });
            days.push(DeviceDay { device: peer, summary: day });
        }
        Ok(days)
    }

    pub fn sync_search(&self, query: &str, device: Option<&str>) -> Result<Vec<PeerHit>, String> {
        self.storage.lock().map_err(|e| e.to_string())?.search_peers(query, device, 100).map_err(|e| e.to_string())
    }

    pub(crate) fn sessions_for_day(&self, st: &Storage, date: NaiveDate) -> ml_storage::Result<Vec<Session>> {
        let (from, to) = time::day_bounds(date);
        let events = st.activity_between(from - sessions::MAX_GAP_SECS, to + sessions::MAX_GAP_SECS)?;
        let mut sessions = sessions::sessionize(&events, &self.categorizer, (from, to));
        apply_overrides(&mut sessions, &st.category_overrides(from, to)?);
        Ok(sessions)
    }

    pub fn cached_recording_settings(&self) -> RecordingSettings {
        self.recording_settings_cache
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|_| RecordingSettings {
                clipboard_enabled: true,
                retention_days: 0,
                excluded_apps: String::new(),
            })
    }

    pub fn recording_settings(&self) -> Result<RecordingSettings, String> {
        let settings = self.storage.lock().map_err(|e| e.to_string())?.recording_settings().map_err(|e| e.to_string())?;
        if let Ok(mut cache) = self.recording_settings_cache.lock() {
            *cache = settings.clone();
        }
        Ok(settings)
    }

    pub fn set_recording_settings(&self, mut settings: RecordingSettings) -> Result<(), String> {
        if settings.retention_days > 3650 || settings.excluded_apps.len() > 16384 {
            return Err(i18n::t(&self.lang(), "record.invalid"));
        }
        let mut apps: Vec<_> = settings.excluded_apps.lines().map(|name| {
            let name = name.trim().to_lowercase();
            name.strip_suffix(".exe").unwrap_or(&name).to_owned()
        }).filter(|name| !name.is_empty()).collect();
        apps.sort_unstable();
        apps.dedup();
        settings.excluded_apps = apps.join("\n");
        let st = self.storage.lock().map_err(|e| e.to_string())?;
        let old = st.recording_settings().map_err(|e| e.to_string())?;
        st.set_recording_settings(&settings).map_err(|e| e.to_string())?;
        if let Ok(mut cache) = self.recording_settings_cache.lock() {
            *cache = settings.clone();
        }
        if old.clipboard_enabled != settings.clipboard_enabled || old.excluded_apps != settings.excluded_apps {
            self.clipboard_config_version.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    pub fn delete_days(&self, from: &str, through: &str) -> Result<(), String> {
        let lang = self.lang();
        let from = parse_date(Some(from), &lang)?;
        let through = parse_date(Some(through), &lang)?;
        if through < from || through > time::today() {
            return Err(i18n::t(&lang, "record.bad_range"));
        }
        self.storage.lock().map_err(|e| e.to_string())?
            .delete_between(time::day_bounds(from).0, time::day_bounds(through).1)
            .map_err(|e| e.to_string())
    }

    pub fn recorded_days(&self, month: &str, device: Option<&str>) -> Result<Vec<String>, String> {
        let mut date = parse_date(Some(month), &self.lang())?;
        let month = date.month();
        let st = self.storage.lock().map_err(|e| e.to_string())?;
        let mut days = Vec::new();
        while date.month() == month && date <= time::today() {
            let (from, to) = time::day_bounds(date);
            let has_records = match device {
                Some("all") => st.has_records(from, to).and_then(|found| {
                    if found { Ok(true) } else { st.has_peer_records(None, from, to) }
                }),
                Some(device) => st.has_peer_records(Some(device), from, to),
                None => st.has_records(from, to),
            };
            if has_records.map_err(|e| e.to_string())? {
                days.push(date.to_string());
            }
            date = date.checked_add_days(Days::new(1)).ok_or_else(|| "invalid calendar date".to_string())?;
        }
        Ok(days)
    }

    pub fn calendar_heatmap(&self, month: &str, device: Option<&str>) -> Result<Vec<WeekDay>, String> {
        let mut date = parse_date(Some(month), &self.lang())?;
        let month_num = date.month();
        let today = time::today();
        let mut month_dates = Vec::with_capacity(31);
        while date.month() == month_num && date <= today {
            month_dates.push(date);
            date = date.checked_add_days(Days::new(1)).ok_or_else(|| "invalid calendar date".to_string())?;
        }
        let (Some(&first_date), Some(&last_date)) = (month_dates.first(), month_dates.last()) else {
            return Ok(Vec::new());
        };
        let month_from = time::day_bounds(first_date).0;
        let month_to = time::day_bounds(last_date).1;

        let st = self.storage.lock().map_err(|e| e.to_string())?;
        let include_local = device.is_none() || device == Some("all");
        let local_events = if include_local {
            st.activity_between(month_from - sessions::MAX_GAP_SECS, month_to + sessions::MAX_GAP_SECS)
                .map_err(|e| e.to_string())?
        } else {
            Vec::new()
        };
        let local_overrides = if include_local && !local_events.is_empty() {
            st.category_overrides(month_from, month_to).map_err(|e| e.to_string())?
        } else {
            Vec::new()
        };

        let peers: Vec<ml_storage::sync::Device> = match device {
            Some(scope) => {
                let filter = (scope != "all").then_some(scope);
                st.sync_peers()
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .filter(|p| filter.is_none_or(|id| id == p.id))
                    .collect()
            }
            None => Vec::new(),
        };
        let mut peer_events = Vec::with_capacity(peers.len());
        for peer in &peers {
            let ev = st
                .peer_activity(&peer.id, month_from - sessions::MAX_GAP_SECS, month_to + sessions::MAX_GAP_SECS)
                .map_err(|e| e.to_string())?;
            peer_events.push(ev);
        }

        let event_ts = |e: &ml_core::ActivityEvent| match e {
            ml_core::ActivityEvent::Window(s) => s.ts,
            ml_core::ActivityEvent::Break(ts) | ml_core::ActivityEvent::Idle(ts) => *ts,
        };

        let mut days = Vec::new();
        for d in month_dates {
            let (from, to) = time::day_bounds(d);
            let mut total = 0;
            if include_local && !local_events.is_empty() {
                let lo = local_events.partition_point(|e| event_ts(e) < from - sessions::MAX_GAP_SECS);
                let hi = local_events.partition_point(|e| event_ts(e) < to + sessions::MAX_GAP_SECS);
                if lo < hi {
                    let mut sessions = sessions::sessionize(&local_events[lo..hi], &self.categorizer, (from, to));
                    if !local_overrides.is_empty() {
                        apply_overrides(&mut sessions, &local_overrides);
                    }
                    total += summary::week_day(&d.to_string(), &sessions).active_secs;
                }
            }
            for ev in &peer_events {
                let lo = ev.partition_point(|e| event_ts(e) < from - sessions::MAX_GAP_SECS);
                let hi = ev.partition_point(|e| event_ts(e) < to + sessions::MAX_GAP_SECS);
                if lo < hi {
                    let (sessions, _) = sessions::sessionize_with_away(&ev[lo..hi], &self.categorizer, (from, to));
                    total += summary::week_day(&d.to_string(), &sessions).active_secs;
                }
            }
            if total > 0 {
                days.push(WeekDay { date: d.to_string(), active_secs: total, top_category: None });
            }
        }
        Ok(days)
    }

    pub fn category_rules(&self) -> Result<Vec<CategoryOverride>, String> {
        self.storage.lock().map_err(|e| e.to_string())?.category_overrides(0, 0).map_err(|e| e.to_string())
    }

    pub fn set_category_override(&self, change: CategoryOverride) -> Result<(), String> {
        if change.app.is_empty() || change.app == HIDDEN_APP || change.app.len() > 1024 || change.title.len() > 4096
            || (change.scope == OverrideScope::Window && change.title.is_empty())
        {
            return Err(i18n::t(&self.lang(), "category.invalid"));
        }
        self.storage.lock().map_err(|e| e.to_string())?.set_category_override(&change).map_err(|e| e.to_string())
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
        let st = self.storage.lock().map_err(|e| e.to_string())?;
        st.clear_clips().map_err(|e| e.to_string())?;
        st.clear_clip_images().map_err(|e| e.to_string())
    }

    pub fn clip_images_for_day(&self, date: Option<&str>) -> Result<Vec<crate::clip_images::ClipImageItem>, String> {
        let date = parse_date(date, &self.lang())?;
        let (from, to) = time::day_bounds(date);
        let rows = self
            .storage
            .lock()
            .map_err(|e| e.to_string())?
            .clip_images_between(from, to)
            .map_err(|e| e.to_string())?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub fn clip_image_preview(&self, id: i64) -> Result<Option<String>, String> {
        let row = self
            .storage
            .lock()
            .map_err(|e| e.to_string())?
            .clip_image_data(id)
            .map_err(|e| e.to_string())?;
        Ok(row.map(|(mime, data)| crate::clip_images::to_data_url(&mime, &data)))
    }

    pub fn copy_clip_image(&self, id: i64) -> Result<(), String> {
        let row = self
            .storage
            .lock()
            .map_err(|e| e.to_string())?
            .clip_image_data(id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "screenshot not found".to_string())?;
        let (w, h, rgba) = crate::clip_images::decode_to_rgba(&row.1)?;
        let seq = ml_collector::set_clipboard_image(&rgba, w, h)
            .ok_or_else(|| "failed to write image to clipboard".to_string())?;
        self.clip_ignore.store(seq, Ordering::Relaxed);
        Ok(())
    }

    pub fn delete_clip_image(&self, id: i64) -> Result<(), String> {
        self.storage
            .lock()
            .map_err(|e| e.to_string())?
            .delete_clip_image(id)
            .map_err(|e| e.to_string())
    }

    pub fn search_clipboard(&self, query: &str) -> Result<ClipboardSearchResult, String> {
        let st = self.storage.lock().map_err(|e| e.to_string())?;
        let clips = st.search_clips(query, 100).map_err(|e| e.to_string())?;
        let images = st
            .search_clip_images(query, 50)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(Into::into)
            .collect();
        Ok(ClipboardSearchResult { clips, images })
    }

    pub fn search(&self, query: &str) -> Result<Vec<SearchHit>, String> {
        self.storage.lock().map_err(|e| e.to_string())?.search(query, 100).map_err(|e| e.to_string())
    }

    pub fn set_goal(&self, category: &str, limit_mins: u32) -> Result<(), String> {
        let category = Category::parse(category).ok_or_else(|| i18n::t(&self.lang(), "err.unknown_category"))?;
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
        let date = parse_date(date, &self.lang())?;
        let sessions = {
            let st = self.storage.lock().map_err(|e| e.to_string())?;
            self.sessions_for_day(&st, date).map_err(|e| e.to_string())?
        };

        let mut csv = format!("\u{feff}{}\r\n", i18n::t(&self.lang(), "csv.header"));
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
