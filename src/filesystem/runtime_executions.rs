//! Bounded foreground interpreter invocations, separate from jobs and leases.
use super::{vfs, DataRoot, FsError};
use std::collections::BTreeMap;
use std::format;
use std::sync::{Arc, Mutex, Weak};

struct Entry {
    owner: u32,
    live: Weak<()>,
}
pub(super) struct Executions {
    next: u64,
    entries: BTreeMap<u64, Entry>,
}
impl Default for Executions {
    fn default() -> Self {
        Self { next: 1, entries: BTreeMap::new() }
    }
}
/// Only the active interpreter owns this capability; proc retains a weak token.
#[must_use]
pub(crate) struct ExecutionObservation {
    _live: Arc<()>,
}
pub(super) type SharedExecutions = Arc<Mutex<Executions>>;
impl DataRoot {
    pub(crate) fn begin_execution(&self, owner: u32) -> Result<ExecutionObservation, FsError> {
        let mut registry = self.runtime_executions.lock().map_err(|_| vfs::Error::Busy)?;
        registry.entries.retain(|_, entry| entry.live.strong_count() != 0);
        // Foreground admission cannot be exhausted by the separate 256-job pool.
        if registry.entries.len() >= 32 { return Err(vfs::Error::Quota.into()); }
        let id = registry.next;
        registry.next = id.checked_add(1).ok_or(vfs::Error::Overflow)?;
        let live = Arc::new(());
        registry.entries.insert(id, Entry { owner, live: Arc::downgrade(&live) });
        Ok(ExecutionObservation { _live: live })
    }
    pub(super) fn refresh_proc_executions(&self) -> Result<(), FsError> {
        let mut registry = self.runtime_executions.lock().map_err(|_| vfs::Error::Busy)?;
        registry.entries.retain(|_, entry| entry.live.strong_count() != 0);
        let rows: Vec<_> = registry.entries.iter().filter_map(|(id, entry)| {
            let _live = entry.live.upgrade()?;
            Some((*id, entry.owner, format!(
                "execution_id: {id}\nowner_uid: {}\nstate: Running\nkind: foreground-execution\n",
                entry.owner
            ).into_bytes()))
        }).collect();
        let vfs::Route::Mounted { volume, .. } = self.vfs.route("/proc")? else {
            return Err(vfs::Error::Unsupported.into());
        };
        Ok(volume.publish_execution_rows(&rows)?)
    }
}
