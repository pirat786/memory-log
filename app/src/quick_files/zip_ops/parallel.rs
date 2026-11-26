use std::io::{self, Cursor, Read, Write};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use zip::{ZipArchive, ZipWriter};

use super::bg_writer::BoundedQueue;
use super::{
    should_compress_mem, should_compress_task, zip_options_for, MiniResult, ZipEntryTask,
    ZipPackStats, HDD_READAHEAD_CHUNKS, PARALLEL_CHUNK_MAX_BYTES, PARALLEL_CHUNK_MAX_FILES,
    RECYCLE_MAX_CAP,
};
use crate::quick_files::io_hints::open_sequential_read;
use crate::quick_files::{ext_lower, is_cancelled, ProgressReporter};

pub(super) fn build_chunks(entries: Vec<ZipEntryTask>) -> Vec<Vec<ZipEntryTask>> {
    let mut chunks: Vec<Vec<ZipEntryTask>> = Vec::new();
    let mut cur_chunk: Vec<ZipEntryTask> = Vec::new();
    let mut cur_bytes = 0u64;
    for task in entries {
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
    chunks
}

fn estimate_chunk_capacity(chunk: &[ZipEntryTask]) -> usize {
    let raw: u64 = chunk.iter().map(|t| t.len).sum();
    (raw.min(16 * 1024 * 1024) as usize) + chunk.len() * 128 + 4096
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_parallel_ssd<W: Write + io::Seek>(
    chunks: Vec<Vec<ZipEntryTask>>,
    workers: usize,
    zip_mode: &str,
    zip: &mut ZipWriter<W>,
    total_in: &mut u64,
    count: &mut usize,
    progress: &mut ProgressReporter<'_>,
    stats: &mut ZipPackStats,
    on_chunk: &mut impl FnMut(u64, &ZipPackStats),
) -> Result<(), String> {
    let queue = Mutex::new(chunks.into_iter());
    let (tx, rx) = mpsc::sync_channel::<MiniResult>(workers.max(2));

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
                    let cap = estimate_chunk_capacity(&chunk);
                    let mut mini = ZipWriter::new(Cursor::new(Vec::with_capacity(cap)));
                    let mut chunk_in = 0u64;
                    let mut chunk_cnt = 0usize;
                    let mut open_dur = Duration::ZERO;
                    let mut read_dur = Duration::ZERO;
                    let mut err_msg: Option<String> = None;

                    for task in chunk {
                        if is_cancelled() {
                            err_msg = Some("отменено".into());
                            break;
                        }
                        let compress = should_compress_task(&task.path, task.len, zip_mode);
                        let opts = zip_options_for(compress, task.len, zip_mode, task.mtime);
                        if let Err(e) = mini.start_file(&task.zip_name, opts) {
                            err_msg = Some(format!("ошибка добавления {}: {e}", task.zip_name));
                            break;
                        }
                        let t_open = Instant::now();
                        let file_res = open_sequential_read(&task.path);
                        open_dur += t_open.elapsed();
                        match file_res {
                            Ok(mut f) => loop {
                                if is_cancelled() {
                                    err_msg = Some("отменено".into());
                                    break;
                                }
                                let t_read = Instant::now();
                                let rd = io::Read::read(&mut f, &mut buf);
                                read_dur += t_read.elapsed();
                                match rd {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        if let Err(e) = io::Write::write_all(&mut mini, &buf[..n]) {
                                            err_msg = Some(format!(
                                                "ошибка сжатия {}: {e}",
                                                task.zip_name
                                            ));
                                            break;
                                        }
                                        chunk_in += n as u64;
                                    }
                                    Err(e) => {
                                        err_msg = Some(format!(
                                            "ошибка чтения {}: {e}",
                                            task.zip_name
                                        ));
                                        break;
                                    }
                                }
                            },
                            Err(e) => {
                                err_msg = Some(format!(
                                    "не удалось открыть {}: {e}",
                                    task.path.display()
                                ));
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
                            if tx
                                .send(Ok((
                                    cursor.into_inner(),
                                    chunk_in,
                                    chunk_cnt,
                                    open_dur,
                                    read_dur,
                                )))
                                .is_err()
                            {
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

        drain_mini_archives(rx, zip, total_in, count, progress, stats, on_chunk)
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_parallel_hdd<W: Write + io::Seek>(
    chunks: Vec<Vec<ZipEntryTask>>,
    workers: usize,
    zip_mode: &str,
    zip: &mut ZipWriter<W>,
    total_in: &mut u64,
    count: &mut usize,
    progress: &mut ProgressReporter<'_>,
    stats: &mut ZipPackStats,
    on_chunk: &mut impl FnMut(u64, &ZipPackStats),
) -> Result<(), String> {
    type RawChunk = Result<(Vec<(ZipEntryTask, Vec<u8>)>, Duration, Duration), String>;

    let raw_queue = BoundedQueue::<RawChunk>::new(HDD_READAHEAD_CHUNKS);
    let (recycle_tx, recycle_rx) = mpsc::channel::<Vec<u8>>();
    let (mini_tx, mini_rx) = mpsc::sync_channel::<MiniResult>(workers.max(2));

    std::thread::scope(|s| -> Result<(), String> {
        let rq = &raw_queue;
        s.spawn(move || {
            for chunk in chunks {
                if is_cancelled() {
                    break;
                }
                let mut open_dur = Duration::ZERO;
                let mut read_dur = Duration::ZERO;
                let mut loaded = Vec::with_capacity(chunk.len());
                let mut err_msg: Option<String> = None;

                for task in chunk {
                    if is_cancelled() {
                        err_msg = Some("отменено".into());
                        break;
                    }
                    let want = task.len as usize;
                    let mut buf = recycle_rx
                        .try_recv()
                        .unwrap_or_else(|_| Vec::with_capacity(want));
                    buf.clear();
                    if buf.capacity() < want {
                        buf.reserve(want - buf.capacity());
                    }

                    let t_open = Instant::now();
                    let file_res = open_sequential_read(&task.path);
                    open_dur += t_open.elapsed();
                    let mut f = match file_res {
                        Ok(f) => f,
                        Err(e) => {
                            err_msg = Some(format!(
                                "не удалось открыть {}: {e}",
                                task.path.display()
                            ));
                            break;
                        }
                    };

                    let t_read = Instant::now();
                    let rd = f.read_to_end(&mut buf);
                    read_dur += t_read.elapsed();
                    if let Err(e) = rd {
                        err_msg = Some(format!("ошибка чтения {}: {e}", task.zip_name));
                        break;
                    }
                    loaded.push((task, buf));
                }

                let ok = match err_msg {
                    Some(e) => rq.push(Err(e)),
                    None => rq.push(Ok((loaded, open_dur, read_dur))),
                };
                if !ok {
                    break;
                }
            }
            rq.close();
        });

        for _ in 0..workers {
            let rq = &raw_queue;
            let tx = mini_tx.clone();
            let rtx = recycle_tx.clone();
            s.spawn(move || loop {
                if is_cancelled() {
                    break;
                }
                let Some(item) = rq.pop() else { break };
                let (loaded, open_dur, read_dur) = match item {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        rq.close();
                        break;
                    }
                };

                let raw: u64 = loaded.iter().map(|(t, _)| t.len).sum();
                let est = (raw.min(16 * 1024 * 1024) as usize) + loaded.len() * 128 + 4096;
                let mut mini = ZipWriter::new(Cursor::new(Vec::with_capacity(est)));
                let mut chunk_in = 0u64;
                let mut chunk_cnt = 0usize;
                let mut err_msg: Option<String> = None;

                for (task, mut data) in loaded {
                    if is_cancelled() {
                        err_msg = Some("отменено".into());
                        break;
                    }
                    let compress = should_compress_mem(&ext_lower(&task.path), &data, zip_mode);
                    let opts = zip_options_for(compress, task.len, zip_mode, task.mtime);
                    if let Err(e) = mini.start_file(&task.zip_name, opts) {
                        err_msg = Some(format!("ошибка добавления {}: {e}", task.zip_name));
                        break;
                    }
                    if let Err(e) = io::Write::write_all(&mut mini, &data) {
                        err_msg = Some(format!("ошибка сжатия {}: {e}", task.zip_name));
                        break;
                    }
                    chunk_in += data.len() as u64;
                    chunk_cnt += 1;
                    if data.capacity() <= RECYCLE_MAX_CAP {
                        data.clear();
                        let _ = rtx.send(data);
                    }
                }

                if let Some(e) = err_msg {
                    let _ = tx.send(Err(e));
                    rq.close();
                    break;
                }
                match mini.finish() {
                    Ok(cursor) => {
                        if tx
                            .send(Ok((
                                cursor.into_inner(),
                                chunk_in,
                                chunk_cnt,
                                open_dur,
                                read_dur,
                            )))
                            .is_err()
                        {
                            rq.close();
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(format!("ошибка финализации блока ZIP: {e}")));
                        rq.close();
                        break;
                    }
                }
            });
        }
        drop(mini_tx);
        drop(recycle_tx);

        let res = drain_mini_archives(mini_rx, zip, total_in, count, progress, stats, on_chunk);
        raw_queue.close();
        res
    })
}

fn drain_mini_archives<W: Write + io::Seek>(
    rx: mpsc::Receiver<MiniResult>,
    zip: &mut ZipWriter<W>,
    total_in: &mut u64,
    count: &mut usize,
    progress: &mut ProgressReporter<'_>,
    stats: &mut ZipPackStats,
    on_chunk: &mut impl FnMut(u64, &ZipPackStats),
) -> Result<(), String> {
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

        let (bytes, chunk_in, chunk_cnt, open_dur, read_dur) = match item {
            Ok(v) => v,
            Err(e) => {
                drop(rx);
                return Err(e);
            }
        };
        stats.file_open += open_dur;
        stats.read += read_dur;

        let t_merge = Instant::now();
        let mut archive = match ZipArchive::new(Cursor::new(bytes)) {
            Ok(a) => a,
            Err(e) => {
                drop(rx);
                return Err(format!("ошибка чтения блока ZIP: {e}"));
            }
        };
        for i in 0..archive.len() {
            let entry = match archive.by_index_raw(i) {
                Ok(e) => e,
                Err(e) => {
                    drop(rx);
                    return Err(format!("ошибка чтения элемента блока ZIP: {e}"));
                }
            };
            if let Err(e) = zip.raw_copy_file(entry) {
                drop(rx);
                return Err(format!("ошибка склейки ZIP: {e}"));
            }
        }
        stats.batch_merge += t_merge.elapsed();

        *total_in += chunk_in;
        *count += chunk_cnt;
        progress.add(chunk_in);
        on_chunk(*total_in, stats);
    }
    Ok(())
}
