//! Account-aware entry points. Legacy data is readable under the old path
//! policy, but writing/deleting it never silently adopts an unknown owner.
use super::ownership::{authorize, payload, user_path};
use super::ownership_model::key;
use super::{DataRoot, FileStat, FsError, ObjectBinding, OwnedCommit, OwnershipState, VirtualMode};
use crate::permissions::Access;
use crate::users::Account;
use crate::config::ROOTFS_DEFAULTS;
use crate::virtual_path::VirtualPath;
use std::io::Read;

impl DataRoot {
    pub fn create_file_as(&self, user: &Account, path: &str) -> Result<(), FsError> {
        self.create_owned_file(user, path, b"", VirtualMode::new(0o600)?).map(|_| ())
    }
    pub fn read_file_as(&self, user: &Account, path: &str, limit: u64) -> Result<Vec<u8>, FsError> {
        let path = user_path(user, path, Access::Read)?.to_string();
        let canonical = key(&path)?;
        if limit > ROOTFS_DEFAULTS.max_file_io_bytes as u64 { return Err(FsError::IoRequestTooLarge(ROOTFS_DEFAULTS.max_file_io_bytes)); }
        let guard = self.namespace_guard()?;
        let catalog = match self.existing_owner_store()? {
            Some(store) => Some(self.load_owner_catalog(&store)?),
            None => None,
        };
        if let Some(info) = catalog.as_ref().and_then(|catalog| catalog.entries.get(&canonical)) {
            authorize(user, info, true, false)?;
            self.verify_public_binding(info, &guard)?;
            let mut bytes = Vec::new();
            self.dir.open(payload(info))?.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > limit { return Err(FsError::TooLarge(limit)); }
            return Ok(bytes);
        }
        // A host-only hardlink/case alias must not turn private managed bytes
        // into "legacy" data with weaker access rules. Unknown probes fail closed.
        if let Some(catalog) = &catalog {
            let relative = self.resolve(&path)?;
            if self.dir.metadata(&relative)?.is_file() {
                for info in catalog.entries.values() {
                    match self.compare_relative_objects(&relative, &payload(info), &guard)? {
                        ObjectBinding::DifferentObject => {}
                        _ => return Err(FsError::OwnershipBindingUnconfirmed),
                    }
                }
            }
        }
        self.read_file(&path, limit)
    }
    pub fn write_file_as(&self, user: &Account, path: &str, bytes: &[u8]) -> Result<(), FsError> {
        self.replace_owned_file(user, path, bytes).map(|_| ())
    }
    pub fn append_file_as(&self, user: &Account, path: &str, bytes: &[u8]) -> Result<(), FsError> {
        let count = self.append_owned_file(user, path, bytes)?;
        if count != bytes.len() { return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "short managed-file append").into()); }
        Ok(())
    }
    pub fn remove_file_as(&self, user: &Account, path: &str) -> Result<(), FsError> {
        self.remove_owned_file(user, path).map(|_| ())
    }
    pub fn copy_file_as(&self, user: &Account, source: &str, destination: &str) -> Result<super::CommitReceipt, FsError> {
        let bytes = self.read_file_as(user, source, ROOTFS_DEFAULTS.max_transaction_bytes as u64)?;
        let commit = self.create_owned_file(user, destination, &bytes, VirtualMode::new(0o600)?)?;
        Ok(super::CommitReceipt { bytes: bytes.len(), directory_durability: commit.directory_durability, cleanup: commit.cleanup })
    }
    pub fn rename_file_as(&self, user: &Account, source: &str, destination: &str) -> Result<super::RenameReceipt, FsError> {
        let commit = self.rename_owned_file(user, source, destination)?;
        Ok(super::RenameReceipt { source_directory: commit.directory_durability, destination_directory: commit.directory_durability })
    }
    pub fn stat_as(&self, user: &Account, path: &str) -> Result<FileStat, FsError> {
        let path = user_path(user, path, Access::Read)?.to_string();
        let guard = self.namespace_guard()?;
        if let Some(store) = self.existing_owner_store()? {
            let catalog = self.load_owner_catalog(&store)?;
            let canonical = key(&path)?;
            if let Some(info) = catalog.entries.get(&canonical) {
                authorize(user, info, true, false)?;
                self.verify_public_binding(info, &guard)?;
            } else {
                let relative = VirtualPath::strict(&path).map_err(|_| FsError::InvalidPath)?.as_relative();
                if self.dir.symlink_metadata(&relative).is_ok_and(|metadata| metadata.is_file()) {
                    for info in catalog.entries.values() {
                        if self.compare_relative_objects(&relative, &payload(info), &guard)? != ObjectBinding::DifferentObject {
                            return Err(FsError::OwnershipBindingUnconfirmed);
                        }
                    }
                }
            }
        }
        self.stat(&path)
    }
    pub fn ownership_as(&self, user: &Account, path: &str) -> Result<OwnershipState, FsError> {
        user_path(user, path, Access::Read)?;
        let state = self.owned_file_info(path)?;
        if let OwnershipState::Managed(info) = &state {
            if user.uid != 0 && user.uid != info.owner.uid { authorize(user, info, true, false)?; }
        }
        Ok(state)
    }
    pub fn open_file_as(&self, user: &Account, path: &str, mode: super::FileMode) -> Result<super::OpenFile, FsError> {
        if mode.readable() { user_path(user, path, Access::Read)?; }
        if mode.writable() { user_path(user, path, Access::Write)?; }
        let canonical = key(path)?;
        let guard = self.namespace_guard()?;
        let mut catalog = match self.existing_owner_store()? {
            Some(store) => self.load_owner_catalog(&store)?,
            None => super::ownership_model::Catalog::empty(),
        };
        if !catalog.entries.contains_key(&canonical) {
            match self.resolve(path) {
                Ok(_) if mode.creation() == super::FileCreation::Exclusive => return Err(FsError::AlreadyExists),
                Ok(_) if mode.writable() => return Err(FsError::LegacyOwnershipUnknown),
                Ok(_) => {
                    // Only an old read-only capability is granted here. Alias
                    // detection occurs under the same namespace lock.
                    let relative = self.resolve(path)?;
                    for info in catalog.entries.values() {
                        if self.compare_relative_objects(&relative, &payload(info), &guard)? != ObjectBinding::DifferentObject {
                            return Err(FsError::OwnershipBindingUnconfirmed);
                        }
                    }
                    return self.open_file(path, mode);
                }
                Err(FsError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound && mode.creation() != super::FileCreation::Existing => {
                    self.create_owned_file_locked(user, path, b"", VirtualMode::new(0o600)?, super::ownership::OwnershipFault::None, &guard)?;
                    let store = self.existing_owner_store()?.ok_or(FsError::UnownedOwnershipStore)?;
                    catalog = self.load_owner_catalog(&store)?;
                }
                Err(error) => return Err(error),
            }
        } else if mode.creation() == super::FileCreation::Exclusive { return Err(FsError::AlreadyExists); }
        let info = catalog.entries.get(&canonical).ok_or(FsError::InvalidOwnershipRecord)?.clone();
        authorize(user, &info, mode.readable(), mode.writable())?;
        self.verify_public_binding(&info, &guard)?;
        let root = std::sync::Arc::new(self.clone_file_domain()?);
        let mut options = cap_std::fs::OpenOptions::new();
        options.read(mode.readable()).write(mode.writable())
            .append(mode.behavior() == super::FileBehavior::Append)
            .truncate(mode.behavior() == super::FileBehavior::Truncate);
        let file = self.dir.open_with(payload(&info), &options)?;
        Ok(super::OpenFile::new_owned(file, mode, info, root))
    }

    pub fn chmod_as(&self, user: &Account, path: &str, mode: VirtualMode) -> Result<OwnedCommit, FsError> {
        self.chmod_owned_file(user, path, mode)
    }
}
