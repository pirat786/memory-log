use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Cursor};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::FilterType;
use image::{DynamicImage, ImageEncoder, ImageFormat, ImageReader, Limits};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const IO_BUF_SIZE: usize = 2 * 1024 * 1024;
const PARALLEL_FILE_MAX_BYTES: u64 = 16 * 1024 * 1024;
const PARALLEL_CHUNK_MAX_BYTES: u64 = 8 * 1024 * 1024;
const PARALLEL_CHUNK_MAX_FILES: usize = 64;
const MAX_IMAGE_ALLOC_BYTES: u64 = 512 * 1024 * 1024;
const MAX_IMAGE_DIM: u32 = 32_768;

static CANCEL_DROP: AtomicBool = AtomicBool::new(false);

pub fn cancel_drop() {
    CANCEL_DROP.store(true, Ordering::Relaxed);
}

fn is_cancelled() -> bool {
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
}

#[derive(Debug, Clone, Serialize)]
pub struct DropProgress {
    pub done: u64,
    pub total: u64,
    pub pct: u8,
}

struct ProgressReporter<'a> {
    app: Option<&'a AppHandle>,
    total: u64,
    done: u64,
    last_emit: Instant,
    last_pct: u8,
}

impl<'a> ProgressReporter<'a> {
    fn new(app: Option<&'a AppHandle>, total: u64) -> Self {
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

    fn add(&mut self, bytes: u64) {
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

fn is_image_ext(ext: &str) -> bool {
    matches!(
        ext,
        "jpg" | "jpeg" | "png" | "webp" | "bmp" | "gif" | "tif" | "tiff"
    )
}

fn is_incompressible_ext(ext: &str) -> bool {
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

fn ext_lower(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default()
}

fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
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

fn copy_stream<R: io::Read, W: io::Write>(
    reader: &mut R,
    writer: &mut W,
    progress: &mut ProgressReporter<'_>,
) -> io::Result<u64> {
    let mut buf = vec![0u8; IO_BUF_SIZE];
    let mut total = 0u64;
    loop {
        if is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "отменено"));
        }
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
        total += n as u64;
        progress.add(n as u64);
    }
    Ok(total)
}

fn decode_image_safe(path: &Path) -> Result<(DynamicImage, ImageFormat), String> {
    let mut reader = ImageReader::open(path)
        .map_err(|e| format!("не удалось открыть {}: {e}", path.display()))?
        .with_guessed_format()
        .map_err(|e| format!("не удалось определить формат {}: {e}", path.display()))?;

    let fmt = reader.format().unwrap_or(ImageFormat::Png);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIM);
    limits.max_image_height = Some(MAX_IMAGE_DIM);
    limits.max_alloc = Some(MAX_IMAGE_ALLOC_BYTES);
    reader.limits(limits);

    let img = reader
        .decode()
        .map_err(|e| format!("ошибка декодирования {}: {e}", path.display()))?;
    Ok((img, fmt))
}

fn encode_image(
    img: &DynamicImage,
    out_path: &Path,
    target_fmt: ImageFormat,
    quality: u8,
) -> Result<(), String> {
    let file = File::create(out_path)
        .map_err(|e| format!("не удалось создать {}: {e}", out_path.display()))?;
    let mut writer = BufWriter::with_capacity(IO_BUF_SIZE, file);

    match target_fmt {
        ImageFormat::Jpeg => {
            let rgb = img.to_rgb8();
            let enc = JpegEncoder::new_with_quality(&mut writer, quality.clamp(30, 95));
            enc.write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                image::ExtendedColorType::Rgb8,
            )
            .map_err(|e| format!("ошибка JPEG-кодирования: {e}"))?;
        }
        ImageFormat::Png => {
            let rgba = img.to_rgba8();
            let enc = PngEncoder::new_with_quality(
                &mut writer,
                CompressionType::Fast,
                PngFilter::Adaptive,
            );
            enc.write_image(
                rgba.as_raw(),
                rgba.width(),
                rgba.height(),
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| format!("ошибка PNG-кодирования: {e}"))?;
        }
        ImageFormat::WebP => {
            img.write_to(&mut writer, ImageFormat::WebP)
                .map_err(|e| format!("ошибка WebP-кодирования: {e}"))?;
        }
        other => {
            img.write_to(&mut writer, other)
                .map_err(|e| format!("ошибка кодирования: {e}"))?;
        }
    }

    io::Write::flush(&mut writer).map_err(|e| format!("ошибка записи: {e}"))?;
    Ok(())
}

