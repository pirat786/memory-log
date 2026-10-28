-- Один и тот же текст хранится один раз: повторное копирование только поднимает запись наверх.
DELETE FROM clipboard WHERE EXISTS (
    SELECT 1 FROM clipboard n
    WHERE n.text = clipboard.text AND (n.ts > clipboard.ts OR (n.ts = clipboard.ts AND n.rowid > clipboard.rowid))
);
CREATE UNIQUE INDEX idx_clipboard_text ON clipboard(text);
