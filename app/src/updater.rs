use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, State};
use tauri_plugin_updater::{Update, UpdaterExt};

#[derive(Default)]
pub struct PendingUpdate(Mutex<Option<Update>>);

#[tauri::command]
pub fn updater_configured(app: AppHandle) -> bool {
    app.config().plugins.0.contains_key("updater")
}

#[tauri::command]
pub async fn check_update(app: AppHandle, pending: State<'_, PendingUpdate>) -> Result<Option<String>, String> {
    let update = app.updater_builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| error.to_string())?
        .check()
        .await
        .map_err(|error| error.to_string())?;
    let version = update.as_ref().map(|update| update.version.clone());
    *pending.0.lock().map_err(|error| error.to_string())? = update;
    Ok(version)
}

#[tauri::command]
pub async fn install_update(version: String, pending: State<'_, PendingUpdate>) -> Result<(), String> {
    let update = {
        let mut pending = pending.0.lock().map_err(|error| error.to_string())?;
        if pending.as_ref().is_none_or(|update| update.version != version) {
            return Err("update.unavailable".into());
        }
        pending.take().ok_or_else(|| "update.unavailable".to_string())?
    };

    // On Windows the plugin verifies the signature, launches the installer, and exits the app.
    update.download_and_install(|_, _| {}, || {}).await.map_err(|error| error.to_string())
}
