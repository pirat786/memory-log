use tauri::{AppHandle, Manager, WebviewWindowBuilder};

use crate::log;

/// Показывает главное окно. Когда оно закрыто, его веб-часть уничтожена (чтобы в памяти
/// оставался один процесс), поэтому окно создаётся заново по настройкам из tauri.conf.json.
pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        return;
    }
    let Some(cfg) = app.config().app.windows.first() else { return };
    match WebviewWindowBuilder::from_config(app, cfg).and_then(|b| b.build()) {
        Ok(w) => {
            let _ = w.set_focus();
        }
        Err(e) => log::info(&format!("не удалось открыть окно: {e}")),
    }
}
