ALTER TABLE activity_settings ADD COLUMN clipboard_enabled INTEGER NOT NULL DEFAULT 1 CHECK (clipboard_enabled IN (0, 1));
ALTER TABLE activity_settings ADD COLUMN retention_days INTEGER NOT NULL DEFAULT 0 CHECK (retention_days BETWEEN 0 AND 3650);
ALTER TABLE activity_settings ADD COLUMN excluded_apps TEXT NOT NULL DEFAULT '';

CREATE TABLE category_overrides (
    scope TEXT NOT NULL CHECK (scope IN ('visit', 'window', 'app')),
    ts INTEGER NOT NULL,
    app TEXT NOT NULL,
    title TEXT NOT NULL,
    category TEXT NOT NULL,
    PRIMARY KEY (scope, ts, app, title)
);
