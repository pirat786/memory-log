use std::sync::{atomic::Ordering, Arc};

use ml_core::{DaySummary, SearchHit, WeekDay};
use ml_core::model::{CategoryOverride, RecordingSettings};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt;

use crate::state::{AppState, Status};
use crate::cloud::{self, SyncStatus};

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
pub async fn week_comparison(state: Shared<'_>, work_hours: Option<[u32; 2]>) -> Result<crate::week_comparison::WeekComparison, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || crate::week_comparison::compare(&state, work_hours))
        .await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn recorded_days(state: Shared, month: String, device: Option<String>) -> Result<Vec<String>, String> {
    state.recorded_days(&month, device.as_deref())
}

#[tauri::command]
pub fn sync_status(state: Shared) -> Result<SyncStatus, String> {
    cloud::status(&state)
}

#[tauri::command]
pub async fn sync_connect(state: Shared<'_>, label: String, include_history: bool) -> Result<(), String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || cloud::connect(&state, label, include_history))
        .await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn sync_disconnect(state: Shared<'_>) -> Result<(), String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || cloud::disconnect(&state)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn sync_rename(state: Shared, label: String) -> Result<(), String> {
    cloud::rename(&state, &label)
}

#[tauri::command]
pub fn sync_now(state: Shared) {
    state.sync.wake();
}

#[tauri::command]
pub async fn sync_device_days(state: Shared<'_>, date: String, device: Option<String>) -> Result<Vec<crate::state::DeviceDay>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.synced_days(&date, device.as_deref())).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn sync_search(state: Shared<'_>, query: String, device: Option<String>) -> Result<Vec<ml_storage::sync::PeerHit>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.sync_search(&query, device.as_deref())).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn recording_settings(state: Shared) -> Result<RecordingSettings, String> {
    state.recording_settings()
}

#[tauri::command]
pub fn set_recording_settings(state: Shared, settings: RecordingSettings) -> Result<(), String> {
    state.set_recording_settings(settings)
}

#[tauri::command]
pub fn delete_days(state: Shared, from: String, through: String) -> Result<(), String> {
    state.delete_days(&from, &through)
}

#[tauri::command]
pub fn category_rules(state: Shared) -> Result<Vec<CategoryOverride>, String> {
    state.category_rules()
}

#[tauri::command]
pub fn set_category_override(state: Shared, change: CategoryOverride) -> Result<(), String> {
    state.set_category_override(change)
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
pub fn set_paused(state: Shared, paused: bool) -> Result<(), String> {
    state.set_paused(paused)
}

#[tauri::command]
pub fn get_idle_minutes(state: Shared) -> u32 {
    state.idle_minutes.load(Ordering::Relaxed)
}

#[tauri::command]
pub fn set_idle_minutes(state: Shared, minutes: u32) -> Result<(), String> {
    state.set_idle_minutes(minutes)
}

#[tauri::command]
pub fn get_language(state: Shared) -> String {
    state.lang()
}

#[tauri::command]
pub fn set_language(state: Shared, code: String) -> Result<(), String> {
    state.set_language(&code)
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
