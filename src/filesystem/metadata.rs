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
