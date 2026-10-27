use std::sync::Arc;
use std::thread;
use std::time::Duration;

use ml_core::PrivacyFilter;

use crate::state::AppState;

const TICK: Duration = Duration::from_secs(3);

/// Фоновый поток: раз в несколько секунд снимает активное окно и пишет в базу.
pub fn spawn(state: Arc<AppState>) {
    thread::spawn(move || {
        let mut collector = ml_collector::default_collector();
        let privacy = PrivacyFilter::default();
        loop {
            if !state.is_paused() {
                if let Some(sample) = collector.sample() {
                    let sample = privacy.apply(sample);
                    if let Ok(storage) = state.storage.lock() {
                        let _ = storage.insert_sample(&sample);
                    }
                }
            }
            thread::sleep(TICK);
        }
    });
}
