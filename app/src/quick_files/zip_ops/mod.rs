mod bg_writer;
mod pack;
mod parallel;
mod solid;
mod unpack;

pub use pack::pack_zip;
pub use solid::{pack_solid, pack_solid_with_level, unpack_gz, unpack_solid, unpack_tar_gz, ULTRA_SOLID_ZSTD_LEVEL};
pub use unpack::unpack_zip;

use std::collections::HashSet;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use zip::write::SimpleFileOptions;
use zip::CompressionMethod;

use super::ext_lower;
use super::io_hints::{is_rotational_drive, open_sequential_read};

pub(super) const PARALLEL_FILE_MAX_BYTES: u64 = 24 * 1024 * 1024;
pub(super) const PARALLEL_CHUNK_MAX_BYTES: u64 = 8 * 1024 * 1024;
pub(super) const PARALLEL_CHUNK_MAX_FILES: usize = 64;
pub(super) const UNPACK_BUF_SSD: usize = 4 * 1024 * 1024;
pub(super) const UNPACK_BUF_HDD: usize = 8 * 1024 * 1024;
pub(super) const HUGE_BUF_SSD: usize = 4 * 1024 * 1024;
pub(super) const HUGE_BUF_HDD: usize = 8 * 1024 * 1024;
pub(super) const HDD_READAHEAD_CHUNKS: usize = 6;
pub(super) const RECYCLE_MAX_CAP: usize = 16 * 1024 * 1024;
const PROBE_SAMPLE_BYTES: usize = 64 * 1024;

pub(super) type MiniResult = Result<(Vec<u8>, u64, usize, Duration, Duration), String>;

pub(super) struct ZipEntryTask {
    pub(super) path: PathBuf,
    pub(super) zip_name: String,
    pub(super) len: u64,
    pub(super) mtime: Option<zip::DateTime>,
}

pub(super) fn system_time_to_zip_dt(st: SystemTime) -> Option<zip::DateTime> {
    use chrono::{Datelike, Timelike};
    let dt: chrono::DateTime<chrono::Local> = st.into();
    let year = dt.year().clamp(1980, 2107) as u16;
    zip::DateTime::from_date_and_time(
        year,
        dt.month() as u8,
        dt.day() as u8,
        dt.hour() as u8,
        dt.minute() as u8,
        dt.second().min(59) as u8,
    )
    .ok()
}

