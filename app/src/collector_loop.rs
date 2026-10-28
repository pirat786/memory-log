use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use ml_collector::{ClipboardWatcher, SystemCollector};
use ml_core::privacy::{looks_like_secret, HIDDEN_APP};
use ml_core::{ClipEntry, PrivacyFilter, Sample};

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
        let mut last_window: Option<Sample> = None;
        let privacy = PrivacyFilter::default();
        // Instant отсчитывается от запуска Windows, поэтому вычитание из него паникует,
        // если ПК включён недавно. Берём «давно» через checked_sub.
        let long_ago = |d: Duration| Instant::now().checked_sub(d).unwrap_or_else(Instant::now);
        let mut last_system = long_ago(SYSTEM_TICK);
        let mut last_prune = long_ago(PRUNE_EVERY);
        let mut errors = 0u32;
        log::info("сборщик запущен");

        let mut last_tick = Instant::now();
        loop {
            // Если поток «проспал» намного дольше обычного (сон ПК, троттлинг Windows), пишем это в журнал.
            let slept = last_tick.elapsed();
            last_tick = Instant::now();
            if slept > Duration::from_secs(30) {
                log::info(&format!("сборщик не работал {} сек (сон ПК или замедление Windows)", slept.as_secs()));
            }
            if last_prune.elapsed() >= PRUNE_EVERY {
                last_prune = Instant::now();
                let now = Utc::now().timestamp();
                if let Ok(st) = state.storage.lock() {
                    if let Err(e) = st.prune(now - KEEP_SYSTEM_DAYS * 86400, now - KEEP_PROCS_DAYS * 86400) {
                        log::info(&format!("не удалось почистить старые данные: {e}"));
                    }
                }
            }

            // Буфер опрашиваем и на паузе, чтобы скопированное во время паузы не записалось после неё.
            let copied = clipboard.poll(state.clip_ignore.load(std::sync::atomic::Ordering::Relaxed));
            if !state.is_paused() {
                let mut result = Ok(());
                if let Some(sample) = windows.sample() {
                    let sample = privacy.apply(sample);
                    last_window = Some(sample.clone());
                    result = state
                        .storage
                        .lock()
                        .map_err(|e| e.to_string())
                        .and_then(|st| st.insert_sample(&sample).map_err(|e| e.to_string()));
                }
                if let (Some(text), true) = (copied, result.is_ok()) {
                    // Окно, где копировали, считаем скрытым по тем же правилам, что и заголовки: текст из него не пишем.
                    let app = last_window.as_ref().map_or_else(String::new, |w| w.app.clone());
                    if app != HIDDEN_APP && !looks_like_secret(&text) {
                        let entry = ClipEntry { ts: Utc::now().timestamp(), app, text };
                        result = state
                            .storage
                            .lock()
                            .map_err(|e| e.to_string())
                            .and_then(|st| st.insert_clip(&entry).map_err(|e| e.to_string()));
                    }
                }
                if result.is_ok() && last_system.elapsed() >= SYSTEM_TICK {
                    last_system = Instant::now();
                    let (sample, procs) = system.snapshot();
                    result = state
                        .storage
                        .lock()
                        .map_err(|e| e.to_string())
                        .and_then(|st| st.insert_system(&sample, &procs).map_err(|e| e.to_string()));
                }
                match result {
                    Ok(()) => errors = 0,
                    Err(e) => {
                        if e.contains("malformed") || e.contains("not a database") {
                            state.recover_database();
                        }
                        errors += 1;
                        if errors <= MAX_LOGGED_ERRORS {
                            log::info(&format!("ошибка записи: {e}"));
                        }
                    }
                }
            }
            thread::sleep(WINDOW_TICK);
        }
    });
}
