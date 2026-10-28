use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition};

use crate::log;
use crate::state::AppState;

/// Сколько записей берём для листания.
const RECENT: usize = 20;
/// Сколько плашка висит после последнего нажатия. Столько же держится «текущий выбор».
const SHOW_FOR: Duration = Duration::from_millis(2500);
/// Размер плашки (логические пиксели) и отступ от низа экрана под панель задач.
const POPUP_W: f64 = 380.0;
const POPUP_H: f64 = 250.0;
const MARGIN_RIGHT: f64 = 16.0;
const MARGIN_BOTTOM: f64 = 64.0;
/// Длинный текст в плашке обрезается.
const PREVIEW_CHARS: usize = 110;

fn preview(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > PREVIEW_CHARS {
        format!("{}…", one_line.chars().take(PREVIEW_CHARS).collect::<String>())
    } else {
        one_line
    }
}

fn place_popup(win: &tauri::WebviewWindow) {
    let Ok(Some(monitor)) = win.current_monitor().or_else(|_| win.primary_monitor()) else { return };
    let scale = monitor.scale_factor();
    let size = monitor.size();
    let pos = monitor.position();
    let x = pos.x as f64 + size.width as f64 - (POPUP_W + MARGIN_RIGHT) * scale;
    let y = pos.y as f64 + size.height as f64 - (POPUP_H + MARGIN_BOTTOM) * scale;
    let _ = win.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
}

/// Горячая клавиша: `delta` = +1 — к более старой записи, −1 — к более новой.
/// Выбранный текст сразу кладётся в буфер обмена, дальше его вставляют обычным Ctrl+V.
pub fn step(app: &AppHandle, state: &Arc<AppState>, delta: i32) {
    let clips = match state.storage.lock().map_err(|e| e.to_string()).and_then(|st| st.recent_clips(RECENT).map_err(|e| e.to_string())) {
        Ok(c) => c,
        Err(e) => return log::info(&format!("листание буфера: {e}")),
    };

    let (sel, generation) = {
        let Ok(mut nav) = state.clip_nav.lock() else { return };
        let active = nav.last.is_some_and(|t| t.elapsed() < SHOW_FOR);
        let from = if active { nav.sel } else { 0 };
        let max = clips.len().saturating_sub(1) as i64;
        nav.sel = (from as i64 + delta as i64).clamp(0, max) as usize;
        nav.last = Some(Instant::now());
        nav.generation += 1;
        (nav.sel, nav.generation)
    };

    if let Some(clip) = clips.get(sel) {
        if let Some(seq) = ml_collector::set_clipboard_text(&clip.text) {
            state.clip_ignore.store(seq, Ordering::Relaxed);
        }
    }

    let Some(win) = app.get_webview_window("popup") else { return };
    let items: Vec<_> = clips.iter().map(|c| json!({ "app": c.app, "text": preview(&c.text) })).collect();
    place_popup(&win);
    let _ = win.emit("clip-nav", json!({ "items": items, "sel": sel }));
    let _ = win.show();

    let app = app.clone();
    let state = state.clone();
    std::thread::spawn(move || {
        std::thread::sleep(SHOW_FOR);
        let still_latest = state.clip_nav.lock().map(|n| n.generation == generation).unwrap_or(true);
        if still_latest {
            if let Some(win) = app.get_webview_window("popup") {
                let _ = win.hide();
            }
        }
    });
}
