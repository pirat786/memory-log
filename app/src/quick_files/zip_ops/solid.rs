use std::collections::HashSet;
use std::fs;
use std::io::{self, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use flate2::read::GzDecoder;
use tauri::AppHandle;
use walkdir::WalkDir;

use super::bg_writer::{BgFileWriter, BoundedQueue};
use super::{
    check_free_space, detect_hdd_mode, is_incompressible_ext, should_compress_task,
    unique_entry_name, ZipEntryTask, HUGE_BUF_HDD, HUGE_BUF_SSD, UNPACK_BUF_HDD, UNPACK_BUF_SSD,
};
use crate::quick_files::io_hints::{create_sequential_write, open_sequential_read, same_drive};
use crate::quick_files::{ext_lower, is_cancelled, unique_path, ProgressReporter, QuickFileReport};

const DEFAULT_SOLID_ZSTD_LEVEL: i32 = 12;
pub const ULTRA_SOLID_ZSTD_LEVEL: i32 = 19;
const SOLID_FAST_FRAME_LEVEL: i32 = -5;
const SOLID_SMALL_FILE_MAX: u64 = 2 * 1024 * 1024;
const SOLID_BATCH_MAX_FILES: usize = 32;
const SOLID_BATCH_MAX_BYTES: u64 = 16 * 1024 * 1024;
const SOLID_UNPACK_QUEUE_MAX: usize = 32;

fn solid_config(default_level: i32) -> (i32, bool) {
    let level = std::env::var("ML_SOLID_LEVEL")
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
        .map(|l| l.clamp(-5, 22))
        .unwrap_or(default_level);
    let hybrid = std::env::var("ML_SOLID_HYBRID")
        .ok()
        .map(|v| v != "0")
        .unwrap_or(true);
    (level, hybrid)
}

struct MultiFrameZstdWriter<W: Write> {
    enc: Option<zstd::stream::write::Encoder<'static, W>>,
}

impl<W: Write> MultiFrameZstdWriter<W> {
    fn open_encoder(
        inner: W,
        level: i32,
        workers: u32,
        ldm: bool,
    ) -> io::Result<zstd::stream::write::Encoder<'static, W>> {
        let mut enc = zstd::stream::write::Encoder::new(inner, level)?;
        let _ = enc.multithread(workers);
        if ldm {
            let _ = enc.long_distance_matching(true);
        }
        Ok(enc)
    }

    fn new(inner: W, level: i32, workers: u32, ldm: bool) -> io::Result<Self> {
        Ok(Self {
            enc: Some(Self::open_encoder(inner, level, workers, ldm)?),
        })
    }

    fn switch_frame(&mut self, level: i32, workers: u32, ldm: bool) -> io::Result<()> {
        let cur = self
            .enc
            .take()
            .ok_or_else(|| io::Error::other("zstd encoder missing"))?;
        let inner = cur.finish()?;
        self.enc = Some(Self::open_encoder(inner, level, workers, ldm)?);
        Ok(())
    }

    fn finish(mut self) -> io::Result<W> {
        let cur = self
            .enc
            .take()
            .ok_or_else(|| io::Error::other("zstd encoder missing"))?;
        cur.finish()
    }
}

impl<W: Write> Write for MultiFrameZstdWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.enc
            .as_mut()
            .ok_or_else(|| io::Error::other("zstd encoder closed"))?
            .write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.enc
            .as_mut()
            .ok_or_else(|| io::Error::other("zstd encoder closed"))?
            .flush()
    }
}

fn maybe_log_solid_pack(
    read_bytes: u64,
    total_expected: u64,
    started: Instant,
    last_log: &mut Instant,
    last_log_bytes: &mut u64,
) {
    if last_log.elapsed() >= Duration::from_secs(5)
        || read_bytes.saturating_sub(*last_log_bytes) >= 1024 * 1024 * 1024
    {
        let dt = last_log.elapsed().as_secs_f64().max(0.001);
        let interval_mbps = ((read_bytes - *last_log_bytes) as f64 / 1_048_576.0) / dt;
        let avg_secs = started.elapsed().as_secs_f64().max(0.001);
        let avg_mbps = (read_bytes as f64 / 1_048_576.0) / avg_secs;
        let pct = if total_expected == 0 {
            100.0
        } else {
            (read_bytes as f64 * 100.0) / total_expected as f64
        };
        crate::log::info(&format!(
            "solid-pack progress: {:.2}/{:.2} GB ({:.1}%), interval={:.1} MB/s, avg={:.1} MB/s",
            read_bytes as f64 / 1_073_741_824.0,
            total_expected as f64 / 1_073_741_824.0,
            pct,
            interval_mbps,
            avg_mbps,
        ));
        *last_log = Instant::now();
        *last_log_bytes = read_bytes;
    }
}

