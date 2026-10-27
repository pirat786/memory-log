//! Локальное хранилище на SQLite. Все данные остаются на диске пользователя.

use std::path::Path;

use ml_core::{Bookmark, Sample, Timestamp};
use rusqlite::{params, Connection};

/// Миграции применяются по порядку; номер версии хранится в `PRAGMA user_version`.
/// Старые файлы не менять, только добавлять новые в конец.
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/001_init.sql"),
    include_str!("../migrations/002_audio_reserved.sql"),
];

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("ошибка базы данных: {0}")]
    Db(#[from] rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, StorageError>;

pub struct Storage {
    conn: Connection,
}

impl Storage {
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
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
            "INSERT INTO samples (ts, app, title, idle) VALUES (?1, ?2, ?3, ?4)",
            params![s.ts, s.app, s.title, s.idle],
        )?;
        Ok(())
    }

    pub fn samples_between(&self, from: Timestamp, to: Timestamp) -> Result<Vec<Sample>> {
        let mut stmt = self
            .conn
            .prepare("SELECT ts, app, title, idle FROM samples WHERE ts >= ?1 AND ts < ?2 ORDER BY ts")?;
        let rows = stmt.query_map(params![from, to], |r| {
            Ok(Sample { ts: r.get(0)?, app: r.get(1)?, title: r.get(2)?, idle: r.get(3)? })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn last_sample(&self) -> Result<Option<Sample>> {
        let mut stmt = self
            .conn
            .prepare("SELECT ts, app, title, idle FROM samples ORDER BY ts DESC LIMIT 1")?;
        let mut rows = stmt.query_map([], |r| {
            Ok(Sample { ts: r.get(0)?, app: r.get(1)?, title: r.get(2)?, idle: r.get(3)? })
        })?;
        Ok(rows.next().transpose()?)
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

    /// Удаляет всё записанное за период («забыть последние N минут»).
    pub fn delete_between(&self, from: Timestamp, to: Timestamp) -> Result<()> {
        self.conn.execute("DELETE FROM samples WHERE ts >= ?1 AND ts < ?2", params![from, to])?;
        self.conn.execute("DELETE FROM bookmarks WHERE ts >= ?1 AND ts < ?2", params![from, to])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_delete() {
        let st = Storage::open_in_memory().unwrap();
        let s = Sample { ts: 100, app: "a".into(), title: "t".into(), idle: false };
        st.insert_sample(&s).unwrap();
        assert_eq!(st.samples_between(0, 200).unwrap(), vec![s.clone()]);
        assert_eq!(st.last_sample().unwrap(), Some(s));
        st.delete_between(0, 200).unwrap();
        assert!(st.samples_between(0, 200).unwrap().is_empty());
    }
}
