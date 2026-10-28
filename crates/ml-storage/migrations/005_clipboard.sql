CREATE TABLE clipboard (
    ts   INTEGER NOT NULL,
    app  TEXT    NOT NULL,
    text TEXT    NOT NULL
);
CREATE INDEX idx_clipboard_ts ON clipboard(ts);
