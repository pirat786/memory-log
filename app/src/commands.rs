use std::sync::{atomic::Ordering, Arc};

use ml_collector::LiveSystem;
use ml_core::{DaySummary, SearchHit, WeekDay};
use ml_core::model::{CategoryOverride, RecordingSettings};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt;

use crate::state::{AppState, Status};
use crate::cloud::{self, SyncStatus};
use crate::sys_tools::{CleanResult, JunkCategory, ListeningPort, PowerStatus, StartupItem};

type Shared<'a> = State<'a, Arc<AppState>>;

#[tauri::command]
pub async fn day_summary(state: Shared<'_>, date: Option<String>) -> Result<DaySummary, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.day_summary(date.as_deref()))
        .await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn week_overview(state: Shared<'_>) -> Result<Vec<WeekDay>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.week_overview())
        .await.map_err(|e| e.to_string())?
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
pub async fn calendar_heatmap(state: Shared<'_>, month: String, device: Option<String>) -> Result<Vec<WeekDay>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.calendar_heatmap(&month, device.as_deref()))
        .await.map_err(|e| e.to_string())?
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
pub async fn sync_remove_peer(state: Shared<'_>, device: String) -> Result<(), String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || cloud::remove_peer(&state, &device))
        .await.map_err(|e| e.to_string())?
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
pub async fn search(state: Shared<'_>, query: String) -> Result<Vec<SearchHit>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.search(&query))
        .await.map_err(|e| e.to_string())?
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
pub async fn clip_images_for_day(
    state: Shared<'_>,
    date: Option<String>,
) -> Result<Vec<crate::clip_images::ClipImageItem>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.clip_images_for_day(date.as_deref()))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn clip_image_preview(state: Shared<'_>, id: i64) -> Result<Option<String>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.clip_image_preview(id))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn copy_clip_image(state: Shared<'_>, id: i64) -> Result<(), String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.copy_clip_image(id))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn delete_clip_image(state: Shared, id: i64) -> Result<(), String> {
    state.delete_clip_image(id)
}

#[tauri::command]
pub async fn search_clipboard(
    state: Shared<'_>,
    query: String,
) -> Result<crate::state::ClipboardSearchResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.search_clipboard(&query))
        .await
        .map_err(|e| e.to_string())?
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

#[tauri::command]
pub async fn live_system(state: Shared<'_>) -> Result<LiveSystem, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = state.live_system.lock().map_err(|e| e.to_string())?;
        let first = guard.is_none();
        let probe = guard.get_or_insert_with(ml_collector::LiveSystemProbe::new);
        if first {
            std::thread::sleep(std::time::Duration::from_millis(180));
        }
        Ok(probe.snapshot())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn kill_process(state: Shared<'_>, name: String) -> Result<usize, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = state.live_system.lock().map_err(|e| e.to_string())?;
        let probe = guard.get_or_insert_with(ml_collector::LiveSystemProbe::new);
        probe.kill_process(&name)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn scan_junk(min_age_hours: u64) -> Result<Vec<JunkCategory>, String> {
    tauri::async_runtime::spawn_blocking(move || crate::sys_tools::scan_junk(min_age_hours))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn clean_junk(ids: Vec<String>, min_age_hours: u64) -> Result<CleanResult, String> {
    tauri::async_runtime::spawn_blocking(move || crate::sys_tools::clean_junk(&ids, min_age_hours))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn startup_items() -> Result<Vec<StartupItem>, String> {
    tauri::async_runtime::spawn_blocking(crate::sys_tools::startup_items)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn set_startup_item(id: String, enabled: bool) -> Result<(), String> {
    crate::sys_tools::set_startup_item(&id, enabled)
}

#[tauri::command]
pub async fn listening_ports(state: Shared<'_>) -> Result<Vec<ListeningPort>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = state.live_system.lock().map_err(|e| e.to_string())?;
        let probe = guard.get_or_insert_with(ml_collector::LiveSystemProbe::new);
        let names = probe.process_names();
        crate::sys_tools::listening_ports(&names)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn reveal_path(path: String) -> Result<(), String> {
    crate::sys_tools::reveal_in_explorer(&path)
}

#[tauri::command]
pub fn power_status(state: Shared) -> Result<PowerStatus, String> {
    let guard = state.power_guard.lock().map_err(|e| e.to_string())?;
    Ok(guard.status(chrono::Utc::now().timestamp()))
}

#[tauri::command]
pub fn set_keep_awake(state: Shared, enabled: bool) -> Result<bool, String> {
    let mut guard = state.power_guard.lock().map_err(|e| e.to_string())?;
    ml_collector::set_keep_awake(enabled);
    guard.keep_awake = enabled;
    Ok(enabled)
}

#[tauri::command]
pub fn set_power_rule(state: Shared, condition: String, action: String, delay_mins: u64) -> Result<PowerStatus, String> {
    let mut guard = state.power_guard.lock().map_err(|e| e.to_string())?;
    let now = chrono::Utc::now().timestamp();
    guard.set_rule(condition, action, delay_mins, now)?;
    Ok(guard.status(now))
}

#[tauri::command]
pub fn cancel_power_rule(state: Shared) -> Result<PowerStatus, String> {
    let mut guard = state.power_guard.lock().map_err(|e| e.to_string())?;
    guard.cancel_rule();
    Ok(guard.status(chrono::Utc::now().timestamp()))
}

#[tauri::command]
pub async fn toggle_widget(app: AppHandle) -> bool {
    crate::window::toggle_widget(&app)
}

#[tauri::command]
pub fn widget_open(app: AppHandle) -> bool {
    app.get_webview_window("widget").is_some()
}

#[tauri::command]
pub fn widget_resize(app: AppHandle, expanded: bool) {
    crate::window::resize_widget(&app, expanded);
}

#[tauri::command]
pub async fn toggle_pet(app: AppHandle) -> bool {
    crate::window::toggle_pet(&app)
}

#[tauri::command]
pub fn pet_open(app: AppHandle) -> bool {
    app.get_webview_window("pet").is_some()
}

#[tauri::command]
pub fn pet_bounds(app: AppHandle) -> (i32, i32, i32) {
    crate::window::pet_bounds(&app)
}

#[tauri::command]
pub fn pet_move(app: AppHandle, x: i32, y: i32) {
    crate::window::move_pet(&app, x, y);
}

#[tauri::command]
pub fn get_layout_fix(state: Shared) -> bool {
    state.layout_fix_enabled()
}

#[tauri::command]
pub fn set_layout_fix(state: Shared, enabled: bool) -> bool {
    state.set_layout_fix_enabled(enabled);
    enabled
}

#[tauri::command]
pub async fn quick_drop(
    app: AppHandle,
    paths: Vec<String>,
    zip_mode: Option<String>,
    delete_after: Option<bool>,
) -> Result<crate::quick_files::QuickDropResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::quick_files::handle_archive_drop(
            paths,
            zip_mode.as_deref().unwrap_or("fast"),
            delete_after.unwrap_or(false),
            Some(&app),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn quick_drop_cancel() {
    crate::quick_files::cancel_drop();
}
