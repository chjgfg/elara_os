use crate::config::ROOTFS_DEFAULTS;
use crate::virtual_path::VirtualPath;
use std::ffi::OsString;
use std::io;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::{fmt, fs, matches, write};

#[derive(Debug)]
pub enum FsError {
    Io(io::Error),
    InvalidRoot,
    InvalidPath,
    Link(PathBuf),
    UnmarkedRoot,
    NotDirectory,
    NotFile,
    AlreadyExists,
    ProtectedPath,
    TooLarge(u64),
}

impl fmt::Display for FsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "Filesystem error: {error}"),
            Self::InvalidRoot => write!(formatter, "An absolute non-drive-root data directory without traversal is required."),
            Self::InvalidPath => write!(formatter, "Only virtual Unix paths are allowed; traversal, device names and host path syntax are refused."),
            Self::Link(path) => write!(formatter, "Symbolic link or reparse point refused: {}", path.display()),
            Self::UnmarkedRoot => write!(formatter, "Dedicated data-root marker is missing or invalid."),
            Self::NotDirectory => write!(formatter, "The requested directory does not exist or is not a directory."),
            Self::NotFile => write!(formatter, "The requested path does not exist or is not a regular file."),
            Self::AlreadyExists => write!(formatter, "The path already exists."),
            Self::ProtectedPath => write!(formatter, "The virtual root and its system directories cannot be removed or replaced."),
            Self::TooLarge(limit) => write!(formatter, "The file exceeds the {limit}-byte display limit."),
        }
    }
}

impl From<io::Error> for FsError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub enum EntryKind {
    Directory,
    File,
    BlockedLink,
}

pub struct Entry {
    pub name: OsString,
    pub kind: EntryKind,
}

pub struct DataRoot {
    root: PathBuf,
}

