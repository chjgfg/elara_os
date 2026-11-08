//! Capability-root open entry. It never accepts or opens ambient host paths.
use super::backend::DataRoot;
use super::objects::OpenFile;
use super::{FileBehavior, FileCreation, FileMode, FsError};
use crate::virtual_path::VirtualPath;
use cap_std::fs::OpenOptions;
use std::io;
use std::path::Path;

impl DataRoot {
    pub fn open_file(&self, virtual_path: &str, mode: FileMode) -> Result<OpenFile, FsError> {
        if self.ownership_requires_actor(virtual_path)? { return Err(FsError::OwnershipContextRequired); }
        let canonical = VirtualPath::strict(virtual_path).map_err(|_| FsError::InvalidPath)?;
        let mut path = match mode.creation() {
            FileCreation::Existing => self.resolve(virtual_path).map_err(classify_open_error)?,
            FileCreation::Exclusive => self.resolve_new(virtual_path)?,
            FileCreation::IfMissing => match self.resolve(virtual_path) {
                Ok(path) => path,
                Err(FsError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                    self.resolve_new(virtual_path)?
                }
                Err(error) => return Err(error),
            },
        };
        let lookup = if path.as_os_str().is_empty() {
            Path::new(".")
        } else {
            &path
        };
        match self.dir.metadata(lookup) {
            Ok(metadata) if metadata.is_dir() => return Err(FsError::IsDirectory),
            Ok(metadata) if !metadata.is_file() => return Err(FsError::NotFile),
            Ok(_) => {}
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    && mode.creation() != FileCreation::Existing => {}
            Err(error) => return Err(classify_open_error(error.into())),
        }
        if mode.writable() {
            self.ensure_write_allowed(virtual_path)?;
        }
        // Read-only opens need no namespace mutation lock. Lock failure on a
        // writable open is an error, never an unlocked fallback.
        let _guard = if mode.writable() {
            Some(self.namespace_guard()?)
        } else {
            None
        };
        if mode.writable() {
            path = match mode.creation() {
                FileCreation::Existing => {
                    self.resolve(virtual_path).map_err(classify_open_error)?
                }
                FileCreation::Exclusive => self.resolve_new(virtual_path)?,
                FileCreation::IfMissing => match self.resolve(virtual_path) {
                    Ok(path) => path,
                    Err(FsError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                        self.resolve_new(virtual_path)?
                    }
                    Err(error) => return Err(error),
                },
            };
        }
        let mut options = OpenOptions::new();
        options
            .read(mode.readable())
            .write(mode.writable())
            .append(mode.behavior() == FileBehavior::Append)
            .truncate(mode.behavior() == FileBehavior::Truncate);
        match mode.creation() {
            FileCreation::Existing => {}
            FileCreation::IfMissing => {
                options.create(true);
            }
            FileCreation::Exclusive => {
                options.create_new(true);
            }
        }
        let file = self
            .dir
            .open_with(path, &options)
            .map_err(|error| classify_open_error(error.into()))?;
        if !file.metadata()?.is_file() {
            return Err(FsError::NotFile);
        }
        Ok(OpenFile::new(file, mode, canonical.to_string()))
    }
}

fn classify_open_error(error: FsError) -> FsError {
    match error {
        FsError::Io(error) if error.kind() == io::ErrorKind::NotFound => FsError::NotFound,
        FsError::Io(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            FsError::AlreadyExists
        }
        other => other,
    }
}
