//! Explicit host-backend metadata. Host timestamps are not virtual ownership.
use super::backend::{linked, virtual_relative};
use super::{DataRoot, DirectoryDurability, FsError};
use cap_std::fs::Metadata;
use std::io;
use std::path::Path;
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    File,
    Directory,
    Symlink,
    BlockedLink,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timestamp {
    Available(SystemTime),
    Unavailable(io::ErrorKind),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStat {
    pub kind: ObjectKind,
    pub length: u64,
    pub created: Timestamp,
    pub modified: Timestamp,
    pub accessed: Timestamp,
    /// Host attribute only, never the virtual permission decision.
    pub host_readonly: bool,
}

pub(super) fn object_kind(metadata: &Metadata) -> ObjectKind {
    if linked(metadata) {
        ObjectKind::BlockedLink
    } else if metadata.is_file() {
        ObjectKind::File
    } else if metadata.is_dir() {
        ObjectKind::Directory
    } else {
        ObjectKind::Other
    }
}
fn timestamp(result: io::Result<cap_std::time::SystemTime>) -> Timestamp {
    match result {
        Ok(time) => Timestamp::Available(time.into_std()),
        Err(error) => Timestamp::Unavailable(error.kind()),
    }
}
impl DataRoot {
    /// Inspect a final blocked link without following it; intermediate links fail.
    pub fn stat(&self, virtual_path: &str) -> Result<FileStat, FsError> {
        let relative = virtual_relative(virtual_path)?;
        let parent = relative.parent().unwrap_or(Path::new(""));
        self.inspect_relative(parent)?;
        let lookup = if relative.as_os_str().is_empty() {
            Path::new(".")
        } else {
            &relative
        };
        let metadata = self.dir.symlink_metadata(lookup).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                FsError::NotFound
            } else {
                error.into()
            }
        })?;
        Ok(FileStat {
            kind: object_kind(&metadata),
            length: metadata.len(),
            created: timestamp(metadata.created()),
            modified: timestamp(metadata.modified()),
            accessed: timestamp(metadata.accessed()),
            host_readonly: metadata.permissions().readonly(),
        })
    }

    pub fn sync_directory(&self, virtual_path: &str) -> Result<DirectoryDurability, FsError> {
        self.validate_directory(virtual_path)?;
        let relative = self.resolve(virtual_path)?;
        let lookup = if relative.as_os_str().is_empty() {
            Path::new(".")
        } else {
            &relative
        };
        Ok(sync_relative_directory(self, lookup))
    }
}

pub(super) fn sync_relative_directory(data: &DataRoot, relative: &Path) -> DirectoryDurability {
    let lookup = if relative.as_os_str().is_empty() {
        Path::new(".")
    } else {
        relative
    };
    match data
        .dir
        .open_dir(lookup)
        .and_then(|dir| dir.into_std_file().sync_all())
    {
        Ok(()) => DirectoryDurability::Synced,
        Err(error) => DirectoryDurability::Unconfirmed(error.kind()),
    }
}

/// Host IDs are scoped to one data-root protocol, not globally comparable OS
/// inode numbers. Unknown/ProtectedLayout convey absence of an identity grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum InodeIdentity {
    HostFile(super::ObjectId),
    HostDirectory(u64),
    Virtual { filesystem: u64, inode: u64 },
    ProtectedLayout,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InodeAccess {
    Managed { owner: super::FileOwner, mode: super::VirtualMode },
    ProtectedLayout,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkCount {
    /// Registered virtual names, not the host OS st_nlink (private witnesses
    /// and payloads must not be counted as public user names).
    Known(u64),
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InodeTimes {
    Host { created: Timestamp, modified: Timestamp, accessed: Timestamp },
    /// Volume-local logical mutation ticks, deliberately not wall-clock time.
    Logical { created: u64, modified: u64, changed: u64 },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InodeMetadata {
    pub inode: InodeIdentity,
    pub kind: ObjectKind,
    pub length: u64,
    pub access: InodeAccess,
    pub links: LinkCount,
    pub times: InodeTimes,
}
impl DataRoot {
    /// Unified observation; no catalog adoption and no fabricated host inode,
    /// owner or clock. All Host projections share one namespace guard.
    pub fn inode_as(&self, actor: &crate::users::Account, path: &str, follow: bool) -> Result<InodeMetadata, FsError> {
        use super::{ownership_model::key, vfs::Route};
        use crate::permissions::Access;
        let guard = self.namespace_guard()?;
        let resolved = self.resolve_namespace_locked(actor, path, Access::Read, follow, &guard)?;
        let permissions = self.load_access_catalog()?;
        if let Route::Mounted { volume, path, .. } = resolved.route {
            let stat = volume.stat(&permissions.actor_credentials(actor), &path, false)?;
            let kind = super::vfs_bridge::virtual_stat(stat.clone()).kind;
            return Ok(InodeMetadata {
                inode: InodeIdentity::Virtual { filesystem: volume.id(), inode: stat.inode },
                kind, length: stat.length,
                access: InodeAccess::Managed { owner: stat.owner, mode: stat.mode },
                links: LinkCount::Known(stat.links),
                times: InodeTimes::Logical { created: stat.created, modified: stat.modified, changed: stat.changed },
            });
        }
        let path = resolved.path;
        let stat = self.stat_host_locked(actor, &path, &guard)?;
        let mut out = InodeMetadata {
            inode: InodeIdentity::Unknown, kind: stat.kind, length: stat.length,
            access: InodeAccess::Unknown, links: LinkCount::Unknown,
            times: InodeTimes::Host { created: stat.created, modified: stat.modified, accessed: stat.accessed },
        };
        if let Some(info) = self.directory_at_locked(&path, &permissions)? {
            out.inode = InodeIdentity::HostDirectory(info.id);
            out.access = InodeAccess::Managed { owner: info.owner, mode: info.mode };
            out.links = LinkCount::Known(1);
        } else if let Some(store) = self.existing_owner_store()? {
            let catalog = self.load_owner_catalog(&store)?;
            if let Some(info) = catalog.entries.get(&key(&path)?) {
                out.inode = InodeIdentity::HostFile(info.id);
                out.access = InodeAccess::Managed { owner: info.owner, mode: info.mode };
                out.links = LinkCount::Known(catalog.entries.names.values().filter(|id| **id == info.id).count() as u64);
            }
        }
        if out.inode == InodeIdentity::Unknown && self.is_protected(&path)? {
            out.inode = InodeIdentity::ProtectedLayout;
            out.access = InodeAccess::ProtectedLayout;
        }
        Ok(out)
    }
}