struct CancelProgressReader<'a, 'b, R: Read> {
    inner: R,
    progress: &'a mut ProgressReporter<'b>,
    read_bytes: u64,
    last_log: &'a mut Instant,
    last_log_bytes: &'a mut u64,
    started: Instant,
    total_expected: u64,
}

impl<'a, 'b, R: Read> Read for CancelProgressReader<'a, 'b, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "отменено"));
        }
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.read_bytes += n as u64;
            self.progress.add(n as u64);
            maybe_log_solid_pack(
                self.read_bytes,
                self.total_expected,
                self.started,
                self.last_log,
                self.last_log_bytes,
            );
        }
        Ok(n)
    }
}

enum SolidBatch {
    Loaded(Vec<(String, fs::Metadata, Vec<u8>)>),
    Stream(ZipEntryTask),
}

fn read_small_batch(batch: Vec<ZipEntryTask>) -> Result<Vec<(String, fs::Metadata, Vec<u8>)>, String> {
    if batch.len() <= 1 {
        let mut out = Vec::with_capacity(batch.len());
        for task in batch {
            if is_cancelled() {
                return Err("отменено".into());
            }
            let file = open_sequential_read(&task.path)
                .map_err(|e| format!("не удалось открыть {}: {e}", task.path.display()))?;
            let meta = file
                .metadata()
                .map_err(|e| format!("ошибка метаданных {}: {e}", task.path.display()))?;
            let file_len = meta.len();
            let mut data = Vec::with_capacity(file_len as usize);
            file.take(file_len)
                .read_to_end(&mut data)
                .map_err(|e| format!("ошибка чтения {}: {e}", task.path.display()))?;
            out.push((task.zip_name, meta, data));
        }
        return Ok(out);
    }

    let workers = 4.min(batch.len());
    let next_idx = AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<(String, fs::Metadata, Vec<u8>)>>> =
        (0..batch.len()).map(|_| Mutex::new(None)).collect();
    let first_err: Mutex<Option<String>> = Mutex::new(None);

    std::thread::scope(|s| {
        for _ in 0..workers {
            let batch_ref = &batch;
            let idx_ref = &next_idx;
            let slots_ref = &slots;
            let err_ref = &first_err;
            s.spawn(move || loop {
                if is_cancelled() || err_ref.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                    break;
                }
                let i = idx_ref.fetch_add(1, Ordering::Relaxed);
                if i >= batch_ref.len() {
                    break;
                }
                let task = &batch_ref[i];
                let file = match open_sequential_read(&task.path) {
                    Ok(f) => f,
                    Err(e) => {
                        let mut g = err_ref.lock().unwrap_or_else(|e| e.into_inner());
                        if g.is_none() {
                            *g = Some(format!("не удалось открыть {}: {e}", task.path.display()));
                        }
                        break;
                    }
                };
                let meta = match file.metadata() {
                    Ok(m) => m,
                    Err(e) => {
                        let mut g = err_ref.lock().unwrap_or_else(|e| e.into_inner());
                        if g.is_none() {
                            *g = Some(format!("ошибка метаданных {}: {e}", task.path.display()));
                        }
                        break;
                    }
                };
                let file_len = meta.len();
                let mut data = Vec::with_capacity(file_len as usize);
                if let Err(e) = file.take(file_len).read_to_end(&mut data) {
                    let mut g = err_ref.lock().unwrap_or_else(|e| e.into_inner());
                    if g.is_none() {
                        *g = Some(format!("ошибка чтения {}: {e}", task.path.display()));
                    }
                    break;
                }
                *slots_ref[i].lock().unwrap_or_else(|e| e.into_inner()) =
                    Some((task.zip_name.clone(), meta, data));
            });
        }
    });

    if let Some(e) = first_err.into_inner().unwrap_or_else(|e| e.into_inner()) {
        return Err(e);
    }
    if is_cancelled() {
        return Err("отменено".into());
    }
    let mut out = Vec::with_capacity(batch.len());
    for slot in slots {
        if let Some(item) = slot.into_inner().unwrap_or_else(|e| e.into_inner()) {
            out.push(item);
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn append_solid_task_stream<W: Write>(
    builder: &mut tar::Builder<W>,
    task: &ZipEntryTask,
    read_buf_size: usize,
    progress: &mut ProgressReporter<'_>,
    total_in: &mut u64,
    count: &mut usize,
    last_log: &mut Instant,
    last_log_bytes: &mut u64,
    started: Instant,
    total_expected: u64,
) -> Result<(), String> {
    if is_cancelled() {
        return Err("отменено".into());
    }
    let file = open_sequential_read(&task.path)
        .map_err(|e| format!("не удалось открыть {}: {e}", task.path.display()))?;
    let meta = file
        .metadata()
        .map_err(|e| format!("ошибка метаданных {}: {e}", task.path.display()))?;
    let file_len = meta.len();

    let mut header = tar::Header::new_gnu();
    header.set_metadata(&meta);
    header.set_size(file_len);
    header.set_cksum();

    let mut reader = CancelProgressReader {
        inner: BufReader::with_capacity(read_buf_size, file.take(file_len)),
        progress,
        read_bytes: *total_in,
        last_log,
        last_log_bytes,
        started,
        total_expected,
    };

    if let Err(e) = builder.append_data(&mut header, &task.zip_name, &mut reader) {
        if is_cancelled() || e.kind() == io::ErrorKind::Interrupted {
            return Err("отменено".into());
        }
        return Err(format!("ошибка упаковки {}: {e}", task.zip_name));
    }
    *total_in = reader.read_bytes;
    *count += 1;
    Ok(())
}

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

pub fn pack_solid(paths: Vec<String>, app: Option<&AppHandle>) -> Result<QuickFileReport, String> {
    pack_solid_with_level(paths, DEFAULT_SOLID_ZSTD_LEVEL, app)
}

pub fn pack_solid_with_level(
    paths: Vec<String>,
    default_level: i32,
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

    let out_path = unique_path(out_dir, &base_stem, "tar.zst");
    let hdd_mode = detect_hdd_mode(&out_path, first);
    let same_drv = same_drive(&out_path, first);
    let (solid_level, hybrid) = solid_config(default_level);

    let out_canon = fs::canonicalize(out_dir)
        .ok()
        .map(|d| d.join(out_path.file_name().unwrap_or_default()));

    let mut total_expected = 0u64;
    let mut entries: Vec<ZipEntryTask> = Vec::new();
    let mut seen_dirs: Vec<(PathBuf, String)> = Vec::new();
    let mut has_children: HashSet<PathBuf> = HashSet::new();
    let mut used_names: HashSet<String> = HashSet::new();

    for root in &valid {
        if is_cancelled() {
            return Err("отменено".into());
        }
        if root.is_file() {
            if out_canon.as_ref() == fs::canonicalize(root).ok().as_ref() {
                continue;
            }
            let len = fs::metadata(root).map(|m| m.len()).unwrap_or(0);
            let raw_name = root
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("file")
                .to_string();
            let name = unique_entry_name(&mut used_names, raw_name);
            total_expected = total_expected.saturating_add(len);
            entries.push(ZipEntryTask {
                path: root.clone(),
                zip_name: name,
                len,
                mtime: None,
            });
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
                if let Some(parent) = p.parent() {
                    has_children.insert(parent.to_path_buf());
                }
                if p.is_dir() {
                    let rel = p.strip_prefix(root).unwrap_or(p);
                    if !rel.as_os_str().is_empty() {
                        let rel_str = rel.to_string_lossy().replace('\\', "/");
                        let dir_name = if valid.len() == 1 {
                            rel_str
                        } else {
                            format!("{root_name}/{rel_str}")
                        };
                        seen_dirs.push((p.to_path_buf(), dir_name));
                    }
                    continue;
                }
                if out_canon.as_ref() == fs::canonicalize(p).ok().as_ref() {
                    continue;
                }
                let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                let rel = p.strip_prefix(root).unwrap_or(p);
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                let raw_zip_name = if valid.len() == 1 {
                    rel_str
                } else {
                    format!("{root_name}/{rel_str}")
                };
                let zip_name = unique_entry_name(&mut used_names, raw_zip_name);
                total_expected = total_expected.saturating_add(len);
                entries.push(ZipEntryTask {
                    path: p.to_path_buf(),
                    zip_name,
                    len,
                    mtime: None,
                });
            }
        }
    }
    let empty_dirs: Vec<(PathBuf, String)> = seen_dirs
        .into_iter()
        .filter(|(p, _)| !has_children.contains(p))
        .collect();

    check_free_space(out_dir, total_expected / 2)?;

    let (mut comp_entries, mut incomp_entries) = if hybrid {
        let mut comp = Vec::new();
        let mut incomp = Vec::new();
        for t in entries {
            let is_comp = if hdd_mode {
                !is_incompressible_ext(&ext_lower(&t.path))
            } else {
                should_compress_task(&t.path, t.len, "fast")
            };
            if is_comp {
                comp.push(t);
            } else {
                incomp.push(t);
            }
        }
        let sort_fn = |a: &ZipEntryTask, b: &ZipEntryTask| {
            ext_lower(&a.path)
                .cmp(&ext_lower(&b.path))
                .then_with(|| a.zip_name.cmp(&b.zip_name))
        };
        comp.sort_by(sort_fn);
        incomp.sort_by(sort_fn);
        (comp, incomp)
    } else {
        entries.sort_by(|a, b| {
            ext_lower(&a.path)
                .cmp(&ext_lower(&b.path))
                .then_with(|| a.zip_name.cmp(&b.zip_name))
        });
        (entries, Vec::new())
    };

    crate::log::info(&format!(
        "solid-pack start: paths={}, hdd={}, same_drive={}, hybrid={}, level={}, comp={}, incomp={}, out={}",
        valid.len(),
        hdd_mode,
        same_drv,
        hybrid,
        solid_level,
        comp_entries.len(),
        incomp_entries.len(),
        out_path.display()
    ));

    let out_file = create_sequential_write(&out_path)
        .map_err(|e| format!("не удалось создать архив {}: {e}", out_path.display()))?;
    let (bg_chunk_size, bg_slots) = if hdd_mode && same_drv {
        (64 * 1024 * 1024, 2)
    } else if hdd_mode {
        (32 * 1024 * 1024, 3)
    } else {
        (16 * 1024 * 1024, 4)
    };
    let (bg_writer, _send_wait) = BgFileWriter::new(out_file, bg_chunk_size, bg_slots);

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 8);
    let (init_level, init_ldm) = if comp_entries.is_empty() && !incomp_entries.is_empty() {
        (SOLID_FAST_FRAME_LEVEL, false)
    } else {
        (solid_level, true)
    };
    let zstd_writer = MultiFrameZstdWriter::new(bg_writer, init_level, workers as u32, init_ldm)
        .map_err(|e| format!("ошибка инициализации Zstd: {e}"))?;

    let mut builder = tar::Builder::new(zstd_writer);
    let mut progress = ProgressReporter::new(app, total_expected);
    let read_buf_size = if hdd_mode { HUGE_BUF_HDD } else { HUGE_BUF_SSD };
    let mut total_in = 0u64;
    let mut count = 0usize;
    let mut last_log = Instant::now();
    let mut last_log_bytes = 0u64;

    let res: Result<(), String> = (|| {
        let has_comp = !comp_entries.is_empty();
        for (phase_idx, group) in [
            std::mem::take(&mut comp_entries),
            std::mem::take(&mut incomp_entries),
        ]
        .into_iter()
        .enumerate()
        {
            if group.is_empty() {
                continue;
            }
            if phase_idx == 1 && has_comp {
                builder
                    .get_mut()
                    .switch_frame(SOLID_FAST_FRAME_LEVEL, workers as u32, false)
                    .map_err(|e| format!("ошибка переключения фрейма Zstd: {e}"))?;
            }
            if !hdd_mode && group.len() >= 16 {
                let (tx, rx) = mpsc::sync_channel::<Result<SolidBatch, String>>(2);
                let scope_res = std::thread::scope(|s| -> Result<(), String> {
                    s.spawn(move || {
                        let mut small_buf = Vec::new();
                        let mut small_bytes = 0u64;
                        let flush_small = |small_buf: &mut Vec<ZipEntryTask>,
                                           small_bytes: &mut u64|
                         -> bool {
                            if small_buf.is_empty() {
                                return true;
                            }
                            let batch = std::mem::take(small_buf);
                            *small_bytes = 0;
                            match read_small_batch(batch) {
                                Ok(items) => tx.send(Ok(SolidBatch::Loaded(items))).is_ok(),
                                Err(e) => {
                                    let _ = tx.send(Err(e));
                                    false
                                }
                            }
                        };

                        for task in group {
                            if is_cancelled() {
                                let _ = tx.send(Err("отменено".into()));
                                return;
                            }
                            if task.len <= SOLID_SMALL_FILE_MAX {
                                if !small_buf.is_empty()
                                    && (small_buf.len() >= SOLID_BATCH_MAX_FILES
                                        || small_bytes + task.len > SOLID_BATCH_MAX_BYTES)
                                    && !flush_small(&mut small_buf, &mut small_bytes)
                                {
                                    return;
                                }
                                small_bytes += task.len;
                                small_buf.push(task);
                            } else {
                                if !flush_small(&mut small_buf, &mut small_bytes) {
                                    return;
                                }
                                if tx.send(Ok(SolidBatch::Stream(task))).is_err() {
                                    return;
                                }
                            }
                        }
                        let _ = flush_small(&mut small_buf, &mut small_bytes);
                    });

                    while let Ok(item) = rx.recv() {
                        match item? {
                            SolidBatch::Loaded(items) => {
                                for (zip_name, meta, data) in items {
                                    if is_cancelled() {
                                        return Err("отменено".into());
                                    }
                                    let mut header = tar::Header::new_gnu();
                                    header.set_metadata(&meta);
                                    header.set_size(data.len() as u64);
                                    header.set_cksum();
                                    builder
                                        .append_data(&mut header, &zip_name, &data[..])
                                        .map_err(|e| format!("ошибка упаковки {zip_name}: {e}"))?;
                                    total_in += data.len() as u64;
                                    count += 1;
                                    progress.add(data.len() as u64);
                                    maybe_log_solid_pack(
                                        total_in,
                                        total_expected,
                                        started,
                                        &mut last_log,
                                        &mut last_log_bytes,
                                    );
                                }
                            }
                            SolidBatch::Stream(task) => {
                                append_solid_task_stream(
                                    &mut builder,
                                    &task,
                                    read_buf_size,
                                    &mut progress,
                                    &mut total_in,
                                    &mut count,
                                    &mut last_log,
                                    &mut last_log_bytes,
                                    started,
                                    total_expected,
                                )?;
                            }
                        }
                    }
                    Ok(())
                });
                scope_res?;
            } else {
                for task in &group {
                    append_solid_task_stream(
                        &mut builder,
                        task,
                        read_buf_size,
                        &mut progress,
                        &mut total_in,
                        &mut count,
                        &mut last_log,
                        &mut last_log_bytes,
                        started,
                        total_expected,
                    )?;
                }
            }
        }
        for (dir_path, dir_name) in &empty_dirs {
            if is_cancelled() {
                return Err("отменено".into());
            }
            let mut header = tar::Header::new_gnu();
            if let Ok(meta) = fs::metadata(dir_path) {
                header.set_metadata(&meta);
            } else {
                header.set_mode(0o755);
            }
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_cksum();
            builder
                .append_data(&mut header, dir_name, io::empty())
                .map_err(|e| format!("ошибка добавления папки {dir_name}: {e}"))?;
        }
        Ok(())
    })();

    if let Err(err) = res {
        drop(builder);
        let _ = fs::remove_file(&out_path);
        return Err(err);
    }

    let encoder = builder
        .into_inner()
        .map_err(|e| format!("ошибка финализации TAR: {e}"))?;
    let bg_writer = encoder
        .finish()
        .map_err(|e| format!("ошибка финализации Zstd: {e}"))?;
    let out_file = bg_writer
        .finish()
        .map_err(|e| format!("ошибка записи архива: {e}"))?;
    drop(out_file);

    let elapsed_ms = started.elapsed().as_millis();
    let secs = started.elapsed().as_secs_f64().max(0.000_001);
    let output_bytes = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
    crate::log::info(&format!(
        "solid-pack done: files={}, in={:.1} MB, out={:.1} MB, total={} ms ({:.1} MB/s)",
        count,
        total_in as f64 / 1_048_576.0,
        output_bytes as f64 / 1_048_576.0,
        elapsed_ms,
        (total_in as f64 / 1_048_576.0) / secs,
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
            .unwrap_or("archive.tar.zst")
            .to_string(),
        deleted_original: false,
    })
}

fn sanitize_rel_path(rel: &Path) -> Option<PathBuf> {
    let mut clean = PathBuf::new();
    for comp in rel.components() {
        match comp {
            Component::Normal(part) => clean.push(part),
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
                    let out_path = out_dir.join(clean_rel);
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
                let out_path = out_dir.join(clean_rel);
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
        "targz-unpack start: archive={}, in={:.1} MB, hdd={}, out={}",
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
    let decoder = GzDecoder::new(counting);
    extract_tar_stream(
        "targz-unpack",
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