pub(super) fn unique_entry_name(used: &mut HashSet<String>, name: String) -> String {
    if used.insert(name.clone()) {
        return name;
    }
    let (prefix, ext) = match name.rsplit_once('/') {
        Some((dir, file)) => match file.rsplit_once('.') {
            Some((stem, e)) if !stem.is_empty() => (format!("{dir}/{stem}"), format!(".{e}")),
            _ => (name.clone(), String::new()),
        },
        None => match name.rsplit_once('.') {
            Some((stem, e)) if !stem.is_empty() => (stem.to_string(), format!(".{e}")),
            _ => (name.clone(), String::new()),
        },
    };
    for i in 2..=9999 {
        let candidate = format!("{prefix} ({i}){ext}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    let fallback = format!("{prefix}_{}{ext}", uuid::Uuid::new_v4());
    used.insert(fallback.clone());
    fallback
}

pub(crate) fn check_free_space(dir: &Path, needed: u64) -> Result<(), String> {
    let Some(avail) = super::io_hints::available_disk_space(dir) else {
        return Ok(());
    };
    let required = needed.saturating_add(32 * 1024 * 1024);
    if avail < required {
        let fmt = |b: u64| {
            if b >= 1_073_741_824 {
                format!("{:.2} ГБ", b as f64 / 1_073_741_824.0)
            } else {
                format!("{:.0} МБ", b as f64 / 1_048_576.0)
            }
        };
        return Err(format!(
            "недостаточно места на диске: доступно {}, нужно ~{}",
            fmt(avail),
            fmt(required)
        ));
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct ZipPackStats {
    pub(super) scan_total: Duration,
    pub(super) scan_canonicalize: Duration,
    pub(super) parallel_total: Duration,
    pub(super) batch_merge: Duration,
    pub(super) stream_wait: Duration,
    pub(super) start_file: Duration,
    pub(super) file_open: Duration,
    pub(super) buf_alloc: Duration,
    pub(super) read: Duration,
    pub(super) zip_write: Duration,
    pub(super) finish_flush: Duration,
}

pub(super) fn is_incompressible_ext(ext: &str) -> bool {
    matches!(
        ext,
        "zip"
            | "7z"
            | "rar"
            | "gz"
            | "bz2"
            | "xz"
            | "zst"
            | "lz4"
            | "br"
            | "tgz"
            | "tbz2"
            | "txz"
            | "jpg"
            | "jpeg"
            | "png"
            | "webp"
            | "gif"
            | "heic"
            | "heif"
            | "avif"
            | "jxl"
            | "cr2"
            | "nef"
            | "arw"
            | "dng"
            | "orf"
            | "rw2"
            | "raw"
            | "mp4"
            | "mkv"
            | "avi"
            | "mov"
            | "webm"
            | "m4v"
            | "wmv"
            | "flv"
            | "ts"
            | "m2ts"
            | "vob"
            | "mp3"
            | "aac"
            | "m4a"
            | "ogg"
            | "opus"
            | "flac"
            | "wma"
            | "pdf"
            | "docx"
            | "xlsx"
            | "pptx"
            | "exe"
            | "dll"
            | "msi"
            | "msp"
            | "cab"
            | "iso"
            | "dmg"
            | "vmdk"
            | "vhdx"
            | "vdi"
            | "qcow2"
            | "jar"
            | "war"
            | "ear"
            | "apk"
            | "aab"
            | "ipa"
            | "whl"
            | "nupkg"
            | "vsix"
            | "xpi"
            | "crx"
            | "deb"
            | "rpm"
            | "pack"
            | "idx"
            | "woff2"
    )
}

fn is_always_text_ext(ext: &str) -> bool {
    matches!(
        ext,
        "txt"
            | "rs"
            | "js"
            | "ts"
            | "jsx"
            | "tsx"
            | "json"
            | "toml"
            | "md"
            | "html"
            | "htm"
            | "css"
            | "scss"
            | "xml"
            | "csv"
            | "tsv"
            | "sql"
            | "log"
            | "svg"
            | "yaml"
            | "yml"
            | "ini"
            | "cfg"
            | "conf"
            | "py"
            | "c"
            | "cpp"
            | "cc"
            | "h"
            | "hpp"
            | "java"
            | "kt"
            | "go"
            | "sh"
            | "ps1"
            | "bat"
            | "cmd"
    )
}

pub(super) fn detect_hdd_mode(out_path: &Path, first: &Path) -> bool {
    if let Ok(v) = std::env::var("ML_FORCE_HDD") {
        if v == "1" {
            return true;
        }
        if v == "0" {
            return false;
        }
    }
    is_rotational_drive(out_path) || is_rotational_drive(first)
}

/// Fast extension-only estimate used during scan (before reading file bytes).
pub(super) fn should_compress_est(ext: &str, zip_mode: &str) -> bool {
    zip_mode == "max" || !is_incompressible_ext(ext)
}

pub(super) fn is_compressible_sample(sample: &[u8]) -> bool {
    if sample.len() < 512 {
        return true;
    }
    let probe = &sample[..sample.len().min(PROBE_SAMPLE_BYTES)];
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    if enc.write_all(probe).is_err() {
        return true;
    }
    match enc.finish() {
        Ok(out) => out.len() * 100 < probe.len() * 95,
        Err(_) => true,
    }
}

fn probe_file_compressible(path: &Path) -> bool {
    let Ok(mut f) = open_sequential_read(path) else {
        return true;
    };
    let mut buf = vec![0u8; PROBE_SAMPLE_BYTES];
    let mut total = 0usize;
    while total < buf.len() {
        match f.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return true,
        }
    }
    is_compressible_sample(&buf[..total])
}

/// Decides whether to compress a file using extension rules + 64 KiB entropy probe.
pub(super) fn should_compress_task(path: &Path, len: u64, zip_mode: &str) -> bool {
    let ext = ext_lower(path);
    if zip_mode == "max" {
        if is_always_text_ext(&ext) || len < 4096 {
            return true;
        }
        return probe_file_compressible(path);
    }
    if is_incompressible_ext(&ext) {
        return false;
    }
    if is_always_text_ext(&ext) || len < PROBE_SAMPLE_BYTES as u64 {
        return true;
    }
    probe_file_compressible(path)
}

/// Same decision as `should_compress_task`, operating on an in-memory buffer (zero extra disk I/O).
pub(super) fn should_compress_mem(ext: &str, data: &[u8], zip_mode: &str) -> bool {
    if zip_mode == "max" {
        if is_always_text_ext(ext) || data.len() < 4096 {
            return true;
        }
        return is_compressible_sample(data);
    }
    if is_incompressible_ext(ext) {
        return false;
    }
    if is_always_text_ext(ext) || data.len() < PROBE_SAMPLE_BYTES {
        return true;
    }
    is_compressible_sample(data)
}

pub(super) fn zip_options_for(
    compress: bool,
    file_len: u64,
    zip_mode: &str,
    mtime: Option<zip::DateTime>,
) -> SimpleFileOptions {
    let mut opts = if !compress {
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
    } else if zip_mode == "max" {
        let level = if file_len <= 16 * 1024 * 1024 { 15 } else { 11 };
        SimpleFileOptions::default()
            .compression_method(CompressionMethod::Zstd)
            .compression_level(Some(level))
    } else {
        SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(1))
    };
    if let Some(dt) = mtime {
        opts = opts.last_modified_time(dt);
    }
    if file_len >= u32::MAX as u64 {
        opts = opts.large_file(true);
    }
    opts
}
