# Memory Log

**English** · [Русский](README.ru.md)

A local "memory of your computer" for Windows: it quietly records what you worked on during the day
and shows a clear summary. **Everything stays on your PC.** Nothing is sent over the network.

## Features

- **Day summary**: active time, what you spent it on (work, communication, social media, games…),
  main activities, rhythm of the day (breaks, program switches), daily goals, a week overview.
- **Day timeline**: click a color-coded activity segment to see its windows, or a marker to jump to a bookmark.
  Activity counting stops after 5 minutes of idle time by default, on Windows lock, or on recording pause.
  Change the idle threshold in Settings.
- **History**: repeated visits to a window expand into a list. Change a visit's category or save a rule
  for its title or app. Search results jump to the matching session, and the date opens a calendar
  that marks days with records.
- **Clipboard history**: text you copied, with the program it came from. Each text is stored once.
  Browse it with **Ctrl+Alt+↑ / ↓** from anywhere: a small popup appears at the bottom right and the
  selected item is put into the clipboard, so you just press Ctrl+V. Copies from password managers and
  password-like text are never recorded; any entry can be deleted, or the whole history cleared.
- **"Remember this"**: bookmark the current moment with an optional note.
- **System tab**: CPU and memory load, network traffic by hour, heaviest programs, disk, battery.
- **Week comparison**: time by category and app, goal progress, and a weekday working-hours filter.
- **Recording settings**: turn off clipboard capture, exclude apps, set a retention limit,
  and delete records for a date range. Light, dark, and Windows themes are available.
- **Tray app**: closing the window keeps recording; with the window closed only one small process runs.
- **Languages**: English and Russian, chosen automatically from Windows and switchable in the app.
  Adding a language takes one file, see [ui/i18n/README.md](ui/i18n/README.md).
- Export a day to CSV, "forget the last 10 minutes", start with Windows.

## Download

Get `memory-log.exe` from the [Releases](../../releases) page and run it. Windows may show a SmartScreen
warning because the file is not code-signed: choose "More info → Run anyway". It needs the WebView2 runtime
(already present on Windows 11 and most Windows 10 installations).

## Privacy

- The database is a local SQLite file in `%APPDATA%\app.memorylog.desktop`.
- Windows of banks, password managers and private/incognito browsing are hidden before they reach the database.
- A visible tray icon and a one-click pause. There is no hidden mode.
- Window titles are recorded, not the content of windows. Keystrokes are not recorded.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for how it works (in Russian).

## Build from source (Windows)

1. [Rust](https://rustup.rs) (stable)
2. Visual Studio Build Tools with the "Desktop development with C++" workload
3. WebView2 (built into Windows 11)

```bash
cargo test --workspace                                   # logic and database tests
cargo build --release --manifest-path app/Cargo.toml --features tauri/custom-protocol
                                                         # target/release/memory-log.exe
```

For an installer or dev mode install the Tauri CLI (`cargo install tauri-cli --version "^2"`), then use
`cd app && cargo tauri dev` or `cd app && cargo tauri build`. Icons: `cd app && cargo tauri icon path/to/image.png`.

## Project layout

```
app/           Tauri shell: window, tray, hotkeys, commands
ui/            interface (plain HTML/CSS/JS) and translations in ui/i18n/
crates/ml-core       pure logic: sessions, summary, categories, privacy
crates/ml-storage    SQLite and migrations
crates/ml-collector  Windows data sources: active window, clipboard, system load
config/        category rules
```
