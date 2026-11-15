//! Capability-directory IO backend. Only the initial grant uses an ambient
//! host path; all subsequent data IO is relative to the pinned root handle.
use super::{Entry, EntryKind, FsError};
use crate::config::ROOTFS_DEFAULTS;
use crate::virtual_path::VirtualPath;
use cap_std::ambient_authority;
#[cfg(windows)]
use cap_std::fs::MetadataExt;
use cap_std::fs::{Dir, Metadata, OpenOptions};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

pub struct DataRoot {
    pub(super) runtime_executions: super::runtime_executions::SharedExecutions,
    pub(super) runtime_jobs: super::runtime_jobs::SharedJobObservations,
    pub(super) vfs: std::sync::Arc<super::vfs::Registry>,
    pub(super) family: std::sync::Arc<()>,
    pub(super) root: PathBuf,
    pub(super) dir: Dir,
    pub(super) log_store_cache: std::sync::OnceLock<Dir>,
    #[cfg(test)]
    pub(super) bypass_write_policy: bool,
    #[cfg(test)]
    pub(super) transaction_fault: super::types::TransactionFault,
    #[cfg(test)]
    pub(super) access_fault: super::access::AccessFault,
    #[cfg(test)]
    pub(super) host_open_fault: super::host_locks::HostOpenFault,
}

impl DataRoot {
    pub fn open(root: &Path) -> Result<Self, FsError> {
        super::validate_root_location(root)?;
        check_plain_path(root)?;
        if !root.is_dir() {
            return Err(FsError::NotDirectory);
        }
        let root = root.canonicalize()?;
        let dir = Dir::open_ambient_dir(&root, ambient_authority())?;
        Self::from_granted_directory(root, dir)
    }

