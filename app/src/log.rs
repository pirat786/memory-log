use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

use chrono::Local;

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
/// Когда файл журнала больше этого размера, он обнуляется.
const MAX_BYTES: u64 = 512 * 1024;

pub fn init(path: PathBuf) {
    if std::fs::metadata(&path).map_or(false, |m| m.len() > MAX_BYTES) {
        let _ = std::fs::remove_file(&path);
    }
    let _ = LOG_PATH.set(path);
}

/// Пишет строку в журнал рядом с базой. Ошибки записи игнорируются.
pub fn info(msg: &str) {
    let Some(path) = LOG_PATH.get() else { return };
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{}] {msg}", Local::now().format("%Y-%m-%d %H:%M:%S"));
    }
}
