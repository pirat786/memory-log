-- Задел под микрофон и расшифровку речи. Пока не используется,
-- но схема уже есть, чтобы потом не ломать базу пользователей.
CREATE TABLE audio_segments (
    id         INTEGER PRIMARY KEY,
    start_ts   INTEGER NOT NULL,
    end_ts     INTEGER NOT NULL,
    source     TEXT    NOT NULL,   -- 'mic' | 'system'
    file_path  TEXT,               -- NULL после автоудаления аудио
    status     TEXT    NOT NULL DEFAULT 'new'  -- new | transcribed | failed
);

CREATE TABLE transcripts (
    id         INTEGER PRIMARY KEY,
    segment_id INTEGER NOT NULL REFERENCES audio_segments(id) ON DELETE CASCADE,
    ts         INTEGER NOT NULL,
    speaker    TEXT,
    text       TEXT    NOT NULL
);
CREATE INDEX idx_transcripts_ts ON transcripts(ts);
