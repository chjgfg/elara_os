//! Search-only Host name leases. Reuse the bounded, recoverable open protocol;
//! directory IDs occupy a separate domain from file IDs. This is cooperative
//! namespace protection, not protection against a hostile host administrator.
use super::access::{directory_owner, DIRECTORY_MARKER};
use super::access_model::AccessCatalog;
use super::backend::virtual_relative;
use super::binding::controlled_binding;
use super::host_locks::OpenLease;
use super::transaction::NamespaceGuard;
use super::{DataRoot, FsError, ObjectBinding};
use crate::users::Account;
use cap_std::fs::OpenOptions;
use std::fs::File;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub(super) enum HostAnchor {
    /// Bootstrap directories cannot be renamed or deleted through the facade.
    Protected,
    Managed(Arc<ManagedAnchor>),
}
pub(super) struct ManagedAnchor {
    directory: u64,
    marker: Mutex<File>,
    _lease: OpenLease,
}
impl std::fmt::Debug for HostAnchor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Protected => f.write_str("ProtectedHostDirectory"),
            Self::Managed(anchor) => f.debug_tuple("HostDirectoryAnchor").field(&anchor.directory).finish(),
        }
    }
}
impl DataRoot {
    pub(super) fn anchor_host_directory_locked(
        &self,
        actor: &Account,
        path: &str,
        catalog: &AccessCatalog,
        guard: &NamespaceGuard<'_>,
    ) -> Result<HostAnchor, FsError> {
        guard.require_root(self)?;
        self.search_locked(actor, path, catalog)?;
        let Some(info) = self.directory_at_locked(path, catalog)? else {
            return if self.is_protected(path)? {
                self.validate_directory_unchecked(path)?;
                Ok(HostAnchor::Protected)
            } else {
                // Never substitute a path-only "pin" for an unknown identity.
                Err(FsError::LegacyOwnershipUnknown)
            };
        };
        self.directory_access_locked(actor, &info, 1, catalog)?;
        let marker = self.dir.open_with(
            directory_owner(info.id), OpenOptions::new().read(true).write(true),
        )?.into_std();
        let root = Arc::new(self.clone_file_domain()?);
        let (id, file) = self.register_host_cwd_locked(info.id, guard)?;
        Ok(HostAnchor::Managed(Arc::new(ManagedAnchor {
            directory: info.id,
            marker: Mutex::new(marker),
            _lease: OpenLease { root, id, file: Some(file) },
        })))
    }
    pub(super) fn validate_host_anchor(
        &self,
        actor: &Account,
        path: &str,
        anchor: &HostAnchor,
    ) -> Result<(), FsError> {
        let _guard = self.namespace_guard()?;
        let catalog = self.load_access_catalog()?;
        self.validate_application_locked(actor, &catalog)?;
        self.search_locked(actor, path, &catalog)?;
        match anchor {
            HostAnchor::Protected => {
                if !self.is_protected(path)? { return Err(FsError::InvalidPath); }
                self.validate_directory_unchecked(path)
            }
            HostAnchor::Managed(anchor) => {
                let info = self.directory_at_locked(path, &catalog)?
                    .ok_or(FsError::OwnershipBindingUnconfirmed)?;
                if info.id != anchor.directory { return Err(FsError::OwnershipBindingUnconfirmed); }
                self.directory_access_locked(actor, &info, 1, &catalog)?;
                let public = self.dir.open_with(
                    virtual_relative(path)?.join(DIRECTORY_MARKER),
                    OpenOptions::new().read(true).write(true),
                )?.into_std();
                let marker = anchor.marker.lock().map_err(|_| FsError::FileObjectPoisoned)?;
                if controlled_binding(&marker, &public) != ObjectBinding::SameObject {
                    return Err(FsError::OwnershipBindingUnconfirmed);
                }
                Ok(())
            }
        }
    }
}