impl DataRoot {
    pub fn open(root: &Path) -> Result<Self, FsError> {
        if !root.is_absolute()
            || root.parent().is_none()
            || root
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        {
            return Err(FsError::InvalidRoot);
        }
        #[cfg(windows)]
        if !matches!(root.components().next(), Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
        {
            return Err(FsError::InvalidRoot);
        }
        check_plain_path(root)?;
        if !root.is_dir() {
            return Err(FsError::NotDirectory);
        }
        let marker = root.join(ROOTFS_DEFAULTS.marker_filename);
        match fs::symlink_metadata(&marker) {
            Ok(_) => check_plain_path(&marker)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(FsError::UnmarkedRoot)
            }
            Err(error) => return Err(error.into()),
        }
        if fs::read(marker)?.as_slice() != ROOTFS_DEFAULTS.marker_contents {
            return Err(FsError::UnmarkedRoot);
        }
        let data_root = Self {
            root: root.canonicalize()?,
        };
        for name in ROOTFS_DEFAULTS.directories {
            if !data_root.resolve(name)?.is_dir() {
                return Err(FsError::NotDirectory);
            }
        }
        Ok(data_root)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn resolve(&self, virtual_path: &str) -> Result<PathBuf, FsError> {
        let path = self.root.join(virtual_relative(virtual_path)?);
        check_plain_path(&path)?;
        let canonical = path.canonicalize()?;
        if !canonical.starts_with(&self.root) {
            return Err(FsError::InvalidPath);
        }
        Ok(canonical)
    }

    pub fn validate_directory(&self, virtual_path: &str) -> Result<(), FsError> {
        if self.resolve(virtual_path)?.is_dir() {
            Ok(())
        } else {
            Err(FsError::NotDirectory)
        }
    }

    pub fn list(&self, virtual_path: &str) -> Result<Vec<Entry>, FsError> {
        let path = self.resolve(virtual_path)?;
        if !path.is_dir() {
            return Err(FsError::NotDirectory);
        }
        let mut entries = Vec::new();
        for result in fs::read_dir(path)? {
            let entry = result?;
            let name = entry.file_name();
            if name == ROOTFS_DEFAULTS.marker_filename {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())?;
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

    /// Resolve a path that may not exist yet; its parent must be an existing directory inside the root.
    fn resolve_new(&self, virtual_path: &str) -> Result<PathBuf, FsError> {
        let relative = virtual_relative(virtual_path)?;
        let name = relative
            .file_name()
            .ok_or(FsError::ProtectedPath)?
            .to_owned();
        let parent = relative
            .parent()
            .map(|parent| self.root.join(parent))
            .unwrap_or_else(|| self.root.clone());
        check_plain_path(&parent).map_err(|error| match error {
            FsError::Io(io) if io.kind() == io::ErrorKind::NotFound => FsError::NotDirectory,
            other => other,
        })?;
        let parent = parent.canonicalize()?;
        if !parent.starts_with(&self.root) {
            return Err(FsError::InvalidPath);
        }
        if !parent.is_dir() {
            return Err(FsError::NotDirectory);
        }
        let target = parent.join(name);
        match fs::symlink_metadata(&target) {
            Ok(metadata) if linked(&metadata) => Err(FsError::Link(target)),
            Ok(_) => Err(FsError::AlreadyExists),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(target),
            Err(error) => Err(error.into()),
        }
    }

    fn is_protected(&self, virtual_path: &str) -> Result<bool, FsError> {
        let relative = virtual_relative(virtual_path)?;
        if relative.as_os_str().is_empty() {
            return Ok(true);
        }
        Ok(ROOTFS_DEFAULTS
            .directories
            .iter()
            .any(|directory| PathBuf::from(directory) == relative))
    }

    pub fn read_file(&self, virtual_path: &str, limit: u64) -> Result<Vec<u8>, FsError> {
        let path = self.resolve(virtual_path)?;
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file() {
            return Err(FsError::NotFile);
        }
        if metadata.len() > limit {
            return Err(FsError::TooLarge(limit));
        }
        Ok(fs::read(path)?)
    }

    pub fn create_directory(&self, virtual_path: &str) -> Result<(), FsError> {
        let target = self.resolve_new(virtual_path)?;
        fs::create_dir(target)?;
        Ok(())
    }

    pub fn create_file(&self, virtual_path: &str) -> Result<(), FsError> {
        let target = self.resolve_new(virtual_path)?;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        Ok(())
    }

    pub fn remove_file(&self, virtual_path: &str) -> Result<(), FsError> {
        if self.is_protected(virtual_path)? {
            return Err(FsError::ProtectedPath);
        }
        let path = self.resolve(virtual_path)?;
        if !fs::symlink_metadata(&path)?.is_file() {
            return Err(FsError::NotFile);
        }
        fs::remove_file(path)?;
        Ok(())
    }

    /// Replace the contents of an existing regular file inside the root.
    /// Data is written to a sibling temporary file first, so a failure never leaves a torn file.
    pub fn write_file(&self, virtual_path: &str, contents: &[u8]) -> Result<(), FsError> {
        if self.is_protected(virtual_path)? {
            return Err(FsError::ProtectedPath);
        }
        let path = self.resolve(virtual_path)?;
        if !fs::symlink_metadata(&path)?.is_file() {
            return Err(FsError::NotFile);
        }
        let mut staged = path.clone();
        staged.set_extension(format!("elara-staged-{}", std::process::id()));
        let result = fs::write(&staged, contents).and_then(|()| fs::rename(&staged, &path));
        if result.is_err() {
            let _ = fs::remove_file(&staged);
        }
        result?;
        Ok(())
    }

    /// Append to an existing regular file inside the root.
    pub fn append_file(&self, virtual_path: &str, contents: &[u8]) -> Result<(), FsError> {
        if self.is_protected(virtual_path)? {
            return Err(FsError::ProtectedPath);
        }
        let path = self.resolve(virtual_path)?;
        if !fs::symlink_metadata(&path)?.is_file() {
            return Err(FsError::NotFile);
        }
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(path)?
            .write_all(contents)?;
        Ok(())
    }
}

fn linked(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    if metadata.file_attributes() & 0x400 != 0 {
        return true;
    }
    metadata.file_type().is_symlink()
}

fn check_plain_path(path: &Path) -> Result<(), FsError> {
    for ancestor in path.ancestors() {
        if linked(&fs::symlink_metadata(ancestor)?) {
            return Err(FsError::Link(ancestor.to_owned()));
        }
    }
    Ok(())
}

fn virtual_relative(path: &str) -> Result<PathBuf, FsError> {
    VirtualPath::strict(path)
        .map(|path| path.as_relative())
        .map_err(|_| FsError::InvalidPath)
}

#[cfg(test)]
mod tests {
    use super::{virtual_relative, DataRoot, FsError, ROOTFS_DEFAULTS};
    use std::path::PathBuf;
    use std::{assert, assert_eq, format, fs, matches};

    fn fixture() -> PathBuf {
        crate::test_support::marked_root("fs")
    }

    #[test]
    fn virtual_paths_are_relative_to_data_root() {
        assert_eq!(
            virtual_relative("/usr/bin").unwrap(),
            PathBuf::from("usr/bin")
        );
        assert_eq!(virtual_relative("/").unwrap(), PathBuf::new());
    }

    #[test]
    fn traversal_host_paths_and_devices_are_rejected() {
        for path in [
            "../",
            "/usr/../etc",
            "C:/Windows",
            "\\\\server\\share",
            "usr/file:stream",
            "NUL",
            "CON.txt",
            "COM¹",
            "usr/end.",
            ".elara-root",
        ] {
            assert!(
                matches!(virtual_relative(path), Err(FsError::InvalidPath)),
                "{path}"
            );
        }
    }

    #[test]
    fn removing_marker_breaks_startup_and_restoring_it_recovers() {
        let root = fixture();
        assert!(DataRoot::open(&root).is_ok());
        fs::rename(
            root.join(ROOTFS_DEFAULTS.marker_filename),
            root.join("marker.saved"),
        )
        .unwrap();
        assert!(matches!(DataRoot::open(&root), Err(FsError::UnmarkedRoot)));
        fs::rename(
            root.join("marker.saved"),
            root.join(ROOTFS_DEFAULTS.marker_filename),
        )
        .unwrap();
        assert!(DataRoot::open(&root).is_ok());
    }

    #[test]
    fn listing_cannot_escape_the_root() {
        let root = fixture();
        let data = DataRoot::open(&root).unwrap();
        assert!(data.list("/usr").is_ok());
        assert!(matches!(
            data.list("/usr/../../"),
            Err(FsError::InvalidPath)
        ));
        assert!(matches!(data.list("C:/Windows"), Err(FsError::InvalidPath)));
    }

    #[test]
    fn directory_links_are_refused() {
        let target = fixture();
        let link = target.parent().unwrap().join(format!(
            "link-{}",
            target.file_name().unwrap().to_string_lossy()
        ));
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd.exe")
                .args(["/d", "/c", "mklink", "/J"])
                .arg(&link)
                .arg(&target)
                .status()
                .unwrap();
            assert!(status.success());
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(matches!(DataRoot::open(&link), Err(FsError::Link(_))));
    }

    #[test]
    fn files_and_directories_are_created_read_and_removed_inside_the_root() {
        let root = fixture();
        let data = DataRoot::open(&root).unwrap();
        data.create_directory("/home/elara").unwrap();
        data.create_file("/home/elara/notes.txt").unwrap();
        assert!(root.join("home/elara/notes.txt").is_file());
        assert_eq!(data.read_file("/home/elara/notes.txt", 64).unwrap(), b"");
        fs::write(root.join("home/elara/notes.txt"), "星澜\n").unwrap();
        assert_eq!(
            data.read_file("/home/elara/notes.txt", 64).unwrap(),
            "星澜\n".as_bytes()
        );
        data.remove_file("/home/elara/notes.txt").unwrap();
        assert!(!root.join("home/elara/notes.txt").exists());
    }

    #[test]
    fn creation_refuses_duplicates_missing_parents_and_escapes() {
        let root = fixture();
        let data = DataRoot::open(&root).unwrap();
        assert!(matches!(
            data.create_directory("/usr"),
            Err(FsError::AlreadyExists)
        ));
        assert!(matches!(
            data.create_file("/missing/file.txt"),
            Err(FsError::NotDirectory)
        ));
        assert!(matches!(
            data.create_file("/usr/../../escape.txt"),
            Err(FsError::InvalidPath)
        ));
        assert!(matches!(
            data.create_file("C:/Windows/escape.txt"),
            Err(FsError::InvalidPath)
        ));
        assert!(matches!(data.create_file("/"), Err(FsError::ProtectedPath)));
        assert!(matches!(
            data.create_file("/NUL"),
            Err(FsError::InvalidPath)
        ));
        assert!(!root.parent().unwrap().join("escape.txt").exists());
    }

    #[test]
    fn removal_protects_the_root_system_directories_and_marker() {
        let root = fixture();
        let data = DataRoot::open(&root).unwrap();
        assert!(matches!(data.remove_file("/"), Err(FsError::ProtectedPath)));
        assert!(matches!(
            data.remove_file("/usr"),
            Err(FsError::ProtectedPath)
        ));
        assert!(matches!(
            data.remove_file("/.elara-root"),
            Err(FsError::InvalidPath)
        ));
        assert!(matches!(
            data.remove_file("/missing.txt"),
            Err(FsError::Io(_))
        ));
        data.create_directory("/home/dir").unwrap();
        assert!(matches!(
            data.remove_file("/home/dir"),
            Err(FsError::NotFile)
        ));
        assert!(root.join("usr").is_dir());
        assert!(root.join(ROOTFS_DEFAULTS.marker_filename).is_file());
    }

    #[test]
    fn reading_enforces_the_display_limit_and_file_type() {
        let root = fixture();
        let data = DataRoot::open(&root).unwrap();
        fs::write(root.join("home/big.txt"), [b'x'; 65]).unwrap();
        assert!(matches!(
            data.read_file("/home/big.txt", 64),
            Err(FsError::TooLarge(64))
        ));
        assert!(matches!(data.read_file("/home", 64), Err(FsError::NotFile)));
    }
}