pub fn process_images(
    paths: Vec<String>,
    mode: &str,
    max_dim: u32,
    quality: u8,
    app: Option<&AppHandle>,
) -> Result<QuickFileReport, String> {
    let started = Instant::now();
    if paths.is_empty() {
        return Err("не переданы изображения".into());
    }

    let total_expected: u64 = paths
        .iter()
        .filter_map(|raw| {
            let p = Path::new(raw);
            if p.is_file() && is_image_ext(&ext_lower(p)) {
                fs::metadata(p).ok().map(|m| m.len())
            } else {
                None
            }
        })
        .sum();
    let mut progress = ProgressReporter::new(app, total_expected);

    let mut total_in = 0u64;
    let mut total_out = 0u64;
    let mut processed = 0usize;
    let mut last_out = PathBuf::new();
    let mut last_dims = String::new();

    for raw in &paths {
        if is_cancelled() {
            return Err("отменено".into());
        }
        let path = PathBuf::from(raw);
        let ext = ext_lower(&path);
        if !path.is_file() || !is_image_ext(&ext) {
            continue;
        }
        let file_size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        total_in += file_size;

        let (mut img, orig_fmt) = decode_image_safe(&path)?;
        if max_dim > 0 && mode != "strip" && (img.width() > max_dim || img.height() > max_dim) {
            let filter = if img.width() > max_dim * 2 || img.height() > max_dim * 2 {
                FilterType::Triangle
            } else {
                FilterType::CatmullRom
            };
            img = img.resize(max_dim, max_dim, filter);
        }

        let (target_fmt, out_ext, suffix) = match mode {
            "webp" => (ImageFormat::WebP, "webp", ""),
            "jpg" | "jpeg" => (ImageFormat::Jpeg, "jpg", ""),
            "strip" => {
                let fmt = match orig_fmt {
                    ImageFormat::Jpeg => ImageFormat::Jpeg,
                    ImageFormat::WebP => ImageFormat::WebP,
                    _ => ImageFormat::Png,
                };
                let e = match fmt {
                    ImageFormat::Jpeg => "jpg",
                    ImageFormat::WebP => "webp",
                    _ => "png",
                };
                (fmt, e, "_clean")
            }
            _ => match orig_fmt {
                ImageFormat::Jpeg | ImageFormat::Bmp | ImageFormat::Tiff => {
                    (ImageFormat::Jpeg, "jpg", "_opt")
                }
                ImageFormat::WebP => (ImageFormat::WebP, "webp", "_opt"),
                _ => (ImageFormat::Png, "png", "_opt"),
            },
        };

        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("image");
        let out_stem = format!("{stem}{suffix}");
        let out_path = unique_path(parent, &out_stem, out_ext);

        encode_image(&img, &out_path, target_fmt, quality)?;

        total_out += fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
        last_dims = format!("{}×{}", img.width(), img.height());
        last_out = out_path;
        processed += 1;
        progress.add(file_size);
    }

    if processed == 0 {
        return Err("подходящих изображений не найдено".into());
    }

    Ok(QuickFileReport {
        kind: "image".into(),
        output_path: last_out.to_string_lossy().into_owned(),
        count: processed,
        input_bytes: total_in,
        output_bytes: total_out,
        elapsed_ms: started.elapsed().as_millis(),
        detail: last_dims,
    })
}

struct ZipEntryTask {
    path: PathBuf,
    zip_name: String,
    len: u64,
}

fn zip_options(ext: &str, file_len: u64, zip_mode: &str) -> SimpleFileOptions {
    let mut opts = if zip_mode == "store" || is_incompressible_ext(ext) {
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
    } else {
        let level = if zip_mode == "normal" { 6 } else { 1 };
        SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(level))
    };
    if file_len >= u32::MAX as u64 {
        opts = opts.large_file(true);
    }
    opts
}

