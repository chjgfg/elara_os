//! Clear virtual executable privilege before mutating bytes, including old FDs.
use super::transaction::NamespaceGuard;
use super::{DataRoot, FsError, ObjectId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PrivilegeClear {
    Unchanged,
    Cleared,
    DetachedGeneration,
}
impl DataRoot {
    pub(super) fn clear_privilege_locked(
        &self,
        id: ObjectId,
        generation: u64,
        guard: &NamespaceGuard<'_>,
    ) -> Result<PrivilegeClear, FsError> {
        guard.require_root(self)?;
        let Some(store) = self.existing_owner_store()? else {
            return Ok(PrivilegeClear::DetachedGeneration);
        };
        let old = self.load_owner_catalog(&store)?;
        let Some(info) = old
            .entries
            .values()
            .find(|v| v.id == id && v.storage_generation == generation)
        else {
            return Ok(PrivilegeClear::DetachedGeneration);
        };
        if info.mode.bits() & 0o6000 == 0 {
            return Ok(PrivilegeClear::Unchanged);
        }
        let mut new = old.clone();
        let changed = new
            .entries
            .inode_mut(&info.path)
            .ok_or(FsError::InvalidOwnershipRecord)?;
        changed.mode = changed.mode.without_privilege();
        new.revision = new
            .revision
            .checked_add(1)
            .ok_or(FsError::OwnershipIdentityExhausted)?;
        let access = self.load_access_catalog()?;
        let mut next = access.clone();
        next.bump()?;
        self.commit_policy_locked(access, next, Some((old, new)), guard)?;
        Ok(PrivilegeClear::Cleared)
    }
}
