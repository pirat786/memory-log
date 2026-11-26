use std::collections::HashSet;
use std::fs;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tauri::AppHandle;
use zip::ZipArchive;

use super::{detect_hdd_mode, UNPACK_BUF_HDD, UNPACK_BUF_SSD};
use crate::quick_files::io_hints::{create_sequential_write, open_sequential_read};
use crate::quick_files::{is_cancelled, unique_path, ProgressReporter, QuickFileReport};

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
    let mut buf = vec![0u8; buf_size];
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
            if entry.size() >= 1024 * 1024 {
                let _ = out_file.set_len(entry.size());
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
