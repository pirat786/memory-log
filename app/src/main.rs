#![windows_subsystem = "windows"]

mod collector_loop;
mod commands;
mod log;
mod state;
mod tray;

use std::sync::Arc;

use tauri::{Manager, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;

use crate::state::AppState;

fn main() {
    // Программа без консоли: причину падения иначе не увидеть.
    std::panic::set_hook(Box::new(|info| log::info(&format!("паника: {info}"))));
    tauri::Builder::default()
        // Вторая копия не запускается, а показывает окно первой: две программы
        // не должны одновременно писать в одну базу.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            log::init(dir.join("memory-log.log"));
            log::info(&format!("запуск, данные в {}", dir.display()));

            let state = Arc::new(AppState::open(&dir.join("memory-log.db"), app.path().download_dir().ok())?);
            collector_loop::spawn(state.clone());
            tray::build(app, state.clone())?;
            app.manage(state);
            Ok(())
        })
        // Закрытие окна прячет его в трей, запись продолжается.
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::day_summary,
            commands::week_overview,
            commands::search,
            commands::add_bookmark,
            commands::set_goal,
            commands::forget_minutes,
            commands::set_paused,
            commands::status,
            commands::export_day_csv,
            commands::get_autostart,
            commands::set_autostart,
        ])
        .run(tauri::generate_context!())
        .expect("не удалось запустить Memory Log");
}
