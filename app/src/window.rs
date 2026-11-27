use std::sync::Arc;

use tauri::{AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, Size, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::log;
use crate::state::AppState;

const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --renderer-process-limit=1";

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

const COLLAPSED_W: f64 = 16.0;
const COLLAPSED_H: f64 = 68.0;
const EXPANDED_W: f64 = 350.0;
const EXPANDED_MIN_H: f64 = 220.0;
const EXPANDED_MAX_H: f64 = 460.0;
const EXPANDED_DEFAULT_H: f64 = 245.0;

fn dock_right_edge(w: &WebviewWindow, width: f64, height: f64) {
    let Ok(Some(monitor)) = w.current_monitor().or_else(|_| w.primary_monitor()) else { return };
    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    let margin = if width > COLLAPSED_W { 10.0 } else { 0.0 };
    let x = area.position.x as f64 + area.size.width as f64 - (width + margin) * scale;
    let y = area.position.y as f64 + (area.size.height as f64 - height * scale) * 0.45;
    let _ = w.set_size(Size::Logical(LogicalSize::new(width, height)));
    let _ = w.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
}

pub fn open_widget(app: &AppHandle) -> bool {
    if app.get_webview_window("widget").is_some() {
        return true;
    }
    match WebviewWindowBuilder::new(app, "widget", WebviewUrl::App("widget.html".into()))
        .title("Memory Log Widget")
        .inner_size(COLLAPSED_W, COLLAPSED_H)
        .transparent(true)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(false)
        .additional_browser_args(BROWSER_ARGS)
        .build()
    {
        Ok(w) => {
            dock_right_edge(&w, COLLAPSED_W, COLLAPSED_H);
            let _ = w.show();
            true
        }
        Err(e) => {
            log::info(&format!("не удалось открыть виджет: {e}"));
            false
        }
    }
}

pub fn toggle_widget(app: &AppHandle) -> bool {
    let open = if let Some(w) = app.get_webview_window("widget") {
        if crate::quick_files::is_drop_active() {
            let _ = w.show();
            let _ = w.set_focus();
            let _ = w.emit("widget-expand", ());
            true
        } else {
            let _ = w.close();
            false
        }
    } else {
        open_widget(app)
    };
    if let Some(state) = app.try_state::<Arc<AppState>>() {
        state.set_widget_enabled(open);
    }
    let _ = app.emit("widget-state", open);
    open
}

pub fn trigger_widget(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("widget") {
        let _ = w.show();
        let _ = w.set_focus();
        let _ = w.emit("widget-toggle", ());
    } else if toggle_widget(app) {
        if let Some(w) = app.get_webview_window("widget") {
            let _ = w.set_focus();
            let _ = w.emit("widget-expand", ());
        }
    }
}

pub fn resize_widget(app: &AppHandle, expanded: bool, height: Option<f64>, focus: bool) {
    if let Some(w) = app.get_webview_window("widget") {
        if expanded {
            let h = height
                .unwrap_or(EXPANDED_DEFAULT_H)
                .clamp(EXPANDED_MIN_H, EXPANDED_MAX_H);
            dock_right_edge(&w, EXPANDED_W, h);
            if focus {
                let _ = w.set_focus();
            }
        } else {
            dock_right_edge(&w, COLLAPSED_W, COLLAPSED_H);
        }
    }
}

const PET_W: f64 = 104.0;
const PET_H: f64 = 74.0;

pub fn pet_bounds(app: &AppHandle) -> (i32, i32, i32) {
    let Some(w) = app.get_webview_window("pet") else { return (0, 800, 500) };
    let Ok(Some(monitor)) = w.current_monitor().or_else(|_| w.primary_monitor()) else {
        return (0, 800, 500);
    };
    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    let min_x = area.position.x;
    let max_x = area.position.x + area.size.width as i32 - (PET_W * scale).round() as i32;
    let y = area.position.y + area.size.height as i32 - (PET_H * scale).round() as i32;
    (min_x, max_x.max(min_x + 1), y)
}

pub fn open_pet(app: &AppHandle) -> bool {
    if app.get_webview_window("pet").is_some() {
        return true;
    }
    match WebviewWindowBuilder::new(app, "pet", WebviewUrl::App("pet.html".into()))
        .title("Memory Log Pet")
        .inner_size(PET_W, PET_H)
        .transparent(true)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(false)
        .focused(false)
        .additional_browser_args(BROWSER_ARGS)
        .build()
    {
        Ok(w) => {
            let (min_x, max_x, y) = pet_bounds(app);
            let start_x = min_x + ((max_x - min_x) / 2);
            let _ = w.set_position(PhysicalPosition::new(start_x, y));
            let _ = w.show();
            true
        }
        Err(e) => {
            log::info(&format!("не удалось открыть питомца: {e}"));
            false
        }
    }
}

pub fn toggle_pet(app: &AppHandle) -> bool {
    let open = if let Some(w) = app.get_webview_window("pet") {
        let _ = w.close();
        false
    } else {
        open_pet(app)
    };
    if let Some(state) = app.try_state::<Arc<AppState>>() {
        state.set_pet_enabled(open);
    }
    let _ = app.emit("pet-state", open);
    open
}

pub fn move_pet(app: &AppHandle, x: i32, y: i32) {
    if let Some(w) = app.get_webview_window("pet") {
        let _ = w.set_position(PhysicalPosition::new(x, y));
    }
}
