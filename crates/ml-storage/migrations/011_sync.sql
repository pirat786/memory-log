CREATE TABLE sync_link (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    device TEXT NOT NULL,
    label TEXT NOT NULL,
    account TEXT NOT NULL DEFAULT '',
    email TEXT NOT NULL DEFAULT '',
    since INTEGER NOT NULL DEFAULT 0,
    enabled INTEGER NOT NULL DEFAULT 0,
    synced_at INTEGER
);

CREATE TABLE sync_dirty (
    day INTEGER PRIMARY KEY,
    revision INTEGER NOT NULL
);
INSERT INTO sync_dirty
SELECT ts / 86400, 1 FROM (SELECT ts FROM samples UNION ALL SELECT ts FROM bookmarks)
GROUP BY ts / 86400;

CREATE TRIGGER sync_sample_added AFTER INSERT ON samples BEGIN
    INSERT INTO sync_dirty VALUES (new.ts / 86400, 1)
    ON CONFLICT(day) DO UPDATE SET revision = revision + 1;
END;
CREATE TRIGGER sync_sample_deleted AFTER DELETE ON samples BEGIN
    INSERT INTO sync_dirty VALUES (old.ts / 86400, 1)
    ON CONFLICT(day) DO UPDATE SET revision = revision + 1;
END;
CREATE TRIGGER sync_bookmark_added AFTER INSERT ON bookmarks BEGIN
    INSERT INTO sync_dirty VALUES (new.ts / 86400, 1)
    ON CONFLICT(day) DO UPDATE SET revision = revision + 1;
END;
CREATE TRIGGER sync_bookmark_deleted AFTER DELETE ON bookmarks BEGIN
    INSERT INTO sync_dirty VALUES (old.ts / 86400, 1)
    ON CONFLICT(day) DO UPDATE SET revision = revision + 1;
END;
CREATE TRIGGER sync_category_added AFTER INSERT ON category_overrides WHEN new.scope = 'visit' BEGIN
    INSERT INTO sync_dirty VALUES (new.ts / 86400, 1)
    ON CONFLICT(day) DO UPDATE SET revision = revision + 1;
END;
CREATE TRIGGER sync_category_changed AFTER UPDATE ON category_overrides WHEN new.scope = 'visit' BEGIN
    INSERT INTO sync_dirty VALUES (new.ts / 86400, 1)
    ON CONFLICT(day) DO UPDATE SET revision = revision + 1;
END;
CREATE TRIGGER sync_category_deleted AFTER DELETE ON category_overrides WHEN old.scope = 'visit' BEGIN
    INSERT INTO sync_dirty VALUES (old.ts / 86400, 1)
    ON CONFLICT(day) DO UPDATE SET revision = revision + 1;
END;

CREATE TABLE sync_peer_files (
    device TEXT NOT NULL,
    day INTEGER NOT NULL,
    file_id TEXT NOT NULL,
    version TEXT NOT NULL,
    label TEXT,
    rules TEXT NOT NULL,
    PRIMARY KEY (device, day)
);
CREATE TABLE sync_peer_samples (
    device TEXT NOT NULL,
    day INTEGER NOT NULL,
    position INTEGER NOT NULL,
    ts INTEGER NOT NULL,
    app TEXT NOT NULL,
    title TEXT NOT NULL,
    idle_secs INTEGER NOT NULL,
    kind INTEGER NOT NULL,
    PRIMARY KEY (device, day, position)
);
CREATE INDEX sync_peer_sample_times ON sync_peer_samples(device, ts);
CREATE TABLE sync_peer_bookmarks (
    device TEXT NOT NULL,
    day INTEGER NOT NULL,
    position INTEGER NOT NULL,
    ts INTEGER NOT NULL,
    app TEXT NOT NULL,
    title TEXT NOT NULL,
    note TEXT NOT NULL,
    PRIMARY KEY (device, day, position)
);
CREATE INDEX sync_peer_bookmark_times ON sync_peer_bookmarks(device, ts);
