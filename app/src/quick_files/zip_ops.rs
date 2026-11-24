use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Cursor};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

use tauri::AppHandle;
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use super::{
    copy_stream, ext_lower, is_cancelled, unique_path, ProgressReporter, QuickFileReport,
    IO_BUF_SIZE,
};

const PARALLEL_FILE_MAX_BYTES: u64 = 16 * 1024 * 1024;
const PARALLEL_CHUNK_MAX_BYTES: u64 = 8 * 1024 * 1024;
const PARALLEL_CHUNK_MAX_FILES: usize = 64;

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
                if len <= PARALLEL_FILE_MAX_BYTES && should_compress(&ext_lower(p), zip_mode) {
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
    copy_stream(&mut reader, zip, progress)
        .map_err(|e| format!("ошибка копирования {zip_name}: {e}"))
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
