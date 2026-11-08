//! Scoped, owner-bound script jobs. These are interpreter workers, never host processes.
use super::control::{Control, Worker};
use super::modules::{ModuleCommands, ModuleFork};
use super::parser::List;
use super::runtime::{self, Context, Report};
use crate::filesystem::DataRoot;
use crate::users::Account;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{Scope, ScopedJoinHandle};
use std::time::Duration;
use std::{format, matches};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct JobId(u64);
impl JobId {
    pub fn parse(text: &str) -> Result<Self, String> {
        let value = text
            .strip_prefix('%')
            .ok_or("scoped job ids use %ID")?
            .parse::<u64>()
            .map_err(|_| "invalid scoped job id")?;
        if value == 0 {
            return Err("invalid scoped job id".into());
        }
        Ok(Self(value))
    }
    pub fn number(self) -> u64 {
        self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    Running,
    PauseRequested,
    Stopped,
    Resuming,
    Cancelling,
    Cancelled,
    Exited(u8),
    Panicked,
}
pub struct Snapshot {
    pub id: JobId,
    pub label: String,
    pub state: JobState,
    pub owner_uid: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseOutcome {
    Stopped,
    Requested,
    Finished,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestOutcome {
    Requested,
    Finished,
}
pub enum Waited {
    Finished { report: Report, cancelled: bool },
    Panicked,
}
pub struct Spawn {
    pub context: Context,
    pub program: List,
    pub label: String,
    pub budget: Arc<runtime::Budget>,
}
enum Phase {
    Running,
    Finished { report: Report, cancelled: bool },
    Panicked,
}
struct Record<'scope> {
    label: String,
    control: Control,
    phase: Phase,
    handle: Option<ScopedJoinHandle<'scope, ()>>,
}
struct Registry<'scope> {
    next: u64,
    records: BTreeMap<JobId, Record<'scope>>,
}
struct Core<'scope, 'env> {
    scope: &'scope Scope<'scope, 'env>,
    registry: Mutex<Registry<'scope>>,
    changed: Condvar,
    owner: AtomicU32,
    closing: AtomicBool,
    capacity: usize,
}
pub struct JobTable<'scope, 'env> {
    core: Arc<Core<'scope, 'env>>,
}
impl Clone for JobTable<'_, '_> {
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
        }
    }
}
impl<'scope, 'env> JobTable<'scope, 'env> {
    pub fn new(scope: &'scope Scope<'scope, 'env>, uid: u32, capacity: usize) -> Self {
        Self {
            core: Arc::new(Core {
                scope,
                registry: Mutex::new(Registry {
                    next: 1,
                    records: BTreeMap::new(),
                }),
                changed: Condvar::new(),
                owner: AtomicU32::new(uid),
                closing: AtomicBool::new(false),
                capacity: capacity.min(16),
            }),
        }
    }
    fn authorize(&self, uid: u32) -> Result<(), String> {
        if self.core.owner.load(Ordering::Acquire) == uid {
            Ok(())
        } else {
            Err("job table actor mismatch".into())
        }
    }
    fn refresh(registry: &mut Registry<'scope>) {
        for record in registry.records.values_mut() {
            if matches!(record.phase, Phase::Running)
                && record
                    .handle
                    .as_ref()
                    .is_some_and(ScopedJoinHandle::is_finished)
            {
                record.phase = Phase::Panicked;
            }
        }
    }
    fn state(record: &Record<'scope>) -> Result<JobState, String> {
        Ok(match &record.phase {
            Phase::Finished {
                cancelled: true, ..
            } => JobState::Cancelled,
            Phase::Finished { report, .. } => JobState::Exited(report.status),
            Phase::Panicked => JobState::Panicked,
            Phase::Running => {
                let flags = record.control.snapshot().map_err(|e| e.to_string())?;
                if flags.cancel_requested {
                    JobState::Cancelling
                } else if flags.pause_requested
                    && flags.workers != 0
                    && flags.workers == flags.stopped_workers
                {
                    JobState::Stopped
                } else if flags.pause_requested {
                    JobState::PauseRequested
                } else if flags.stopped_workers != 0 {
                    JobState::Resuming
                } else {
                    JobState::Running
                }
            }
        })
    }
    pub fn spawn(
        &self,
        root: &'env DataRoot,
        user: &Account,
        request: Spawn,
        modules: &dyn ModuleCommands,
    ) -> Result<JobId, String> {
        let Spawn {
            context,
            program,
            label,
            budget,
        } = request;
        self.authorize(user.uid)?;
        let mut registry = self
            .core
            .registry
            .lock()
            .map_err(|_| "job registry poisoned")?;
        self.authorize(user.uid)?;
        if self.core.closing.load(Ordering::Acquire) {
            return Err("job environment is closing".into());
        }
        if registry.records.len() >= self.core.capacity {
            return Err("job table full; wait/reap completed jobs before spawning more".into());
        }
        let id = JobId(registry.next);
        let next = registry
            .next
            .checked_add(1)
            .ok_or("job ids exhausted; no wrap/reuse")?;
        // Reserve admission under the registry lock before granting module
        // capabilities; fork is a nonblocking clone, never a script callback.
        let control = Control::new();
        let worker = control.worker().map_err(|e| e.to_string())?;
        let mut modules = match modules.fork(root, user, worker.clone())? {
            ModuleFork::Ready(modules) => modules,
            ModuleFork::Unsupported => {
                return Err("module context does not support scoped workers".into())
            }
        };
        registry.next = next;
        registry.records.insert(
            id,
            Record {
                label,
                control: control.clone(),
                phase: Phase::Running,
                handle: None,
            },
        );
        let jobs = self.clone();
        let actor = user.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("elara-job-{}", id.0))
            .spawn_scoped(self.core.scope, move || {
                let mut context = context;
                let mut report = runtime::execute_job(
                    &program,
                    root,
                    &actor,
                    &mut context,
                    modules.as_mut(),
                    runtime::JobRun {
                        jobs: jobs.clone(),
                        worker,
                        id,
                        budget,
                    },
                );
                let cancelled = control
                    .snapshot()
                    .map(|flags| flags.cancellation_observed)
                    .unwrap_or(false);
                if cancelled {
                    report.status = 130;
                }
                if let Ok(mut registry) = jobs.core.registry.lock() {
                    if let Some(record) = registry.records.get_mut(&id) {
                        record.phase = Phase::Finished { report, cancelled };
                    }
                    jobs.core.changed.notify_all();
                }
            });
        match spawned {
            Ok(handle) => {
                registry
                    .records
                    .get_mut(&id)
                    .ok_or("job entry missing")?
                    .handle = Some(handle);
                Ok(id)
            }
            Err(error) => {
                registry.records.remove(&id);
                Err(error.to_string())
            }
        }
    }
    pub fn snapshot(&self, uid: u32) -> Result<Vec<Snapshot>, String> {
        self.authorize(uid)?;
        let mut registry = self
            .core
            .registry
            .lock()
            .map_err(|_| "job registry poisoned")?;
        Self::refresh(&mut registry);
        registry
            .records
            .iter()
            .map(|(id, record)| {
                Ok(Snapshot {
                    id: *id,
                    label: record.label.clone(),
                    state: Self::state(record)?,
                    owner_uid: uid,
                })
            })
            .collect()
    }
    fn control(&self, uid: u32, id: JobId) -> Result<(Control, bool), String> {
        self.authorize(uid)?;
        let mut registry = self
            .core
            .registry
            .lock()
            .map_err(|_| "job registry poisoned")?;
        Self::refresh(&mut registry);
        let record = registry
            .records
            .get(&id)
            .ok_or_else(|| format!("no scoped job %{}", id.0))?;
        Ok((
            record.control.clone(),
            !matches!(record.phase, Phase::Running),
        ))
    }
    pub fn pause(&self, uid: u32, id: JobId) -> Result<PauseOutcome, String> {
        let (control, finished) = self.control(uid, id)?;
        if finished {
            return Ok(PauseOutcome::Finished);
        }
        control.request_pause().map_err(|e| e.to_string())?;
        if control
            .wait_stopped(Duration::from_secs(1))
            .map_err(|e| e.to_string())?
        {
            return Ok(PauseOutcome::Stopped);
        }
        let (_, finished) = self.control(uid, id)?;
        Ok(if finished {
            PauseOutcome::Finished
        } else {
            PauseOutcome::Requested
        })
    }
    pub fn resume(&self, uid: u32, id: JobId) -> Result<RequestOutcome, String> {
        let (control, finished) = self.control(uid, id)?;
        if finished {
            return Ok(RequestOutcome::Finished);
        }
        control.resume().map_err(|e| e.to_string())?;
        Ok(RequestOutcome::Requested)
    }
    pub fn cancel(&self, uid: u32, id: JobId) -> Result<RequestOutcome, String> {
        let (control, finished) = self.control(uid, id)?;
        if finished {
            return Ok(RequestOutcome::Finished);
        }
        control.cancel().map_err(|e| e.to_string())?;
        Ok(RequestOutcome::Requested)
    }
    pub fn wait(
        &self,
        uid: u32,
        id: JobId,
        current: Option<JobId>,
        worker: Option<&Worker>,
    ) -> Result<Waited, String> {
        self.authorize(uid)?;
        if current == Some(id) {
            return Err("a job cannot wait on itself".into());
        }
        loop {
            if let Some(worker) = worker {
                worker.checkpoint().map_err(|e| e.to_string())?;
            }
            let mut registry = self
                .core
                .registry
                .lock()
                .map_err(|_| "job registry poisoned")?;
            Self::refresh(&mut registry);
            let record = registry
                .records
                .get(&id)
                .ok_or_else(|| format!("no scoped job %{}", id.0))?;
            if !matches!(record.phase, Phase::Running) {
                let mut record = registry.records.remove(&id).ok_or("job disappeared")?;
                drop(registry);
                if let Some(handle) = record.handle.take() {
                    if handle.join().is_err() {
                        return Ok(Waited::Panicked);
                    }
                }
                return match record.phase {
                    Phase::Finished { report, cancelled } => {
                        Ok(Waited::Finished { report, cancelled })
                    }
                    Phase::Panicked => Ok(Waited::Panicked),
                    Phase::Running => Err("job completion state inconsistent".into()),
                };
            }
            let waited = self
                .core
                .changed
                .wait_timeout(registry, Duration::from_millis(20))
                .map_err(|_| "job registry poisoned")?;
            drop(waited);
        }
    }
    pub fn shutdown(&self) -> Result<usize, String> {
        self.core.closing.store(true, Ordering::Release);
        let entries: Vec<_> = {
            let registry = self
                .core
                .registry
                .lock()
                .map_err(|_| "job registry poisoned")?;
            registry
                .records
                .iter()
                .map(|(id, record)| (*id, record.control.clone()))
                .collect()
        };
        for (_, control) in &entries {
            control.cancel().map_err(|e| e.to_string())?;
        }
        let uid = self.core.owner.load(Ordering::Acquire);
        let mut reaped = 0;
        for (id, _) in entries {
            self.wait(uid, id, None, None)?;
            reaped += 1;
        }
        Ok(reaped)
    }
    pub fn complete_owner_switch(&self, uid: u32) -> Result<(), String> {
        let registry = self
            .core
            .registry
            .lock()
            .map_err(|_| "job registry poisoned")?;
        if !self.core.closing.load(Ordering::Acquire) || !registry.records.is_empty() {
            return Err("job owner switch requires completed shutdown".into());
        }
        self.core.owner.store(uid, Ordering::Release);
        self.core.closing.store(false, Ordering::Release);
        Ok(())
    }
    pub fn switch_owner(&self, uid: u32) -> Result<usize, String> {
        let count = self.shutdown()?;
        self.complete_owner_switch(uid)?;
        Ok(count)
    }
}
