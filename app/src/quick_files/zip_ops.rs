use std::collections::HashSet;
use std::fs;
use std::io::{self, BufReader, BufWriter, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use tauri::AppHandle;
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use super::io_hints::{create_sequential_write, is_rotational_drive, open_sequential_read};
use super::{
    ext_lower, is_cancelled, unique_path, ProgressReporter, QuickFileReport,
};

const PARALLEL_FILE_MAX_BYTES: u64 = 16 * 1024 * 1024;
const PARALLEL_CHUNK_MAX_BYTES: u64 = 8 * 1024 * 1024;
const PARALLEL_CHUNK_MAX_FILES: usize = 64;
const WRITE_BUF_SIZE: usize = 16 * 1024 * 1024;
const STREAM_PRELOAD_MAX_BYTES: u64 = 24 * 1024 * 1024;
const PRELOAD_SLOTS: usize = 24;
const UNPACK_BUF_SIZE: usize = 4 * 1024 * 1024;

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

struct ZipEntryTask {
    path: PathBuf,
    zip_name: String,
    len: u64,
}

fn should_compress(ext: &str, zip_mode: &str) -> bool {
    zip_mode == "max" || !is_incompressible_ext(ext)
}

fn zip_options(ext: &str, file_len: u64, zip_mode: &str) -> SimpleFileOptions {
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

#[derive(Default)]
struct ZipPackStats {
    scan_total: Duration,
    scan_canonicalize: Duration,
    batch_total: Duration,
    batch_merge: Duration,
    stream_wait: Duration,
    entry_meta: Duration,
    start_file: Duration,
    file_open: Duration,
    buf_alloc: Duration,
    read: Duration,
    zip_write: Duration,
    finish_flush: Duration,
}

fn timed_canonicalize(path: &Path, acc: &mut Duration) -> Option<PathBuf> {
    let t = Instant::now();
    let res = fs::canonicalize(path).ok();
    *acc += t.elapsed();
    res
}

pub fn pack_zip(
    paths: Vec<String>,
    zip_mode: &str,
    app: Option<&AppHandle>,
) -> Result<QuickFileReport, String> {
    let started = Instant::now();
    let mut stats = ZipPackStats::default();
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
        let name_os = if first.is_dir() {
            first.file_name()
        } else {
            first.file_stem().or_else(|| first.file_name())
        };
        name_os
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
    let hdd_mode = is_rotational_drive(&out_path) || is_rotational_drive(first);
    crate::log::info(&format!(
        "zip-pack start: paths={}, mode={}, hdd={}, out={}",
        valid.len(),
        zip_mode,
        hdd_mode,
        out_path.display()
    ));

    let scan_started = Instant::now();
    let out_canon = timed_canonicalize(out_dir, &mut stats.scan_canonicalize)
        .map(|d| d.join(out_path.file_name().unwrap_or_default()));

    let mut total_expected = 0u64;
    let mut stream_entries: Vec<ZipEntryTask> = Vec::new();
    let mut batch_entries: Vec<ZipEntryTask> = Vec::new();

    for root in &valid {
        if is_cancelled() {
            return Err("отменено".into());
        }
        if root.is_file() {
            if root
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
                && out_canon.as_ref() == timed_canonicalize(root, &mut stats.scan_canonicalize).as_ref()
            {
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
            if len <= PARALLEL_FILE_MAX_BYTES && should_compress(&ext_lower(root), zip_mode) {
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
                if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip"))
                    && out_canon.as_ref()
                        == timed_canonicalize(p, &mut stats.scan_canonicalize).as_ref()
                {
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
                if len <= PARALLEL_FILE_MAX_BYTES && should_compress(&ext_lower(p), zip_mode) {
                    batch_entries.push(task);
                } else {
                    stream_entries.push(task);
                }
            }
        }
    }
    stats.scan_total = scan_started.elapsed();
    crate::log::info(&format!(
        "zip-pack scan: {} ms (canonicalize={} ms), stream={}, batch={}, total={:.1} MB",
        stats.scan_total.as_millis(),
        stats.scan_canonicalize.as_millis(),
        stream_entries.len(),
        batch_entries.len(),
        total_expected as f64 / 1_048_576.0
    ));

    let mut progress = ProgressReporter::new(app, total_expected);

    let out_file = create_sequential_write(&out_path)
        .map_err(|e| format!("не удалось создать архив {}: {e}", out_path.display()))?;
    let preallocated = if batch_entries.is_empty() && total_expected > 0 {
        let prealloc = total_expected
            .saturating_add((stream_entries.len() as u64).saturating_mul(256))
            .saturating_add(65_536);
        out_file.set_len(prealloc).is_ok()
    } else {
        false
    };
    let writer = BufWriter::with_capacity(WRITE_BUF_SIZE, out_file);
    let mut zip = ZipWriter::new_stream(writer);

    let mut total_in = 0u64;
    let mut count = 0usize;

    let res: Result<(), String> = (|| {
        if !batch_entries.is_empty() {
            let batch_started = Instant::now();
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

            let max_workers = if hdd_mode { 4 } else { 8 };
            let workers = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .clamp(1, max_workers)
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
                                match open_sequential_read(&task.path) {
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

                loop {
                    if is_cancelled() {
                        drop(rx);
                        return Err("отменено".into());
                    }
                    let item = match rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(item) => item,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    let (bytes, chunk_in, chunk_cnt) = match item {
                        Ok(v) => v,
                        Err(e) => {
                            drop(rx);
                            return Err(e);
                        }
                    };
                    let t_merge = Instant::now();
                    let archive = match ZipArchive::new(Cursor::new(bytes)) {
                        Ok(a) => a,
                        Err(e) => {
                            drop(rx);
                            return Err(format!("ошибка чтения блока ZIP: {e}"));
                        }
                    };
                    if let Err(e) = zip.merge_archive(archive) {
                        drop(rx);
                        return Err(format!("ошибка склейки ZIP: {e}"));
                    }
                    stats.batch_merge += t_merge.elapsed();
                    total_in += chunk_in;
                    count += chunk_cnt;
                    progress.add(chunk_in);
                }
                Ok(())
            })?;
            stats.batch_total = batch_started.elapsed();
            crate::log::info(&format!(
                "zip-pack batch: {} ms (merge={} ms), files={}, input={:.1} MB",
                stats.batch_total.as_millis(),
                stats.batch_merge.as_millis(),
                count,
                total_in as f64 / 1_048_576.0
            ));
        }

        let mut last_log = Instant::now();
        let mut last_log_bytes = 0u64;
        let mut stream_bytes = 0u64;

        let mut preload_entries: Vec<ZipEntryTask> = Vec::new();
        let mut huge_entries: Vec<ZipEntryTask> = Vec::new();
        for task in stream_entries {
            if task.len <= STREAM_PRELOAD_MAX_BYTES {
                preload_entries.push(task);
            } else {
                huge_entries.push(task);
            }
        }

        let maybe_log_progress = |last_log: &mut Instant,
                                  last_log_bytes: &mut u64,
                                  stream_bytes: u64,
                                  total_in: u64,
                                  stats: &ZipPackStats| {
            if last_log.elapsed() >= Duration::from_secs(5)
                || stream_bytes.saturating_sub(*last_log_bytes) >= 1024 * 1024 * 1024
            {
                let interval_secs = last_log.elapsed().as_secs_f64().max(0.001);
                let interval_mb = (stream_bytes - *last_log_bytes) as f64 / 1_048_576.0;
                let interval_mbps = interval_mb / interval_secs;
                let avg_secs = started.elapsed().as_secs_f64().max(0.001);
                let avg_mbps = (total_in as f64 / 1_048_576.0) / avg_secs;
                let pct = if total_expected == 0 {
                    100.0
                } else {
                    (total_in as f64 * 100.0) / total_expected as f64
                };
                crate::log::info(&format!(
                    "zip-pack progress: {:.2}/{:.2} GB ({:.1}%), interval={:.1} MB/s, avg={:.1} MB/s | wait={}ms start_file={}ms open={}ms read={}ms write={}ms",
                    total_in as f64 / 1_073_741_824.0,
                    total_expected as f64 / 1_073_741_824.0,
                    pct,
                    interval_mbps,
                    avg_mbps,
                    stats.stream_wait.as_millis(),
                    stats.start_file.as_millis(),
                    stats.file_open.as_millis(),
                    stats.read.as_millis(),
                    stats.zip_write.as_millis(),
                ));
                *last_log = Instant::now();
                *last_log_bytes = stream_bytes;
            }
        };

        if !preload_entries.is_empty() {
            let readers = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .clamp(2, 4)
                .min(preload_entries.len());
            let next_idx = AtomicUsize::new(0);
            let (tx, rx) = mpsc::sync_channel::<Result<(usize, Vec<u8>, Duration, Duration), String>>(
                PRELOAD_SLOTS,
            );
            let (recycle_tx, recycle_rx) = mpsc::channel::<Vec<u8>>();
            let recycle_rx = Mutex::new(recycle_rx);
            let entries_ref = &preload_entries;
            let next_ref = &next_idx;
            let recycle_ref = &recycle_rx;

            std::thread::scope(|s| -> Result<(), String> {
                for _ in 0..readers {
                    let tx = tx.clone();
                    s.spawn(move || loop {
                        if is_cancelled() {
                            break;
                        }
                        let idx = next_ref.fetch_add(1, Ordering::Relaxed);
                        if idx >= entries_ref.len() {
                            break;
                        }
                        let task = &entries_ref[idx];
                        let want_cap = task.len as usize;
                        let mut buf = recycle_ref
                            .lock()
                            .ok()
                            .and_then(|g| g.try_recv().ok())
                            .unwrap_or_else(|| Vec::with_capacity(want_cap));
                        buf.clear();
                        if buf.capacity() < want_cap {
                            buf.reserve(want_cap);
                        }

                        let t_open = Instant::now();
                        let mut f = match open_sequential_read(&task.path) {
                            Ok(f) => f,
                            Err(e) => {
                                let _ = tx.send(Err(format!(
                                    "не удалось открыть {}: {e}",
                                    task.path.display()
                                )));
                                break;
                            }
                        };
                        let open_dur = t_open.elapsed();

                        let t_read = Instant::now();
                        if let Err(e) = f.read_to_end(&mut buf) {
                            let _ = tx.send(Err(format!(
                                "ошибка чтения {}: {e}",
                                task.zip_name
                            )));
                            break;
                        }
                        let read_dur = t_read.elapsed();

                        if tx.send(Ok((idx, buf, open_dur, read_dur))).is_err() {
                            break;
                        }
                    });
                }
                drop(tx);

                loop {
                    if is_cancelled() {
                        drop(rx);
                        return Err("отменено".into());
                    }
                    let t_wait = Instant::now();
                    let item = match rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(item) => item,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            stats.stream_wait += t_wait.elapsed();
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    stats.stream_wait += t_wait.elapsed();

                    let (idx, mut buf, open_dur, read_dur) = match item {
                        Ok(v) => v,
                        Err(e) => {
                            drop(rx);
                            return Err(e);
                        }
                    };
                    stats.file_open += open_dur;
                    stats.read += read_dur;

                    let task = &entries_ref[idx];
                    let file_len = buf.len() as u64;
                    let opts = zip_options(&ext_lower(&task.path), file_len, zip_mode);

                    let t_start = Instant::now();
                    if let Err(e) = zip.start_file(&task.zip_name, opts) {
                        drop(rx);
                        return Err(format!("ошибка добавления {}: {e}", task.zip_name));
                    }
                    stats.start_file += t_start.elapsed();

                    let t_write = Instant::now();
                    if let Err(e) = zip.write_all(&buf) {
                        drop(rx);
                        return Err(format!("ошибка копирования {}: {e}", task.zip_name));
                    }
                    stats.zip_write += t_write.elapsed();

                    total_in += file_len;
                    stream_bytes += file_len;
                    count += 1;
                    progress.add(file_len);

                    if buf.capacity() > 8 * 1024 * 1024 {
                        buf = Vec::with_capacity(2 * 1024 * 1024);
                    }
                    let _ = recycle_tx.send(buf);

                    maybe_log_progress(
                        &mut last_log,
                        &mut last_log_bytes,
                        stream_bytes,
                        total_in,
                        &stats,
                    );
                }
                Ok(())
            })?;
        }

        if !huge_entries.is_empty() {
            let t_alloc = Instant::now();
            let mut copy_buf = vec![0u8; 4 * 1024 * 1024];
            stats.buf_alloc += t_alloc.elapsed();

            for task in huge_entries {
                if is_cancelled() {
                    return Err("отменено".into());
                }
                let written = write_huge_entry(
                    &mut zip,
                    &task,
                    zip_mode,
                    &mut copy_buf,
                    &mut progress,
                    &mut stats,
                )?;
                total_in += written;
                stream_bytes += written;
                count += 1;

                maybe_log_progress(
                    &mut last_log,
                    &mut last_log_bytes,
                    stream_bytes,
                    total_in,
                    &stats,
                );
            }
        }
        Ok(())
    })();

    if let Err(err) = res {
        drop(zip);
        let _ = fs::remove_file(&out_path);
        return Err(err);
    }

    let t_finish = Instant::now();
    let mut stream_writer = zip
        .finish()
        .map_err(|e| format!("ошибка финализации ZIP: {e}"))?;
    let final_len = io::Seek::stream_position(&mut stream_writer)
        .map_err(|e| format!("ошибка позиции ZIP: {e}"))?;
    let mut buf_writer = stream_writer.into_inner();
    io::Write::flush(&mut buf_writer).map_err(|e| format!("ошибка записи ZIP: {e}"))?;
    let out_file = buf_writer
        .into_inner()
        .map_err(|e| format!("ошибка записи ZIP: {}", e.error()))?;
    if preallocated {
        out_file
            .set_len(final_len)
            .map_err(|e| format!("ошибка усечения ZIP: {e}"))?;
    }
    drop(out_file);
    stats.finish_flush = t_finish.elapsed();

    let elapsed_ms = started.elapsed().as_millis();
    let total_secs = started.elapsed().as_secs_f64().max(0.000_001);
    let pct_of = |d: Duration| (d.as_secs_f64() * 100.0) / total_secs;
    let avg_mbps = (total_in as f64 / 1_048_576.0) / total_secs;
    let output_bytes = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);

    crate::log::info(&format!(
        "zip-pack done: files={}, in={:.1} MB, out={:.1} MB, total={} ms ({:.1} MB/s) | scan={}ms({:.1}%) canon={}ms({:.1}%) batch={}ms({:.1}%) merge={}ms({:.1}%) wait={}ms({:.1}%) meta={}ms({:.1}%) start_file={}ms({:.1}%) open={}ms({:.1}%) alloc={}ms({:.1}%) read={}ms({:.1}%) write={}ms({:.1}%) finish={}ms({:.1}%)",
        count,
        total_in as f64 / 1_048_576.0,
        output_bytes as f64 / 1_048_576.0,
        elapsed_ms,
        avg_mbps,
        stats.scan_total.as_millis(),
        pct_of(stats.scan_total),
        stats.scan_canonicalize.as_millis(),
        pct_of(stats.scan_canonicalize),
        stats.batch_total.as_millis(),
        pct_of(stats.batch_total),
        stats.batch_merge.as_millis(),
        pct_of(stats.batch_merge),
        stats.stream_wait.as_millis(),
        pct_of(stats.stream_wait),
        stats.entry_meta.as_millis(),
        pct_of(stats.entry_meta),
        stats.start_file.as_millis(),
        pct_of(stats.start_file),
        stats.file_open.as_millis(),
        pct_of(stats.file_open),
        stats.buf_alloc.as_millis(),
        pct_of(stats.buf_alloc),
        stats.read.as_millis(),
        pct_of(stats.read),
        stats.zip_write.as_millis(),
        pct_of(stats.zip_write),
        stats.finish_flush.as_millis(),
        pct_of(stats.finish_flush),
    ));

    Ok(QuickFileReport {
        kind: "zip-pack".into(),
        output_path: out_path.to_string_lossy().into_owned(),
        count,
        input_bytes: total_in,
        output_bytes,
        elapsed_ms,
        detail: out_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("archive.zip")
            .to_string(),
        deleted_original: false,
    })
}

fn write_huge_entry<W: Write + io::Seek>(
    zip: &mut ZipWriter<W>,
    task: &ZipEntryTask,
    zip_mode: &str,
    copy_buf: &mut [u8],
    progress: &mut ProgressReporter<'_>,
    stats: &mut ZipPackStats,
) -> Result<u64, String> {
    let opts = zip_options(&ext_lower(&task.path), task.len, zip_mode);

    let t_start = Instant::now();
    zip.start_file(&task.zip_name, opts)
        .map_err(|e| format!("ошибка добавления {}: {e}", task.zip_name))?;
    stats.start_file += t_start.elapsed();

    let t_open = Instant::now();
    let mut file = open_sequential_read(&task.path)
        .map_err(|e| format!("не удалось открыть {}: {e}", task.path.display()))?;
    stats.file_open += t_open.elapsed();

    let mut total = 0u64;
    loop {
        if is_cancelled() {
            return Err("отменено".into());
        }
        let t_read = Instant::now();
        let n = file
            .read(copy_buf)
            .map_err(|e| format!("ошибка чтения {}: {e}", task.zip_name))?;
        stats.read += t_read.elapsed();
        if n == 0 {
            break;
        }
        let t_write = Instant::now();
        zip.write_all(&copy_buf[..n])
            .map_err(|e| format!("ошибка копирования {}: {e}", task.zip_name))?;
        stats.zip_write += t_write.elapsed();
        total += n as u64;
        progress.add(n as u64);
    }
    Ok(total)
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

    crate::log::info(&format!(
        "zip-unpack start: archive={}, in={:.1} MB, out={}",
        path.display(),
        input_bytes as f64 / 1_048_576.0,
        out_dir.display()
    ));

    let file = open_sequential_read(&path)
        .map_err(|e| format!("не удалось открыть архив {}: {e}", path.display()))?;
    let reader = BufReader::with_capacity(UNPACK_BUF_SIZE, file);
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
    let mut created_dirs: HashSet<PathBuf> = HashSet::new();
    created_dirs.insert(out_dir.clone());
    let mut buf = vec![0u8; UNPACK_BUF_SIZE];
    let mut open_dur = Duration::ZERO;
    let mut read_dur = Duration::ZERO;
    let mut write_dur = Duration::ZERO;

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
                if created_dirs.insert(out_path.clone()) {
                    fs::create_dir_all(&out_path)
                        .map_err(|e| format!("не удалось создать папку: {e}"))?;
                }
                continue;
            }
            if let Some(p) = out_path.parent() {
                if created_dirs.insert(p.to_path_buf()) {
                    fs::create_dir_all(p).map_err(|e| format!("не удалось создать папку: {e}"))?;
                }
            }
            let t_open = Instant::now();
            let mut out_file = create_sequential_write(&out_path)
                .map_err(|e| format!("не удалось создать файл {}: {e}", out_path.display()))?;
            open_dur += t_open.elapsed();

            loop {
                if is_cancelled() {
                    return Err("отменено".into());
                }
                let t_read = Instant::now();
                let n = entry
                    .read(&mut buf)
                    .map_err(|e| format!("ошибка распаковки {}: {e}", out_path.display()))?;
                read_dur += t_read.elapsed();
                if n == 0 {
                    break;
                }
                let t_write = Instant::now();
                out_file
                    .write_all(&buf[..n])
                    .map_err(|e| format!("ошибка записи {}: {e}", out_path.display()))?;
                write_dur += t_write.elapsed();
                output_bytes += n as u64;
                progress.add(n as u64);
            }
            count += 1;
        }
        Ok(())
    })();

    if let Err(err) = res {
        drop(archive);
        let _ = fs::remove_dir_all(&out_dir);
        return Err(err);
    }

    let elapsed_ms = started.elapsed().as_millis();
    let secs = started.elapsed().as_secs_f64().max(0.000_001);
    crate::log::info(&format!(
        "zip-unpack done: files={}, out={:.1} MB, total={} ms ({:.1} MB/s) | open={}ms read={}ms write={}ms",
        count,
        output_bytes as f64 / 1_048_576.0,
        elapsed_ms,
        (output_bytes as f64 / 1_048_576.0) / secs,
        open_dur.as_millis(),
        read_dur.as_millis(),
        write_dur.as_millis(),
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
