use tauri::{AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, Size, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::log;

const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --in-process-gpu --renderer-process-limit=1";

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
const EXPANDED_W: f64 = 320.0;
const EXPANDED_H: f64 = 268.0;

fn dock_right_edge(w: &WebviewWindow, width: f64, height: f64) {
    let Ok(Some(monitor)) = w.current_monitor().or_else(|_| w.primary_monitor()) else { return };
    let scale = monitor.scale_factor();
    let size = monitor.size();
    let pos = monitor.position();
    let margin = if width > COLLAPSED_W { 10.0 } else { 0.0 };
    let x = pos.x as f64 + size.width as f64 - (width + margin) * scale;
    let y = pos.y as f64 + (size.height as f64 - height * scale) * 0.45;
    let _ = w.set_size(Size::Logical(LogicalSize::new(width, height)));
    let _ = w.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
}

pub fn toggle_widget(app: &AppHandle) -> bool {
    if let Some(w) = app.get_webview_window("widget") {
        let _ = w.close();
        return false;
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

pub fn resize_widget(app: &AppHandle, expanded: bool) {
    if let Some(w) = app.get_webview_window("widget") {
        if expanded {
            dock_right_edge(&w, EXPANDED_W, EXPANDED_H);
            let _ = w.set_focus();
        } else {
            dock_right_edge(&w, COLLAPSED_W, COLLAPSED_H);
        }
    }
}
