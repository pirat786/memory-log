use std::sync::Arc;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::App;

use crate::state::AppState;

pub fn build(app: &App, state: Arc<AppState>) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Открыть сводку", true, None::<&str>)?;
    let mark = MenuItem::with_id(app, "mark", "Запомнить это", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause", "Пауза / продолжить", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &mark, &pause, &quit])?;

    let mut builder = TrayIconBuilder::new().menu(&menu).tooltip("Memory Log");
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "open" => crate::window::show_main(app),
            "mark" => {
                let _ = state.add_bookmark("");
            }
            "pause" => state.set_paused(!state.is_paused()),
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}