pub fn pack_zip(
    paths: Vec<String>,
    zip_mode: &str,
    app: Option<&AppHandle>,
) -> Result<QuickFileReport, String> {
    let started = Instant::now();
    let valid: Vec<PathBuf> = paths
        .into_iter()
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .collect();
    if valid.is_empty() {
        return Err("нет файлов для упаковки".into());
    }

    let first = &valid[0];
    let out_dir = first.parent().unwrap_or_else(|| Path::new("."));
    let base_stem = if valid.len() == 1 {
        first
            .file_stem()
            .or_else(|| first.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("archive")
            .to_string()
    } else {
        out_dir
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("archive")
            .to_string()
    };

    let out_path = unique_path(out_dir, &base_stem, "zip");
    let out_canon = fs::canonicalize(out_dir)
        .ok()
        .map(|d| d.join(out_path.file_name().unwrap_or_default()));

    let mut total_expected = 0u64;
    let mut stream_entries: Vec<ZipEntryTask> = Vec::new();
    let mut batch_entries: Vec<ZipEntryTask> = Vec::new();

    for root in &valid {
        if is_cancelled() {
            return Err("отменено".into());
        }
        if root.is_file() {
            if out_canon.as_ref() == fs::canonicalize(root).ok().as_ref() {
                continue;
            }
            let len = fs::metadata(root).map(|m| m.len()).unwrap_or(0);
            let name = root
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("file")
                .to_string();
            total_expected = total_expected.saturating_add(len);
            let task = ZipEntryTask {
                path: root.clone(),
                zip_name: name,
                len,
            };
            if zip_mode != "store"
                && len <= PARALLEL_FILE_MAX_BYTES
                && !is_incompressible_ext(&ext_lower(root))
            {
                batch_entries.push(task);
            } else {
                stream_entries.push(task);
            }
        } else if root.is_dir() {
            let root_name = root
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("folder");
            for entry in WalkDir::new(root).follow_links(false) {
                if is_cancelled() {
                    return Err("отменено".into());
                }
                let entry = entry.map_err(|e| format!("ошибка обхода папки: {e}"))?;
                let p = entry.path();
                if p.is_dir() {
                    continue;
                }
                if out_canon.as_ref() == fs::canonicalize(p).ok().as_ref() {
                    continue;
                }
                let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                let rel = p.strip_prefix(root).unwrap_or(p);
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                let zip_name = if valid.len() == 1 {
                    rel_str
                } else {
                    format!("{root_name}/{rel_str}")
                };
                total_expected = total_expected.saturating_add(len);
                let task = ZipEntryTask {
                    path: p.to_path_buf(),
                    zip_name,
                    len,
                };
                if zip_mode != "store"
                    && len <= PARALLEL_FILE_MAX_BYTES
                    && !is_incompressible_ext(&ext_lower(p))
                {
                    batch_entries.push(task);
                } else {
                    stream_entries.push(task);
                }
            }
        }
    }

    let mut progress = ProgressReporter::new(app, total_expected);

    let out_file = File::create(&out_path)
        .map_err(|e| format!("не удалось создать архив {}: {e}", out_path.display()))?;
    let writer = BufWriter::with_capacity(IO_BUF_SIZE, out_file);
    let mut zip = ZipWriter::new(writer);

    let mut total_in = 0u64;
    let mut count = 0usize;

    let res: Result<(), String> = (|| {
        if !batch_entries.is_empty() {
            let mut chunks: Vec<Vec<ZipEntryTask>> = Vec::new();
            let mut cur_chunk: Vec<ZipEntryTask> = Vec::new();
            let mut cur_bytes = 0u64;
            for task in batch_entries {
                if !cur_chunk.is_empty()
                    && (cur_bytes + task.len > PARALLEL_CHUNK_MAX_BYTES
                        || cur_chunk.len() >= PARALLEL_CHUNK_MAX_FILES)
                {
                    chunks.push(std::mem::take(&mut cur_chunk));
                    cur_bytes = 0;
                }
                cur_bytes = cur_bytes.saturating_add(task.len);
                cur_chunk.push(task);
            }
            if !cur_chunk.is_empty() {
                chunks.push(cur_chunk);
            }

            let workers = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .clamp(1, 8)
                .min(chunks.len());
            let queue = std::sync::Mutex::new(chunks.into_iter());
            let (tx, rx) = mpsc::sync_channel::<Result<(Vec<u8>, u64, usize), String>>(workers * 2);

            std::thread::scope(|s| -> Result<(), String> {
                for _ in 0..workers {
                    let q = &queue;
                    let tx = tx.clone();
                    s.spawn(move || {
                        let mut buf = vec![0u8; 256 * 1024];
                        loop {
                            if is_cancelled() {
                                break;
                            }
                            let next = q.lock().ok().and_then(|mut g| g.next());
                            let Some(chunk) = next else { break };
                            let mut mini = ZipWriter::new(Cursor::new(Vec::new()));
                            let mut chunk_in = 0u64;
                            let mut chunk_cnt = 0usize;
                            let mut err_msg: Option<String> = None;

                            for task in chunk {
                                if is_cancelled() {
                                    err_msg = Some("отменено".into());
                                    break;
                                }
                                let opts = zip_options(&ext_lower(&task.path), task.len, zip_mode);
                                if let Err(e) = mini.start_file(&task.zip_name, opts) {
                                    err_msg = Some(format!("ошибка добавления {}: {e}", task.zip_name));
                                    break;
                                }
                                match File::open(&task.path) {
                                    Ok(mut f) => loop {
                                        if is_cancelled() {
                                            err_msg = Some("отменено".into());
                                            break;
                                        }
                                        match io::Read::read(&mut f, &mut buf) {
                                            Ok(0) => break,
                                            Ok(n) => {
                                                if let Err(e) = io::Write::write_all(&mut mini, &buf[..n]) {
                                                    err_msg = Some(format!("ошибка сжатия {}: {e}", task.zip_name));
                                                    break;
                                                }
                                                chunk_in += n as u64;
                                            }
                                            Err(e) => {
                                                err_msg = Some(format!("ошибка чтения {}: {e}", task.zip_name));
                                                break;
                                            }
                                        }
                                    },
                                    Err(e) => {
                                        err_msg = Some(format!("не удалось открыть {}: {e}", task.path.display()));
                                        break;
                                    }
                                }
                                if err_msg.is_some() {
                                    break;
                                }
                                chunk_cnt += 1;
                            }

                            if let Some(e) = err_msg {
                                let _ = tx.send(Err(e));
                                break;
                            }
                            match mini.finish() {
                                Ok(cursor) => {
                                    if tx.send(Ok((cursor.into_inner(), chunk_in, chunk_cnt))).is_err() {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    let _ = tx.send(Err(format!("ошибка финализации блока ZIP: {e}")));
                                    break;
                                }
                            }
                        }
                    });
                }
                drop(tx);

                for item in rx {
                    if is_cancelled() {
                        return Err("отменено".into());
                    }
                    let (bytes, chunk_in, chunk_cnt) = item?;
                    let archive = ZipArchive::new(Cursor::new(bytes))
                        .map_err(|e| format!("ошибка чтения блока ZIP: {e}"))?;
                    zip.merge_archive(archive)
                        .map_err(|e| format!("ошибка склейки ZIP: {e}"))?;
                    total_in += chunk_in;
                    count += chunk_cnt;
                    progress.add(chunk_in);
                }
                Ok(())
            })?;
        }

        for task in stream_entries {
            if is_cancelled() {
                return Err("отменено".into());
            }
            total_in += write_zip_entry(&mut zip, &task.path, &task.zip_name, zip_mode, &mut progress)?;
            count += 1;
        }
        Ok(())
    })();

    if let Err(err) = res {
        drop(zip);
        let _ = fs::remove_file(&out_path);
        return Err(err);
    }

    let mut writer = zip
        .finish()
        .map_err(|e| format!("ошибка финализации ZIP: {e}"))?;
    io::Write::flush(&mut writer).map_err(|e| format!("ошибка записи ZIP: {e}"))?;
    drop(writer);

    let output_bytes = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
    Ok(QuickFileReport {
        kind: "zip-pack".into(),
        output_path: out_path.to_string_lossy().into_owned(),
        count,
        input_bytes: total_in,
        output_bytes,
        elapsed_ms: started.elapsed().as_millis(),
        detail: out_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("archive.zip")
            .to_string(),
    })
}

fn write_zip_entry<W: io::Write + io::Seek>(
    zip: &mut ZipWriter<W>,
    path: &Path,
    zip_name: &str,
    zip_mode: &str,
    progress: &mut ProgressReporter<'_>,
) -> Result<u64, String> {
    let meta = fs::metadata(path)
        .map_err(|e| format!("не удалось прочитать {}: {e}", path.display()))?;
    let file_len = meta.len();
    let opts = zip_options(&ext_lower(path), file_len, zip_mode);

    zip.start_file(zip_name, opts)
        .map_err(|e| format!("ошибка добавления {zip_name}: {e}"))?;
    let file = File::open(path)
        .map_err(|e| format!("не удалось открыть {}: {e}", path.display()))?;
    let mut reader = BufReader::with_capacity(IO_BUF_SIZE, file);
    let copied = copy_stream(&mut reader, zip, progress)
        .map_err(|e| format!("ошибка копирования {zip_name}: {e}"))?;
    Ok(copied)
}

pub fn unpack_zip(archive_path: String, app: Option<&AppHandle>) -> Result<QuickFileReport, String> {
    let started = Instant::now();
    let path = PathBuf::from(&archive_path);
    if !path.is_file() {
        return Err("архив не найден".into());
    }

    let input_bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unpacked");
    let out_dir = unique_path(parent, stem, "");
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("не удалось создать папку {}: {e}", out_dir.display()))?;

    let file = File::open(&path)
        .map_err(|e| format!("не удалось открыть архив {}: {e}", path.display()))?;
    let reader = BufReader::with_capacity(IO_BUF_SIZE, file);
    let mut archive =
        ZipArchive::new(reader).map_err(|e| format!("повреждённый или неподдерживаемый ZIP: {e}"))?;

    let mut total_expected = 0u64;
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            total_expected = total_expected.saturating_add(entry.size());
        }
    }
    let mut progress = ProgressReporter::new(app, total_expected);

    let mut count = 0usize;
    let mut output_bytes = 0u64;

    let res: Result<(), String> = (|| {
        for i in 0..archive.len() {
            if is_cancelled() {
                return Err("отменено".into());
            }
            let mut entry = archive
                .by_index(i)
                .map_err(|e| format!("ошибка чтения элемента архива: {e}"))?;
            let Some(rel_path) = entry.enclosed_name() else {
                continue;
            };
            let out_path = out_dir.join(rel_path);
            if entry.is_dir() {
                fs::create_dir_all(&out_path)
                    .map_err(|e| format!("не удалось создать папку: {e}"))?;
                continue;
            }
            if let Some(p) = out_path.parent() {
                fs::create_dir_all(p).map_err(|e| format!("не удалось создать папку: {e}"))?;
            }
            let out_file = File::create(&out_path)
                .map_err(|e| format!("не удалось создать файл {}: {e}", out_path.display()))?;
            let mut writer = BufWriter::with_capacity(IO_BUF_SIZE, out_file);
            output_bytes += copy_stream(&mut entry, &mut writer, &mut progress)
                .map_err(|e| format!("ошибка распаковки {}: {e}", out_path.display()))?;
            io::Write::flush(&mut writer).map_err(|e| format!("ошибка записи: {e}"))?;
            count += 1;
        }
        Ok(())
    })();

    if let Err(err) = res {
        drop(archive);
        let _ = fs::remove_dir_all(&out_dir);
        return Err(err);
    }

    Ok(QuickFileReport {
        kind: "zip-unpack".into(),
        output_path: out_dir.to_string_lossy().into_owned(),
        count,
        input_bytes,
        output_bytes,
        elapsed_ms: started.elapsed().as_millis(),
        detail: out_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unpacked")
            .to_string(),
    })
}

