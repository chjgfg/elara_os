//! Bounded in-process byte pipes with backpressure, EOF and cancellation points.
use super::control::Worker;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub chunk_bytes: usize,
    pub queued_chunks: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            chunk_bytes: 4096,
            queued_chunks: 8,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stats {
    pub enqueued: u64,
    pub read: u64,
    pub peak_queued_bytes: usize,
    pub blocked_writes: usize,
}
struct State {
    queue: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    stats: Stats,
    reader_open: bool,
    writer_open: bool,
}
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    config: Config,
}
#[derive(Clone)]
pub struct Probe {
    shared: Arc<Shared>,
}
pub struct Reader {
    shared: Arc<Shared>,
    worker: Worker,
    pending: Vec<u8>,
    at: usize,
}
pub struct Writer {
    shared: Arc<Shared>,
    worker: Worker,
    pending: Vec<u8>,
}
fn poisoned() -> io::Error {
    io::Error::other("pipe state poisoned")
}
fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "pipe reader closed")
}
pub fn bounded(
    config: Config,
    reader: Worker,
    writer: Worker,
) -> io::Result<(Reader, Writer, Probe)> {
    if config.chunk_bytes == 0
        || config.chunk_bytes > 4096
        || config.queued_chunks == 0
        || config.queued_chunks > 8
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pipe limits: chunk 1..4096, queue 1..8",
        ));
    }
    let shared = Arc::new(Shared {
        config,
        changed: Condvar::new(),
        state: Mutex::new(State {
            queue: VecDeque::new(),
            queued_bytes: 0,
            stats: Stats {
                enqueued: 0,
                read: 0,
                peak_queued_bytes: 0,
                blocked_writes: 0,
            },
            reader_open: true,
            writer_open: true,
        }),
    });
    Ok((
        Reader {
            shared: shared.clone(),
            worker: reader,
            pending: Vec::new(),
            at: 0,
        },
        Writer {
            shared: shared.clone(),
            worker: writer,
            pending: Vec::with_capacity(config.chunk_bytes),
        },
        Probe { shared },
    ))
}
impl Probe {
    pub fn stats(&self) -> io::Result<Stats> {
        Ok(self.shared.state.lock().map_err(|_| poisoned())?.stats)
    }
}
impl Read for Reader {
    fn read(&mut self, target: &mut [u8]) -> io::Result<usize> {
        self.worker.checkpoint()?;
        if target.is_empty() {
            return Ok(0);
        }
        while self.at == self.pending.len() {
            self.worker.checkpoint()?;
            let mut state = self.shared.state.lock().map_err(|_| poisoned())?;
            if let Some(chunk) = state.queue.pop_front() {
                state.queued_bytes -= chunk.len();
                self.pending = chunk;
                self.at = 0;
                self.shared.changed.notify_all();
                break;
            }
            if !state.writer_open {
                return Ok(0);
            }
            let waited = self
                .shared
                .changed
                .wait_timeout(state, Duration::from_millis(20))
                .map_err(|_| poisoned())?;
            drop(waited);
        }
        let count = target.len().min(self.pending.len() - self.at);
        let mut state = self.shared.state.lock().map_err(|_| poisoned())?;
        let read = state
            .stats
            .read
            .checked_add(count as u64)
            .ok_or_else(|| io::Error::other("pipe read counter overflow"))?;
        target[..count].copy_from_slice(&self.pending[self.at..self.at + count]);
        self.at += count;
        state.stats.read = read;
        Ok(count)
    }
}
impl Writer {
    fn publish(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        loop {
            self.worker.checkpoint()?;
            let mut state = self.shared.state.lock().map_err(|_| poisoned())?;
            if !state.reader_open {
                return Err(closed());
            }
            if state.queue.len() < self.shared.config.queued_chunks {
                let count = self.pending.len();
                let total = state
                    .stats
                    .enqueued
                    .checked_add(count as u64)
                    .ok_or_else(|| io::Error::other("pipe write counter overflow"))?;
                state.queue.push_back(std::mem::take(&mut self.pending));
                self.pending = Vec::with_capacity(self.shared.config.chunk_bytes);
                state.queued_bytes += count;
                state.stats.enqueued = total;
                state.stats.peak_queued_bytes =
                    state.stats.peak_queued_bytes.max(state.queued_bytes);
                self.shared.changed.notify_all();
                return Ok(());
            }
            state.stats.blocked_writes = state.stats.blocked_writes.saturating_add(1);
            let waited = self
                .shared
                .changed
                .wait_timeout(state, Duration::from_millis(20))
                .map_err(|_| poisoned())?;
            drop(waited);
        }
    }
}
impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.worker.checkpoint()?;
        if bytes.is_empty() {
            return Ok(0);
        }
        if self.pending.len() == self.shared.config.chunk_bytes {
            self.publish()?;
        }
        let count = bytes
            .len()
            .min(self.shared.config.chunk_bytes - self.pending.len());
        self.pending.extend_from_slice(&bytes[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.publish()
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.state.lock() {
            state.reader_open = false;
        }
        self.shared.changed.notify_all();
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        // Never retry/flush from Drop: that could block while tearing down a failed pipeline.
        if let Ok(mut state) = self.shared.state.lock() {
            state.writer_open = false;
        }
        self.shared.changed.notify_all();
    }
}
