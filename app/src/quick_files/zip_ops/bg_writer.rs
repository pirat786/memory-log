use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::quick_files::is_cancelled;

struct BoundedQueueState<T> {
    items: VecDeque<T>,
    closed: bool,
    max: usize,
}

/// Bounded MPMC queue with instant close/wakeup and cancel check.
pub(super) struct BoundedQueue<T> {
    state: Mutex<BoundedQueueState<T>>,
    cvar: Condvar,
}

impl<T> BoundedQueue<T> {
    pub(super) fn new(max: usize) -> Self {
        Self {
            state: Mutex::new(BoundedQueueState {
                items: VecDeque::with_capacity(max),
                closed: false,
                max: max.max(1),
            }),
            cvar: Condvar::new(),
        }
    }

    pub(super) fn push(&self, item: T) -> bool {
        let mut g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while g.items.len() >= g.max && !g.closed && !is_cancelled() {
            let (next, _) = self
                .cvar
                .wait_timeout(g, Duration::from_millis(50))
                .unwrap_or_else(|e| e.into_inner());
            g = next;
        }
        if g.closed || is_cancelled() {
            return false;
        }
        g.items.push_back(item);
        self.cvar.notify_all();
        true
    }

    pub(super) fn pop(&self) -> Option<T> {
        let mut g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while g.items.is_empty() && !g.closed && !is_cancelled() {
            let (next, _) = self
                .cvar
                .wait_timeout(g, Duration::from_millis(50))
                .unwrap_or_else(|e| e.into_inner());
            g = next;
        }
        if is_cancelled() {
            return None;
        }
        let item = g.items.pop_front();
        if item.is_some() {
            self.cvar.notify_all();
        }
        item
    }

    pub(super) fn close(&self) {
        let mut g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        g.closed = true;
        self.cvar.notify_all();
    }
}

/// Dedicated background disk writer that overlaps disk flushes with CPU/merge work.
pub(super) struct BgFileWriter {
    buf: Vec<u8>,
    chunk_size: usize,
    tx: Option<mpsc::SyncSender<Vec<u8>>>,
    recycle_rx: mpsc::Receiver<Vec<u8>>,
    handle: Option<JoinHandle<io::Result<File>>>,
    send_wait: Arc<AtomicU64>,
    aborted: Arc<AtomicBool>,
}

impl BgFileWriter {
    pub(super) fn new(
        mut file: File,
        chunk_size: usize,
        queue_slots: usize,
    ) -> (Self, Arc<AtomicU64>) {
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(queue_slots);
        let (recycle_tx, recycle_rx) = mpsc::channel::<Vec<u8>>();
        let send_wait = Arc::new(AtomicU64::new(0));
        let aborted = Arc::new(AtomicBool::new(false));
        let aborted_bg = Arc::clone(&aborted);
        let max_recycle_cap = chunk_size.saturating_mul(2);

        let handle = std::thread::spawn(move || -> io::Result<File> {
            while let Ok(mut chunk) = rx.recv() {
                if aborted_bg.load(Ordering::Relaxed) {
                    break;
                }
                file.write_all(&chunk)?;
                if chunk.capacity() <= max_recycle_cap {
                    chunk.clear();
                    let _ = recycle_tx.send(chunk);
                }
            }
            if !aborted_bg.load(Ordering::Relaxed) {
                file.flush()?;
            }
            Ok(file)
        });

        let writer = Self {
            buf: Vec::with_capacity(chunk_size),
            chunk_size,
            tx: Some(tx),
            recycle_rx,
            handle: Some(handle),
            send_wait: Arc::clone(&send_wait),
            aborted,
        };
        (writer, send_wait)
    }

    fn send_full_buf(&mut self) -> io::Result<()> {
        if is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "отменено"));
        }
        if self.buf.is_empty() {
            return Ok(());
        }
        let next = self
            .recycle_rx
            .try_recv()
            .unwrap_or_else(|_| Vec::with_capacity(self.chunk_size));
        let full = std::mem::replace(&mut self.buf, next);
        let Some(tx) = self.tx.as_ref() else {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "writer closed"));
        };
        let t = Instant::now();
        let res = tx.send(full);
        self.send_wait
            .fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
        res.map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "background disk write failed"))
    }

    pub(super) fn finish(mut self) -> io::Result<File> {
        let send_err = self.send_full_buf().err();
        drop(self.tx.take());
        let join_res = match self.handle.take() {
            Some(h) => h
                .join()
                .map_err(|_| io::Error::other("writer thread panicked"))?,
            None => Err(io::Error::other("writer thread missing")),
        };
        match (join_res, send_err) {
            (Err(e), _) => Err(e),
            (Ok(_), Some(e)) => Err(e),
            (Ok(file), None) => Ok(file),
        }
    }
}

impl Write for BgFileWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut off = 0;
        while off < data.len() {
            let avail = self.chunk_size.saturating_sub(self.buf.len());
            if avail == 0 {
                self.send_full_buf()?;
                continue;
            }
            let take = avail.min(data.len() - off);
            self.buf.extend_from_slice(&data[off..off + take]);
            off += take;
            if self.buf.len() >= self.chunk_size {
                self.send_full_buf()?;
            }
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.send_full_buf()
    }
}

impl Drop for BgFileWriter {
    fn drop(&mut self) {
        if self.handle.is_some() {
            self.aborted.store(true, Ordering::Relaxed);
        }
        drop(self.tx.take());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
