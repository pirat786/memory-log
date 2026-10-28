use std::sync::Arc;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::App;

use crate::i18n;
use crate::state::AppState;

pub fn build(app: &App, state: Arc<AppState>) -> tauri::Result<()> {
    let lang = state.lang();
    let item = |id: &str, key: &str| MenuItem::with_id(app, id, i18n::t(&lang, key), true, None::<&str>);
    let open = item("open", "tray.open")?;
    let mark = item("mark", "tray.mark")?;
    let pause = item("pause", "tray.pause")?;
    let quit = item("quit", "tray.quit")?;
    let menu = Menu::with_items(app, &[&open, &mark, &pause, &quit])?;
    if let Ok(mut items) = state.tray_items.lock() {
        items.extend([("tray.open", open), ("tray.mark", mark), ("tray.pause", pause), ("tray.quit", quit)]);
    }

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
