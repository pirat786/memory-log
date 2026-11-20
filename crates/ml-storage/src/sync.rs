use std::collections::HashSet;

use ml_core::model::{ActivityEvent, Bookmark, CategoryOverride, OverrideScope, Sample, SearchHit};
use ml_core::privacy::HIDDEN_APP;
use rusqlite::{params, Transaction};
use serde::{Deserialize, Serialize};

use crate::{Result, Storage, BREAK_EVENT, IDLE_EVENT};

pub const DAY_SECONDS: i64 = 86_400;
pub const META_DAY: i64 = -1;

#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub id: String,
    pub label: String,
}

pub struct Link {
    pub device: Device,
    pub account: String,
    pub email: String,
    pub since: i64,
    pub enabled: bool,
    pub synced_at: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Shard {
    pub protocol: u32,
    pub device: String,
    pub day: i64,
    pub label: Option<String>,
    pub events: Vec<ActivityEvent>,
    pub bookmarks: Vec<Bookmark>,
    pub rules: Vec<CategoryOverride>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PeerHit {
    pub device: String,
    pub hit: SearchHit,
}

fn remove_shard(tx: &Transaction<'_>, device: &str, day: i64) -> Result<()> {
    for table in ["sync_peer_files", "sync_peer_samples", "sync_peer_bookmarks"] {
        tx.execute(&format!("DELETE FROM {table} WHERE device = ?1 AND day = ?2"), params![device, day])?;
    }
    Ok(())
}

fn clear_peers(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("DELETE FROM sync_peer_files; DELETE FROM sync_peer_samples; DELETE FROM sync_peer_bookmarks;")?;
    Ok(())
}

impl Storage {
    pub fn init_sync_device(&self, id: &str, label: &str) -> Result<()> {
        self.conn.execute("INSERT OR IGNORE INTO sync_link (id, device, label) VALUES (1, ?1, ?2)", params![id, label])?;
        Ok(())
    }

    pub fn sync_link(&self) -> Result<Link> {
        Ok(self.conn.query_row("SELECT device, label, account, email, since, enabled, synced_at FROM sync_link WHERE id = 1", [], |r| {
            Ok(Link {
                device: Device { id: r.get(0)?, label: r.get(1)? },
                account: r.get(2)?, email: r.get(3)?, since: r.get(4)?, enabled: r.get(5)?, synced_at: r.get(6)?,
            })
        })?)
    }

    pub fn enable_sync(&self, account: &str, email: &str, label: &str, include_history: bool, now: i64) -> Result<()> {
        let previous = self.sync_link()?;
        let changed = previous.account != account;
        let since = if include_history { 0 } else if changed { now } else { previous.since };
        let tx = self.conn.unchecked_transaction()?;
        if changed {
            clear_peers(&tx)?;
        }
        if changed || since != previous.since {
            tx.execute(
                "INSERT INTO sync_dirty SELECT ts / 86400, 1 FROM
                 (SELECT ts FROM samples UNION ALL SELECT ts FROM bookmarks) WHERE ts >= ?1 GROUP BY ts / 86400
                 ON CONFLICT(day) DO UPDATE SET revision = revision + 1", [since],
            )?;
        }
        tx.execute("UPDATE sync_link SET label = ?1, account = ?2, email = ?3, since = ?4, enabled = 1,
                    synced_at = CASE WHEN account = ?2 THEN synced_at ELSE NULL END WHERE id = 1",
            params![label, account, email, since])?;
        tx.commit()?;
        Ok(())
    }

    pub fn disable_sync(&self) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("UPDATE sync_link SET enabled = 0 WHERE id = 1", [])?;
        clear_peers(&tx)?;
        tx.commit()?;
        Ok(())
    }

    pub fn rename_sync_device(&self, label: &str) -> Result<()> {
        self.conn.execute("UPDATE sync_link SET label = ?1 WHERE id = 1", [label])?;
        Ok(())
    }

    pub fn finish_sync(&self, at: i64) -> Result<()> {
        self.conn.execute("UPDATE sync_link SET synced_at = ?1 WHERE id = 1", [at])?;
        Ok(())
    }

    pub fn dirty_sync_days(&self, since: i64, limit: u32) -> Result<Vec<(i64, i64)>> {
        let mut stmt = self.conn.prepare("SELECT day, revision FROM sync_dirty WHERE day >= ?1 ORDER BY day DESC LIMIT ?2")?;
        let rows = stmt.query_map(params![since / DAY_SECONDS, limit], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn requeue_sync_history(&self, since: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO sync_dirty SELECT ts / 86400, 1 FROM
             (SELECT ts FROM samples UNION ALL SELECT ts FROM bookmarks) WHERE ts >= ?1 GROUP BY ts / 86400",
            [since],
        )?;
        Ok(())
    }

    pub fn acknowledge_sync_day(&self, day: i64, revision: i64) -> Result<()> {
        self.conn.execute("DELETE FROM sync_dirty WHERE day = ?1 AND revision = ?2", params![day, revision])?;
        Ok(())
    }

    pub fn export_sync_shard(&self, link: &Link, day: i64) -> Result<Shard> {
        let mut shard = Shard {
            protocol: 1, device: link.device.id.clone(), day,
            label: None, events: Vec::new(), bookmarks: Vec::new(), rules: Vec::new(),
        };
        if day == META_DAY {
            shard.label = Some(link.device.label.clone());
            shard.rules = self.category_overrides(0, 0)?;
        } else {
            let from = (day * DAY_SECONDS).max(link.since);
            let to = (day + 1) * DAY_SECONDS;
            shard.events = self.activity_between(from, to)?;
            shard.bookmarks = self.bookmarks_between(from, to)?;
            shard.rules = self.category_overrides(from, to)?.into_iter()
                .filter(|rule| rule.scope == OverrideScope::Visit).collect();
        }
        Ok(shard)
    }

    pub fn peer_sync_versions(&self) -> Result<Vec<(String, i64, String, String)>> {
        let mut stmt = self.conn.prepare("SELECT device, day, file_id, version FROM sync_peer_files")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn import_sync_shard(&self, file_id: &str, version: &str, shard: Shard) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        remove_shard(&tx, &shard.device, shard.day)?;
        {
            let mut samples = tx.prepare("INSERT INTO sync_peer_samples VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)")?;
            for (position, event) in shard.events.iter().enumerate() {
                let (ts, app, title, idle, kind) = match event {
                    ActivityEvent::Window(s) => (s.ts, s.app.as_str(), s.title.as_str(), s.idle_secs, 0),
                    ActivityEvent::Break(ts) => (*ts, "", "", 0, BREAK_EVENT),
                    ActivityEvent::Idle(ts) => (*ts, "", "", 0, IDLE_EVENT),
                };
                samples.execute(params![shard.device, shard.day, position, ts, app, title, idle, kind])?;
            }
            let mut bookmarks = tx.prepare("INSERT INTO sync_peer_bookmarks VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)")?;
            for (position, bookmark) in shard.bookmarks.iter().enumerate() {
                bookmarks.execute(params![shard.device, shard.day, position, bookmark.ts, bookmark.app, bookmark.title, bookmark.note])?;
            }
        }
        tx.execute("INSERT INTO sync_peer_files VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![shard.device, shard.day, file_id, version, shard.label, serde_json::to_string(&shard.rules)?])?;
        tx.commit()?;
        Ok(())
    }

    pub fn reconcile_sync_peers(&self, live: &HashSet<(String, i64)>) -> Result<()> {
        let removed: Vec<_> = self.peer_sync_versions()?.into_iter()
            .map(|(device, day, _, _)| (device, day)).filter(|key| !live.contains(key)).collect();
        let tx = self.conn.unchecked_transaction()?;
        for (device, day) in removed {
            remove_shard(&tx, &device, day)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn sync_peers(&self) -> Result<Vec<Device>> {
        let mut stmt = self.conn.prepare(
            "SELECT device, COALESCE(MAX(label), substr(device, 1, 8)) FROM sync_peer_files GROUP BY device ORDER BY 2, device",
        )?;
        let rows = stmt.query_map([], |r| Ok(Device { id: r.get(0)?, label: r.get(1)? }))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn remove_sync_peer(&self, device: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for table in ["sync_peer_files", "sync_peer_samples", "sync_peer_bookmarks"] {
            tx.execute(&format!("DELETE FROM {table} WHERE device = ?1"), [device])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn peer_activity(&self, device: &str, from: i64, to: i64) -> Result<Vec<ActivityEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT ts, app, title, idle_secs, kind FROM sync_peer_samples WHERE device = ?1 AND ts >= ?2 AND ts < ?3 ORDER BY ts, day, position",
        )?;
        let rows = stmt.query_map(params![device, from, to], |r| match r.get::<_, u8>(4)? {
            BREAK_EVENT => Ok(ActivityEvent::Break(r.get(0)?)),
            IDLE_EVENT => Ok(ActivityEvent::Idle(r.get(0)?)),
            _ => Ok(ActivityEvent::Window(Sample { ts: r.get(0)?, app: r.get(1)?, title: r.get(2)?, idle_secs: r.get(3)? })),
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn peer_bookmarks(&self, device: &str, from: i64, to: i64) -> Result<Vec<Bookmark>> {
        let mut stmt = self.conn.prepare(
            "SELECT ts, app, title, note FROM sync_peer_bookmarks WHERE device = ?1 AND ts >= ?2 AND ts < ?3 ORDER BY ts, position",
        )?;
        let rows = stmt.query_map(params![device, from, to], |r| {
            Ok(Bookmark { ts: r.get(0)?, app: r.get(1)?, title: r.get(2)?, note: r.get(3)? })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn has_peer_records(&self, device: Option<&str>, from: i64, to: i64) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sync_peer_samples WHERE (?1 IS NULL OR device = ?1) AND ts >= ?2 AND ts < ?3 AND kind != 1)
              OR EXISTS(SELECT 1 FROM sync_peer_bookmarks WHERE (?1 IS NULL OR device = ?1) AND ts >= ?2 AND ts < ?3)",
            params![device, from, to], |r| r.get(0),
        )?)
    }

    pub fn peer_rules(&self, device: &str, from: i64, to: i64) -> Result<Vec<CategoryOverride>> {
        let mut stmt = self.conn.prepare(
            "SELECT rules FROM sync_peer_files WHERE device = ?1 AND (day = -1 OR day BETWEEN ?2 AND ?3) ORDER BY day",
        )?;
        let rows = stmt.query_map(params![device, from / DAY_SECONDS, (to - 1) / DAY_SECONDS], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.extend(serde_json::from_str::<Vec<CategoryOverride>>(&row?)?);
        }
        Ok(out)
    }

    pub fn search_peers(&self, query: &str, device: Option<&str>, limit: usize) -> Result<Vec<PeerHit>> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT device, app, title, MIN(ts), MAX(ts), COUNT(*) FROM sync_peer_samples
             WHERE (?1 IS NULL OR device = ?1) AND kind = 0 AND idle_secs < ?2 AND app != ?3
             GROUP BY device, app, title ORDER BY MAX(ts) DESC",
        )?;
        let rows = stmt.query_map(params![device, ml_core::sessions::LEGACY_IDLE_SECS, HIDDEN_APP], |r| {
            Ok(PeerHit { device: r.get(0)?, hit: SearchHit {
                app: r.get(1)?, title: r.get(2)?, first_ts: r.get(3)?, last_ts: r.get(4)?, count: r.get(5)?,
            } })
        })?;
        let mut hits = Vec::new();
        for row in rows {
            let row = row?;
            if row.hit.app.to_lowercase().contains(&query) || row.hit.title.to_lowercase().contains(&query) {
                hits.push(row);
                if hits.len() == limit {
                    break;
                }
            }
        }
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ml_core::ClipEntry;

    fn record(ts: i64, title: &str) -> Sample {
        Sample { ts, app: "editor".into(), title: title.into(), idle_secs: 0 }
    }

    #[test]
    fn link_cuts_off_old_history_and_keeps_clipboard_local() {
        let storage = Storage::open_in_memory().unwrap();
        storage.init_sync_device("local-device", "PC").unwrap();
        storage.insert_sample(&record(10, "old")).unwrap();
        storage.enable_sync("account", "name@example.test", "PC", false, 20).unwrap();
        storage.insert_sample(&record(21, "new")).unwrap();
        storage.insert_clip(&ClipEntry { ts: 22, app: "editor".into(), text: "clipboard only".into() }).unwrap();
        let shard = storage.export_sync_shard(&storage.sync_link().unwrap(), 0).unwrap();
        assert_eq!(shard.events, [ActivityEvent::Window(record(21, "new"))]);
        assert!(!serde_json::to_string(&shard).unwrap().contains("clipboard only"));
    }

    #[test]
    fn reupload_replaces_peer_records_and_acknowledgement_preserves_new_writes() {
        let source = Storage::open_in_memory().unwrap();
        let peer = Storage::open_in_memory().unwrap();
        source.init_sync_device("source", "Home").unwrap();
        source.enable_sync("account", "", "Home", true, 0).unwrap();
        let link = source.sync_link().unwrap();
        source.insert_sample(&record(10, "first")).unwrap();
        let [(day, revision)] = source.dirty_sync_days(0, 10).unwrap()[..] else { panic!("expected one dirty day") };
        peer.import_sync_shard("file", "1", source.export_sync_shard(&link, day).unwrap()).unwrap();
        assert_eq!(peer.peer_activity("source", 0, 30).unwrap().len(), 1);
        source.insert_sample(&record(20, "second")).unwrap();
        source.acknowledge_sync_day(day, revision).unwrap();
        assert_ne!(source.dirty_sync_days(0, 10).unwrap()[0].1, revision);
        source.delete_between(0, 30).unwrap();
        peer.import_sync_shard("file", "2", source.export_sync_shard(&link, day).unwrap()).unwrap();
        assert!(peer.peer_activity("source", 0, 30).unwrap().is_empty());
        assert!(peer.search_peers("first", None, 100).unwrap().is_empty());
    }
}
