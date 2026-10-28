#![windows_subsystem = "windows"]

mod clip_nav;
mod collector_loop;
mod commands;
mod log;
mod state;
mod tray;
mod window;

use std::sync::Arc;

use tauri::{Manager, RunEvent};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tauri_plugin_autostart::MacosLauncher;

use crate::state::AppState;

fn main() {
    // Программа без консоли: причину падения иначе не увидеть.
    std::panic::set_hook(Box::new(|info| log::info(&format!("паника: {info}"))));
    tauri::Builder::default()
        // Вторая копия не запускается, а показывает окно первой: две программы
        // не должны одновременно писать в одну базу.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            window::show_main(app);
        }))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    let Some(state) = app.try_state::<Arc<AppState>>() else { return };
                    // Вниз по списку — к более старым записям, вверх — к новым.
                    let delta = if shortcut.key == Code::ArrowDown { 1 } else { -1 };
                    clip_nav::step(app, &state, delta);
                })
                .build(),
        )
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            log::init(dir.join("memory-log.log"));
            log::info(&format!("запуск, данные в {}", dir.display()));

            let state = Arc::new(AppState::open(&dir.join("memory-log.db"), app.path().download_dir().ok())?);
            collector_loop::spawn(state.clone());
            tray::build(app, state.clone())?;
            app.manage(state);

            // Ctrl+Alt+стрелки, а не просто Ctrl+стрелки: те нужны текстовым редакторам и браузеру.
            let mods = Some(Modifiers::CONTROL | Modifiers::ALT);
            for key in [Code::ArrowDown, Code::ArrowUp] {
                if let Err(e) = app.global_shortcut().register(Shortcut::new(mods, key)) {
                    log::info(&format!("не удалось занять сочетание клавиш для листания буфера: {e}"));
                }
            }
            Ok(())
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
            commands::clip_nav_state,
            commands::delete_clip,
            commands::clear_clips,
            commands::export_day_csv,
            commands::get_autostart,
            commands::set_autostart,
        ])
        .build(tauri::generate_context!())
        .expect("не удалось собрать Memory Log")
        // Закрытие окна уничтожает его веб-часть (в памяти остаётся один процесс), запись и трей работают.
        // Выход только из меню трея: он приходит с кодом, а закрытие последнего окна — без кода.
        .run(|_app, event| {
            if let RunEvent::ExitRequested { api, code, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}
