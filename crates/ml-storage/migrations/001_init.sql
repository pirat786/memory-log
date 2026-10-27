CREATE TABLE samples (
    ts    INTEGER NOT NULL,
    app   TEXT    NOT NULL,
    title TEXT    NOT NULL,
    idle  INTEGER NOT NULL
);
CREATE INDEX idx_samples_ts ON samples(ts);

CREATE TABLE bookmarks (
    ts    INTEGER NOT NULL,
    app   TEXT    NOT NULL,
    title TEXT    NOT NULL,
    note  TEXT    NOT NULL
);
CREATE INDEX idx_bookmarks_ts ON bookmarks(ts);
