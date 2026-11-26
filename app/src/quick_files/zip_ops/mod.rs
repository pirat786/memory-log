mod bg_writer;
mod pack;
mod parallel;
mod unpack;

pub use pack::pack_zip;
pub use unpack::unpack_zip;

use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use zip::write::SimpleFileOptions;
use zip::CompressionMethod;

use super::io_hints::is_rotational_drive;

pub(super) const PARALLEL_FILE_MAX_BYTES: u64 = 24 * 1024 * 1024;
pub(super) const PARALLEL_CHUNK_MAX_BYTES: u64 = 8 * 1024 * 1024;
pub(super) const PARALLEL_CHUNK_MAX_FILES: usize = 64;
pub(super) const UNPACK_BUF_SSD: usize = 4 * 1024 * 1024;
pub(super) const UNPACK_BUF_HDD: usize = 8 * 1024 * 1024;
pub(super) const HUGE_BUF_SSD: usize = 4 * 1024 * 1024;
pub(super) const HUGE_BUF_HDD: usize = 8 * 1024 * 1024;
pub(super) const HDD_READAHEAD_CHUNKS: usize = 6;
pub(super) const RECYCLE_MAX_CAP: usize = 16 * 1024 * 1024;

pub(super) type MiniResult = Result<(Vec<u8>, u64, usize, Duration, Duration), String>;

pub(super) struct ZipEntryTask {
    pub(super) path: PathBuf,
    pub(super) zip_name: String,
    pub(super) len: u64,
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

pub(super) fn should_compress(ext: &str, zip_mode: &str) -> bool {
    zip_mode == "max" || !is_incompressible_ext(ext)
}

pub(super) fn zip_options(ext: &str, file_len: u64, zip_mode: &str) -> SimpleFileOptions {
    let mut opts = if !should_compress(ext, zip_mode) {
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
    } else {
        let level = if zip_mode == "max" { 9 } else { 1 };
        SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(level))
    };
    if file_len >= u32::MAX as u64 {
        opts = opts.large_file(true);
    }
    opts
}
