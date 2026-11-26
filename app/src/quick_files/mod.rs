pub mod images;
pub mod io_hints;
pub mod zip_ops;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use walkdir::WalkDir;

pub(crate) const IO_BUF_SIZE: usize = 2 * 1024 * 1024;

static CANCEL_DROP: AtomicBool = AtomicBool::new(false);

pub fn cancel_drop() {
    CANCEL_DROP.store(true, Ordering::Relaxed);
}

pub(crate) fn is_cancelled() -> bool {
    CANCEL_DROP.load(Ordering::Relaxed)
}

#[derive(Debug, Clone, Serialize)]
pub struct QuickFileReport {
    pub kind: String,
    pub output_path: String,
    pub count: usize,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub elapsed_ms: u128,
    pub detail: String,
    pub deleted_original: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DropProgress {
    pub done: u64,
    pub total: u64,
    pub pct: u8,
}

pub(crate) struct ProgressReporter<'a> {
    app: Option<&'a AppHandle>,
    total: u64,
    done: u64,
    last_emit: Instant,
    last_pct: u8,
}

impl<'a> ProgressReporter<'a> {
    pub(crate) fn new(app: Option<&'a AppHandle>, total: u64) -> Self {
        let mut r = Self {
            app,
            total,
            done: 0,
            last_emit: Instant::now(),
            last_pct: 255,
        };
        r.emit(true);
        r
    }

    pub(crate) fn add(&mut self, bytes: u64) {
        self.done = self.done.saturating_add(bytes);
        self.emit(false);
    }

    fn emit(&mut self, force: bool) {
        let Some(app) = self.app else { return };
        let pct = if self.total == 0 {
            0
        } else {
            ((self.done.min(self.total) * 100) / self.total) as u8
        };
        if !force && pct == self.last_pct && self.last_emit.elapsed().as_millis() < 80 {
            return;
        }
        self.last_pct = pct;
        self.last_emit = Instant::now();
        let _ = app.emit(
            "quick-drop-progress",
            DropProgress {
                done: self.done,
                total: self.total,
                pct,
            },
        );
    }
}

pub(crate) fn ext_lower(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default()
}

pub(crate) fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let dot_ext = if ext.is_empty() {
        String::new()
    } else {
        format!(".{ext}")
    };
    let candidate = dir.join(format!("{stem}{dot_ext}"));
    if !candidate.exists() {
        return candidate;
    }
    for i in 2..=9999 {
        let next = dir.join(format!("{stem} ({i}){dot_ext}"));
        if !next.exists() {
            return next;
        }
    }
    dir.join(format!("{stem}_{}{dot_ext}", uuid::Uuid::new_v4()))
}

#[cfg(test)]
pub use images::process_images;
#[cfg(test)]
pub use zip_ops::{pack_zip, unpack_zip};

pub fn handle_drop(
    paths: Vec<String>,
    mode: &str,
    max_dim: u32,
    quality: u8,
    zip_mode: &str,
    delete_after: bool,
    app: Option<&AppHandle>,
) -> Result<QuickFileReport, String> {
    CANCEL_DROP.store(false, Ordering::Relaxed);
    if paths.is_empty() {
        return Err("список файлов пуст".into());
    }
    if mode == "zip" {
        return pack_and_maybe_delete(paths, zip_mode, delete_after, app);
    }
    if matches!(mode, "compress" | "webp" | "jpg" | "jpeg" | "strip") {
        return images::process_images(paths, mode, max_dim, quality, app);
    }
    if paths.len() == 1 {
        let p = Path::new(&paths[0]);
        if p.is_file() && ext_lower(p) == "zip" && mode == "auto" {
            return zip_ops::unpack_zip(paths[0].clone(), app);
        }
    }
    let all_images = paths.iter().all(|raw| {
        let p = Path::new(raw);
        (p.is_file() && images::is_image_ext(&ext_lower(p)))
            || (p.is_dir() && images::dir_has_only_images(p))
    });
    if all_images {
        let img_mode = if mode == "auto" { "compress" } else { mode };
        return images::process_images(paths, img_mode, max_dim, quality, app);
    }
    pack_and_maybe_delete(paths, zip_mode, delete_after, app)
}

