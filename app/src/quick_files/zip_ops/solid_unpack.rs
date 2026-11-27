use std::collections::HashSet;
use std::fs;
use std::io::{self, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;

use flate2::read::GzDecoder;
use tauri::AppHandle;

use super::bg_writer::BoundedQueue;
use super::{
    check_free_space, detect_hdd_mode, SOLID_SMALL_FILE_MAX, UNPACK_BUF_HDD, UNPACK_BUF_SSD,
};
use crate::quick_files::io_hints::{create_sequential_write, open_sequential_read};
use crate::quick_files::jxl::{
    self, CodecManifest, ImageFormat, CODEC_MANIFEST_NAME, MAX_CODEC_MANIFEST_BYTES,
};
use crate::quick_files::{is_cancelled, unique_path, ProgressReporter, QuickFileReport};

const SOLID_UNPACK_QUEUE_MAX: usize = 32;

struct CountingReader<R: Read> {
    inner: R,
    bytes: Arc<AtomicU64>,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.bytes.fetch_add(n as u64, Ordering::Relaxed);
        }
        Ok(n)
    }
}

pub(super) fn sanitize_rel_path(rel: &Path) -> Option<PathBuf> {
    let mut clean = PathBuf::new();
    for comp in rel.components() {
        match comp {
            Component::Normal(part) => {
                let name = part.to_string_lossy();
                let stem = name.split('.').next().unwrap_or_default().trim_end().to_ascii_uppercase();
                let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
                    || stem.strip_prefix("COM").or_else(|| stem.strip_prefix("LPT"))
                        .is_some_and(|suffix| matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"));
                if device || name.contains([':', '\\', '<', '>', '"', '|', '?', '*']) || name.ends_with(['.', ' ']) {
                    return None;
                }
                clean.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if clean.as_os_str().is_empty() {
        None
    } else {
        Some(clean)
    }
}

fn strip_archive_stem(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unpacked");
    let lower = name.to_ascii_lowercase();
    for suffix in [".tar.zst", ".tar.gz", ".tzst", ".tgz", ".gz"] {
        if lower.ends_with(suffix) && name.len() > suffix.len() {
            return name[..name.len() - suffix.len()].to_string();
        }
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unpacked")
        .to_string()
}

fn read_codec_manifest<R: Read>(entry: R) -> Result<CodecManifest, String> {
    let mut data = Vec::new();
    entry
        .take(MAX_CODEC_MANIFEST_BYTES as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|e| format!("ошибка чтения манифеста кодеков: {e}"))?;
    if data.len() > MAX_CODEC_MANIFEST_BYTES {
        return Err("слишком большой манифест кодеков".into());
    }
    let manifest: CodecManifest = serde_json::from_slice(&data)
        .map_err(|e| format!("повреждённый манифест кодеков: {e}"))?;
    if manifest.v != 1 {
        return Err(format!("неподдерживаемая версия манифеста кодеков: {}", manifest.v));
    }
    Ok(manifest)
}

fn restore_jxl_entries(
    out_dir: &Path,
    manifest: CodecManifest,
    output_bytes: &mut u64,
) -> Result<(), String> {
    if manifest.jxl.is_empty() {
        return Ok(());
    }
    let mut tasks = Vec::with_capacity(manifest.jxl.len());
    for entry in manifest.jxl {
        let stored = sanitize_rel_path(Path::new(&entry.stored))
            .ok_or_else(|| format!("небезопасный путь в манифесте кодеков: {}", entry.stored))?;
        let original = sanitize_rel_path(Path::new(&entry.original))
            .ok_or_else(|| format!("небезопасный путь в манифесте кодеков: {}", entry.original))?;
        let format = ImageFormat::from_str(&entry.format)
            .ok_or_else(|| format!("неизвестный формат в манифесте кодеков: {}", entry.format))?;
        tasks.push((out_dir.join(stored), out_dir.join(original), format));
    }

    let workers = 4.min(tasks.len());
    let next_idx = AtomicUsize::new(0);
    let total_jxl = AtomicU64::new(0);
    let total_restored = AtomicU64::new(0);
    let first_err: Mutex<Option<String>> = Mutex::new(None);

    std::thread::scope(|s| {
        for _ in 0..workers {
            let tasks_ref = &tasks;
            let idx_ref = &next_idx;
            let jxl_ref = &total_jxl;
            let res_ref = &total_restored;
            let err_ref = &first_err;
            s.spawn(move || loop {
                if is_cancelled() || err_ref.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                    break;
                }
                let i = idx_ref.fetch_add(1, Ordering::Relaxed);
                if i >= tasks_ref.len() {
                    break;
                }
                let (ref src_jxl, ref dst_orig, format) = tasks_ref[i];
                let jxl_len = fs::metadata(src_jxl).map(|m| m.len()).unwrap_or(0);
                match jxl::decode_jxl_to_file(src_jxl, dst_orig, format, 2) {
                    Ok(restored_len) => {
                        if let Err(e) = fs::remove_file(src_jxl) {
                            let mut g = err_ref.lock().unwrap_or_else(|e| e.into_inner());
                            if g.is_none() {
                                *g = Some(format!("ошибка удаления {}: {e}", src_jxl.display()));
                            }
                            break;
                        }
                        jxl_ref.fetch_add(jxl_len, Ordering::Relaxed);
                        res_ref.fetch_add(restored_len, Ordering::Relaxed);
                    }
                    Err(e) => {
                        let mut g = err_ref.lock().unwrap_or_else(|e| e.into_inner());
                        if g.is_none() {
                            *g = Some(e);
                        }
                        break;
                    }
                }
            });
        }
    });

    if let Some(e) = first_err.into_inner().unwrap_or_else(|e| e.into_inner()) {
        return Err(e);
    }
    if is_cancelled() {
        return Err("отменено".into());
    }
    *output_bytes = output_bytes
        .saturating_sub(total_jxl.load(Ordering::Relaxed))
        .saturating_add(total_restored.load(Ordering::Relaxed));
    Ok(())
}

fn extract_tar_stream<R: Read>(
    label: &str,
    path: &Path,
    input_bytes: u64,
    out_dir: &Path,
    hdd_mode: bool,
    buf_size: usize,
    stream: R,
    compressed_read: Arc<AtomicU64>,
    started: Instant,
    app: Option<&AppHandle>,
) -> Result<QuickFileReport, String> {
    let mut archive = tar::Archive::new(stream);
    let mut progress = ProgressReporter::new(app, input_bytes);
    let mut count = 0usize;
    let mut output_bytes = 0u64;
    let mut created_dirs: HashSet<PathBuf> = HashSet::new();
    created_dirs.insert(out_dir.to_path_buf());
    let mut buf = vec![0u8; buf_size];
    let mut codec_manifest: Option<CodecManifest> = None;

    let res: Result<(), String> = if !hdd_mode {
        let queue = BoundedQueue::<(PathBuf, Vec<u8>)>::new(SOLID_UNPACK_QUEUE_MAX);
        let (recycle_tx, recycle_rx) = mpsc::channel::<Vec<u8>>();
        let first_err: Mutex<Option<String>> = Mutex::new(None);
        let arch_ref = &mut archive;
        let dirs_ref = &mut created_dirs;
        let prog_ref = &mut progress;
        let buf_ref = &mut buf;
        let cnt_ref = &mut count;
        let out_bytes_ref = &mut output_bytes;
        let manifest_ref = &mut codec_manifest;

        let scope_res = std::thread::scope(|s| -> Result<(), String> {
            for _ in 0..4 {
                let q = &queue;
                let rtx = recycle_tx.clone();
                let err_ref = &first_err;
                s.spawn(move || loop {
                    if is_cancelled() {
                        break;
                    }
                    let Some((out_path, mut data)) = q.pop() else {
                        break;
                    };
                    let mut out_file = match create_sequential_write(&out_path) {
                        Ok(f) => f,
                        Err(e) => {
                            let mut g = err_ref.lock().unwrap_or_else(|e| e.into_inner());
                            if g.is_none() {
                                *g = Some(format!(
                                    "не удалось создать файл {}: {e}",
                                    out_path.display()
                                ));
                            }
                            q.close();
                            break;
                        }
                    };
                    if let Err(e) = out_file.write_all(&data) {
                        let mut g = err_ref.lock().unwrap_or_else(|e| e.into_inner());
                        if g.is_none() {
                            *g = Some(format!(
                                "ошибка записи {}: {e}",
                                out_path.display()
                            ));
                        }
                        q.close();
                        break;
                    }
                    if data.capacity() <= SOLID_SMALL_FILE_MAX as usize {
                        data.clear();
                        let _ = rtx.send(data);
                    }
                });
            }
            drop(recycle_tx);

            let decode_res: Result<(), String> = (|| {
                let entries = arch_ref
                    .entries()
                    .map_err(|e| format!("ошибка чтения TAR: {e}"))?;
                for entry_res in entries {
                    if is_cancelled() {
                        return Err("отменено".into());
                    }
                    if let Some(e) = first_err.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                        return Err(e.clone());
                    }
                    let mut entry = entry_res.map_err(|e| format!("ошибка элемента TAR: {e}"))?;
                    let rel_raw = entry
                        .path()
                        .map_err(|e| format!("ошибка пути в TAR: {e}"))?
                        .into_owned();
                    let Some(clean_rel) = sanitize_rel_path(&rel_raw) else {
                        continue;
                    };
                    let out_path = out_dir.join(&clean_rel);
                    let kind = entry.header().entry_type();
                    if kind.is_dir() {
                        if dirs_ref.insert(out_path.clone()) {
                            fs::create_dir_all(&out_path)
                                .map_err(|e| format!("не удалось создать папку: {e}"))?;
                        }
                        continue;
                    }
                    if !kind.is_file() {
                        continue;
                    }
                    if clean_rel == Path::new(CODEC_MANIFEST_NAME) {
                        if manifest_ref.is_some() {
                            return Err("дублирующийся манифест кодеков".into());
                        }
                        *manifest_ref = Some(read_codec_manifest(entry)?);
                        continue;
                    }
                    if let Some(p) = out_path.parent() {
                        if dirs_ref.insert(p.to_path_buf()) {
                            fs::create_dir_all(p)
                                .map_err(|e| format!("не удалось создать папку: {e}"))?;
                        }
                    }
                    let size = entry.size();
                    if size <= SOLID_SMALL_FILE_MAX {
                        let mut data = recycle_rx
                            .try_recv()
                            .unwrap_or_else(|_| Vec::with_capacity(size as usize));
                        data.clear();
                        entry
                            .read_to_end(&mut data)
                            .map_err(|e| format!("ошибка распаковки {}: {e}", out_path.display()))?;
                        let n = data.len() as u64;
                        if !queue.push((out_path, data)) {
                            if is_cancelled() {
                                return Err("отменено".into());
                            }
                            if let Some(e) =
                                first_err.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
                            {
                                return Err(e.clone());
                            }
                            return Err("ошибка очереди записи".into());
                        }
                        *out_bytes_ref += n;
                        prog_ref.set(compressed_read.load(Ordering::Relaxed));
                        *cnt_ref += 1;
                    } else {
                        let mut out_file = create_sequential_write(&out_path).map_err(|e| {
                            format!("не удалось создать файл {}: {e}", out_path.display())
                        })?;
                        if size >= 4 * 1024 * 1024 {
                            let _ = out_file.set_len(size);
                        }
                        loop {
                            if is_cancelled() {
                                return Err("отменено".into());
                            }
                            let n = entry.read(buf_ref).map_err(|e| {
                                format!("ошибка распаковки {}: {e}", out_path.display())
                            })?;
                            if n == 0 {
                                break;
                            }
                            out_file.write_all(&buf_ref[..n]).map_err(|e| {
                                format!("ошибка записи {}: {e}", out_path.display())
                            })?;
                            *out_bytes_ref += n as u64;
                            prog_ref.set(compressed_read.load(Ordering::Relaxed));
                        }
                        *cnt_ref += 1;
                    }
                }
                Ok(())
            })();

            queue.close();
            decode_res
        });

        scope_res?;
        if let Some(e) = first_err.into_inner().unwrap_or_else(|e| e.into_inner()) {
            Err(e)
        } else {
            Ok(())
        }
    } else {
        (|| {
            let entries = archive
                .entries()
                .map_err(|e| format!("ошибка чтения TAR: {e}"))?;
            for entry_res in entries {
                if is_cancelled() {
                    return Err("отменено".into());
                }
                let mut entry = entry_res.map_err(|e| format!("ошибка элемента TAR: {e}"))?;
                let rel_raw = entry
                    .path()
                    .map_err(|e| format!("ошибка пути в TAR: {e}"))?
                    .into_owned();
                let Some(clean_rel) = sanitize_rel_path(&rel_raw) else {
                    continue;
                };
                let out_path = out_dir.join(&clean_rel);
                let kind = entry.header().entry_type();
                if kind.is_dir() {
                    if created_dirs.insert(out_path.clone()) {
                        fs::create_dir_all(&out_path)
                            .map_err(|e| format!("не удалось создать папку: {e}"))?;
                    }
                    continue;
                }
                if !kind.is_file() {
                    continue;
                }
                if clean_rel == Path::new(CODEC_MANIFEST_NAME) {
                    if codec_manifest.is_some() {
                        return Err("дублирующийся манифест кодеков".into());
                    }
                    codec_manifest = Some(read_codec_manifest(entry)?);
                    continue;
                }
                if let Some(p) = out_path.parent() {
                    if created_dirs.insert(p.to_path_buf()) {
                        fs::create_dir_all(p)
                            .map_err(|e| format!("не удалось создать папку: {e}"))?;
                    }
                }
                let size = entry.size();
                let mut out_file = create_sequential_write(&out_path)
                    .map_err(|e| format!("не удалось создать файл {}: {e}", out_path.display()))?;
                if size >= 4 * 1024 * 1024 {
                    let _ = out_file.set_len(size);
                }
                loop {
                    if is_cancelled() {
                        return Err("отменено".into());
                    }
                    let n = entry
                        .read(&mut buf)
                        .map_err(|e| format!("ошибка распаковки {}: {e}", out_path.display()))?;
                    if n == 0 {
                        break;
                    }
                    out_file
                        .write_all(&buf[..n])
                        .map_err(|e| format!("ошибка записи {}: {e}", out_path.display()))?;
                    output_bytes += n as u64;
                    progress.set(compressed_read.load(Ordering::Relaxed));
                }
                count += 1;
            }
            Ok(())
        })()
    };

    if let Err(err) = res {
        drop(archive);
        let _ = fs::remove_dir_all(out_dir);
        return Err(err);
    }
    drop(archive);
    if let Some(manifest) = codec_manifest {
        if let Err(err) = restore_jxl_entries(out_dir, manifest, &mut output_bytes) {
            let _ = fs::remove_dir_all(out_dir);
            return Err(err);
        }
    }
    progress.set(input_bytes);

    let elapsed_ms = started.elapsed().as_millis();
    let secs = started.elapsed().as_secs_f64().max(0.000_001);
    crate::log::info(&format!(
        "{label} done: src={}, files={}, out={:.1} MB, total={} ms ({:.1} MB/s)",
        path.display(),
        count,
        output_bytes as f64 / 1_048_576.0,
        elapsed_ms,
        (output_bytes as f64 / 1_048_576.0) / secs,
    ));

    Ok(QuickFileReport {
        kind: "zip-unpack".into(),
        output_path: out_dir.to_string_lossy().into_owned(),
        count,
        input_bytes,
        output_bytes,
        elapsed_ms,
        detail: out_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unpacked")
            .to_string(),
        deleted_original: false,
    })
}

pub fn unpack_solid(archive_path: String, app: Option<&AppHandle>) -> Result<QuickFileReport, String> {
    let started = Instant::now();
    let path = PathBuf::from(&archive_path);
    if !path.is_file() {
        return Err("архив не найден".into());
    }

    let input_bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    check_free_space(parent, input_bytes)?;

    let stem = strip_archive_stem(&path);
    let out_dir = unique_path(parent, &stem, "");
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("не удалось создать папку {}: {e}", out_dir.display()))?;

    let hdd_mode = detect_hdd_mode(&out_dir, &path);
    let buf_size = if hdd_mode {
        UNPACK_BUF_HDD
    } else {
        UNPACK_BUF_SSD
    };
    crate::log::info(&format!(
        "solid-unpack start: archive={}, in={:.1} MB, hdd={}, out={}",
        path.display(),
        input_bytes as f64 / 1_048_576.0,
        hdd_mode,
        out_dir.display()
    ));

    let file = open_sequential_read(&path)
        .map_err(|e| format!("не удалось открыть архив {}: {e}", path.display()))?;
    let counter = Arc::new(AtomicU64::new(0));
    let counting = CountingReader {
        inner: BufReader::with_capacity(buf_size, file),
        bytes: Arc::clone(&counter),
    };
    let decoder = match zstd::stream::read::Decoder::new(counting) {
        Ok(d) => d,
        Err(e) => {
            let _ = fs::remove_dir_all(&out_dir);
            return Err(format!("повреждённый или неподдерживаемый Zstd: {e}"));
        }
    };
    extract_tar_stream(
        "solid-unpack",
        &path,
        input_bytes,
        &out_dir,
        hdd_mode,
        buf_size,
        decoder,
        counter,
        started,
        app,
    )
}

pub fn unpack_tar_gz(
    archive_path: String,
    app: Option<&AppHandle>,
) -> Result<QuickFileReport, String> {
    unpack_tar_file(archive_path, true, app)
}

pub fn unpack_tar(archive_path: String, app: Option<&AppHandle>) -> Result<QuickFileReport, String> {
    unpack_tar_file(archive_path, false, app)
}

fn unpack_tar_file(
    archive_path: String,
    gzip: bool,
    app: Option<&AppHandle>,
) -> Result<QuickFileReport, String> {
    let started = Instant::now();
    let path = PathBuf::from(&archive_path);
    if !path.is_file() {
        return Err("архив не найден".into());
    }

    let input_bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    check_free_space(parent, input_bytes)?;

    let stem = strip_archive_stem(&path);
    let out_dir = unique_path(parent, &stem, "");
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("не удалось создать папку {}: {e}", out_dir.display()))?;

    let hdd_mode = detect_hdd_mode(&out_dir, &path);
    let buf_size = if hdd_mode {
        UNPACK_BUF_HDD
    } else {
        UNPACK_BUF_SSD
    };
    let label = if gzip { "targz-unpack" } else { "tar-unpack" };
    crate::log::info(&format!(
        "{label} start: archive={}, in={:.1} MB, hdd={}, out={}",
        path.display(),
        input_bytes as f64 / 1_048_576.0,
        hdd_mode,
        out_dir.display()
    ));

    let file = open_sequential_read(&path)
        .map_err(|e| format!("не удалось открыть архив {}: {e}", path.display()))?;
    let counter = Arc::new(AtomicU64::new(0));
    let counting = CountingReader {
        inner: BufReader::with_capacity(buf_size, file),
        bytes: Arc::clone(&counter),
    };
    let stream: Box<dyn Read + Send> = if gzip { Box::new(GzDecoder::new(counting)) } else { Box::new(counting) };
    extract_tar_stream(
        label,
        &path,
        input_bytes,
        &out_dir,
        hdd_mode,
        buf_size,
        stream,
        counter,
        started,
        app,
    )
}

pub fn unpack_gz(archive_path: String, app: Option<&AppHandle>) -> Result<QuickFileReport, String> {
    let started = Instant::now();
    let path = PathBuf::from(&archive_path);
    if !path.is_file() {
        return Err("архив не найден".into());
    }

    let input_bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    check_free_space(parent, input_bytes)?;

    let target_name = strip_archive_stem(&path);
    let target_path = Path::new(&target_name);
    let stem = target_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&target_name);
    let ext = target_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let out_path = unique_path(parent, stem, ext);

    let hdd_mode = detect_hdd_mode(&out_path, &path);
    let buf_size = if hdd_mode {
        UNPACK_BUF_HDD
    } else {
        UNPACK_BUF_SSD
    };

    let file = open_sequential_read(&path)
        .map_err(|e| format!("не удалось открыть архив {}: {e}", path.display()))?;
    let counter = Arc::new(AtomicU64::new(0));
    let counting = CountingReader {
        inner: BufReader::with_capacity(buf_size, file),
        bytes: Arc::clone(&counter),
    };
    let mut decoder = GzDecoder::new(counting);
    let mut out_file = create_sequential_write(&out_path)
        .map_err(|e| format!("не удалось создать файл {}: {e}", out_path.display()))?;

    let mut progress = ProgressReporter::new(app, input_bytes);
    let mut buf = vec![0u8; buf_size];
    let mut output_bytes = 0u64;

    let res: Result<(), String> = (|| loop {
        if is_cancelled() {
            return Err("отменено".into());
        }
        let n = decoder
            .read(&mut buf)
            .map_err(|e| format!("ошибка распаковки {}: {e}", path.display()))?;
        if n == 0 {
            return Ok(());
        }
        out_file
            .write_all(&buf[..n])
            .map_err(|e| format!("ошибка записи {}: {e}", out_path.display()))?;
        output_bytes += n as u64;
        progress.set(counter.load(Ordering::Relaxed));
    })();

    if let Err(err) = res {
        drop(out_file);
        let _ = fs::remove_file(&out_path);
        return Err(err);
    }
    progress.set(input_bytes);

    let elapsed_ms = started.elapsed().as_millis();
    crate::log::info(&format!(
        "gz-unpack done: out={}, out_bytes={:.1} MB, total={} ms",
        out_path.display(),
        output_bytes as f64 / 1_048_576.0,
        elapsed_ms
    ));

    Ok(QuickFileReport {
        kind: "zip-unpack".into(),
        output_path: out_path.to_string_lossy().into_owned(),
        count: 1,
        input_bytes,
        output_bytes,
        elapsed_ms,
        detail: out_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unpacked")
            .to_string(),
        deleted_original: false,
    })
}
