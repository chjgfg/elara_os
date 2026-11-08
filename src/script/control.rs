//! Cooperative execution control, not host signals or an OS scheduler.
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Flags {
    paused: bool,
    cancelled: bool,
    cancel_observed: bool,
    workers: usize,
    stopped: usize,
}
struct State {
    flags: Mutex<Flags>,
    wake: Condvar,
}
#[derive(Clone)]
pub struct Control {
    state: Arc<State>,
}
struct Slot {
    control: Control,
    stopped: AtomicBool,
}
#[derive(Clone)]
pub struct Worker {
    slot: Arc<Slot>,
    interrupt: Option<Arc<AtomicBool>>,
}
#[derive(Clone)]
pub(crate) struct Interrupt {
    flag: Arc<AtomicBool>,
}
impl Interrupt {
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlSnapshot {
    pub pause_requested: bool,
    pub cancel_requested: bool,
    pub cancellation_observed: bool,
    pub workers: usize,
    pub stopped_workers: usize,
}
fn poisoned() -> io::Error {
    io::Error::other("execution control lock poisoned")
}
impl Default for Control {
    fn default() -> Self {
        Self::new()
    }
}
impl Control {
    pub fn new() -> Self {
        Self {
            state: Arc::new(State {
                flags: Mutex::new(Flags::default()),
                wake: Condvar::new(),
            }),
        }
    }
    pub fn worker(&self) -> io::Result<Worker> {
        let mut flags = self.state.flags.lock().map_err(|_| poisoned())?;
        flags.workers = flags
            .workers
            .checked_add(1)
            .ok_or_else(|| io::Error::other("worker counter overflow"))?;
        Ok(Worker {
            slot: Arc::new(Slot {
                control: self.clone(),
                stopped: AtomicBool::new(false),
            }),
            interrupt: None,
        })
    }
    pub fn request_pause(&self) -> io::Result<()> {
        let mut flags = self.state.flags.lock().map_err(|_| poisoned())?;
        flags.paused = true;
        self.state.wake.notify_all();
        Ok(())
    }
    pub fn resume(&self) -> io::Result<()> {
        let mut flags = self.state.flags.lock().map_err(|_| poisoned())?;
        flags.paused = false;
        self.state.wake.notify_all();
        Ok(())
    }
    pub fn cancel(&self) -> io::Result<()> {
        let mut flags = self.state.flags.lock().map_err(|_| poisoned())?;
        flags.cancelled = true;
        flags.paused = false;
        self.state.wake.notify_all();
        Ok(())
    }
    pub fn snapshot(&self) -> io::Result<ControlSnapshot> {
        let flags = self.state.flags.lock().map_err(|_| poisoned())?;
        Ok(ControlSnapshot {
            pause_requested: flags.paused,
            cancel_requested: flags.cancelled,
            cancellation_observed: flags.cancel_observed,
            workers: flags.workers,
            stopped_workers: flags.stopped,
        })
    }
    /// True only after every registered worker acknowledged suspension.
    pub fn wait_stopped(&self, timeout: Duration) -> io::Result<bool> {
        let deadline = Instant::now() + timeout;
        let mut flags = self.state.flags.lock().map_err(|_| poisoned())?;
        while flags.paused
            && !flags.cancelled
            && flags.workers != 0
            && flags.stopped < flags.workers
        {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            flags = self
                .state
                .wake
                .wait_timeout(flags, remaining)
                .map_err(|_| poisoned())?
                .0;
        }
        Ok(flags.paused && flags.workers != 0 && flags.stopped == flags.workers)
    }
}
impl Worker {
    pub(crate) fn with_interrupt(&self, interrupt: &Interrupt) -> Self {
        Self {
            slot: self.slot.clone(),
            interrupt: Some(interrupt.flag.clone()),
        }
    }
    pub fn control(&self) -> Control {
        self.slot.control.clone()
    }
    pub fn checkpoint(&self) -> io::Result<()> {
        let state = &self.slot.control.state;
        let mut flags = state.flags.lock().map_err(|_| poisoned())?;
        loop {
            if flags.cancelled
                || self
                    .interrupt
                    .as_ref()
                    .is_some_and(|flag| flag.load(Ordering::Acquire))
            {
                if flags.cancelled {
                    flags.cancel_observed = true;
                    state.wake.notify_all();
                }
                if self.slot.stopped.swap(false, Ordering::AcqRel) {
                    flags.stopped -= 1;
                    state.wake.notify_all();
                }
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "execution cancelled",
                ));
            }
            if !flags.paused {
                if self.slot.stopped.swap(false, Ordering::AcqRel) {
                    flags.stopped -= 1;
                    state.wake.notify_all();
                }
                return Ok(());
            }
            if !self.slot.stopped.swap(true, Ordering::AcqRel) {
                flags.stopped += 1;
                state.wake.notify_all();
            }
            flags = state
                .wake
                .wait_timeout(flags, Duration::from_millis(20))
                .map_err(|_| poisoned())?
                .0;
        }
    }
    /// Counts active waiting time; paused time is not charged to the timer.
    pub fn sleep(&self, duration: Duration) -> io::Result<()> {
        let state = &self.slot.control.state;
        let mut remaining = duration;
        loop {
            self.checkpoint()?;
            if remaining.is_zero() {
                return Ok(());
            }
            let flags = state.flags.lock().map_err(|_| poisoned())?;
            if flags.cancelled || flags.paused {
                drop(flags);
                continue;
            }
            let before = Instant::now();
            let waited = state
                .wake
                .wait_timeout(flags, remaining.min(Duration::from_millis(20)))
                .map_err(|_| poisoned())?;
            drop(waited);
            remaining = remaining.saturating_sub(before.elapsed());
        }
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        if let Ok(mut flags) = self.control.state.flags.lock() {
            flags.workers -= 1;
            if self.stopped.load(Ordering::Acquire) {
                flags.stopped -= 1;
            }
            self.control.state.wake.notify_all();
        }
    }
}
