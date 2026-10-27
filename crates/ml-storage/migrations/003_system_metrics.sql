CREATE TABLE system_samples (
    ts          INTEGER NOT NULL,
    cpu         REAL    NOT NULL,
    mem         REAL    NOT NULL,
    mem_used_mb INTEGER NOT NULL,
    net_rx      INTEGER NOT NULL,
    net_tx      INTEGER NOT NULL,
    disk_free_gb REAL,
    battery_pct INTEGER,
    on_battery  INTEGER,
    temp_c      REAL,
    uptime_s    INTEGER NOT NULL
);
CREATE INDEX idx_system_ts ON system_samples(ts);

CREATE TABLE proc_samples (
    ts     INTEGER NOT NULL,
    name   TEXT    NOT NULL,
    cpu    REAL    NOT NULL,
    mem_mb INTEGER NOT NULL
);
CREATE INDEX idx_proc_ts ON proc_samples(ts);

CREATE TABLE goals (
    category   TEXT PRIMARY KEY,
    limit_mins INTEGER NOT NULL
);
