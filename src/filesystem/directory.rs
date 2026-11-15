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
enum DirectoryBackend {
    Host(Dir),
    Virtual(super::vfs::Directory),
}
pub struct DirectoryCursor {
    directory: DirectoryBackend,
    entries: Vec<DirectoryEntry>,
    position: usize,
    namespace: Option<NamespaceView>,
    host_view: Option<super::host_directory::HostDirectoryView>,
}
struct NamespaceView {
    registry: std::sync::Arc<super::vfs::Registry>,
    path: String,
    mounts: Vec<(String, std::sync::Arc<super::vfs::Volume>)>,
}
fn merge_mounts(
    entries: &mut Vec<DirectoryEntry>,
    mounts: &[(String, std::sync::Arc<super::vfs::Volume>)],
) -> Result<(), FsError> {
    for (name, _) in mounts {
        entries.retain(|e| e.name != std::ffi::OsStr::new(name));
        entries.push(DirectoryEntry {
            name: name.into(),
            kind: ObjectKind::Directory,
        });
    }
    if entries.len() > ROOTFS_DEFAULTS.max_directory_entries {
        return Err(FsError::DirectorySnapshotTooLarge);
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(())
}
impl DirectoryCursor {
    pub(super) fn with_host_links(
        mut self,
        root: &DataRoot,
        path: &str,
        guard: &super::transaction::NamespaceGuard<'_>,
    ) -> Result<Self, FsError> {
        if let DirectoryBackend::Host(dir) = &self.directory {
            let view = super::host_directory::HostDirectoryView::new(root, path)?;
            view.project(root, dir, &mut self.entries, guard)?;
            self.host_view = Some(view);
        }
        Ok(self)
    }
    pub(super) fn with_namespace(
        mut self,
        registry: std::sync::Arc<super::vfs::Registry>,
        path: String,
    ) -> Result<Self, FsError> {
        let mounts = self.mount_children(&registry, &path)?;
        merge_mounts(&mut self.entries, &mounts)?;
        self.namespace = Some(NamespaceView {
            registry,
            path,
            mounts,
        });
        Ok(self)
    }
    fn mount_children(
        &self,
        registry: &super::vfs::Registry,
        prefix: &str,
    ) -> Result<Vec<(String, std::sync::Arc<super::vfs::Volume>)>, FsError> {
        let path = match &self.directory {
            DirectoryBackend::Host(_) => prefix.to_owned(),
            DirectoryBackend::Virtual(dir) => match dir.namespace_path() {
                Ok(path) if path == "/" => prefix.to_owned(),
                Ok(path) => std::format!("{prefix}{path}"),
                Err(super::vfs::Error::NotFound) => return Ok(Vec::new()),
                Err(error) => return Err(error.into()),
            },
        };
        Ok(registry.children(&path)?)
    }
    pub(super) fn virtual_directory(directory: super::vfs::Directory) -> Result<Self, FsError> {
        let entries = virtual_snapshot(&directory)?;
        Ok(Self {
            directory: DirectoryBackend::Virtual(directory),
            entries,
            position: 0,
            namespace: None,
            host_view: None,
        })
    }
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
        let mut entries = match &self.directory {
            DirectoryBackend::Host(dir) => match &self.host_view {
                Some(view) => view.snapshot(dir)?,
                None => snapshot(dir)?,
            },
            DirectoryBackend::Virtual(dir) => virtual_snapshot(dir)?,
        };
        if let Some(view) = &self.namespace {
            let mounts = self.mount_children(&view.registry, &view.path)?;
            merge_mounts(&mut entries, &mounts)?;
            if let Some(view) = &mut self.namespace {
                view.mounts = mounts;
            }
        }
        self.entries = entries;
        self.rewind();
        Ok(())
    }
}
impl DataRoot {
    pub fn open_directory(&self, virtual_path: &str) -> Result<DirectoryCursor, FsError> {
        if self.ownership_requires_actor(virtual_path)? {
            return Err(FsError::OwnershipContextRequired);
        }
        self.open_directory_unchecked(virtual_path)
    }
    pub(super) fn open_directory_unchecked(
        &self,
        virtual_path: &str,
    ) -> Result<DirectoryCursor, FsError> {
        self.validate_directory_unchecked(virtual_path)?;
        let relative = self.resolve(virtual_path)?;
        let lookup = if relative.as_os_str().is_empty() {
            Path::new(".")
        } else {
            &relative
        };
        let directory = self.dir.open_dir(lookup)?;
        let entries = snapshot(&directory)?;
        Ok(DirectoryCursor {
            directory: DirectoryBackend::Host(directory),
            entries,
            position: 0,
            namespace: None,
            host_view: None,
        })
    }
}
pub(super) fn snapshot(directory: &Dir) -> Result<Vec<DirectoryEntry>, FsError> {
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

fn virtual_snapshot(directory: &super::vfs::Directory) -> Result<Vec<DirectoryEntry>, FsError> {
    let entries = directory.entries()?;
    if entries.len() > ROOTFS_DEFAULTS.max_directory_entries {
        return Err(FsError::DirectorySnapshotTooLarge);
    }
    Ok(entries
        .into_iter()
        .map(|(name, stat)| DirectoryEntry {
            name: name.into(),
            kind: super::vfs_bridge::virtual_stat(stat).kind,
        })
        .collect())
}
