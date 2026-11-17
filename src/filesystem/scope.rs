//! Durable task leases. Recovery probes exact registered locks and objects;
//! unknown /tmp names are never scanned, adopted or recursively removed.
use super::access_model::AccessCatalog;
use super::ownership::store_path;
use super::transaction::NamespaceGuard;
use super::{CleanupStatus, DataRoot, DeleteCondition, FsError, TaskScopeId, Umask};
use crate::users::Account;
use cap_std::fs::OpenOptions;
use std::fs::{File, TryLockError};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::{fmt, format, matches, write};
#[derive(Clone, Copy, Debug, Default)]
pub struct CreationOptions<'a> {
    pub umask: Umask,
    pub scope: Option<&'a TaskScope>,
}
impl CreationOptions<'_> {
    pub(super) fn condition(
        self,
        root: &DataRoot,
        user: &Account,
        path: &str,
    ) -> Result<DeleteCondition, FsError> {
        if let Some(scope) = self.scope {
            if !Arc::ptr_eq(&root.family, &scope.0.root.family)
                || scope.0.user.uid != user.uid
                || !scope.0.user.same_execution_domain(user)
                || root.load_access_catalog()?.tasks.get(&scope.id()) != Some(&user.uid)
            {
                return Err(FsError::TaskScopeMismatch);
            }
        }
        match root.resource_condition_locked(path)? {
            super::resource_model::CreationResource::Registered(condition) => return Ok(condition),
            super::resource_model::CreationResource::Ordinary => {}
        }
        match self.scope {
            Some(scope) => {
                if ephemeral_path(path) {
                    Ok(DeleteCondition::TaskExit(scope.id()))
                } else {
                    Ok(DeleteCondition::OwnerRequest)
                }
            }
            None if runtime_path(path) => Err(FsError::TaskScopeMismatch),
            None => Ok(DeleteCondition::OwnerRequest),
        }
    }
}
pub(super) fn runtime_path(path: &str) -> bool {
    crate::config::write_class(path) == crate::config::WriteClass::RuntimeState
}
pub(super) fn ephemeral_path(path: &str) -> bool {
    matches!(
        crate::config::write_class(path),
        crate::config::WriteClass::Temporary | crate::config::WriteClass::RuntimeState
    )
}
#[derive(Clone)]
pub struct TaskScope(Arc<Lease>);
impl fmt::Debug for TaskScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TaskScope({})", self.id().value())
    }
}
struct Lease {
    root: DataRoot,
    user: Account,
    id: TaskScopeId,
    lock: Option<File>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        // Drop never panics. Any failure leaves a registered lease for explicit
        // recovery; report paths use finish() while the caller can observe it.
        if let Ok(guard) = self.root.namespace_guard() {
            let result = self.root.reap_task_locked(self.id, &self.user, &guard);
            if result.is_ok_and(|report| report.deferred == 0) {
                self.lock.take();
                let _ = self.root.retire_task_locked(self.id, &guard);
            }
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TaskRecovery {
    pub files: usize,
    pub directories: usize,
    pub live: usize,
    pub deferred: usize,
}
impl TaskRecovery {
    pub fn cleanup(self) -> CleanupStatus {
        if self.deferred == 0 {
            CleanupStatus::Complete
        } else {
            CleanupStatus::Deferred(io::ErrorKind::Other)
        }
    }
}
pub(super) fn lock_path(id: TaskScopeId) -> PathBuf {
    store_path().join(format!("task-{}.lock", id.value()))
}
pub(super) fn lock_magic(id: TaskScopeId, uid: u32) -> Vec<u8> {
    format!("ELARA_TASK_LEASE_V1\n{} {uid}\n", id.value()).into_bytes()
}
impl TaskScope {
    pub fn id(&self) -> TaskScopeId {
        self.0.id
    }
    /// Consumes this reference. Other references represent a still-live task.
    pub fn finish(self) -> Result<TaskRecovery, FsError> {
        let mut lease = match Arc::try_unwrap(self.0) {
            Ok(lease) => lease,
            Err(_) => {
                return Ok(TaskRecovery {
                    live: 1,
                    ..TaskRecovery::default()
                })
            }
        };
        let guard = lease.root.namespace_guard()?;
        let report = lease.root.reap_task_locked(lease.id, &lease.user, &guard)?;
        if report.deferred == 0 {
            lease.lock.take();
            lease.root.retire_task_locked(lease.id, &guard)?;
        }
        // Release namespace lock before Lease::drop (which also takes that lock).
        drop(guard);
        Ok(report)
    }
}
impl DataRoot {
    pub fn begin_task_as(&self, user: &Account) -> Result<TaskScope, FsError> {
        let guard = self.namespace_guard()?;
        let old = self.load_access_catalog()?;
        if old.tasks.len() >= 64 {
            return Err(FsError::OwnershipCapacity);
        }
        let root = self.clone_file_domain()?;
        let mut new = old.clone();
        let id = TaskScopeId(new.next_task);
        new.next_task = new
            .next_task
            .checked_add(1)
            .ok_or(FsError::OwnershipIdentityExhausted)?;
        new.tasks.insert(id, user.uid);
        new.bump()?;
        self.commit_policy_locked(old, new, None, &guard)?;
        let mut lock = self
            .dir
            .open_with(
                lock_path(id),
                OpenOptions::new().read(true).write(true).create_new(true),
            )?
            .into_std();
        lock.write_all(&lock_magic(id, user.uid))?;
        lock.sync_all()?;
        lock.lock().map_err(FsError::TransactionLockUnavailable)?;
        Ok(TaskScope(Arc::new(Lease {
            root,
            user: user.clone(),
            id,
            lock: Some(lock),
        })))
    }
    pub fn recover_tasks(&self) -> Result<TaskRecovery, FsError> {
        let guard = self.namespace_guard()?;
        let catalog = self.load_access_catalog()?;
        let mut report = TaskRecovery::default();
        for (id, uid) in catalog.tasks {
            let user = Account {
                identity: Default::default(),
                name: format!("recovery-{uid}"),
                uid,
                home: "/tmp".into(),
            };
            let path = lock_path(id);
            if self
                .dir
                .symlink_metadata(&path)
                .is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
            {
                let directories = self
                    .load_access_catalog()?
                    .directories
                    .values()
                    .any(|d| d.delete_condition == DeleteCondition::TaskExit(id));
                let files = if let Some(store) = self.existing_owner_store()? {
                    self.load_owner_catalog(&store)?
                        .entries
                        .values()
                        .any(|f| f.delete_condition == DeleteCondition::TaskExit(id))
                } else {
                    false
                };
                if !directories && !files {
                    self.retire_task_locked(id, &guard)?;
                } else {
                    report.deferred += 1;
                }
                continue;
            }
            if self.inspect_relative(&path).is_err()
                || !self.dir.metadata(&path).is_ok_and(|m| m.is_file())
            {
                report.deferred += 1;
                continue;
            }
            let mut lock = match self
                .dir
                .open_with(&path, OpenOptions::new().read(true).write(true))
            {
                Ok(file) => file.into_std(),
                Err(_) => {
                    report.deferred += 1;
                    continue;
                }
            };
            match lock.try_lock() {
                Err(TryLockError::WouldBlock) => {
                    report.live += 1;
                    continue;
                }
                Err(TryLockError::Error(_)) => {
                    report.deferred += 1;
                    continue;
                }
                Ok(()) => {}
            }
            let mut bytes = Vec::new();
            if (&mut lock).take(129).read_to_end(&mut bytes).is_err()
                || bytes != lock_magic(id, uid)
            {
                report.deferred += 1;
                continue;
            }
            let reclaimed = self.reap_task_locked(id, &user, &guard)?;
            report.files += reclaimed.files;
            report.directories += reclaimed.directories;
            report.deferred += reclaimed.deferred;
            drop(lock);
            if reclaimed.deferred == 0 {
                self.retire_task_locked(id, &guard)?;
            }
        }
        Ok(report)
    }
    fn reap_task_locked(
        &self,
        id: TaskScopeId,
        user: &Account,
        guard: &NamespaceGuard<'_>,
    ) -> Result<TaskRecovery, FsError> {
        let catalog = self.load_access_catalog()?;
        if !catalog.tasks.contains_key(&id) {
            return Ok(TaskRecovery::default());
        }
        if catalog.tasks.get(&id) != Some(&user.uid) {
            return Err(FsError::TaskScopeMismatch);
        }
        let mut report = TaskRecovery::default();
        let reaper = Account {
            identity: Default::default(),
            name: "task-reaper".into(),
            uid: 0,
            home: "/root".into(),
        };
        if let Some(store) = self.existing_owner_store()? {
            let files = self.load_owner_catalog(&store)?;
            for info in files
                .entries
                .values()
                .filter(|info| info.delete_condition == DeleteCondition::TaskExit(id))
            {
                if info.owner.uid != user.uid || !ephemeral_path(&info.path) {
                    report.deferred += 1;
                    continue;
                }
                if self
                    .remove_owned_file_locked(&reaper, &info.path, guard)
                    .is_ok()
                {
                    report.files += 1;
                } else {
                    report.deferred += 1;
                }
            }
        }
        let mut directories: Vec<_> = catalog
            .directories
            .values()
            .filter(|d| d.delete_condition == DeleteCondition::TaskExit(id))
            .collect();
        directories.sort_by(|a, b| b.path.len().cmp(&a.path.len()));
        for info in directories {
            if self
                .remove_directory_locked(&info.path, &reaper, guard)
                .is_ok()
            {
                report.directories += 1;
            } else {
                report.deferred += 1;
            }
        }
        Ok(report)
    }
    fn retire_task_locked(
        &self,
        id: TaskScopeId,
        guard: &NamespaceGuard<'_>,
    ) -> Result<(), FsError> {
        let old = self.load_access_catalog()?;
        if !old.tasks.contains_key(&id) {
            return Ok(());
        }
        let mut new: AccessCatalog = old.clone();
        new.tasks.remove(&id);
        new.bump()?;
        // Objects are already gone. Keep registration until lock unlink succeeds.
        match self.dir.remove_file(lock_path(id)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        if let Some(store) = self.existing_owner_store()? {
            let files = self.load_owner_catalog(&store)?;
            new.file_acl
                .retain(|id, _| files.entries.values().any(|f| f.id.value() == *id));
        }
        self.commit_policy_locked(old, new, None, guard)?;
        Ok(())
    }
}
