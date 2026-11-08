//! Pinned directory handles with bounded, sorted snapshots and explicit cursors.
use super::backend::visible_name;
use super::metadata::object_kind;
use super::{DataRoot, FsError, ObjectKind};
use crate::config::ROOTFS_DEFAULTS;
use cap_std::fs::Dir;
use std::ffi::OsString;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: OsString,
    pub kind: ObjectKind,
}

/// Pages refer to one snapshot, not a live directory or a POSIX dirent cookie.
/// Refresh atomically replaces the in-memory snapshot and rewinds on success.
pub struct DirectoryCursor {
    directory: Dir,
    entries: Vec<DirectoryEntry>,
    position: usize,
}
impl DirectoryCursor {
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn position(&self) -> usize {
        self.position
    }
    pub fn rewind(&mut self) {
        self.position = 0;
    }
    pub fn seek(&mut self, position: usize) -> Result<usize, FsError> {
        if position > self.entries.len() {
            return Err(FsError::InvalidOffset);
        }
        self.position = position;
        Ok(position)
    }
    pub fn read(&mut self, maximum: usize) -> Result<Vec<DirectoryEntry>, FsError> {
        if maximum > ROOTFS_DEFAULTS.max_directory_page {
            return Err(FsError::DirectoryPageTooLarge);
        }
        let end = self
            .position
            .saturating_add(maximum)
            .min(self.entries.len());
        let mut page = Vec::new();
        page.try_reserve_exact(end - self.position)
            .map_err(|_| FsError::AllocationFailed)?;
        page.extend_from_slice(&self.entries[self.position..end]);
        self.position = end;
        Ok(page)
    }
    pub fn refresh(&mut self) -> Result<(), FsError> {
        let entries = snapshot(&self.directory)?;
        self.entries = entries;
        self.rewind();
        Ok(())
    }
}
impl DataRoot {
    pub fn open_directory(&self, virtual_path: &str) -> Result<DirectoryCursor, FsError> {
        self.validate_directory(virtual_path)?;
        let relative = self.resolve(virtual_path)?;
        let lookup = if relative.as_os_str().is_empty() {
            Path::new(".")
        } else {
            &relative
        };
        let directory = self.dir.open_dir(lookup)?;
        let entries = snapshot(&directory)?;
        Ok(DirectoryCursor {
            directory,
            entries,
            position: 0,
        })
    }
}
fn snapshot(directory: &Dir) -> Result<Vec<DirectoryEntry>, FsError> {
    let mut entries = Vec::new();
    for result in directory.entries()? {
        let entry = result?;
        let name = entry.file_name();
        if !visible_name(&name) {
            continue;
        }
        if entries.len() >= ROOTFS_DEFAULTS.max_directory_entries {
            return Err(FsError::DirectorySnapshotTooLarge);
        }
        let metadata = directory.symlink_metadata(&name)?;
        entries
            .try_reserve(1)
            .map_err(|_| FsError::AllocationFailed)?;
        entries.push(DirectoryEntry {
            name,
            kind: object_kind(&metadata),
        });
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}