fn pack_and_maybe_delete(
    paths: Vec<String>,
    zip_mode: &str,
    delete_after: bool,
    app: Option<&AppHandle>,
) -> Result<QuickFileReport, String> {
    let orig_paths = if delete_after { Some(paths.clone()) } else { None };
    let mut rep = zip_ops::pack_zip(paths, zip_mode, app)?;
    if let Some(orig) = orig_paths {
        if rep.kind == "zip-pack"
            && rep.output_bytes > 0
            && Path::new(&rep.output_path).is_file()
            && !is_cancelled()
        {
            if let Ok(del) = fast_delete(orig, None) {
                rep.deleted_original = true;
                crate::log::info(&format!(
                    "zip-pack deleted original: files={}, bytes={}",
                    del.count, del.input_bytes
                ));
            }
        }
    }
    Ok(rep)
}

fn is_unsafe_delete_target(p: &Path) -> bool {
    let norm = fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    if norm.parent().is_none() {
        return true;
    }
    let s = norm.to_string_lossy().to_ascii_lowercase();
    let trimmed = s.trim_start_matches(r"\\?\").trim_end_matches('\\');
    if trimmed.len() <= 3 {
        return true;
    }
    for env_key in ["USERPROFILE", "SystemRoot", "ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(val) = std::env::var_os(env_key) {
            let env_norm = val.to_string_lossy().to_ascii_lowercase();
            if trimmed == env_norm.trim_end_matches('\\') {
                return true;
            }
        }
    }
    matches!(trimmed, r"c:\users" | r"c:\windows")
}

pub fn fast_delete(paths: Vec<String>, app: Option<&AppHandle>) -> Result<QuickFileReport, String> {
    CANCEL_DROP.store(false, Ordering::Relaxed);
    let started = Instant::now();
    if paths.is_empty() {
        return Err("нечего удалять".into());
    }

    let targets: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    for p in &targets {
        if p.exists() && is_unsafe_delete_target(p) {
            return Err(format!("защищённый путь: {}", p.display()));
        }
    }

    let mut files: Vec<(PathBuf, u64)> = Vec::new();
    let mut dirs: Vec<PathBuf> = Vec::new();

    for p in &targets {
        if is_cancelled() {
            return Err("отменено".into());
        }
        if p.is_file() {
            let len = fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            files.push((p.clone(), len));
        } else if p.is_dir() {
            for entry in WalkDir::new(p).contents_first(true).into_iter().flatten() {
                if is_cancelled() {
                    return Err("отменено".into());
                }
                let ep = entry.path();
                if entry.file_type().is_dir() {
                    dirs.push(ep.to_path_buf());
                } else {
                    let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    files.push((ep.to_path_buf(), len));
                }
            }
        }
    }

    let total_bytes: u64 = files.iter().map(|(_, len)| *len).sum();
    let mut progress = ProgressReporter::new(app, total_bytes);
    let deleted_files = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism()
        .map(|n| n.get().clamp(2, 8))
        .unwrap_or(4);

    if files.len() > 32 {
        let next_idx = AtomicUsize::new(0);
        let total_files = files.len();
        let (tx, rx) = mpsc::channel::<u64>();
        std::thread::scope(|s| {
            for _ in 0..workers {
                let files_ref = &files;
                let idx_ref = &next_idx;
                let del_ref = &deleted_files;
                let tx_ref = tx.clone();
                s.spawn(move || loop {
                    if is_cancelled() {
                        break;
                    }
                    let i = idx_ref.fetch_add(1, Ordering::Relaxed);
                    if i >= total_files {
                        break;
                    }
                    let (ref path, len) = files_ref[i];
                    if fs::remove_file(path).is_ok() {
                        del_ref.fetch_add(1, Ordering::Relaxed);
                        let _ = tx_ref.send(len);
                    }
                });
            }
            drop(tx);
            while let Ok(bytes) = rx.recv() {
                progress.add(bytes);
            }
        });
    } else {
        for (path, len) in &files {
            if is_cancelled() {
                return Err("отменено".into());
            }
            if fs::remove_file(path).is_ok() {
                deleted_files.fetch_add(1, Ordering::Relaxed);
                progress.add(*len);
            }
        }
    }

    if is_cancelled() {
        return Err("отменено".into());
    }

    for d in &dirs {
        let _ = fs::remove_dir(d);
    }
    for p in &targets {
        if p.is_dir() && p.exists() {
            let _ = fs::remove_dir_all(p);
        }
    }

    let count = deleted_files.load(Ordering::Relaxed);
    Ok(QuickFileReport {
        kind: "delete".into(),
        output_path: String::new(),
        count,
        input_bytes: total_bytes,
        output_bytes: 0,
        elapsed_ms: started.elapsed().as_millis(),
        detail: format!("{count} f"),
        deleted_original: false,
    })
}
