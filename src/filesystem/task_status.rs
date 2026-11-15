//! Read-only lease observations. Never invokes recovery or host process APIs.
use super::{scope, DataRoot, FsError, TaskScopeId};
use cap_std::fs::OpenOptions;
use std::fs::TryLockError;
use std::io::{self, Read};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskLeaseState {
    /// A cooperative lease lock is held; not evidence of interpreter Running.
    LeaseHeld,
    Recoverable,
    MissingLease,
    Unconfirmed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskLeaseStatus {
    pub id: TaskScopeId,
    pub owner_uid: u32,
    pub state: TaskLeaseState,
}
impl DataRoot {
    pub(super) fn refresh_proc_tasks(&self) -> Result<(), FsError> {
        // Serialize observation and publication together, not two independent
        // snapshots that could publish in reversed order.
        let _guard = self.namespace_guard()?;
        let catalog = self.load_access_catalog()?;
        let mut rows = Vec::with_capacity(catalog.tasks.len());
        for (id, uid) in catalog.tasks {
            rows.push(TaskLeaseStatus {
                id,
                owner_uid: uid,
                state: self.task_lease_state(id, uid),
            });
        }
        let super::vfs::Route::Mounted { volume, .. } = self.vfs.route("/proc")? else {
            return Err(super::vfs::Error::Unsupported.into());
        };
        Ok(volume.publish_tasks(&rows)?)
    }
    pub(super) fn task_lease_state(&self, id: TaskScopeId, uid: u32) -> TaskLeaseState {
        let path = scope::lock_path(id);
        match self.dir.symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return TaskLeaseState::MissingLease,
            Err(_) => return TaskLeaseState::Unconfirmed,
            Ok(_) => {}
        }
        if self.inspect_relative(&path).is_err()
            || !self.dir.metadata(&path).is_ok_and(|m| m.is_file())
        {
            return TaskLeaseState::Unconfirmed;
        }
        let Ok(file) = self
            .dir
            .open_with(&path, OpenOptions::new().read(true).write(true))
        else {
            return TaskLeaseState::Unconfirmed;
        };
        let mut file = file.into_std();
        match file.try_lock() {
            Err(TryLockError::WouldBlock) => TaskLeaseState::LeaseHeld,
            Err(TryLockError::Error(_)) => TaskLeaseState::Unconfirmed,
            Ok(()) => {
                let mut bytes = Vec::new();
                let valid = (&mut file).take(129).read_to_end(&mut bytes).is_ok()
                    && bytes == scope::lock_magic(id, uid);
                if file.unlock().is_err() || !valid {
                    TaskLeaseState::Unconfirmed
                } else {
                    TaskLeaseState::Recoverable
                }
            }
        }
    }
}
