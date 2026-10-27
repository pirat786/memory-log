# Memory Log

Локальная «память компьютера»: что вы делали за день, что было важным. Всё хранится на вашем ПК.

Архитектура: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Что нужно для разработки (Windows)

1. [Rust](https://rustup.rs) (stable)
2. Visual Studio Build Tools с компонентом «Разработка классических приложений на C++»
3. WebView2 (в Windows 11 уже есть)
4. Tauri CLI: `cargo install tauri-cli --version "^2"`

## Команды

```bash
cargo test --workspace      # тесты логики и базы
cd app && cargo tauri dev   # запуск в режиме разработки
cd app && cargo tauri build # установщик .exe
```

Перед первой сборкой нужны иконки: `cd app && cargo tauri icon путь/к/картинке.png`.
