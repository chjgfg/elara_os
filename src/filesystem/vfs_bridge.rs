//! Account-aware bridge; mounts cannot redirect IO into host paths.
use super::{
    vfs::{self, Route},
    *,
};
use crate::{permissions::Access, users::Account};
use std::matches;
use std::sync::Arc;
/// A checked, search-only working directory with a cooperative name lease.
/// Bootstrap Host directories are protected by the immutable namespace policy.
#[derive(Clone, Debug)]
pub struct WorkingDirectory {
    path: String,
    family: Arc<()>,
    backing: WorkingBacking,
}
#[derive(Clone, Debug)]
enum WorkingBacking {
    Host(super::host_cwd::HostAnchor),
    Mounted(vfs::DirectoryAnchor),
}
impl WorkingDirectory {
    pub fn path(&self) -> &str {
        &self.path
    }
    pub(crate) fn belongs_to(&self, root: &DataRoot) -> bool {
        Arc::ptr_eq(&self.family, &root.family)
    }
    pub(crate) fn validate_as(&self, root: &DataRoot, actor: &Account) -> Result<(), FsError> {
        if !self.belongs_to(root) { return Err(FsError::InvalidRoot); }
        match &self.backing {
            WorkingBacking::Host(anchor) => root.validate_host_anchor(actor, &self.path, anchor),
            WorkingBacking::Mounted(_) => Ok(()),
        }
    }
    pub fn volume_id(&self) -> Option<u64> {
        match &self.backing {
            WorkingBacking::Host(_) => None,
            WorkingBacking::Mounted(pin) => Some(pin.volume_id()),
        }
    }
}
impl DataRoot {
    pub fn working_directory_as(
        &self,
        actor: &Account,
        path: &str,
    ) -> Result<WorkingDirectory, FsError> {
        let (path, backing) = match self.mounted(actor, path, Access::Read)? {
            Route::Host => {
                let guard = self.namespace_guard()?;
                let path = self.resolve_host_locked(actor, path, Access::Read, true, &guard)?;
                let catalog = self.load_access_catalog()?;
                let pin = self.anchor_host_directory_locked(actor, &path, &catalog, &guard)?;
                (path, WorkingBacking::Host(pin))
            }
            Route::Mounted {
                volume,
                path,
                mount,
            } => {
                let pin = volume.anchor_directory(&self.credentials_as(actor)?, &path)?;
                let path = if pin.path() == "/" {
                    mount
                } else {
                    std::format!("{mount}{}", pin.path())
                };
                (path, WorkingBacking::Mounted(pin))
            }
        };
        Ok(WorkingDirectory {
            path,
            family: self.family.clone(),
            backing,
        })
    }
    fn mount_destination(
        &self,
        actor: &Account,
        point: &str,
        credentials: &Credentials,
    ) -> Result<String, FsError> {
        if actor.uid != 0 {
            return Err(FsError::OwnershipDenied);
        }
        let path = super::ownership::user_path(actor, point, Access::Write)?.to_string();
        if !path.starts_with("/mnt/") {
            return Err(FsError::InvalidPath);
        }
        match self.vfs.route(&path)? {
            Route::Host => {
                if path[5..].contains('/') {
                    return Err(FsError::InvalidPath);
                }
                match self
                    .dir
                    .symlink_metadata(super::backend::virtual_relative(&path)?)
                {
                    Ok(_) => return Err(FsError::AlreadyExists),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Route::Mounted {
                volume,
                path: relative,
                ..
            } => match volume.stat(credentials, &relative, false) {
                Ok(_) => return Err(FsError::AlreadyExists),
                Err(vfs::Error::NotFound) => {}
                Err(error) => return Err(error.into()),
            },
        }
        Ok(path)
    }
    pub(super) fn attach_volume_as(
        &self,
        actor: &Account,
        point: &str,
        volume: Arc<vfs::Volume>,
    ) -> Result<(), FsError> {
        let credentials = self.credentials_as(actor)?;
        // Same host namespace gate as create/rename: no check-then-publish gap.
        let _guard = self.namespace_guard()?;
        let point = self.mount_destination(actor, point, &credentials)?;
        Ok(self.vfs.mount(&credentials, &point, volume)?)
    }
    pub fn save_volume_as(
        &self,
        actor: &Account,
        point: &str,
        image: &str,
    ) -> Result<(OwnedCommit, vfs::ImageStats), FsError> {
        if actor.uid != 0 {
            return Err(FsError::OwnershipDenied);
        }
        let Route::Mounted { volume, path, .. } = self.mounted(actor, point, Access::Read)? else {
            return Err(vfs::Error::Unsupported.into());
        };
        if path != "/" || !matches!(self.vfs.route(image)?, Route::Host) {
            return Err(FsError::InvalidPath);
        }
        let (bytes, stats) = volume.image(&self.credentials_as(actor)?)?;
        let receipt = match self.owned_file_info(image)? {
            OwnershipState::Managed(_) => self.replace_owned_file(actor, image, &bytes)?,
            OwnershipState::LegacyUnknown => {
                self.create_owned_file(actor, image, &bytes, VirtualMode::new(0o600)?)?
            }
            _ => return Err(FsError::OwnershipBindingUnconfirmed),
        };
        Ok((receipt, stats))
    }
    pub fn load_volume_as(
        &self,
        actor: &Account,
        image: &str,
        point: &str,
    ) -> Result<vfs::Recovery, FsError> {
        if actor.uid != 0 {
            return Err(FsError::OwnershipDenied);
        }
        if !matches!(self.vfs.route(image)?, Route::Host) {
            return Err(FsError::InvalidPath);
        }
        let bytes = self.read_file_as(
            actor,
            image,
            crate::config::ROOTFS_DEFAULTS.max_file_io_bytes as u64,
        )?;
        let loaded = vfs::Volume::load_image(&bytes)?;
        self.attach_volume_as(actor, point, loaded.volume)?;
        Ok(loaded.recovery)
    }
    pub fn mount_memory_as(
        &self,
        actor: &Account,
        point: &str,
        limits: vfs::Limits,
    ) -> Result<(), FsError> {
        let credentials = self.credentials_as(actor)?;
        if actor.uid != 0 {
            return Err(FsError::OwnershipDenied);
        }
        let volume = vfs::Volume::memory(
            FileOwner {
                uid: actor.uid,
                gid: credentials.gid,
            },
            limits,
        )?;
        self.attach_volume_as(actor, point, volume)?;
        Ok(())
    }
    pub fn mount_pseudo_as(
        &self,
        actor: &Account,
        point: &str,
        kind: vfs::PseudoKind,
    ) -> Result<(), FsError> {
        self.attach_volume_as(actor, point, vfs::Volume::pseudo(kind)?)?;
        Ok(())
    }
    pub fn unmount_as(&self, actor: &Account, point: &str) -> Result<(), FsError> {
        Ok(self.vfs.unmount(&self.credentials_as(actor)?, point)?)
    }
    pub fn mounts_as(&self, actor: &Account) -> Result<Vec<String>, FsError> {
        if actor.uid != 0 {
            return Err(FsError::OwnershipDenied);
        }
        Ok(self.vfs.mounts()?)
    }
    pub(super) fn mounted(
        &self,
        actor: &Account,
        path: &str,
        access: Access,
    ) -> Result<Route, FsError> {
        self.mounted_follow(actor, path, access, true)
    }
    pub(super) fn mounted_name(
        &self,
        actor: &Account,
        path: &str,
        access: Access,
    ) -> Result<Route, FsError> {
        self.mounted_follow(actor, path, access, false)
    }
    pub(super) fn mounted_follow(
        &self,
        actor: &Account,
        path: &str,
        access: Access,
        follow: bool,
    ) -> Result<Route, FsError> {
        let guard = self.namespace_guard()?;
        Ok(self.resolve_namespace_locked(actor, path, access, follow, &guard)?.route)
    }
    pub fn symlink_global_as(
        &self,
        actor: &Account,
        target: &str,
        path: &str,
    ) -> Result<(), FsError> {
        if !target.starts_with('/') || target.len() > 4096 {
            return Err(FsError::InvalidPath);
        }
        crate::virtual_path::VirtualPath::strict(target).map_err(|_| FsError::InvalidPath)?;
        match self.mounted_name(actor, path, Access::Write)? {
            Route::Mounted { volume, path, .. } => {
                volume.symlink_global(&self.credentials_as(actor)?, target, &path)?;
                Ok(())
            }
            Route::Host => self.create_host_symlink(actor, target, path),
        }
    }
    pub fn hardlink_as(
        &self,
        actor: &Account,
        source: &str,
        destination: &str,
    ) -> Result<(), FsError> {
        let a = self.mounted_name(actor, source, Access::Read)?;
        let b = self.mounted_name(actor, destination, Access::Write)?;
        if std::matches!((&a, &b), (Route::Host, Route::Host)) {
            return self.hardlink_owned_file(actor, source, destination);
        }
        let (volume, source, destination) = mounted_pair(a, b)?;
        Ok(volume.link(&self.credentials_as(actor)?, &source, &destination)?)
    }
    pub fn symlink_as(&self, actor: &Account, target: &str, path: &str) -> Result<(), FsError> {
        match self.mounted_name(actor, path, Access::Write)? {
            Route::Mounted { volume, path, .. } => {
                volume.symlink(&self.credentials_as(actor)?, target, &path)?;
                Ok(())
            }
            Route::Host => self.create_host_symlink(actor, target, path),
        }
    }
    pub fn readlink_as(&self, actor: &Account, path: &str) -> Result<String, FsError> {
        match self.mounted_name(actor, path, Access::Read)? {
            Route::Mounted { volume, path, .. } => {
                Ok(volume.readlink(&self.credentials_as(actor)?, &path)?)
            }
            Route::Host => self.read_host_symlink(actor, path),
        }
    }
    pub fn rename_replace_as(
        &self,
        actor: &Account,
        source: &str,
        destination: &str,
    ) -> Result<(), FsError> {
        let from = self.mounted_name(actor, source, Access::Write)?;
        let to = self.mounted_name(actor, destination, Access::Write)?;
        if matches!((&from, &to), (Route::Host, Route::Host)) {
            return self.rename_replace_host_as(actor, source, destination);
        }
        let (volume, source, destination) = mounted_pair(from, to)?;
        Ok(volume.rename(&self.credentials_as(actor)?, &source, &destination, true)?)
    }
    pub fn volume_stats_as(&self, actor: &Account, path: &str) -> Result<vfs::Stats, FsError> {
        match self.mounted(actor, path, Access::Read)? {
            Route::Mounted { volume, .. } => Ok(volume.stats()?),
            Route::Host => Err(vfs::Error::Unsupported.into()),
        }
    }
    pub fn deduplicate_volume_as(
        &self,
        actor: &Account,
        point: &str,
    ) -> Result<vfs::DedupStats, FsError> {
        let Route::Mounted { volume, path, .. } = self.mounted(actor, point, Access::Write)? else {
            return Err(vfs::Error::Unsupported.into());
        };
        if path != "/" {
            return Err(FsError::InvalidPath);
        }
        Ok(volume.deduplicate(&self.credentials_as(actor)?)?)
    }
    pub fn volume_quota_as(
        &self,
        actor: &Account,
        path: &str,
        uid: u32,
        limits: vfs::Limits,
    ) -> Result<(), FsError> {
        match self.mounted(actor, path, Access::Write)? {
            Route::Mounted { volume, .. } => {
                Ok(volume.quota(&self.credentials_as(actor)?, uid, limits)?)
            }
            Route::Host => Err(vfs::Error::Unsupported.into()),
        }
    }
    pub fn snapshot_mount_as(
        &self,
        actor: &Account,
        source: &str,
        destination: &str,
        readonly: bool,
    ) -> Result<(), FsError> {
        if actor.uid != 0 {
            return Err(FsError::OwnershipDenied);
        }
        let credentials = self.credentials_as(actor)?;
        let Route::Mounted { volume, path, .. } = self.mounted(actor, source, Access::Read)? else {
            return Err(vfs::Error::Unsupported.into());
        };
        if path != "/" {
            return Err(FsError::InvalidPath);
        }
        self.attach_volume_as(actor, destination, volume.snapshot(&credentials, readonly)?)?;
        Ok(())
    }
    pub fn overlay_mount_as(
        &self,
        actor: &Account,
        lower: &str,
        upper: &str,
        destination: &str,
        whiteouts: &[String],
    ) -> Result<(), FsError> {
        if actor.uid != 0 {
            return Err(FsError::OwnershipDenied);
        }
        let Route::Mounted {
            volume: lower,
            path: lower_path,
            ..
        } = self.mounted(actor, lower, Access::Read)?
        else {
            return Err(vfs::Error::Unsupported.into());
        };
        let Route::Mounted {
            volume: upper,
            path: upper_path,
            ..
        } = self.mounted(actor, upper, Access::Read)?
        else {
            return Err(vfs::Error::Unsupported.into());
        };
        if lower_path != "/" || upper_path != "/" {
            return Err(FsError::InvalidPath);
        }
        let volume =
            vfs::Volume::overlay_snapshot(&self.credentials_as(actor)?, &lower, &upper, whiteouts)?;
        self.attach_volume_as(actor, destination, volume)
    }
    pub(super) fn virtual_commit(
        &self,
        volume: &Arc<vfs::Volume>,
        id: u64,
    ) -> Result<OwnedCommit, FsError> {
        Ok(OwnedCommit {
            object: CommitObject::Virtual {
                filesystem: volume.id(),
                inode: id,
            },
            revision: volume.revision()?,
            directory_durability: DirectoryDurability::Unconfirmed(std::io::ErrorKind::Unsupported),
            cleanup: CleanupStatus::Complete,
        })
    }
}
pub(super) fn volatile_receipt(bytes: usize) -> CommitReceipt {
    CommitReceipt {
        bytes,
        directory_durability: DirectoryDurability::Unconfirmed(std::io::ErrorKind::Unsupported),
        cleanup: CleanupStatus::Complete,
    }
}
pub(super) fn virtual_stat(stat: vfs::InodeStat) -> FileStat {
    FileStat {
        kind: match stat.kind {
            vfs::InodeKind::File => ObjectKind::File,
            vfs::InodeKind::Directory => ObjectKind::Directory,
            vfs::InodeKind::Symlink => ObjectKind::Symlink,
            vfs::InodeKind::Device => ObjectKind::Other,
        },
        length: stat.length,
        created: Timestamp::Unavailable(std::io::ErrorKind::Unsupported),
        modified: Timestamp::Unavailable(std::io::ErrorKind::Unsupported),
        accessed: Timestamp::Unavailable(std::io::ErrorKind::Unsupported),
        host_readonly: false,
    }
}

/// Mutations keep the resolved volume pinned and never re-route the original alias.
pub(super) fn mounted_pair(a: Route, b: Route) -> Result<(Arc<vfs::Volume>, String, String), FsError> {
    match (a, b) {
        (Route::Mounted { volume: a, path: source, .. }, Route::Mounted { volume: b, path: destination, .. })
            if Arc::ptr_eq(&a, &b) => Ok((a, source, destination)),
        _ => Err(vfs::Error::CrossDevice.into()),
    }
}
