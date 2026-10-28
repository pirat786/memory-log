//! Локальное хранилище на SQLite. Все данные остаются на диске пользователя.

use std::path::Path;

use ml_core::privacy::HIDDEN_APP;
use ml_core::{Bookmark, Category, ClipEntry, Goal, ProcSample, Sample, SearchHit, SystemSample, Timestamp};
use rusqlite::{params, Connection};

/// Миграции применяются по порядку; номер версии хранится в `PRAGMA user_version`.
/// Старые файлы не менять, только добавлять новые в конец.
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/001_init.sql"),
    include_str!("../migrations/002_audio_reserved.sql"),
    include_str!("../migrations/003_system_metrics.sql"),
    include_str!("../migrations/004_drop_idle.sql"),
    include_str!("../migrations/005_clipboard.sql"),
    include_str!("../migrations/006_clipboard_unique.sql"),
];

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("ошибка базы данных: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("база данных повреждена: {0}")]
    Corrupt(String),
    #[error("не удалось переместить повреждённую базу: {0}")]
    Io(#[from] std::io::Error),
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
            StorageError::Io(_) => false,
        }
    }
}

/// Сколько записей буфера обмена хранится максимум.
const MAX_CLIPS: usize = 2000;

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
    Ok(Sample { ts: r.get(0)?, app: r.get(1)?, title: r.get(2)? })
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
        let applied: usize = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", i + 1)?;
            tx.commit()?;
        }
        Ok(Self { conn })
    }

    pub fn insert_sample(&self, s: &Sample) -> Result<()> {
        self.conn.execute(
            "INSERT INTO samples (ts, app, title) VALUES (?1, ?2, ?3)",
            params![s.ts, s.app, s.title],
        )?;
        Ok(())
    }

    pub fn samples_between(&self, from: Timestamp, to: Timestamp) -> Result<Vec<Sample>> {
        let mut stmt = self
            .conn
            .prepare("SELECT ts, app, title FROM samples WHERE ts >= ?1 AND ts < ?2 ORDER BY ts")?;
        let rows = stmt.query_map(params![from, to], read_sample)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn last_sample(&self) -> Result<Option<Sample>> {
        let mut stmt = self.conn.prepare("SELECT ts, app, title FROM samples ORDER BY ts DESC LIMIT 1")?;
        let mut rows = stmt.query_map([], read_sample)?;
        Ok(rows.next().transpose()?)
    }

    /// Одинаковый текст не дублируется: повторное копирование поднимает старую запись наверх.
    pub fn insert_clip(&self, c: &ClipEntry) -> Result<()> {
        self.conn.execute(
            "INSERT INTO clipboard (ts, app, text) VALUES (?1, ?2, ?3)
             ON CONFLICT(text) DO UPDATE SET ts = excluded.ts, app = excluded.app",
            params![c.ts, c.app, c.text],
        )?;
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
        tx.execute(
            "INSERT INTO system_samples (ts, cpu, mem, mem_used_mb, net_rx, net_tx, disk_free_gb, battery_pct, on_battery, temp_c, uptime_s)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                s.ts, s.cpu_pct, s.mem_pct, s.mem_used_mb, s.net_rx as i64, s.net_tx as i64,
                s.disk_free_gb, s.battery_pct, s.on_battery, s.temp_c, s.uptime_s as i64
            ],
        )?;
        for p in procs {
            tx.execute(
                "INSERT INTO proc_samples (ts, name, cpu, mem_mb) VALUES (?1, ?2, ?3, ?4)",
                params![p.ts, p.name, p.cpu_pct, p.mem_mb],
            )?;
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
        let mut stmt = self.conn.prepare(
            "SELECT app, title, MIN(ts), MAX(ts), COUNT(*) FROM samples
             GROUP BY app, title ORDER BY MAX(ts) DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SearchHit { app: r.get(0)?, title: r.get(1)?, first_ts: r.get(2)?, last_ts: r.get(3)?, count: r.get(4)? })
        })?;
        let mut out = Vec::new();
        for hit in rows {
            let hit = hit?;
            if hit.app == HIDDEN_APP {
                continue;
            }
            if hit.app.to_lowercase().contains(&q) || hit.title.to_lowercase().contains(&q) {
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
        for table in ["samples", "bookmarks", "system_samples", "proc_samples", "clipboard"] {
            self.conn.execute(&format!("DELETE FROM {table} WHERE ts >= ?1 AND ts < ?2"), params![from, to])?;
        }
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
        self.conn.execute("DELETE FROM system_samples WHERE ts < ?1", params![system_before])?;
        self.conn.execute("DELETE FROM proc_samples WHERE ts < ?1", params![procs_before])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts: i64, app: &str, title: &str) -> Sample {
        Sample { ts, app: app.into(), title: title.into() }
    }

    #[test]
    fn roundtrip_and_delete() {
        let st = Storage::open_in_memory().unwrap();
        let s = sample(100, "a", "t");
        st.insert_sample(&s).unwrap();
        assert_eq!(st.samples_between(0, 200).unwrap(), vec![s.clone()]);
        assert_eq!(st.last_sample().unwrap(), Some(s));
        st.delete_between(0, 200).unwrap();
        assert!(st.samples_between(0, 200).unwrap().is_empty());
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
