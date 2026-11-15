//! Process-local scoped-job observations. No host process API or control authority.
use super::{vfs, DataRoot, FsError};
use std::collections::BTreeMap;
use std::format;
use std::sync::{Arc, Mutex, Weak};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeJobState {
    Running,
    PauseRequested,
    Stopped,
    Resuming,
    Cancelling,
    Cancelled,
    Exited(u8),
    TaskExited(i32),
    Panicked,
}
#[derive(Clone, Copy)]
pub(crate) enum ObservationKind {
    ScopedJob,
    LegacyTask,
}
pub(crate) trait JobObservation: Send + Sync {
    fn kind(&self) -> ObservationKind {
        ObservationKind::ScopedJob
    }
    fn state(&self) -> Result<RuntimeJobState, FsError>;
}
struct Entry {
    owner: u32,
    job: u64,
    source: Weak<dyn JobObservation>,
}
pub(super) struct JobObservations {
    next: u64,
    entries: BTreeMap<u64, Entry>,
}
impl Default for JobObservations {
    fn default() -> Self {
        Self {
            next: 1,
            entries: BTreeMap::new(),
        }
    }
}
impl DataRoot {
    pub(crate) fn observe_job(
        &self,
        owner: u32,
        job: u64,
        source: Arc<dyn JobObservation>,
    ) -> Result<(), FsError> {
        let mut jobs = self.runtime_jobs.lock().map_err(|_| vfs::Error::Busy)?;
        jobs.entries
            .retain(|_, entry| entry.source.strong_count() != 0);
        if jobs.entries.len() >= 256 {
            return Err(vfs::Error::Quota.into());
        }
        let id = jobs.next;
        jobs.next = id.checked_add(1).ok_or(vfs::Error::Overflow)?;
        jobs.entries.insert(
            id,
            Entry {
                owner,
                job,
                source: Arc::downgrade(&source),
            },
        );
        Ok(())
    }
    pub(super) fn refresh_proc_jobs(&self) -> Result<(), FsError> {
        // Hold the observation registry through publication to prevent reversal.
        // Sources may lock control flags, never the job table or this registry.
        let mut jobs = self.runtime_jobs.lock().map_err(|_| vfs::Error::Busy)?;
        let mut rows = Vec::new();
        jobs.entries
            .retain(|_, entry| entry.source.strong_count() != 0);
        for (id, entry) in &jobs.entries {
            if let Some(source) = entry.source.upgrade() {
                let state = source.state()?;
                let (field, kind) = match source.kind() {
                    ObservationKind::ScopedJob => ("job_id", "scoped-job"),
                    ObservationKind::LegacyTask => ("task_id", "legacy-task"),
                };
                rows.push((*id, entry.owner, format!(
                    "observation_id: {id}\n{field}: {}\nowner_uid: {}\nstate: {state:?}\nkind: {kind}\n",
                    entry.job, entry.owner,
                ).into_bytes()));
            }
        }
        let vfs::Route::Mounted { volume, .. } = self.vfs.route("/proc")? else {
            return Err(vfs::Error::Unsupported.into());
        };
        Ok(volume.publish_job_rows(&rows)?)
    }
}
pub(super) type SharedJobObservations = Arc<Mutex<JobObservations>>;
