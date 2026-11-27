pub mod io_hints;
mod jxl;
pub mod zip_ops;

use std::fs;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

static CANCEL_DROP: AtomicBool = AtomicBool::new(false);
static ACTIVE_DROPS: AtomicUsize = AtomicUsize::new(0);

struct ActiveDropGuard;

impl ActiveDropGuard {
    fn enter() -> Self {
        ACTIVE_DROPS.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for ActiveDropGuard {
    fn drop(&mut self) {
        ACTIVE_DROPS.fetch_sub(1, Ordering::AcqRel);
    }
}

pub fn is_drop_active() -> bool {
    ACTIVE_DROPS.load(Ordering::Acquire) > 0
}

pub fn cancel_and_wait_drop(timeout: Duration) {
    if !is_drop_active() {
        return;
    }
    cancel_drop();
    crate::log::info("ожидание отмены архивации перед выходом");
    let started = Instant::now();
    while is_drop_active() && started.elapsed() < timeout {
        std::thread::sleep(Duration::from_millis(25));
    }
}

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

#[derive(Default, Serialize)]
pub struct QuickDropResult {
    pub reports: Vec<QuickFileReport>,
    pub errors: Vec<String>,
}

impl QuickDropResult {
    fn record(&mut self, result: Result<QuickFileReport, String>) {
        match result {
            Ok(report) => self.reports.push(report),
            Err(error) if !is_cancelled() => self.errors.push(error),
            Err(_) => {}
        }
    }
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

    pub(crate) fn set(&mut self, done: u64) {
        self.done = done;
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
pub use zip_ops::{pack_solid, pack_solid_ultra, pack_zip, unpack_solid, unpack_zip};
#[cfg(test)]
pub(crate) use zip_ops::check_free_space;

fn archive_kind_from_name(path: &Path) -> Option<&'static str> {
    let lower = path
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if lower.ends_with(".zip") {
        Some("zip")
    } else if lower.ends_with(".tar.zst") || lower.ends_with(".tzst") {
        Some("tar_zst")
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        Some("tar_gz")
    } else if lower.ends_with(".gz") {
        Some("gz")
    } else if lower.ends_with(".7z") {
        Some("7z")
    } else if lower.ends_with(".rar") {
        Some("rar")
    } else if lower.ends_with(".tar") {
        Some("tar")
    } else if matches!(ext_lower(path).as_str(), "xz" | "bz2" | "zst" | "zipx") {
        Some("unsupported")
    } else {
        None
    }
}

fn has_tar_header(data: &[u8]) -> bool {
    data.get(257..262) == Some(b"ustar")
}

fn is_zip_based_format(path: &Path) -> bool {
    matches!(ext_lower(path).as_str(),
        "docx" | "docm" | "dotx" | "dotm" | "xlsx" | "xlsm" | "xlsb" | "xltx" | "xltm"
        | "pptx" | "pptm" | "ppsx" | "ppsm" | "potx" | "potm" | "sldx" | "sldm"
        | "odt" | "ods" | "odp" | "odg" | "odf" | "ott" | "ots" | "otp"
        | "epub" | "cbz" | "apk" | "aab" | "ipa" | "jar" | "war" | "ear" | "xpi" | "vsix"
        | "nupkg" | "whl" | "npz" | "kmz" | "ora" | "kra" | "3mf" | "usdz"
        | "pages" | "numbers" | "key" | "pkpass" | "sb3"
    )
}

fn archive_kind_from_content(path: &Path, name_hint: Option<&str>) -> Option<&'static str> {
    let mut data = [0; 512];
    let mut file = fs::File::open(path).ok()?;
    let mut prefix = Vec::with_capacity(512);
    file.by_ref().take(512).read_to_end(&mut prefix).ok()?;
    if [b"PK\x03\x04", b"PK\x05\x06", b"PK\x07\x08"].iter().any(|magic| prefix.starts_with(*magic)) {
        return (!is_zip_based_format(path)).then_some("zip");
    }
    if prefix.starts_with(b"7z\xBC\xAF\x27\x1C") {
        return Some("7z");
    }
    if prefix.starts_with(b"Rar!\x1A\x07\x00") || prefix.starts_with(b"Rar!\x1A\x07\x01\x00") {
        return Some("rar");
    }
    if has_tar_header(&prefix) {
        return Some("tar");
    }
    if prefix.starts_with(b"\x1F\x8B") {
        if ext_lower(path) == "svgz" {
            return None;
        }
        file.rewind().ok()?;
        let tar = name_hint == Some("tar_gz")
            || (flate2::read::GzDecoder::new(file.take(65_536)).read_exact(&mut data).is_ok() && has_tar_header(&data));
        return Some(if tar { "tar_gz" } else { "gz" });
    }
    if prefix.starts_with(b"\x28\xB5\x2F\xFD") {
        if name_hint == Some("tar_zst") {
            return Some("tar_zst");
        }
        file.rewind().ok()?;
        let mut decoder = zstd::stream::read::Decoder::new(file.take(65_536)).ok()?;
        decoder.window_log_max(26).ok()?;
        let tar = decoder.read_exact(&mut data).is_ok() && has_tar_header(&data);
        return Some(if tar { "tar_zst" } else { "unsupported" });
    }
    if prefix.starts_with(b"\xFD7zXZ\x00") || prefix.starts_with(b"BZh") {
        return Some("unsupported");
    }
    None
}

fn archive_kind(path: &Path) -> Option<&'static str> {
    let name = archive_kind_from_name(path);
    archive_kind_from_content(path, name).or(name)
}

fn unpack_one(path: String, kind: &str, app: Option<&AppHandle>) -> Result<QuickFileReport, String> {
    match kind {
        "zip" => zip_ops::unpack_zip(path, app),
        "tar_zst" => zip_ops::unpack_solid(path, app),
        "tar_gz" => zip_ops::unpack_tar_gz(path, app),
        "gz" => zip_ops::unpack_gz(path, app),
        "tar" => zip_ops::unpack_tar(path, app),
        "7z" => zip_ops::unpack_sevenz(path, app),
        "rar" => zip_ops::unpack_rar(path, app),
        _ => Err(format!("неподдерживаемый архив: {path}")),
    }
}

pub fn handle_archive_drop(
    paths: Vec<String>,
    zip_mode: &str,
    delete_after: bool,
    app: Option<&AppHandle>,
) -> Result<QuickDropResult, String> {
    let _active_guard = ActiveDropGuard::enter();
    CANCEL_DROP.store(false, Ordering::Relaxed);
    if paths.is_empty() {
        return Err("список файлов пуст".into());
    }
    let mut archives = Vec::new();
    let mut to_pack = Vec::new();
    for raw in paths {
        if is_cancelled() {
            return Err("отменено".into());
        }
        let path = Path::new(&raw);
        if path.is_file() {
            match archive_kind(path) {
                Some(kind) => archives.push((raw, kind)),
                None => to_pack.push(raw),
            }
        } else {
            to_pack.push(raw);
        }
    }

    let mut result = QuickDropResult::default();
    for (archive, kind) in archives {
        if is_cancelled() {
            break;
        }
        result.record(unpack_one(archive, kind, app));
    }
    if !to_pack.is_empty() && !is_cancelled() {
        let orig_paths = delete_after.then(|| to_pack.clone());
        match pack_archive(to_pack, zip_mode, app) {
            Ok(mut rep) => {
                if let Some(orig) = orig_paths {
                    if rep.kind == "zip-pack"
                        && rep.output_bytes > 0
                        && Path::new(&rep.output_path).is_file()
                        && !is_cancelled()
                    {
                        match delete_originals(&orig, Path::new(&rep.output_path)) {
                            Ok(()) => {
                                rep.deleted_original = true;
                                crate::log::info(&format!(
                                    "zip-pack deleted original: {}",
                                    rep.output_path
                                ));
                            }
                            Err(err) => result.errors.push(err),
                        }
                    }
                }
                result.record(Ok(rep));
            }
            Err(err) => result.record(Err(err)),
        }
    }
    if is_cancelled() {
        result.errors.push("отменено".into());
    }
    Ok(result)
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

fn delete_originals(paths: &[String], output_path: &Path) -> Result<(), String> {
    let out_canon = fs::canonicalize(output_path).ok();
    for raw in paths {
        let p = Path::new(raw);
        if !p.exists() {
            continue;
        }
        if is_unsafe_delete_target(p) {
            return Err(format!("защищённый путь: {}", p.display()));
        }
        if let (Some(out), Ok(src)) = (&out_canon, fs::canonicalize(p)) {
            if &src == out {
                continue;
            }
        }
        if p.is_dir() {
            fs::remove_dir_all(p)
                .map_err(|e| format!("не удалось удалить {}: {e}", p.display()))?;
        } else {
            fs::remove_file(p)
                .map_err(|e| format!("не удалось удалить {}: {e}", p.display()))?;
        }
    }
    Ok(())
}

fn pack_archive(paths: Vec<String>, zip_mode: &str, app: Option<&AppHandle>) -> Result<QuickFileReport, String> {
    match zip_mode {
        "solid" => zip_ops::pack_solid(paths, app),
        "ultra" => zip_ops::pack_solid_ultra(paths, app),
        _ => zip_ops::pack_zip(paths, zip_mode, app),
    }
}
