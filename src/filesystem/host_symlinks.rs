//! Virtual links are registered inodes, never native host symlinks/reparse points.
//! Resolution and the caller's operation share one namespace guard.
use super::{ownership_model::key, *};
use crate::{permissions::Access, users::Account, virtual_path::VirtualPath};
use std::{matches};

pub(super) fn validate_target(target: &str) -> Result<(), FsError> {
    if target.is_empty()
        || target.len() > 4096
        || target.contains([char::from(92), ':'])
        || target.chars().any(char::is_control)
    {
        return Err(FsError::InvalidPath);
    }
    for part in target
        .split('/')
        .filter(|s| !s.is_empty() && !matches!(*s, "." | ".."))
    {
        VirtualPath::strict(part).map_err(|_| FsError::InvalidPath)?;
    }
    Ok(())
}
impl DataRoot {
    pub(super) fn resolve_host_locked(
        &self,
        actor: &Account,
        path: &str,
        access: Access,
        follow: bool,
        guard: &super::transaction::NamespaceGuard<'_>,
    ) -> Result<String, FsError> {
        let resolved = self.resolve_namespace_locked(actor, path, access, follow, guard)?;
        match resolved.route {
            vfs::Route::Host => Ok(resolved.path),
            vfs::Route::Mounted { .. } => Err(vfs::Error::CrossDevice.into()),
        }
    }
    pub(super) fn create_host_symlink(
        &self,
        actor: &Account,
        target: &str,
        path: &str,
    ) -> Result<(), FsError> {
        validate_target(target)?;
        let guard = self.namespace_guard()?;
        self.create_owned_node_configured_locked(
            actor,
            path,
            target.as_bytes(),
            (
                super::access_model::CreationMode::Exact(VirtualMode::new(0o777)?),
                super::ownership::OwnershipFault::None,
                CreationOptions::default(),
                HostFileKind::Symlink {
                    target: target.to_owned(),
                },
            ),
            &guard,
        )?;
        Ok(())
    }
    pub(super) fn read_host_symlink(&self, actor: &Account, path: &str) -> Result<String, FsError> {
        let guard = self.namespace_guard()?;
        let path = self.resolve_host_locked(actor, path, Access::Read, false, &guard)?;
        let store = self
            .existing_owner_store()?
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        let info = self
            .load_owner_catalog(&store)?
            .entries
            .get(&key(&path)?)
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        self.verify_public_binding(&info, &guard)?;
        match info.kind {
            HostFileKind::Symlink { target } => Ok(target),
            HostFileKind::Regular => Err(FsError::InvalidPath),
        }
    }
}
