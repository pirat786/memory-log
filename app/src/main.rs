#![windows_subsystem = "windows"]

mod collector_loop;
mod commands;
mod state;
mod tray;

use std::sync::Arc;

use tauri::{Manager, WindowEvent};

use crate::state::AppState;

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let state = Arc::new(AppState::open(&dir.join("memory-log.db"))?);

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
            commands::add_bookmark,
            commands::set_paused,
            commands::is_paused,
        ])
        .run(tauri::generate_context!())
        .expect("не удалось запустить Memory Log");
}
