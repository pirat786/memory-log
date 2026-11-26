use std::collections::HashSet;
use std::fs;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tauri::AppHandle;
use zip::ZipArchive;

use super::{check_free_space, detect_hdd_mode, UNPACK_BUF_HDD, UNPACK_BUF_SSD};
use crate::quick_files::io_hints::{create_sequential_write, open_sequential_read};
use crate::quick_files::{is_cancelled, unique_path, ProgressReporter, QuickFileReport};

const PARALLEL_UNPACK_BUF: usize = 1024 * 1024;
const PARALLEL_READER_BUF: usize = 512 * 1024;
const SMALL_UNPACK_MAX_BYTES: u64 = 512 * 1024;
const SET_LEN_MIN_BYTES: u64 = 4 * 1024 * 1024;

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

    let hdd_mode = detect_hdd_mode(&out_dir, &path);
    let buf_size = if hdd_mode {
        UNPACK_BUF_HDD
    } else {
        UNPACK_BUF_SSD
    };

    crate::log::info(&format!(
        "zip-unpack start: archive={}, in={:.1} MB, hdd={}, out={}",
        path.display(),
        input_bytes as f64 / 1_048_576.0,
        hdd_mode,
        out_dir.display()
    ));

    let file = open_sequential_read(&path)
        .map_err(|e| format!("не удалось открыть архив {}: {e}", path.display()))?;
    let reader = BufReader::with_capacity(buf_size, file);
    let mut archive = match ZipArchive::new(reader) {
        Ok(a) => a,
        Err(e) => {
            let _ = fs::remove_dir_all(&out_dir);
            return Err(format!("повреждённый или неподдерживаемый ZIP: {e}"));
        }
    };

    let mut total_expected = 0u64;
    let mut created_dirs: HashSet<PathBuf> = HashSet::new();
    created_dirs.insert(out_dir.clone());
    let mut files: Vec<(usize, PathBuf, u64)> = Vec::new();

    let scan_res: Result<(), String> = (|| {
        for i in 0..archive.len() {
            if is_cancelled() {
                return Err("отменено".into());
            }
            let entry = archive
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
                    fs::create_dir_all(p)
                        .map_err(|e| format!("не удалось создать папку: {e}"))?;
                }
            }
            let size = entry.size();
            total_expected = total_expected.saturating_add(size);
            files.push((i, out_path, size));
        }
        Ok(())
    })();

    if let Err(e) = scan_res {
        drop(archive);
        let _ = fs::remove_dir_all(&out_dir);
        return Err(e);
    }

    if let Err(e) = check_free_space(parent, total_expected) {
        drop(archive);
        let _ = fs::remove_dir_all(&out_dir);
        return Err(e);
    }

    let mut progress = ProgressReporter::new(app, total_expected);
    let mut count = 0usize;
    let mut output_bytes = 0u64;
    let mut open_dur = Duration::ZERO;
    let mut read_dur = Duration::ZERO;
    let mut write_dur = Duration::ZERO;

    let res: Result<(), String> = if !hdd_mode && files.len() >= 8 {
        drop(archive);
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 4)
            .min(files.len());
        let next_idx = AtomicUsize::new(0);
        let (tx, rx) = mpsc::channel::<Result<(u64, Duration, Duration, Duration), String>>();

        std::thread::scope(|s| -> Result<(), String> {
            for _ in 0..workers {
                let path_ref = &path;
                let files_ref = &files;
                let idx_ref = &next_idx;
                let tx = tx.clone();
                s.spawn(move || {
                    let f = match open_sequential_read(path_ref) {
                        Ok(f) => f,
                        Err(e) => {
                            let _ = tx.send(Err(format!(
                                "не удалось открыть архив {}: {e}",
                                path_ref.display()
                            )));
                            return;
                        }
                    };
                    let mut arch = match ZipArchive::new(BufReader::with_capacity(
                        PARALLEL_READER_BUF,
                        f,
                    )) {
                        Ok(a) => a,
                        Err(e) => {
                            let _ = tx.send(Err(format!("ошибка чтения архива: {e}")));
                            return;
                        }
                    };
                    let mut buf = vec![0u8; PARALLEL_UNPACK_BUF];
                    let mut small_buf = Vec::with_capacity(SMALL_UNPACK_MAX_BYTES as usize);
                    loop {
                        if is_cancelled() {
                            break;
                        }
                        let pos = idx_ref.fetch_add(1, Ordering::Relaxed);
                        if pos >= files_ref.len() {
                            break;
                        }
                        let (entry_idx, ref out_path, size) = files_ref[pos];
                        let mut entry = match arch.by_index(entry_idx) {
                            Ok(e) => e,
                            Err(e) => {
                                let _ = tx.send(Err(format!(
                                    "ошибка чтения элемента архива: {e}"
                                )));
                                break;
                            }
                        };
                        let t_open = Instant::now();
                        let mut out_file = match create_sequential_write(out_path) {
                            Ok(f) => f,
                            Err(e) => {
                                let _ = tx.send(Err(format!(
                                    "не удалось создать файл {}: {e}",
                                    out_path.display()
                                )));
                                break;
                            }
                        };
                        if size >= SET_LEN_MIN_BYTES {
                            let _ = out_file.set_len(size);
                        }
                        let mut f_open = t_open.elapsed();
                        let mut f_read = Duration::ZERO;
                        let mut f_write = Duration::ZERO;

                        if size <= SMALL_UNPACK_MAX_BYTES {
                            small_buf.clear();
                            let t_read = Instant::now();
                            if let Err(e) = entry.read_to_end(&mut small_buf) {
                                let _ = tx.send(Err(format!(
                                    "ошибка распаковки {}: {e}",
                                    out_path.display()
                                )));
                                return;
                            }
                            f_read += t_read.elapsed();
                            let t_write = Instant::now();
                            if let Err(e) = out_file.write_all(&small_buf) {
                                let _ = tx.send(Err(format!(
                                    "ошибка записи {}: {e}",
                                    out_path.display()
                                )));
                                return;
                            }
                            f_write += t_write.elapsed();
                            if tx
                                .send(Ok((small_buf.len() as u64, f_open, f_read, f_write)))
                                .is_err()
                            {
                                return;
                            }
                            continue;
                        }

                        loop {
                            if is_cancelled() {
                                return;
                            }
                            let t_read = Instant::now();
                            let n = match entry.read(&mut buf) {
                                Ok(n) => n,
                                Err(e) => {
                                    let _ = tx.send(Err(format!(
                                        "ошибка распаковки {}: {e}",
                                        out_path.display()
                                    )));
                                    return;
                                }
                            };
                            f_read += t_read.elapsed();
                            if n == 0 {
                                break;
                            }
                            let t_write = Instant::now();
                            if let Err(e) = out_file.write_all(&buf[..n]) {
                                let _ = tx.send(Err(format!(
                                    "ошибка записи {}: {e}",
                                    out_path.display()
                                )));
                                return;
                            }
                            f_write += t_write.elapsed();
                            if tx
                                .send(Ok((
                                    n as u64,
                                    std::mem::take(&mut f_open),
                                    std::mem::take(&mut f_read),
                                    std::mem::take(&mut f_write),
                                )))
                                .is_err()
                            {
                                return;
                            }
                        }
                        if !f_open.is_zero() || !f_read.is_zero() || !f_write.is_zero() {
                            if tx.send(Ok((0, f_open, f_read, f_write))).is_err() {
                                return;
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
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(Ok((n, od, rd, wd))) => {
                        open_dur += od;
                        read_dur += rd;
                        write_dur += wd;
                        if n > 0 {
                            output_bytes += n;
                            progress.add(n);
                        }
                    }
                    Ok(Err(e)) => {
                        drop(rx);
                        return Err(e);
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            count = files.len();
            Ok(())
        })
    } else {
        let mut buf = vec![0u8; buf_size];
        let seq_res: Result<(), String> = (|| {
            for (entry_idx, out_path, size) in &files {
                if is_cancelled() {
                    return Err("отменено".into());
                }
                let mut entry = archive
                    .by_index(*entry_idx)
                    .map_err(|e| format!("ошибка чтения элемента архива: {e}"))?;
                let t_open = Instant::now();
                let mut out_file = create_sequential_write(out_path)
                    .map_err(|e| format!("не удалось создать файл {}: {e}", out_path.display()))?;
                if *size >= SET_LEN_MIN_BYTES {
                    let _ = out_file.set_len(*size);
                }
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
        drop(archive);
        seq_res
    };

    if let Err(err) = res {
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
