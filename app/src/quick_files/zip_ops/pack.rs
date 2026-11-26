use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tauri::AppHandle;
use walkdir::WalkDir;
use zip::ZipWriter;

use super::bg_writer::BgFileWriter;
use super::parallel::{build_chunks, run_parallel_hdd, run_parallel_ssd};
use super::{
    detect_hdd_mode, should_compress, zip_options, ZipEntryTask, ZipPackStats, HUGE_BUF_HDD,
    HUGE_BUF_SSD, PARALLEL_FILE_MAX_BYTES,
};
use crate::quick_files::io_hints::{create_sequential_write, open_sequential_read, same_drive};
use crate::quick_files::{ext_lower, is_cancelled, unique_path, ProgressReporter, QuickFileReport};

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
    let hdd_mode = detect_hdd_mode(&out_path, first);
    let same_drv = same_drive(&out_path, first);
    crate::log::info(&format!(
        "zip-pack start: paths={}, mode={}, hdd={}, same_drive={}, out={}",
        valid.len(),
        zip_mode,
        hdd_mode,
        same_drv,
        out_path.display()
    ));

    let scan_started = Instant::now();
    let out_canon = timed_canonicalize(out_dir, &mut stats.scan_canonicalize)
        .map(|d| d.join(out_path.file_name().unwrap_or_default()));

    let mut total_expected = 0u64;
    let mut compressible_bytes = 0u64;
    let mut incompressible_bytes = 0u64;
    let mut parallel_entries: Vec<ZipEntryTask> = Vec::new();
    let mut huge_entries: Vec<ZipEntryTask> = Vec::new();

    for root in &valid {
        if is_cancelled() {
            return Err("отменено".into());
        }
        if root.is_file() {
            if root
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
                && out_canon.as_ref()
                    == timed_canonicalize(root, &mut stats.scan_canonicalize).as_ref()
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
            if should_compress(&ext_lower(root), zip_mode) {
                compressible_bytes = compressible_bytes.saturating_add(len);
            } else {
                incompressible_bytes = incompressible_bytes.saturating_add(len);
            }
            let task = ZipEntryTask {
                path: root.clone(),
                zip_name: name,
                len,
            };
            if len <= PARALLEL_FILE_MAX_BYTES {
                parallel_entries.push(task);
            } else {
                huge_entries.push(task);
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
                if should_compress(&ext_lower(p), zip_mode) {
                    compressible_bytes = compressible_bytes.saturating_add(len);
                } else {
                    incompressible_bytes = incompressible_bytes.saturating_add(len);
                }
                let task = ZipEntryTask {
                    path: p.to_path_buf(),
                    zip_name,
                    len,
                };
                if len <= PARALLEL_FILE_MAX_BYTES {
                    parallel_entries.push(task);
                } else {
                    huge_entries.push(task);
                }
            }
        }
    }
    stats.scan_total = scan_started.elapsed();

    let total_files = parallel_entries.len() + huge_entries.len();
    let out_file = create_sequential_write(&out_path)
        .map_err(|e| format!("не удалось создать архив {}: {e}", out_path.display()))?;
    let preallocated = if total_expected >= 1024 * 1024 {
        let est = incompressible_bytes
            .saturating_add(compressible_bytes / 2)
            .saturating_add((total_files as u64).saturating_mul(128))
            .saturating_add(65_536);
        out_file.set_len(est).is_ok()
    } else {
        false
    };

    crate::log::info(&format!(
        "zip-pack scan: {} ms (canonicalize={} ms), parallel={}, huge={}, total={:.1} MB, prealloc={}",
        stats.scan_total.as_millis(),
        stats.scan_canonicalize.as_millis(),
        parallel_entries.len(),
        huge_entries.len(),
        total_expected as f64 / 1_048_576.0,
        preallocated
    ));

    let mut progress = ProgressReporter::new(app, total_expected);

    let (bg_chunk_size, bg_slots) = if hdd_mode && same_drv {
        (64 * 1024 * 1024, 2)
    } else if hdd_mode {
        (32 * 1024 * 1024, 3)
    } else {
        (16 * 1024 * 1024, 4)
    };
    let (bg_writer, send_wait) = BgFileWriter::new(out_file, bg_chunk_size, bg_slots);
    let mut zip = ZipWriter::new_stream(bg_writer);

    let mut total_in = 0u64;
    let mut count = 0usize;
    let mut last_log = Instant::now();
    let mut last_log_bytes = 0u64;

    let maybe_log_progress = |last_log: &mut Instant,
                              last_log_bytes: &mut u64,
                              total_in: u64,
                              stats: &ZipPackStats,
                              send_wait: &AtomicU64| {
        if last_log.elapsed() >= Duration::from_secs(5)
            || total_in.saturating_sub(*last_log_bytes) >= 1024 * 1024 * 1024
        {
            let interval_secs = last_log.elapsed().as_secs_f64().max(0.001);
            let interval_mb = (total_in - *last_log_bytes) as f64 / 1_048_576.0;
            let interval_mbps = interval_mb / interval_secs;
            let avg_secs = started.elapsed().as_secs_f64().max(0.001);
            let avg_mbps = (total_in as f64 / 1_048_576.0) / avg_secs;
            let pct = if total_expected == 0 {
                100.0
            } else {
                (total_in as f64 * 100.0) / total_expected as f64
            };
            let bg_wait_ms = Duration::from_nanos(send_wait.load(Ordering::Relaxed)).as_millis();
            crate::log::info(&format!(
                "zip-pack progress: {:.2}/{:.2} GB ({:.1}%), interval={:.1} MB/s, avg={:.1} MB/s | wait={}ms merge={}ms open={}ms read={}ms write={}ms bg_wait={}ms",
                total_in as f64 / 1_073_741_824.0,
                total_expected as f64 / 1_073_741_824.0,
                pct,
                interval_mbps,
                avg_mbps,
                stats.stream_wait.as_millis(),
                stats.batch_merge.as_millis(),
                stats.file_open.as_millis(),
                stats.read.as_millis(),
                stats.zip_write.as_millis(),
                bg_wait_ms,
            ));
            *last_log = Instant::now();
            *last_log_bytes = total_in;
        }
    };

    let res: Result<(), String> = (|| {
        if !parallel_entries.is_empty() {
            let par_started = Instant::now();
            let chunks = build_chunks(parallel_entries);
            let workers = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .clamp(if hdd_mode { 2 } else { 4 }, 8)
                .min(chunks.len());

            if hdd_mode {
                run_parallel_hdd(
                    chunks,
                    workers,
                    zip_mode,
                    &mut zip,
                    &mut total_in,
                    &mut count,
                    &mut progress,
                    &mut stats,
                    &mut |cur_in, st| {
                        maybe_log_progress(
                            &mut last_log,
                            &mut last_log_bytes,
                            cur_in,
                            st,
                            &send_wait,
                        )
                    },
                )?;
            } else {
                run_parallel_ssd(
                    chunks,
                    workers,
                    zip_mode,
                    &mut zip,
                    &mut total_in,
                    &mut count,
                    &mut progress,
                    &mut stats,
                    &mut |cur_in, st| {
                        maybe_log_progress(
                            &mut last_log,
                            &mut last_log_bytes,
                            cur_in,
                            st,
                            &send_wait,
                        )
                    },
                )?;
            }
            stats.parallel_total = par_started.elapsed();
            crate::log::info(&format!(
                "zip-pack parallel ({}): {} ms (merge={} ms), files={}, input={:.1} MB",
                if hdd_mode { "hdd" } else { "ssd" },
                stats.parallel_total.as_millis(),
                stats.batch_merge.as_millis(),
                count,
                total_in as f64 / 1_048_576.0
            ));
        }

        if !huge_entries.is_empty() {
            let buf_cap = if hdd_mode { HUGE_BUF_HDD } else { HUGE_BUF_SSD };
            let t_alloc = Instant::now();
            let mut copy_buf = vec![0u8; buf_cap];
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
                count += 1;

                maybe_log_progress(
                    &mut last_log,
                    &mut last_log_bytes,
                    total_in,
                    &stats,
                    &send_wait,
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
    let bg_writer = stream_writer.into_inner();
    let out_file = bg_writer
        .finish()
        .map_err(|e| format!("ошибка записи ZIP: {e}"))?;
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
    let bg_wait_dur = Duration::from_nanos(send_wait.load(Ordering::Relaxed));
    let output_bytes = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);

    crate::log::info(&format!(
        "zip-pack done: files={}, in={:.1} MB, out={:.1} MB, total={} ms ({:.1} MB/s) | scan={}ms({:.1}%) canon={}ms({:.1}%) parallel={}ms({:.1}%) merge={}ms({:.1}%) wait={}ms({:.1}%) start_file={}ms({:.1}%) open={}ms({:.1}%) alloc={}ms({:.1}%) read={}ms({:.1}%) write={}ms({:.1}%) bg_wait={}ms({:.1}%) finish={}ms({:.1}%)",
        count,
        total_in as f64 / 1_048_576.0,
        output_bytes as f64 / 1_048_576.0,
        elapsed_ms,
        avg_mbps,
        stats.scan_total.as_millis(),
        pct_of(stats.scan_total),
        stats.scan_canonicalize.as_millis(),
        pct_of(stats.scan_canonicalize),
        stats.parallel_total.as_millis(),
        pct_of(stats.parallel_total),
        stats.batch_merge.as_millis(),
        pct_of(stats.batch_merge),
        stats.stream_wait.as_millis(),
        pct_of(stats.stream_wait),
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
        bg_wait_dur.as_millis(),
        pct_of(bg_wait_dur),
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