    /// Bootstrap transfers its pinned directory here; no ambient re-open.
    pub(crate) fn from_granted_directory(root: PathBuf, dir: Dir) -> Result<Self, FsError> {
        super::validate_root_location(&root)?;
        let marker = Path::new(ROOTFS_DEFAULTS.marker_filename);
        match dir.symlink_metadata(marker) {
            Ok(metadata) if linked(&metadata) => return Err(FsError::Link(root.join(marker))),
            Ok(metadata) if !metadata.is_file() => return Err(FsError::UnmarkedRoot),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(FsError::UnmarkedRoot)
            }
            Err(error) => return Err(error.into()),
        }
        let mut bytes = Vec::new();
        dir.open(marker)?
            .take((ROOTFS_DEFAULTS.marker_contents.len() + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.as_slice() != ROOTFS_DEFAULTS.marker_contents {
            return Err(FsError::UnmarkedRoot);
        }
        let data = Self {
            runtime_jobs: Default::default(),
            runtime_executions: Default::default(),
            family: std::sync::Arc::new(()),
            vfs: std::sync::Arc::new(super::vfs::Registry::default()),
            root,
            dir,
            log_store_cache: std::sync::OnceLock::new(),
            #[cfg(test)]
            bypass_write_policy: false,
            #[cfg(test)]
            transaction_fault: super::types::TransactionFault::None,
            #[cfg(test)]
            access_fault: super::access::AccessFault::None,
            #[cfg(test)]
            host_open_fault: Default::default(),
        };
        for name in ROOTFS_DEFAULTS.directories {
            data.validate_directory_unchecked(name)?;
        }
        data.install_system_providers()?;
        Ok(data)
    }

    /// Duplicate the already granted directory capability for a file object's
    /// synchronization domain. Never re-authorize an ambient host path.
    pub(super) fn clone_file_domain(&self) -> Result<Self, FsError> {
        Ok(Self {
            runtime_jobs: self.runtime_jobs.clone(),
            runtime_executions: self.runtime_executions.clone(),
            vfs: self.vfs.clone(),
            family: self.family.clone(),
            root: self.root.clone(),
            dir: self.dir.try_clone()?,
            log_store_cache: std::sync::OnceLock::new(),
            #[cfg(test)]
            bypass_write_policy: self.bypass_write_policy,
            #[cfg(test)]
            transaction_fault: self.transaction_fault,
            #[cfg(test)]
            access_fault: self.access_fault,
            #[cfg(test)]
            host_open_fault: self.host_open_fault,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn inspect_relative(&self, path: &Path) -> Result<(), FsError> {
        let mut current = PathBuf::new();
        for component in path.components() {
            let Component::Normal(name) = component else {
                return Err(FsError::InvalidPath);
            };
            current.push(name);
            let metadata = self.dir.symlink_metadata(&current)?;
            if linked(&metadata) {
                return Err(FsError::Link(self.root.join(&current)));
            }
            if current != path && !metadata.is_dir() {
                return Err(FsError::NotDirectory);
            }
        }
        Ok(())
    }

    pub(super) fn resolve(&self, virtual_path: &str) -> Result<PathBuf, FsError> {
        let relative = virtual_relative(virtual_path)?;
        self.inspect_relative(&relative)?;
        let lookup = if relative.as_os_str().is_empty() {
            Path::new(".")
        } else {
            &relative
        };
        self.dir.metadata(lookup)?;
        Ok(relative)
    }

    pub fn validate_directory(&self, virtual_path: &str) -> Result<(), FsError> {
        if self.ownership_requires_actor(virtual_path)? {
            return Err(FsError::OwnershipContextRequired);
        }
        self.validate_directory_unchecked(virtual_path)
    }
    pub(super) fn validate_directory_unchecked(&self, virtual_path: &str) -> Result<(), FsError> {
        let path = self.resolve(virtual_path)?;
        let lookup = if path.as_os_str().is_empty() {
            Path::new(".")
        } else {
            &path
        };
        if self.dir.metadata(lookup)?.is_dir() {
            Ok(())
        } else {
            Err(FsError::NotDirectory)
        }
    }

    pub fn list(&self, virtual_path: &str) -> Result<Vec<Entry>, FsError> {
        if self.ownership_requires_actor(virtual_path)? {
            return Err(FsError::OwnershipContextRequired);
        }
        self.list_unchecked(virtual_path)
    }
    pub(super) fn list_unchecked(&self, virtual_path: &str) -> Result<Vec<Entry>, FsError> {
        self.validate_directory_unchecked(virtual_path)?;
        let path = self.resolve(virtual_path)?;
        let lookup = if path.as_os_str().is_empty() {
            Path::new(".")
        } else {
            &path
        };
        let mut entries = Vec::new();
        for result in self.dir.read_dir(lookup)? {
            let entry = result?;
            let name = entry.file_name();
            if !visible_name(&name) {
                continue;
            }
            let metadata = self.dir.symlink_metadata(path.join(&name))?;
            let kind = if linked(&metadata) {
                EntryKind::BlockedLink
            } else if metadata.is_dir() {
                EntryKind::Directory
            } else {
                EntryKind::File
            };
            entries.push(Entry { name, kind });
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    pub(super) fn resolve_new(&self, virtual_path: &str) -> Result<PathBuf, FsError> {
        let relative = virtual_relative(virtual_path)?;
        relative.file_name().ok_or(FsError::ProtectedPath)?;
        let parent = relative.parent().unwrap_or(Path::new(""));
        self.inspect_relative(parent).map_err(|error| match error {
            FsError::Io(io) if io.kind() == io::ErrorKind::NotFound => FsError::NotDirectory,
            other => other,
        })?;
        let lookup = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        if !self.dir.metadata(lookup)?.is_dir() {
            return Err(FsError::NotDirectory);
        }
        match self.dir.symlink_metadata(&relative) {
            Ok(metadata) if linked(&metadata) => Err(FsError::Link(self.root.join(&relative))),
            Ok(_) => Err(FsError::AlreadyExists),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(relative),
            Err(error) => Err(error.into()),
        }
    }

    pub fn read_file(&self, virtual_path: &str, limit: u64) -> Result<Vec<u8>, FsError> {
        if self.ownership_requires_actor(virtual_path)? {
            return Err(FsError::OwnershipContextRequired);
        }
        let path = self.resolve(virtual_path)?;
        if !self.dir.metadata(&path)?.is_file() {
            return Err(FsError::NotFile);
        }
        let file = self.dir.open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(FsError::NotFile);
        }
        if metadata.len() > limit {
            return Err(FsError::TooLarge(limit));
        }
        let mut bytes = Vec::new();
        file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(FsError::TooLarge(limit));
        }
        Ok(bytes)
    }

    pub fn create_directory(&self, virtual_path: &str) -> Result<(), FsError> {
        self.resolve_new(virtual_path)?;
        self.ensure_write_allowed(virtual_path)?;
        let _lock = self.namespace_guard()?;
        self.ensure_write_allowed(virtual_path)?;
        let path = self.resolve_new(virtual_path)?;
        self.dir.create_dir(path)?;
        Ok(())
    }

    pub fn create_file(&self, virtual_path: &str) -> Result<(), FsError> {
        self.resolve_new(virtual_path)?;
        self.ensure_write_allowed(virtual_path)?;
        let _lock = self.namespace_guard()?;
        self.ensure_write_allowed(virtual_path)?;
        let path = self.resolve_new(virtual_path)?;
        self.dir
            .open_with(path, OpenOptions::new().write(true).create_new(true))?;
        Ok(())
    }

    pub(super) fn resolve_existing(
        &self,
        virtual_path: &str,
    ) -> Result<(PathBuf, Metadata), FsError> {
        if self.is_protected(virtual_path)? {
            return Err(FsError::ProtectedPath);
        }
        let path = self.resolve(virtual_path).map_err(|error| match error {
            FsError::Io(io) if io.kind() == io::ErrorKind::NotFound => FsError::NotFound,
            other => other,
        })?;
        self.ensure_write_allowed(virtual_path)?;
        let metadata = self.dir.symlink_metadata(&path)?;
        Ok((path, metadata))
    }

    pub fn remove_file(&self, virtual_path: &str) -> Result<(), FsError> {
        self.resolve_existing(virtual_path)?;
        let _lock = self.namespace_guard()?;
        let (path, metadata) = self.resolve_existing(virtual_path)?;
        if metadata.is_dir() {
            return Err(FsError::IsDirectory);
        }
        if !metadata.is_file() {
            return Err(FsError::NotFile);
        }
        self.dir.remove_file(path)?;
        Ok(())
    }

    pub fn remove_directory(&self, virtual_path: &str) -> Result<(), FsError> {
        self.resolve_existing(virtual_path)?;
        let _lock = self.namespace_guard()?;
        let (path, metadata) = self.resolve_existing(virtual_path)?;
        if !metadata.is_dir() {
            return Err(FsError::NotDirectory);
        }
        if self.dir.read_dir(&path)?.next().is_some() {
            return Err(FsError::DirectoryNotEmpty);
        }
        self.dir.remove_dir(path)?;
        Ok(())
    }

    /// Compatibility API: replacement is atomic-visible, not a promise of
    /// power-loss durability. Call replace_file for the explicit receipt.
    pub fn write_file(&self, virtual_path: &str, contents: &[u8]) -> Result<(), FsError> {
        self.replace_file(virtual_path, contents).map(|_| ())
    }

    pub fn append_file(&self, virtual_path: &str, contents: &[u8]) -> Result<(), FsError> {
        self.resolve_existing(virtual_path)?;
        let _lock = self.namespace_guard()?;
        let (path, metadata) = self.resolve_existing(virtual_path)?;
        if !metadata.is_file() {
            return Err(FsError::NotFile);
        }
        self.dir
            .open_with(path, OpenOptions::new().append(true))?
            .write_all(contents)?;
        Ok(())
    }
}

pub(super) fn visible_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return true;
    };
    !name.eq_ignore_ascii_case(ROOTFS_DEFAULTS.marker_filename)
        && !name.eq_ignore_ascii_case(ROOTFS_DEFAULTS.transaction_directory)
        && !name.eq_ignore_ascii_case(crate::config::PRIVATE_LOG_DIRECTORY)
        && !name.eq_ignore_ascii_case(crate::config::PRIVATE_OWNER_DIRECTORY)
        && !name.eq_ignore_ascii_case(super::access::DIRECTORY_MARKER)
}

pub(super) fn linked(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    if metadata.file_attributes() & 0x400 != 0 {
        return true;
    }
    metadata.file_type().is_symlink()
}

fn check_plain_path(path: &Path) -> Result<(), FsError> {
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(FsError::Link(ancestor.to_owned()));
            }
        }
        if metadata.file_type().is_symlink() {
            return Err(FsError::Link(ancestor.to_owned()));
        }
    }
    Ok(())
}

pub(super) fn virtual_relative(path: &str) -> Result<PathBuf, FsError> {
    VirtualPath::strict(path)
        .map(|path| path.as_relative())
        .map_err(|_| FsError::InvalidPath)
}
