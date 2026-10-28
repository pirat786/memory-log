# Languages / Языки

All user-visible text lives in this folder. Both the interface (`ui/`) and the Rust side
(tray menu, CSV header, error messages) read these same files, so a language is added in one place.

## Add a language

1. Copy `en.json` to `<code>.json` (for example `de.json`) and translate the values.
   Keep the keys and the `{placeholders}` unchanged. Missing keys fall back to English.
2. Add one line to `languages.json`:
   `{ "code": "de", "name": "Deutsch", "locale": "de-DE" }`
3. Rebuild. The language shows up in the selector and is picked automatically
   when it matches the Windows display language.

`unit.bytes` is a comma-separated list: bytes, kilobytes, megabytes, gigabytes, terabytes.

## Добавить язык

1. Скопируйте `en.json` в `<код>.json` и переведите значения. Ключи и `{подстановки}` не менять.
   Недостающие ключи берутся из английского.
2. Добавьте строку в `languages.json`.
3. Пересоберите программу. Язык появится в списке и выберется сам, если совпадёт с языком Windows.
