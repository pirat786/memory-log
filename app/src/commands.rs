use std::sync::Arc;

use ml_core::DaySummary;
use tauri::State;

use crate::state::AppState;

type Shared<'a> = State<'a, Arc<AppState>>;

#[tauri::command]
pub fn day_summary(state: Shared, date: Option<String>) -> Result<DaySummary, String> {
    state.day_summary(date.as_deref())
}

#[tauri::command]
pub fn add_bookmark(state: Shared, note: String) -> Result<(), String> {
    state.add_bookmark(&note)
}

#[tauri::command]
pub fn set_paused(state: Shared, paused: bool) {
    state.set_paused(paused);
}

#[tauri::command]
pub fn is_paused(state: Shared) -> bool {
    state.is_paused()
}