pub fn handle_drop(
    paths: Vec<String>,
    mode: &str,
    max_dim: u32,
    quality: u8,
    zip_mode: &str,
    app: Option<&AppHandle>,
) -> Result<QuickFileReport, String> {
    CANCEL_DROP.store(false, Ordering::Relaxed);
    if paths.is_empty() {
        return Err("список файлов пуст".into());
    }
    if mode == "zip" {
        return pack_zip(paths, zip_mode, app);
    }
    if paths.len() == 1 {
        let p = Path::new(&paths[0]);
        if p.is_file() && ext_lower(p) == "zip" && mode == "auto" {
            return unpack_zip(paths[0].clone(), app);
        }
    }
    let all_images = paths.iter().all(|raw| {
        let p = Path::new(raw);
        p.is_file() && is_image_ext(&ext_lower(p))
    });
    if all_images {
        let img_mode = if mode == "auto" { "compress" } else { mode };
        return process_images(paths, img_mode, max_dim, quality, app);
    }
    pack_zip(paths, zip_mode, app)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};
    use std::io::Write;

    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("ml_qf_{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn large_zip_pack_and_unpack_streaming() {
        let dir = TestDir::new();
        let src_dir = dir.path().join("big_data");
        fs::create_dir_all(&src_dir).unwrap();

        // 16 MB compressible log + 8 MB incompressible dummy jpg
        let log_path = src_dir.join("big.log");
        {
            let mut w = BufWriter::new(File::create(&log_path).unwrap());
            let line = b"2026-11-20T16:30:00Z INFO memory_log::collector heartbeat ok status=200\n";
            for _ in 0..240_000 {
                w.write_all(line).unwrap();
            }
        }
        let jpg_path = src_dir.join("photo.jpg");
        {
            let mut w = BufWriter::new(File::create(&jpg_path).unwrap());
            let chunk = vec![0xABu8; 1024 * 1024];
            for _ in 0..8 {
                w.write_all(&chunk).unwrap();
            }
        }

        let pack_rep = pack_zip(vec![src_dir.to_string_lossy().into_owned()], "fast", None).unwrap();
        assert_eq!(pack_rep.count, 2);
        assert!(pack_rep.input_bytes > 20 * 1024 * 1024);
        assert!(pack_rep.output_bytes < pack_rep.input_bytes / 2);

        let unpack_rep = unpack_zip(pack_rep.output_path.clone(), None).unwrap();
        assert_eq!(unpack_rep.count, 2);
        assert_eq!(unpack_rep.output_bytes, pack_rep.input_bytes);
    }

    #[test]
    fn large_image_compress_and_resize() {
        let dir = TestDir::new();
        let img_path = dir.path().join("huge.png");

        // 4000x3000 synthetic image (~36 MB raw RGB)
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(4000, 3000, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
        });
        DynamicImage::ImageRgb8(img).save(&img_path).unwrap();

        let rep = process_images(
            vec![img_path.to_string_lossy().into_owned()],
            "jpg",
            1920,
            82,
            None,
        )
        .unwrap();
        assert_eq!(rep.count, 1);
        assert_eq!(rep.detail, "1920×1440");
        assert!(Path::new(&rep.output_path).exists());
        assert!(rep.output_bytes < rep.input_bytes);
    }

    #[test]
    fn unpack_blocks_zip_slip() {
        let dir = TestDir::new();
        let zip_path = dir.path().join("evil.zip");
        {
            let f = File::create(&zip_path).unwrap();
            let mut zw = ZipWriter::new(f);
            zw.start_file("../escape.txt", SimpleFileOptions::default())
                .unwrap();
            zw.write_all(b"pwned").unwrap();
            zw.start_file("safe.txt", SimpleFileOptions::default())
                .unwrap();
            zw.write_all(b"ok").unwrap();
            zw.finish().unwrap();
        }
        let rep = unpack_zip(zip_path.to_string_lossy().into_owned(), None).unwrap();
        assert_eq!(rep.count, 1);
        assert!(!dir.path().join("escape.txt").exists());
        assert!(Path::new(&rep.output_path).join("safe.txt").exists());
    }
}
