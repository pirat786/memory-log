CREATE TABLE IF NOT EXISTS activity_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    idle_minutes INTEGER NOT NULL CHECK (idle_minutes BETWEEN 1 AND 60)
);
INSERT OR IGNORE INTO activity_settings VALUES (1, 5);
