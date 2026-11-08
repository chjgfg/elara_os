//! Startup rollback of owned transaction leftovers. Recovery never writes
//! destination files, never follows links and never sweeps ordinary /tmp data.
use super::backend::{linked, DataRoot};
use super::transaction::{discard_owned, lock_store, store_path, validate_store, NamespaceGuard, STORE_MARKER};
use super::types::FsError;
use std::io;

use super::RecoveryOutcome;

impl DataRoot {
    pub fn recover_transactions(&self) -> Result<RecoveryOutcome, FsError> {
        match self.dir.symlink_metadata(store_path()) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(RecoveryOutcome::NoStore)
            }
            Err(error) => return Err(error.into()),
            Ok(metadata) if linked(&metadata) || !metadata.is_dir() => {
                return Err(FsError::UnownedTransactionStore)
            }
            Ok(_) => {}
        }
        let store = validate_store(self)?;
        let lock = NamespaceGuard::from_locked(self, lock_store(&store)?);
        self.recover_transactions_locked(&lock)
    }

    /// The caller already holds the one shared mutation/recovery lock.
    pub(super) fn recover_transactions_locked(&self, guard: &NamespaceGuard<'_>) -> Result<RecoveryOutcome, FsError> {
        guard.require_root(self)?;
        self.recover_owner_intent_locked(guard)?;
        let store = validate_store(self)?;
        let mut names = Vec::new();
        for result in store.entries()? {
            let entry = result?;
            let name = entry.file_name();
            if name == STORE_MARKER || name == "store.lock" {
                continue;
            }
            names.push(name);
        }
        let mut discarded = 0;
        let mut preserved = 0;
        for name in names {
            if discard_owned(&store, &name)? {
                discarded += 1;
            } else {
                preserved += 1;
            }
        }
        Ok(RecoveryOutcome::Inspected {
            discarded,
            preserved,
        })
    }
}
