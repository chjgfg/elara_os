//! Virtual executable images, never host processes or host permission changes.
//! Authorization and bytes are captured under one namespace lock. The resulting
//! image cannot be changed through an old FD or by replacing its pathname.
use super::ownership::{payload, user_path};
use super::ownership_model::key;
use super::{Credentials, DataRoot, FsError, OwnedFileInfo};
use crate::config::ROOTFS_DEFAULTS;
use crate::permissions::Access;
use crate::users::Account;
use std::io::Read;

/// A bounded, immutable snapshot. Credentials describe the load authorization,
/// not a grant to run future filesystem operations as a different identity.
#[derive(Debug)]
pub enum ExecutableFile {
    Host(OwnedFileInfo),
    Virtual {
        filesystem: u64,
        inode: super::vfs::InodeStat,
    },
}
impl ExecutableFile {
    pub fn owner(&self) -> super::FileOwner {
        match self {
            Self::Host(file) => file.owner,
            Self::Virtual { inode, .. } => inode.owner,
        }
    }
    pub fn mode(&self) -> super::VirtualMode {
        match self {
            Self::Host(file) => file.mode,
            Self::Virtual { inode, .. } => inode.mode,
        }
    }
}
pub struct ExecutableImage {
    file: ExecutableFile,
    authorized_as: Credentials,
    bytes: Vec<u8>,
}
impl std::fmt::Debug for ExecutableImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutableImage")
            .field("file", &self.file)
            .field("authorized_as", &self.authorized_as)
            .field("byte_count", &self.bytes.len())
            .finish_non_exhaustive()
    }
}
impl ExecutableImage {
    pub fn file(&self) -> &ExecutableFile {
        &self.file
    }
    pub fn authorized_as(&self) -> &Credentials {
        &self.authorized_as
    }
    /// Derive only virtual credentials; no host account or process operation.
    pub(crate) fn execution_account(
        &self,
        root: &DataRoot,
        actor: &Account,
    ) -> Result<Account, FsError> {
        use crate::users::{ExecutionIdentity, Identity};
        let prior = match &actor.identity {
            Identity::Executed(id) => id.clone(),
            Identity::Login => {
                let mut supplementary = self.authorized_as.groups.clone();
                supplementary.remove(&self.authorized_as.gid);
                ExecutionIdentity {
                    real_uid: actor.uid,
                    real_gid: self.authorized_as.gid,
                    effective_gid: self.authorized_as.gid,
                    saved_uid: actor.uid,
                    saved_gid: self.authorized_as.gid,
                    supplementary,
                }
            }
        };
        let mut result = actor.clone();
        if self.file.mode().is_setuid() && self.file.owner().uid != actor.uid {
            let accounts = crate::users::load(root).map_err(|_| FsError::InvalidOwnershipRecord)?;
            let owner = accounts
                .iter()
                .find(|a| a.uid == self.file.owner().uid)
                .ok_or(FsError::InvalidOwnershipRecord)?;
            result.uid = owner.uid;
            result.name = owner.name.clone();
            result.home = owner.home.clone();
        }
        let gid = if self.file.mode().is_setgid() {
            self.file.owner().gid
        } else {
            prior.effective_gid
        };
        result.identity = Identity::Executed(ExecutionIdentity {
            effective_gid: gid,
            saved_uid: result.uid,
            saved_gid: gid,
            ..prior
        });
        Ok(result)
    }
    // Interpreter-internal content: an execute grant is not a public read API.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl DataRoot {
    /// Only registered regular files are executable. No legacy/host fallback.
    /// Execute permission does not grant the caller file-content read permission.
    /// Even virtual root needs at least one execute mode bit on a regular file.
    pub fn load_executable_as(
        &self,
        user: &Account,
        path: &str,
        limit: u64,
    ) -> Result<ExecutableImage, FsError> {
        self.load_executable_checked(user, path, limit, |owner, mode, credentials, acl| {
            if mode.bits() & 0o111 == 0 || !acl.permits(owner, mode, credentials, 1) {
                return Err(FsError::OwnershipDenied);
            }
            Ok(())
        })
    }

    fn load_executable_checked(
        &self,
        user: &Account,
        path: &str,
        limit: u64,
        check: impl FnOnce(
            super::FileOwner,
            super::VirtualMode,
            &Credentials,
            &super::Acl,
        ) -> Result<(), FsError>,
    ) -> Result<ExecutableImage, FsError> {
        let path = user_path(user, path, Access::Read)?.to_string();
        if limit > ROOTFS_DEFAULTS.max_file_io_bytes as u64 {
            return Err(FsError::IoRequestTooLarge(
                ROOTFS_DEFAULTS.max_file_io_bytes,
            ));
        }
        if let super::vfs::Route::Mounted {
            volume,
            path: relative,
            ..
        } = self.mounted(user, &path, Access::Read)?
        {
            let credentials = self.credentials_as(user)?;
            let (inode, bytes) =
                volume.executable(&credentials, &relative, limit, |inode, acl| {
                    check(inode.owner, inode.mode, &credentials, acl)
                })?;
            return Ok(ExecutableImage {
                file: ExecutableFile::Virtual {
                    filesystem: volume.id(),
                    inode,
                },
                authorized_as: credentials,
                bytes,
            });
        }
        let guard = self.namespace_guard()?;
        let path = self.resolve_host_locked(user, &path, Access::Read, true, &guard)?;
        let access = self.load_access_catalog()?;
        self.search_locked(user, &path, &access)?;
        let relative = self
            .resolve(&path)
            .map_err(super::open::classify_open_error)?;
        if !self.dir.metadata(&relative)?.is_file() {
            return Err(FsError::NotFile);
        }
        let store = self
            .existing_owner_store()?
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        let catalog = self.load_owner_catalog(&store)?;
        let info = catalog
            .entries
            .get(&key(&path)?)
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        let credentials = access.actor_credentials(user);
        let acl = access
            .file_acl
            .get(&info.id.value())
            .cloned()
            .unwrap_or_default();
        check(info.owner, info.mode, &credentials, &acl)?;
        self.verify_public_binding(&info, &guard)?;
        self.check_host_io_locked(&info, None, false, &guard)?;
        let mut bytes = Vec::new();
        self.dir
            .open(payload(&info))?
            .take(limit + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(FsError::TooLarge(limit));
        }
        Ok(ExecutableImage {
            file: ExecutableFile::Host(info.clone()),
            authorized_as: credentials,
            bytes,
        })
    }

    #[cfg(test)]
    pub(super) fn load_without_execute_guard(
        &self,
        user: &Account,
        path: &str,
        limit: u64,
    ) -> Result<ExecutableImage, FsError> {
        self.load_executable_checked(user, path, limit, |_, _, _, _| Ok(()))
    }
}
