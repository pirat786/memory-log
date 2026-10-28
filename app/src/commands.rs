use std::sync::Arc;

use ml_core::{DaySummary, SearchHit, WeekDay};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt;

use crate::state::{AppState, Status};

type Shared<'a> = State<'a, Arc<AppState>>;

#[tauri::command]
pub fn day_summary(state: Shared, date: Option<String>) -> Result<DaySummary, String> {
    state.day_summary(date.as_deref())
}

#[tauri::command]
pub fn week_overview(state: Shared) -> Result<Vec<WeekDay>, String> {
    state.week_overview()
}

#[tauri::command]
pub fn search(state: Shared, query: String) -> Result<Vec<SearchHit>, String> {
    state.search(&query)
}

#[tauri::command]
pub fn add_bookmark(state: Shared, note: String) -> Result<(), String> {
    state.add_bookmark(&note)
}

#[tauri::command]
pub fn set_goal(state: Shared, category: String, limit_mins: u32) -> Result<(), String> {
    state.set_goal(&category, limit_mins)
}

#[tauri::command]
pub fn forget_minutes(state: Shared, minutes: u32) -> Result<(), String> {
    state.forget_minutes(minutes)
}

#[tauri::command]
pub fn set_paused(state: Shared, paused: bool) {
    state.set_paused(paused);
}

#[tauri::command]
pub fn delete_clip(state: Shared, text: String) -> Result<(), String> {
    state.delete_clip(&text)
}

#[tauri::command]
pub fn clear_clips(state: Shared) -> Result<(), String> {
    state.clear_clips()
}

#[tauri::command]
pub fn clip_nav_state(state: Shared) -> Option<serde_json::Value> {
    state.clip_payload.lock().ok().and_then(|p| p.clone())
}

#[tauri::command]
pub fn status(state: Shared) -> Result<Status, String> {
    state.status()
}

#[tauri::command]
pub fn export_day_csv(app: AppHandle, state: Shared, date: Option<String>) -> Result<String, String> {
    let dir = app.path().document_dir().map_err(|e| e.to_string())?.join("MemoryLog");
    state.export_day_csv(date.as_deref(), &dir)
}

#[tauri::command]
pub fn get_autostart(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
pub fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    let launcher = app.autolaunch();
    if enabled { launcher.enable() } else { launcher.disable() }.map_err(|e| e.to_string())
}
