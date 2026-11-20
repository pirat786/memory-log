use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use ml_collector::{ClipboardWatcher, SystemCollector};
use ml_core::privacy::{looks_like_secret, HIDDEN_APP};
use ml_core::{ClipEntry, PrivacyFilter};

use crate::log;
use crate::state::AppState;

/// Как часто снимаем активное окно.
const WINDOW_TICK: Duration = Duration::from_secs(3);
/// Как часто снимаем нагрузку, сеть и батарею.
const SYSTEM_TICK: Duration = Duration::from_secs(10);
/// Как часто чистим старые подробные данные.
const PRUNE_EVERY: Duration = Duration::from_secs(6 * 3600);
const KEEP_SYSTEM_DAYS: i64 = 180;
const KEEP_PROCS_DAYS: i64 = 30;
/// Сколько подряд ошибок записи подряд не засоряют журнал.
const MAX_LOGGED_ERRORS: u32 = 20;

/// Фоновый поток: пишет активное окно и состояние ПК в базу.
pub fn spawn(state: Arc<AppState>) {
    thread::spawn(move || {
        ml_collector::disable_power_throttling();
        let mut windows = ml_collector::default_collector();
        let mut system = SystemCollector::new();
        let mut clipboard = ClipboardWatcher::new();
        let mut break_recorded = false;
        let mut last_window_app = String::new();
        let privacy = PrivacyFilter::default();
        // Instant отсчитывается от запуска Windows, поэтому вычитание из него паникует,
        // если ПК включён недавно. Берём «давно» через checked_sub.
        let long_ago = |d: Duration| Instant::now().checked_sub(d).unwrap_or_else(Instant::now);
        let mut last_system = long_ago(SYSTEM_TICK);
        let mut last_prune: Option<Instant> = None;
        let mut last_retention = 0;
        let mut clipboard_config_version = 0;
        let mut errors = 0u32;
        log::info("сборщик запущен");

        let mut last_tick = Instant::now();
        loop {
            // Если поток «проспал» намного дольше обычного (сон ПК, троттлинг Windows), пишем это в журнал.
            let slept = last_tick.elapsed();
            last_tick = Instant::now();
            let interrupted = slept > Duration::from_secs(30);
            if interrupted {
                log::info(&format!("сборщик не работал {} сек (сон ПК или замедление Windows)", slept.as_secs()));
            }
            let mut result = (|| -> Result<(), String> {
                // Один замок для записи окна и постановки на паузу сохраняет порядок событий.
                let storage = state.storage.lock().map_err(|e| e.to_string())?;
                let settings = storage.recording_settings().map_err(|e| e.to_string())?;
                if last_prune.is_none_or(|last| last.elapsed() >= PRUNE_EVERY) || last_retention != settings.retention_days {
                    let now = Utc::now().timestamp();
                    if settings.retention_days > 0 {
                        let cutoff = now - i64::from(settings.retention_days) * 86400;
                        storage.delete_between(i64::MIN, cutoff).map_err(|e| e.to_string())?;
                    }
                    storage.prune(now - KEEP_SYSTEM_DAYS * 86400, now - KEEP_PROCS_DAYS * 86400)
                        .map_err(|e| e.to_string())?;
                    last_prune = Some(Instant::now());
                    last_retention = settings.retention_days;
                }
                let config_version = state.clipboard_config_version.load(Ordering::Relaxed);
                let clipboard_changed = config_version != clipboard_config_version;
                clipboard_config_version = config_version;
                if state.is_paused() {
                    clipboard.skip();
                    return Ok(());
                }
                let interactive = ml_collector::interactive_desktop();
                let sample = if interactive { windows.sample() } else { None };
                let excluded = sample.as_ref().is_some_and(|sample| settings.excludes(&sample.app));
                let copied = if settings.clipboard_enabled && !clipboard_changed && interactive && !excluded {
                    clipboard.poll(state.clip_ignore.load(Ordering::Relaxed))
                } else {
                    clipboard.skip();
                    None
                };
                let sample = sample.map(|sample| privacy.apply(sample));
                let threshold = state.idle_minutes.load(Ordering::Relaxed) * 60;
                let idle = interactive && sample.as_ref().map_or_else(
                    || ml_collector::idle_seconds().unwrap_or_default(),
                    |s| s.idle_secs,
                ) >= threshold;
                let ts = sample.as_ref().map_or_else(|| Utc::now().timestamp(), |s| s.ts);
                let inactive = !interactive || excluded || idle;
                state.inactive.store(inactive, Ordering::Relaxed);
                if (!interactive || excluded || interrupted) && !break_recorded {
                    storage.insert_activity_break(ts).map_err(|e| e.to_string())?;
                    break_recorded = true;
                }
                if !interactive || excluded {
                    last_window_app.clear();
                    return Ok(());
                }
                if idle {
                    storage.insert_activity_idle(ts).map_err(|e| e.to_string())?;
                    break_recorded = false;
                }
                if let Some(sample) = sample {
                    if !idle {
                        storage.insert_sample(&sample).map_err(|e| e.to_string())?;
                        break_recorded = false;
                    }
                    last_window_app = sample.app;
                }
                if let Some(capture) = copied {
                    let known_source = capture.owner_app.is_some() || settings.excluded_apps.is_empty();
                    let app = capture.owner_app.unwrap_or_else(|| last_window_app.clone());
                    let source = privacy.apply(ml_core::Sample { ts, app, title: String::new(), idle_secs: 0 });
                    if known_source && last_window_app != HIDDEN_APP && source.app != HIDDEN_APP
                        && !settings.excludes(&source.app) && !looks_like_secret(&capture.text)
                    {
                        let entry = ClipEntry { ts, app: source.app, text: capture.text };
                        storage.insert_clip(&entry).map_err(|e| e.to_string())?;
                    }
                }
                Ok(())
            })();
            if result.is_ok() && !state.is_paused() && last_system.elapsed() >= SYSTEM_TICK {
                last_system = Instant::now();
                let (sample, mut procs) = system.snapshot();
                result = state.storage.lock().map_err(|e| e.to_string()).and_then(|st| {
                    if state.is_paused() {
                        return Ok(());
                    }
                    let settings = st.recording_settings().map_err(|e| e.to_string())?;
                    procs.retain(|proc| !settings.excludes(&proc.name));
                    st.insert_system(&sample, &procs).map_err(|e| e.to_string())
                });
            }
            match result {
                Ok(()) => errors = 0,
                Err(e) => {
                    if e.contains("malformed") || e.contains("not a database") {
                        state.recover_database();
                        break_recorded = false;
                    }
                    errors += 1;
                    if errors <= MAX_LOGGED_ERRORS {
                        log::info(&format!("ошибка записи: {e}"));
                    }
                }
            }
            thread::sleep(WINDOW_TICK);
        }
    });
}
