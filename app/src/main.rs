#![windows_subsystem = "windows"]

mod clip_nav;
mod cloud;
mod collector_loop;
mod commands;
mod i18n;
mod log;
mod quick_files;
mod state;
mod sys_tools;
#[cfg(test)]
mod tests;
mod tray;
#[cfg(windows)]
mod updater;
mod week_comparison;
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
                    if shortcut.key == Code::Space {
                        window::trigger_widget(app);
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
            #[cfg(windows)]
            if updater::updater_configured(app.handle().clone()) {
                app.handle().plugin(tauri_plugin_updater::Builder::new().build())?;
                app.manage(updater::PendingUpdate::default());
            }
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            log::init(dir.join("memory-log.log"));
            log::info(&format!("запуск, данные в {}", dir.display()));

            let state = Arc::new(AppState::open(&dir.join("memory-log.db"), app.path().download_dir().ok())?);
            collector_loop::spawn(state.clone());
            cloud::spawn(state.clone());
            tray::build(app, state.clone())?;
            let restore_widget = state.widget_enabled();
            app.manage(state);
            if restore_widget {
                window::open_widget(app.handle());
            }

            // Ctrl+Alt+стрелки, а не просто Ctrl+стрелки: те нужны текстовым редакторам и браузеру.
            let mods = Some(Modifiers::CONTROL | Modifiers::ALT);
            for key in [Code::ArrowDown, Code::ArrowUp, Code::Space] {
                if let Err(e) = app.global_shortcut().register(Shortcut::new(mods, key)) {
                    log::info(&format!("не удалось занять сочетание клавиш для листания буфера: {e}"));
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::day_summary,
            commands::week_overview,
            commands::week_comparison,
            commands::recorded_days,
            commands::calendar_heatmap,
            commands::recording_settings,
            commands::set_recording_settings,
            commands::delete_days,
            commands::category_rules,
            commands::set_category_override,
            commands::sync_status,
            commands::sync_connect,
            commands::sync_disconnect,
            commands::sync_rename,
            commands::sync_now,
            commands::sync_device_days,
            commands::sync_search,
            commands::sync_remove_peer,
            #[cfg(windows)]
            updater::updater_configured,
            #[cfg(windows)]
            updater::check_update,
            #[cfg(windows)]
            updater::install_update,
            commands::search,
            commands::add_bookmark,
            commands::set_goal,
            commands::forget_minutes,
            commands::set_paused,
            commands::get_idle_minutes,
            commands::set_idle_minutes,
            commands::status,
            commands::clip_nav_state,
            commands::get_language,
            commands::set_language,
            commands::delete_clip,
            commands::clear_clips,
            commands::export_day_csv,
            commands::get_autostart,
            commands::set_autostart,
            commands::live_system,
            commands::kill_process,
            commands::scan_junk,
            commands::clean_junk,
            commands::startup_items,
            commands::set_startup_item,
            commands::listening_ports,
            commands::reveal_path,
            commands::power_status,
            commands::set_keep_awake,
            commands::set_power_rule,
            commands::cancel_power_rule,
            commands::toggle_widget,
            commands::widget_open,
            commands::widget_resize,
            commands::open_main_window,
            commands::quick_drop,
            commands::quick_drop_cancel,
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
