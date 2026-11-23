//! Локальное хранилище на SQLite. Все данные остаются на диске пользователя.

use std::path::Path;

use ml_core::privacy::HIDDEN_APP;
use ml_core::model::{CategoryOverride, OverrideScope, RecordingSettings};
use ml_core::sessions::LEGACY_IDLE_SECS;
use ml_core::{ActivityEvent, Bookmark, Category, ClipEntry, Goal, ProcSample, Sample, SearchHit, SystemSample, Timestamp};
use rusqlite::{params, Connection};

pub mod sync;

/// Миграции применяются по порядку; номер версии хранится в `PRAGMA user_version`.
/// Старые файлы не менять, только добавлять новые в конец.
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/001_init.sql"),
    include_str!("../migrations/002_audio_reserved.sql"),
    include_str!("../migrations/003_system_metrics.sql"),
    include_str!("../migrations/004_drop_idle.sql"),
    include_str!("../migrations/005_clipboard.sql"),
    include_str!("../migrations/006_clipboard_unique.sql"),
    ADD_IDLE_SECONDS,
    include_str!("../migrations/008_activity.sql"),
    // База 0.2.0 могла получить версию 8 без столбца idle_secs.
    ADD_IDLE_SECONDS,
    include_str!("../migrations/010_preferences.sql"),
    include_str!("../migrations/011_sync.sql"),
    include_str!("../migrations/012_clipboard_images.sql"),
];
const ADD_IDLE_SECONDS: &str = include_str!("../migrations/007_idle_secs.sql");
const ACTIVITY_SCHEMA_VERSION: usize = 8;
const IDLE_COMPAT_SCHEMA_VERSION: usize = 9;
const BREAK_EVENT: u8 = 1;
const IDLE_EVENT: u8 = 2;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("ошибка базы данных: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("база данных повреждена: {0}")]
    Corrupt(String),
    #[error("не удалось переместить повреждённую базу: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid synchronization data: {0}")]
    SyncData(#[from] serde_json::Error),
}

impl StorageError {
    /// Ошибка говорит о повреждении файла базы, а не о временной проблеме.
    pub fn is_corruption(&self) -> bool {
        match self {
            StorageError::Corrupt(_) => true,
            StorageError::Db(e) => {
                let text = e.to_string();
                text.contains("malformed") || text.contains("not a database")
            }
            StorageError::Io(_) | StorageError::SyncData(_) => false,
        }
    }
}

/// Сколько записей буфера обмена хранится максимум.
const MAX_CLIPS: usize = 2000;
/// Сколько скриншотов буфера обмена хранится максимум.
const MAX_CLIP_IMAGES: i64 = 200;

#[derive(Debug, Clone)]
pub struct ClipImageRow {
    pub id: i64,
    pub ts: Timestamp,
    pub app: String,
    pub width: u32,
    pub height: u32,
    pub mime: String,
    pub size_bytes: u64,
    pub thumb: Vec<u8>,
}

pub type Result<T> = std::result::Result<T, StorageError>;

pub struct Storage {
    conn: Connection,
}

/// Переименовывает файлы повреждённой базы, чтобы их можно было разобрать вручную.
fn quarantine(path: &Path) -> Result<std::path::PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("db");
    let moved = path.with_file_name(format!("{stem}.corrupt-{stamp}.db"));
    for suffix in ["", "-wal", "-shm"] {
        let from = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
        if from.exists() {
            std::fs::rename(&from, format!("{}{suffix}", moved.display()))?;
        }
    }
    Ok(moved)
}

fn read_sample(r: &rusqlite::Row) -> rusqlite::Result<Sample> {
    Ok(Sample { ts: r.get(0)?, app: r.get(1)?, title: r.get(2)?, idle_secs: r.get(3)? })
}

