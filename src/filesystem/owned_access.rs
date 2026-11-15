//! Account-aware entry points. Legacy data is readable under the old path
//! policy, but writing/deleting it never silently adopts an unknown owner.
use super::ownership::{payload, user_path};
use super::ownership_model::key;
use super::{DataRoot, FileStat, FsError, ObjectBinding, OwnedCommit, OwnershipState, VirtualMode};
use crate::config::ROOTFS_DEFAULTS;
use crate::permissions::Access;
use crate::users::Account;
use crate::virtual_path::VirtualPath;
use std::io::Read;

impl DataRoot {
    pub fn create_file_as(&self, user: &Account, path: &str) -> Result<(), FsError> {
        self.create_owned_file(user, path, b"", VirtualMode::new(0o600)?)
            .map(|_| ())
    }
    pub fn create_file_with_options(
        &self,
        user: &Account,
        path: &str,
        bytes: &[u8],
        options: super::CreationOptions<'_>,
    ) -> Result<OwnedCommit, FsError> {
        user_path(user, path, Access::Write)?;
        if let super::vfs::Route::Mounted {
            volume,
            path: relative,
            ..
        } = self.mounted_name(user, path, Access::Write)?
        {
            options.condition(self, user, path)?;
            let id = volume.create_with_umask(
                &self.credentials_as(user)?,
                &relative,
                bytes,
                options.umask,
            )?;
            return self.virtual_commit(&volume, id);
        }
        let guard = self.namespace_guard()?;
        self.create_owned_file_configured_locked(
            user,
            path,
            bytes,
            (
                super::access_model::CreationMode::Requested(VirtualMode::new(0o666)?),
                super::ownership::OwnershipFault::None,
                options,
            ),
            &guard,
        )
    }
    pub fn read_file_as(&self, user: &Account, path: &str, limit: u64) -> Result<Vec<u8>, FsError> {
        let path = user_path(user, path, Access::Read)?.to_string();
        if let super::vfs::Route::Mounted { volume, path, .. } =
            self.mounted(user, &path, Access::Read)?
        {
            if limit > ROOTFS_DEFAULTS.max_file_io_bytes as u64 {
                return Err(FsError::IoRequestTooLarge(
                    ROOTFS_DEFAULTS.max_file_io_bytes,
                ));
            }
            let file = volume.open(
                &self.credentials_as(user)?,
                &path,
                super::FileMode::READ,
                super::vfs::ShareMode::Shared,
            )?;
            if file.stat()?.length > limit {
                return Err(FsError::TooLarge(limit));
            }
            return Ok(file.read(limit as usize)?);
        }
        if limit > ROOTFS_DEFAULTS.max_file_io_bytes as u64 {
            return Err(FsError::IoRequestTooLarge(
                ROOTFS_DEFAULTS.max_file_io_bytes,
            ));
        }
        let guard = self.namespace_guard()?;
        let path = self.resolve_host_locked(user, &path, Access::Read, true, &guard)?;
        let canonical = key(&path)?;
        self.search_locked(user, &path, &self.load_access_catalog()?)?;
        let catalog = match self.existing_owner_store()? {
            Some(store) => Some(self.load_owner_catalog(&store)?),
            None => None,
        };
        if let Some(info) = catalog
            .as_ref()
            .and_then(|catalog| catalog.entries.get(&canonical))
        {
            self.authorize_file(user, &info, true, false)?;
            self.verify_public_binding(&info, &guard)?;
            self.check_host_io_locked(&info, None, false, &guard)?;
            let mut bytes = Vec::new();
            self.dir
                .open(payload(&info))?
                .take(limit.saturating_add(1))
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > limit {
                return Err(FsError::TooLarge(limit));
            }
            return Ok(bytes);
        }
        // A host-only hardlink/case alias must not turn private managed bytes
        // into "legacy" data with weaker access rules. Unknown probes fail closed.
        if let Some(catalog) = &catalog {
            let relative = self.resolve(&path)?;
            if self.dir.metadata(&relative)?.is_file() {
                for info in catalog.entries.values() {
                    match self.compare_relative_objects(&relative, &payload(&info), &guard)? {
                        ObjectBinding::DifferentObject => {}
                        _ => return Err(FsError::OwnershipBindingUnconfirmed),
                    }
                }
            }
        }
        let relative = self.resolve(&path)?;
        if !self.dir.metadata(&relative)?.is_file() {
            return Err(FsError::NotFile);
        }
        let mut bytes = Vec::new();
        self.dir
            .open(relative)?
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(FsError::TooLarge(limit));
        }
        Ok(bytes)
    }
    pub fn write_file_as(&self, user: &Account, path: &str, bytes: &[u8]) -> Result<(), FsError> {
        self.replace_owned_file(user, path, bytes).map(|_| ())
    }
    pub fn append_file_as(&self, user: &Account, path: &str, bytes: &[u8]) -> Result<(), FsError> {
        let count = self.append_owned_file(user, path, bytes)?;
        if count != bytes.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "short managed-file append",
            )
            .into());
        }
        Ok(())
    }
    pub fn remove_file_as(&self, user: &Account, path: &str) -> Result<(), FsError> {
        if let super::vfs::Route::Mounted { volume, path, .. } =
            self.mounted_name(user, path, Access::Write)?
        {
            return Ok(volume.unlink(&self.credentials_as(user)?, &path, false)?);
        }
        self.remove_owned_file(user, path).map(|_| ())
    }
    pub fn copy_file_as(
        &self,
        user: &Account,
        source: &str,
        destination: &str,
    ) -> Result<super::CommitReceipt, FsError> {
        let bytes =
            self.read_file_as(user, source, ROOTFS_DEFAULTS.max_transaction_bytes as u64)?;
        let commit = self.create_owned_file(user, destination, &bytes, VirtualMode::new(0o600)?)?;
        Ok(super::CommitReceipt {
            bytes: bytes.len(),
            directory_durability: commit.directory_durability,
            cleanup: commit.cleanup,
        })
    }
    pub fn rename_file_as(
        &self,
        user: &Account,
        source: &str,
        destination: &str,
    ) -> Result<super::RenameReceipt, FsError> {
        let commit = self.rename_owned_file(user, source, destination)?;
        Ok(super::RenameReceipt {
            source_directory: commit.directory_durability,
            destination_directory: commit.directory_durability,
        })
    }
    pub fn stat_as(&self, user: &Account, path: &str) -> Result<FileStat, FsError> {
        if let super::vfs::Route::Mounted { volume, path, .. } =
            self.mounted_name(user, path, Access::Read)?
        {
            return Ok(super::vfs_bridge::virtual_stat(volume.stat(
                &self.credentials_as(user)?,
                &path,
                false,
            )?));
        }
        let path = user_path(user, path, Access::Read)?.to_string();
        let guard = self.namespace_guard()?;
        let path = self.resolve_host_locked(user, &path, Access::Read, false, &guard)?;
        let access = self.load_access_catalog()?;
        self.search_locked(user, &path, &access)?;
        if let Some(info) = self.directory_at_locked(&path, &access)? {
            self.directory_access_locked(user, &info, 1, &access)?;
        }
        if let Some(store) = self.existing_owner_store()? {
            let catalog = self.load_owner_catalog(&store)?;
            let canonical = key(&path)?;
            if let Some(info) = catalog.entries.get(&canonical) {
                if info.kind.is_regular() { self.authorize_file(user, &info, true, false)?; }
                self.verify_public_binding(&info, &guard)?;
                if let super::HostFileKind::Symlink { target } = info.kind {
                    let mut stat = self.stat(&path)?;
                    stat.kind = super::ObjectKind::Symlink;
                    stat.length = target.len() as u64;
                    return Ok(stat);
                }
            } else {
                let relative = VirtualPath::strict(&path)
                    .map_err(|_| FsError::InvalidPath)?
                    .as_relative();
                if self
                    .dir
                    .symlink_metadata(&relative)
                    .is_ok_and(|metadata| metadata.is_file())
                {
                    for info in catalog.entries.values() {
                        if self.compare_relative_objects(&relative, &payload(&info), &guard)?
                            != ObjectBinding::DifferentObject
                        {
                            return Err(FsError::OwnershipBindingUnconfirmed);
                        }
                    }
                }
            }
        }
        self.stat(&path)
    }
    pub fn ownership_as(&self, user: &Account, path: &str) -> Result<OwnershipState, FsError> {
        if let super::vfs::Route::Mounted { volume, path, .. } =
            self.mounted_name(user, path, Access::Read)?
        {
            return Ok(OwnershipState::Virtual {
                filesystem: volume.id(),
                inode: volume.stat(&self.credentials_as(user)?, &path, false)?,
            });
        }
        user_path(user, path, Access::Read)?;
        let guard = self.namespace_guard()?;
        let path = self.resolve_host_locked(user, path, Access::Read, false, &guard)?;
        let state = self.owned_file_info_locked(&path, &guard)?;
        if let OwnershipState::Managed(info) = &state {
            if user.uid != 0 && user.uid != info.owner.uid {
                self.authorize_file(user, info, true, false)?;
            }
        }
        Ok(state)
    }
    pub fn open_file_as(
        &self,
        user: &Account,
        path: &str,
        mode: super::FileMode,
    ) -> Result<super::OpenFile, FsError> {
        self.open_file_with_options(user, path, mode, super::CreationOptions::default())
    }
    pub fn open_file_with_options(
        &self,
        user: &Account,
        path: &str,
        mode: super::FileMode,
        options: super::CreationOptions<'_>,
    ) -> Result<super::OpenFile, FsError> {
        self.open_file_shared_with_options(user, path, mode, super::vfs::ShareMode::Shared, options)
    }
    pub fn open_file_shared_with_options(
        &self,
        user: &Account,
        path: &str,
        mode: super::FileMode,
        share: super::vfs::ShareMode,
        options: super::CreationOptions<'_>,
    ) -> Result<super::OpenFile, FsError> {
        if let super::system_providers::DeviceTarget::Virtual {
            volume,
            path: relative,
        } = self.system_device_as(user, path)?
        {
            options.condition(self, user, path)?;
            let file = volume.open_with_umask(
                &self.credentials_as(user)?,
                &relative,
                mode,
                share,
                options.umask,
            )?;
            return Ok(super::OpenFile::new_virtual(file, mode, path.to_owned()));
        }
        if let super::vfs::Route::Mounted {
            volume,
            path: relative,
            ..
        } = self.mounted_follow(
            user,
            path,
            if mode.writable() {
                Access::Write
            } else {
                Access::Read
            },
            mode.creation() != super::FileCreation::Exclusive,
        )? {
            options.condition(self, user, path)?;
            let file = volume.open_with_umask(
                &self.credentials_as(user)?,
                &relative,
                mode,
                share,
                options.umask,
            )?;
            return Ok(super::OpenFile::new_virtual(file, mode, path.to_owned()));
        }
        // Existing file opens grant capabilities, not a new lifecycle record.
        // New-file creation below always requires condition(), including /run.
        if options.scope.is_some() {
            options.condition(self, user, path)?;
        }
        use super::open::{classify_open_error, existing_options};
        use super::ownership::storage_class;
        use std::path::Path;
        if mode.readable() {
            user_path(user, path, Access::Read)?;
        }
        if mode.writable() {
            user_path(user, path, Access::Write)?;
        }
        let guard = self.namespace_guard()?;
        let resolved_path = self.resolve_host_locked(user, path, if mode.writable() { Access::Write } else { Access::Read }, mode.creation() != super::FileCreation::Exclusive, &guard)?;
        let path = resolved_path.as_str();
        let canonical = key(path)?;
        self.search_locked(user, path, &self.load_access_catalog()?)?;
        let mut created = false;
        let mut catalog = match self.existing_owner_store()? {
            Some(store) => self.load_owner_catalog(&store)?,
            None => super::ownership_model::Catalog::empty(),
        };
        if !catalog.entries.contains_key(&canonical) {
            match self.resolve(path) {
                Ok(relative) => {
                    let lookup = if relative.as_os_str().is_empty() {
                        Path::new(".")
                    } else {
                        &relative
                    };
                    let metadata = self
                        .dir
                        .metadata(lookup)
                        .map_err(|error| classify_open_error(error.into()))?;
                    if metadata.is_dir() {
                        return Err(FsError::IsDirectory);
                    }
                    if !metadata.is_file() {
                        return Err(FsError::NotFile);
                    }
                    if mode.writable() {
                        storage_class(path)?;
                    }
                    if mode.creation() == super::FileCreation::Exclusive {
                        return Err(FsError::AlreadyExists);
                    }
                    if mode.writable() {
                        return Err(FsError::LegacyOwnershipUnknown);
                    }
                    for info in catalog.entries.values() {
                        if self.compare_relative_objects(&relative, &payload(&info), &guard)?
                            != ObjectBinding::DifferentObject
                        {
                            return Err(FsError::OwnershipBindingUnconfirmed);
                        }
                    }
                    if share != super::vfs::ShareMode::Shared {
                        return Err(super::vfs::Error::Unsupported.into());
                    }
                    let file = self.dir.open_with(relative, &existing_options(mode))?;
                    return Ok(super::OpenFile::new(file, mode, path.to_owned()));
                }
                Err(FsError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    if mode.creation() == super::FileCreation::Existing {
                        return Err(FsError::NotFound);
                    }
                    self.check_host_open_capacity_locked(&guard)?;
                    self.create_owned_file_configured_locked(
                        user,
                        path,
                        b"",
                        (
                            super::access_model::CreationMode::Requested(VirtualMode::new(0o666)?),
                            super::ownership::OwnershipFault::None,
                            options,
                        ),
                        &guard,
                    )?;
                    created = true;
                    let store = self
                        .existing_owner_store()?
                        .ok_or(FsError::UnownedOwnershipStore)?;
                    catalog = self.load_owner_catalog(&store)?;
                }
                Err(error) => return Err(classify_open_error(error)),
            }
        } else if mode.creation() == super::FileCreation::Exclusive {
            return Err(FsError::AlreadyExists);
        }
        let info = catalog
            .entries
            .get(&canonical)
            .ok_or(FsError::InvalidOwnershipRecord)?
            .clone();
        if !created {
            self.authorize_file(user, &info, mode.readable(), mode.writable())?;
        }
        if mode.writable() {
            storage_class(path)?;
        }
        self.verify_public_binding(&info, &guard)?;
        if mode.writable() {
            let length = if mode.behavior() == super::FileBehavior::Truncate {
                0
            } else {
                self.dir.metadata(payload(&info))?.len()
            };
            self.check_resource_write_locked(&info, length, false)?;
        }
        let root = std::sync::Arc::new(self.clone_file_domain()?);
        let (id, lease_file) = self.register_host_open_locked(
            &info,
            share,
            mode.behavior() == super::FileBehavior::Truncate,
            &guard,
        )?;
        // Keep only the raw token until all fallible work under this gate ends;
        // OpenLease::drop takes the gate and must never run reentrantly here.
        if mode.behavior() == super::FileBehavior::Truncate {
            self.clear_privilege_locked(info.id, info.storage_generation, &guard)?;
        }
        let options = existing_options(mode);
        let file = self
            .dir
            .open_with(payload(&info), &options)
            .map_err(|error| classify_open_error(error.into()))?;
        drop(guard);
        let lease = super::host_locks::OpenLease {
            root,
            id,
            file: Some(lease_file),
        };
        Ok(super::OpenFile::new_owned(file, mode, info, lease))
    }

    pub fn chmod_as(
        &self,
        user: &Account,
        path: &str,
        mode: VirtualMode,
    ) -> Result<OwnedCommit, FsError> {
        if let super::vfs::Route::Mounted { volume, path, .. } =
            self.mounted(user, path, Access::Write)?
        {
            volume.chmod(&self.credentials_as(user)?, &path, mode)?;
            return self.virtual_commit(
                &volume,
                volume.stat(&self.credentials_as(user)?, &path, true)?.inode,
            );
        }
        let receipt = self.set_access_as(user, path, Some(mode), None, None)?;
        let guard = self.namespace_guard()?;
        let path = self.resolve_host_locked(user, path, Access::Write, true, &guard)?;
        let store = self
            .existing_owner_store()?
            .ok_or(FsError::UnownedOwnershipStore)?;
        let catalog = self.load_owner_catalog(&store)?;
        let info = catalog.entries.get(&key(&path)?).ok_or(FsError::LegacyOwnershipUnknown)?;
        self.verify_public_binding(&info, &guard)?;
        let revision = catalog.revision;
        drop(guard);
        Ok(OwnedCommit {
            object: super::CommitObject::Host(info.id),
            revision,
            directory_durability: receipt.directory_durability,
            cleanup: receipt.cleanup,
        })
    }
}
