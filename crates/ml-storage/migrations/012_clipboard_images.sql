CREATE TABLE clipboard_images (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    ts      INTEGER NOT NULL,
    app     TEXT    NOT NULL,
    width   INTEGER NOT NULL,
    height  INTEGER NOT NULL,
    mime    TEXT    NOT NULL,
    thumb   BLOB    NOT NULL,
    data    BLOB    NOT NULL,
    sha256  BLOB    NOT NULL UNIQUE
);
CREATE INDEX idx_clipboard_images_ts ON clipboard_images(ts);