impl Storage {
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    /// Открывает базу и проверяет её целостность. Если файл повреждён, он откладывается
    /// в сторону (рядом, с пометкой `corrupt`), а вместо него создаётся новая пустая база.
    /// Возвращает путь к отложенному файлу, если это случилось.
    pub fn open_or_recover(path: &Path) -> Result<(Self, Option<std::path::PathBuf>)> {
        let checked = Self::open(path).and_then(|s| s.check_integrity().map(|_| s));
        match checked {
            Ok(storage) => Ok((storage, None)),
            Err(e) if e.is_corruption() => {
                let moved = quarantine(path)?;
                Ok((Self::open(path)?, Some(moved)))
            }
            Err(e) => Err(e),
        }
    }

    fn check_integrity(&self) -> Result<()> {
        let result: String = self.conn.pragma_query_value(None, "integrity_check", |r| r.get(0))?;
        if result == "ok" {
            Ok(())
        } else {
            Err(StorageError::Corrupt(result))
        }
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "cache_size", -2000)?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        let applied: usize = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
            let tx = conn.transaction()?;
            let version = i + 1;
            // В разных базах версии 7 столбец is_break мог уже существовать.
            if version == ACTIVITY_SCHEMA_VERSION {
                let has_break_column = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM pragma_table_info('samples') WHERE name = 'is_break')",
                    [],
                    |row| row.get::<_, bool>(0),
                )?;
                if !has_break_column {
                    tx.execute("ALTER TABLE samples ADD COLUMN is_break INTEGER NOT NULL DEFAULT 0", [])?;
                }
            }
            let needs_idle_compat = version != IDLE_COMPAT_SCHEMA_VERSION || !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('samples') WHERE name = 'idle_secs')",
                [],
                |row| row.get::<_, bool>(0),
            )?;
            if needs_idle_compat {
                tx.execute_batch(sql)?;
            }
            tx.pragma_update(None, "user_version", version)?;
            tx.commit()?;
        }
        Ok(Self { conn })
    }

    pub fn insert_sample(&self, s: &Sample) -> Result<()> {
        let mut stmt = self.conn.prepare_cached(
            "INSERT INTO samples (ts, app, title, idle_secs) VALUES (?1, ?2, ?3, ?4)",
        )?;
        stmt.execute(params![s.ts, s.app, s.title, s.idle_secs])?;
        Ok(())
    }

    pub fn insert_activity_break(&self, ts: Timestamp) -> Result<()> {
        self.insert_activity_event(ts, BREAK_EVENT)
    }

    pub fn insert_activity_idle(&self, ts: Timestamp) -> Result<()> {
        self.insert_activity_event(ts, IDLE_EVENT)
    }

    fn insert_activity_event(&self, ts: Timestamp, kind: u8) -> Result<()> {
        let mut stmt = self.conn.prepare_cached("INSERT INTO samples (ts, app, title, is_break) VALUES (?1, '', '', ?2)")?;
        stmt.execute(params![ts, kind])?;
        Ok(())
    }

    pub fn activity_between(&self, from: Timestamp, to: Timestamp) -> Result<Vec<ActivityEvent>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT ts, app, title, idle_secs, is_break FROM samples WHERE ts >= ?1 AND ts < ?2 ORDER BY ts, rowid")?;
        let rows = stmt.query_map(params![from, to], |row| match row.get::<_, u8>(4)? {
            0 => Ok(ActivityEvent::Window(read_sample(row)?)),
            IDLE_EVENT => Ok(ActivityEvent::Idle(row.get(0)?)),
            _ => Ok(ActivityEvent::Break(row.get(0)?)),
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn idle_minutes(&self) -> Result<u32> {
        Ok(self.conn.query_row("SELECT idle_minutes FROM activity_settings WHERE id = 1", [], |row| row.get(0))?)
    }

    pub fn set_idle_minutes(&self, minutes: u32) -> Result<()> {
        self.conn.execute("UPDATE activity_settings SET idle_minutes = ?1 WHERE id = 1", [minutes])?;
        Ok(())
    }

    pub fn recording_settings(&self) -> Result<RecordingSettings> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT clipboard_enabled, retention_days, excluded_apps FROM activity_settings WHERE id = 1",
        )?;
        Ok(stmt.query_row([], |r| {
            Ok(RecordingSettings { clipboard_enabled: r.get(0)?, retention_days: r.get(1)?, excluded_apps: r.get(2)? })
        })?)
    }

    pub fn set_recording_settings(&self, settings: &RecordingSettings) -> Result<()> {
        self.conn.execute(
            "UPDATE activity_settings SET clipboard_enabled = ?1, retention_days = ?2, excluded_apps = ?3 WHERE id = 1",
            params![settings.clipboard_enabled, settings.retention_days, settings.excluded_apps],
        )?;
        Ok(())
    }

    pub fn set_category_override(&self, change: &CategoryOverride) -> Result<()> {
        let ts = if change.scope == OverrideScope::Visit { change.ts } else { 0 };
        let title = if change.scope == OverrideScope::App { "" } else { &change.title };
        if let Some(category) = change.category {
            self.conn.execute(
                "INSERT INTO category_overrides (scope, ts, app, title, category) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (scope, ts, app, title) DO UPDATE SET category = excluded.category",
                params![change.scope.as_str(), ts, change.app, title, category.as_str()],
            )?;
        } else {
            self.conn.execute(
                "DELETE FROM category_overrides WHERE scope = ?1 AND ts = ?2 AND app = ?3 AND title = ?4",
                params![change.scope.as_str(), ts, change.app, title],
            )?;
        }
        Ok(())
    }

    pub fn category_overrides(&self, from: Timestamp, to: Timestamp) -> Result<Vec<CategoryOverride>> {
        let mut stmt = self.conn.prepare(
            "SELECT scope, ts, app, title, category FROM category_overrides
             WHERE scope != 'visit' OR (ts >= ?1 AND ts < ?2) ORDER BY scope, app, title",
        )?;
        let rows = stmt.query_map(params![from, to], |r| {
            let scope = match r.get::<_, String>(0)?.as_str() {
                "visit" => OverrideScope::Visit,
                "window" => OverrideScope::Window,
                _ => OverrideScope::App,
            };
            Ok(CategoryOverride {
                scope, ts: r.get(1)?, app: r.get(2)?, title: r.get(3)?,
                category: Category::parse(&r.get::<_, String>(4)?),
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn has_records(&self, from: Timestamp, to: Timestamp) -> Result<bool> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM samples WHERE ts >= ?1 AND ts < ?2 AND is_break != 1)
                 OR EXISTS(SELECT 1 FROM bookmarks WHERE ts >= ?1 AND ts < ?2)
                 OR EXISTS(SELECT 1 FROM clipboard WHERE ts >= ?1 AND ts < ?2)
                 OR EXISTS(SELECT 1 FROM system_samples WHERE ts >= ?1 AND ts < ?2)",
        )?;
        Ok(stmt.query_row(params![from, to], |r| r.get(0))?)
    }

    pub fn last_sample(&self) -> Result<Option<Sample>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT ts, app, title, idle_secs FROM samples WHERE is_break = 0 ORDER BY ts DESC, rowid DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map([], read_sample)?;
        Ok(rows.next().transpose()?)
    }

    /// Одинаковый текст не дублируется: повторное копирование поднимает старую запись наверх.
    pub fn insert_clip(&self, c: &ClipEntry) -> Result<()> {
        let mut stmt = self.conn.prepare_cached(
            "INSERT INTO clipboard (ts, app, text) VALUES (?1, ?2, ?3)
             ON CONFLICT(text) DO UPDATE SET ts = excluded.ts, app = excluded.app",
        )?;
        stmt.execute(params![c.ts, c.app, c.text])?;
        Ok(())
    }

    pub fn clips_between(&self, from: Timestamp, to: Timestamp) -> Result<Vec<ClipEntry>> {
        let mut stmt =
            self.conn.prepare("SELECT ts, app, text FROM clipboard WHERE ts >= ?1 AND ts < ?2 ORDER BY ts")?;
        let rows = stmt.query_map(params![from, to], |r| Ok(ClipEntry { ts: r.get(0)?, app: r.get(1)?, text: r.get(2)? }))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Последние скопированные тексты, от новых к старым (для листания горячими клавишами).
    pub fn recent_clips(&self, limit: usize) -> Result<Vec<ClipEntry>> {
        let mut stmt = self.conn.prepare("SELECT ts, app, text FROM clipboard ORDER BY ts DESC LIMIT ?1")?;
        let rows = stmt.query_map(params![limit as i64], |r| Ok(ClipEntry { ts: r.get(0)?, app: r.get(1)?, text: r.get(2)? }))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Удаляет одну запись буфера (по тексту, он уникален).
    pub fn delete_clip(&self, text: &str) -> Result<()> {
        self.conn.execute("DELETE FROM clipboard WHERE text = ?1", params![text])?;
        Ok(())
    }

    pub fn clear_clips(&self) -> Result<()> {
        self.conn.execute("DELETE FROM clipboard", [])?;
        Ok(())
    }

    pub fn insert_clip_image(
        &self,
        ts: Timestamp,
        app: &str,
        width: u32,
        height: u32,
        mime: &str,
        thumb: &[u8],
        data: &[u8],
        sha256: &[u8],
    ) -> Result<()> {
        let mut stmt = self.conn.prepare_cached(
            "INSERT INTO clipboard_images (ts, app, width, height, mime, thumb, data, sha256)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(sha256) DO UPDATE SET ts = excluded.ts, app = excluded.app",
        )?;
        stmt.execute(params![ts, app, width, height, mime, thumb, data, sha256])?;
        Ok(())
    }

    pub fn clip_images_between(&self, from: Timestamp, to: Timestamp) -> Result<Vec<ClipImageRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, ts, app, width, height, mime, length(data), thumb
             FROM clipboard_images WHERE ts >= ?1 AND ts < ?2 ORDER BY ts DESC, id DESC",
        )?;
        let rows = stmt.query_map(params![from, to], |r| {
            Ok(ClipImageRow {
                id: r.get(0)?,
                ts: r.get(1)?,
                app: r.get(2)?,
                width: r.get(3)?,
                height: r.get(4)?,
                mime: r.get(5)?,
                size_bytes: r.get::<_, i64>(6)?.max(0) as u64,
                thumb: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn recent_clip_images(&self, limit: usize) -> Result<Vec<ClipImageRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, ts, app, width, height, mime, length(data), thumb
             FROM clipboard_images ORDER BY ts DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(ClipImageRow {
                id: r.get(0)?,
                ts: r.get(1)?,
                app: r.get(2)?,
                width: r.get(3)?,
                height: r.get(4)?,
                mime: r.get(5)?,
                size_bytes: r.get::<_, i64>(6)?.max(0) as u64,
                thumb: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn clip_image_data(&self, id: i64) -> Result<Option<(String, Vec<u8>)>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT mime, data FROM clipboard_images WHERE id = ?1")?;
        let mut rows = stmt.query_map(params![id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
        Ok(rows.next().transpose()?)
    }

    pub fn delete_clip_image(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM clipboard_images WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn clear_clip_images(&self) -> Result<()> {
        self.conn.execute("DELETE FROM clipboard_images", [])?;
        Ok(())
    }

    pub fn add_bookmark(&self, b: &Bookmark) -> Result<()> {
        self.conn.execute(
            "INSERT INTO bookmarks (ts, app, title, note) VALUES (?1, ?2, ?3, ?4)",
            params![b.ts, b.app, b.title, b.note],
        )?;
        Ok(())
    }

    pub fn bookmarks_between(&self, from: Timestamp, to: Timestamp) -> Result<Vec<Bookmark>> {
        let mut stmt = self
            .conn
            .prepare("SELECT ts, app, title, note FROM bookmarks WHERE ts >= ?1 AND ts < ?2 ORDER BY ts")?;
        let rows = stmt.query_map(params![from, to], |r| {
            Ok(Bookmark { ts: r.get(0)?, app: r.get(1)?, title: r.get(2)?, note: r.get(3)? })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Снимок ПК и топ программ записываются одной транзакцией.
    pub fn insert_system(&self, s: &SystemSample, procs: &[ProcSample]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut sys_stmt = tx.prepare_cached(
                "INSERT INTO system_samples (ts, cpu, mem, mem_used_mb, net_rx, net_tx, disk_free_gb, battery_pct, on_battery, temp_c, uptime_s)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;
            sys_stmt.execute(params![
                s.ts, s.cpu_pct, s.mem_pct, s.mem_used_mb, s.net_rx as i64, s.net_tx as i64,
                s.disk_free_gb, s.battery_pct, s.on_battery, s.temp_c, s.uptime_s as i64
            ])?;
            let mut proc_stmt = tx.prepare_cached(
                "INSERT INTO proc_samples (ts, name, cpu, mem_mb) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for p in procs {
                proc_stmt.execute(params![p.ts, p.name, p.cpu_pct, p.mem_mb])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn system_between(&self, from: Timestamp, to: Timestamp) -> Result<Vec<SystemSample>> {
        let mut stmt = self.conn.prepare(
            "SELECT ts, cpu, mem, mem_used_mb, net_rx, net_tx, disk_free_gb, battery_pct, on_battery, temp_c, uptime_s
             FROM system_samples WHERE ts >= ?1 AND ts < ?2 ORDER BY ts",
        )?;
        let rows = stmt.query_map(params![from, to], |r| {
            Ok(SystemSample {
                ts: r.get(0)?,
                cpu_pct: r.get(1)?,
                mem_pct: r.get(2)?,
                mem_used_mb: r.get(3)?,
                net_rx: r.get::<_, i64>(4)? as u64,
                net_tx: r.get::<_, i64>(5)? as u64,
                disk_free_gb: r.get(6)?,
                battery_pct: r.get(7)?,
                on_battery: r.get(8)?,
                temp_c: r.get(9)?,
                uptime_s: r.get::<_, i64>(10)? as u64,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn procs_between(&self, from: Timestamp, to: Timestamp) -> Result<Vec<ProcSample>> {
        let mut stmt = self
            .conn
            .prepare("SELECT ts, name, cpu, mem_mb FROM proc_samples WHERE ts >= ?1 AND ts < ?2 ORDER BY ts")?;
        let rows = stmt.query_map(params![from, to], |r| {
            Ok(ProcSample { ts: r.get(0)?, name: r.get(1)?, cpu_pct: r.get(2)?, mem_mb: r.get(3)? })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn goals(&self) -> Result<Vec<Goal>> {
        let mut stmt = self.conn.prepare("SELECT category, limit_mins FROM goals ORDER BY category")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (cat, limit_mins) = row?;
            if let Some(category) = Category::parse(&cat) {
                out.push(Goal { category, limit_mins });
            }
        }
        Ok(out)
    }

    /// `limit_mins == 0` удаляет цель.
    pub fn set_goal(&self, category: Category, limit_mins: u32) -> Result<()> {
        if limit_mins == 0 {
            self.conn.execute("DELETE FROM goals WHERE category = ?1", params![category.as_str()])?;
        } else {
            self.conn.execute(
                "INSERT OR REPLACE INTO goals (category, limit_mins) VALUES (?1, ?2)",
                params![category.as_str(), limit_mins],
            )?;
        }
        Ok(())
    }

    /// Поиск по всей истории: программа или заголовок окна содержит запрос (без учёта регистра).
    /// Регистр сравниваем в Rust, потому что SQLite не умеет это для кириллицы.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Ok(vec![]);
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT app, title, MIN(ts), MAX(ts), COUNT(*) FROM samples
             WHERE is_break = 0 AND idle_secs < ?1 AND app != ?2
             GROUP BY app, title ORDER BY MAX(ts) DESC",
        )?;
        let rows = stmt.query_map(params![LEGACY_IDLE_SECS, HIDDEN_APP], |r| {
            Ok(SearchHit { app: r.get(0)?, title: r.get(1)?, first_ts: r.get(2)?, last_ts: r.get(3)?, count: r.get(4)? })
        })?;
        let q_ascii = q.is_ascii();
        let q_bytes = q.as_bytes();
        let mut out = Vec::with_capacity(limit.min(100));
        for hit in rows {
            let hit = hit?;
            let matched = if q_ascii && hit.app.is_ascii() && hit.title.is_ascii() {
                hit.app.as_bytes().windows(q_bytes.len()).any(|w| w.eq_ignore_ascii_case(q_bytes))
                    || hit.title.as_bytes().windows(q_bytes.len()).any(|w| w.eq_ignore_ascii_case(q_bytes))
            } else {
                hit.app.to_lowercase().contains(&q) || hit.title.to_lowercase().contains(&q)
            };
            if matched {
                out.push(hit);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Удаляет всё записанное за период («забыть последние N минут»).
    pub fn delete_between(&self, from: Timestamp, to: Timestamp) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for table in ["samples", "bookmarks", "system_samples", "proc_samples", "clipboard", "clipboard_images"] {
            tx.execute(&format!("DELETE FROM {table} WHERE ts >= ?1 AND ts < ?2"), params![from, to])?;
        }
        tx.execute("DELETE FROM category_overrides WHERE scope = 'visit' AND ts >= ?1 AND ts < ?2", params![from, to])?;
        tx.commit()?;
        Ok(())
    }

    /// Чистка старых подробных данных, чтобы база не росла бесконечно.
    pub fn prune(&self, system_before: Timestamp, procs_before: Timestamp) -> Result<()> {
        self.conn.execute("DELETE FROM clipboard WHERE ts < ?1", params![procs_before])?;
        // Кроме срока, ограничиваем и число записей, чтобы база не раздувалась от частого копирования.
        self.conn.execute(
            "DELETE FROM clipboard WHERE rowid NOT IN (SELECT rowid FROM clipboard ORDER BY ts DESC LIMIT ?1)",
            params![MAX_CLIPS as i64],
        )?;
        self.conn.execute("DELETE FROM clipboard_images WHERE ts < ?1", params![procs_before])?;
        self.conn.execute(
            "DELETE FROM clipboard_images WHERE id NOT IN (SELECT id FROM clipboard_images ORDER BY ts DESC, id DESC LIMIT ?1)",
            params![MAX_CLIP_IMAGES],
        )?;
        self.conn.execute("DELETE FROM system_samples WHERE ts < ?1", params![system_before])?;
        self.conn.execute("DELETE FROM proc_samples WHERE ts < ?1", params![procs_before])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts: i64, app: &str, title: &str) -> Sample {
        Sample { ts, app: app.into(), title: title.into(), idle_secs: 0 }
    }

    #[test]
    fn roundtrip_and_delete() {
        let st = Storage::open_in_memory().unwrap();
        let s = sample(100, "a", "t");
        st.insert_sample(&s).unwrap();
        assert!(matches!(&st.activity_between(0, 200).unwrap()[..], [ActivityEvent::Window(sample)] if sample == &s));
        assert_eq!(st.last_sample().unwrap(), Some(s));
        st.delete_between(0, 200).unwrap();
        assert!(st.activity_between(0, 200).unwrap().is_empty());
    }

    #[test]
    fn recording_settings_and_period_deletion_preserve_rules_and_outside_records() {
        let storage = Storage::open_in_memory().unwrap();
        let settings = RecordingSettings {
            clipboard_enabled: false, retention_days: 30, excluded_apps: "notepad\nchrome".into(),
        };
        storage.set_recording_settings(&settings).unwrap();
        let saved = storage.recording_settings().unwrap();
        assert!(!saved.clipboard_enabled);
        assert_eq!(saved.retention_days, 30);
        assert!(saved.excludes("Notepad.EXE"));
        storage.insert_sample(&sample(10, "editor", "Document")).unwrap();
        storage.insert_sample(&sample(20, "editor", "Document")).unwrap();
        let mut rule = CategoryOverride {
            scope: OverrideScope::Visit, ts: 10, app: "editor".into(), title: "Document".into(), category: Some(Category::Work),
        };
        storage.set_category_override(&rule).unwrap();
        rule.scope = OverrideScope::Window;
        storage.set_category_override(&rule).unwrap();
        assert!(storage.has_records(10, 20).unwrap());
        storage.delete_between(10, 20).unwrap();
        assert!(!storage.has_records(10, 20).unwrap());
        assert!(storage.has_records(20, 21).unwrap());
        let overrides = storage.category_overrides(0, 30).unwrap();
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].scope, OverrideScope::Window);
    }

    #[test]
    fn migrates_existing_windows_and_keeps_breaks_out_of_search() {
        let conn = Connection::open_in_memory().unwrap();
        for (index, sql) in MIGRATIONS.iter().take(6).enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.pragma_update(None, "user_version", index + 1).unwrap();
        }
        conn.execute("INSERT INTO samples (ts, app, title) VALUES (10, 'editor', 'Document')", []).unwrap();
        let storage = Storage::init(conn).unwrap();
        assert_eq!(storage.idle_minutes().unwrap(), 5);
        storage.set_idle_minutes(12).unwrap();
        assert_eq!(storage.idle_minutes().unwrap(), 12);

        storage.insert_activity_break(10).unwrap();
        let history = storage.activity_between(0, 20).unwrap();
        assert!(matches!(&history[..], [ActivityEvent::Window(sample), ActivityEvent::Break(10)] if sample.app == "editor"));
        assert_eq!(storage.last_sample().unwrap().unwrap().app, "editor");
        assert_eq!(storage.search("editor", 10).unwrap()[0].count, 1);
        storage.delete_between(0, 20).unwrap();
        assert!(storage.activity_between(0, 20).unwrap().is_empty());
    }

    #[test]
    fn opens_legacy_activity_schemas() {
        for (version, had_break_column) in [(7, false), (7, true), (8, true)] {
            let conn = Connection::open_in_memory().unwrap();
            for sql in MIGRATIONS.iter().take(6) {
                conn.execute_batch(sql).unwrap();
            }
            if had_break_column {
                conn.execute("ALTER TABLE samples ADD COLUMN is_break INTEGER NOT NULL DEFAULT 0", []).unwrap();
                conn.execute_batch(MIGRATIONS[ACTIVITY_SCHEMA_VERSION - 1]).unwrap();
                conn.execute("UPDATE activity_settings SET idle_minutes = 11", []).unwrap();
            } else {
                conn.execute("ALTER TABLE samples ADD COLUMN idle_secs INTEGER NOT NULL DEFAULT 0", []).unwrap();
            }
            conn.execute("INSERT INTO samples (ts, app, title) VALUES (10, 'editor', 'Document')", []).unwrap();
            conn.pragma_update(None, "user_version", version).unwrap();

            let storage = Storage::init(conn).unwrap();
            assert_eq!(storage.idle_minutes().unwrap(), if had_break_column { 11 } else { 5 });
            assert_eq!(storage.last_sample().unwrap().unwrap().app, "editor");
            storage.insert_activity_break(11).unwrap();
            storage.insert_activity_idle(14).unwrap();
            assert!(matches!(
                &storage.activity_between(0, 20).unwrap()[..],
                [ActivityEvent::Window(_), ActivityEvent::Break(11), ActivityEvent::Idle(14)]
            ));
            assert_eq!(
                storage.conn.pragma_query_value(None, "user_version", |r| r.get::<_, usize>(0)).unwrap(),
                MIGRATIONS.len(),
            );
        }
    }

    #[test]
    fn system_and_procs_roundtrip() {
        let st = Storage::open_in_memory().unwrap();
        let s = SystemSample {
            ts: 5, cpu_pct: 12.5, mem_pct: 40.0, mem_used_mb: 4096, net_rx: 10, net_tx: 20,
            disk_free_gb: Some(50.0), battery_pct: Some(80), on_battery: Some(true), temp_c: None, uptime_s: 99,
        };
        let p = ProcSample { ts: 5, name: "chrome".into(), cpu_pct: 3.0, mem_mb: 500 };
        st.insert_system(&s, &[p.clone()]).unwrap();
        assert_eq!(st.system_between(0, 10).unwrap(), vec![s]);
        assert_eq!(st.procs_between(0, 10).unwrap(), vec![p]);
        st.prune(10, 10).unwrap();
        assert!(st.system_between(0, 10).unwrap().is_empty());
    }

    #[test]
    fn corrupt_file_is_set_aside() {
        let dir = std::env::temp_dir().join(format!("ml-corrupt-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("memory-log.db");
        std::fs::write(&path, vec![0x42u8; 8192]).unwrap();

        let (storage, moved) = Storage::open_or_recover(&path).unwrap();
        assert!(moved.as_ref().is_some_and(|m| m.exists()));
        storage.insert_sample(&sample(1, "a", "t")).unwrap();
        drop(storage);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clipboard_roundtrip_and_dedup() {
        let st = Storage::open_in_memory().unwrap();
        let c = |ts, text: &str| ClipEntry { ts, app: "chrome".into(), text: text.into() };
        st.insert_clip(&c(1, "a")).unwrap();
        st.insert_clip(&c(2, "a")).unwrap();
        st.insert_clip(&c(3, "b")).unwrap();
        assert_eq!(st.clips_between(0, 10).unwrap(), vec![c(2, "a"), c(3, "b")]);
        assert_eq!(st.recent_clips(1).unwrap(), vec![c(3, "b")]);
        st.delete_clip("b").unwrap();
        assert_eq!(st.recent_clips(5).unwrap(), vec![c(2, "a")]);
        st.clear_clips().unwrap();
        assert!(st.recent_clips(5).unwrap().is_empty());
        st.insert_clip(&c(2, "a")).unwrap();
        st.insert_clip(&c(3, "b")).unwrap();
        // Копирование старого текста не создаёт дубль, а поднимает его наверх.
        st.insert_clip(&c(4, "a")).unwrap();
        assert_eq!(st.recent_clips(5).unwrap(), vec![c(4, "a"), c(3, "b")]);
        st.delete_between(0, 10).unwrap();
        assert!(st.clips_between(0, 10).unwrap().is_empty());
    }

    #[test]
    fn goals_set_and_remove() {
        let st = Storage::open_in_memory().unwrap();
        st.set_goal(Category::Social, 60).unwrap();
        assert_eq!(st.goals().unwrap(), vec![Goal { category: Category::Social, limit_mins: 60 }]);
        st.set_goal(Category::Social, 0).unwrap();
        assert!(st.goals().unwrap().is_empty());
    }

    #[test]
    fn search_is_case_insensitive_for_cyrillic() {
        let st = Storage::open_in_memory().unwrap();
        st.insert_sample(&sample(1, "chrome", "Рецепт борща")).unwrap();
        st.insert_sample(&sample(2, "chrome", "Другое")).unwrap();
        let hits = st.search("БОРЩ", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Рецепт борща");
    }
}
