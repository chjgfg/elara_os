//! Reference-counted open-file descriptions. Logical position is authoritative:
//! positioned IO may move the host cursor but never changes logical position.
use super::{DataRoot, FileBehavior, FileMode, FsError, OwnedFileInfo, OwnershipState};
use super::transaction::NamespaceGuard;
use crate::config::ROOTFS_DEFAULTS;
use cap_std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilePosition {
    Known(u64),
    Unconfirmed,
}

struct FileObject {
    file: File,
    mode: FileMode,
    opened_path: String,
    position: FilePosition,
    ownership: OwnershipState,
    #[cfg(test)]
    fail_append_position: bool,
}

/// Clones share one description and position; reopening creates another.
/// This object is a granted capability, not a user-identity authorization API.
#[derive(Clone)]
pub struct OpenFile {
    inner: Arc<Mutex<FileObject>>,
    domain: IoDomain,
}
#[derive(Clone)]
enum IoDomain { Legacy, Managed(Arc<DataRoot>) }
enum GuardDomain<'a> { Legacy, Managed { _guard: NamespaceGuard<'a> } }
struct FileObjectGuard<'a> {
    object: MutexGuard<'a, FileObject>,
    _domain: GuardDomain<'a>,
}
impl std::ops::Deref for FileObjectGuard<'_> {
    type Target = FileObject;
    fn deref(&self) -> &FileObject { &self.object }
}
impl std::ops::DerefMut for FileObjectGuard<'_> {
    fn deref_mut(&mut self) -> &mut FileObject { &mut self.object }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileInfo {
    /// Name at open time; rename does not change the identity of this handle.
    pub opened_path: String,
    pub mode: FileMode,
    pub position: FilePosition,
    pub length: u64,
    pub ownership: OwnershipState,
}

impl OpenFile {
    pub(super) fn new(file: File, mode: FileMode, opened_path: String) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FileObject {
                file,
                mode,
                opened_path,
                position: FilePosition::Known(0),
                ownership: OwnershipState::LegacyUnknown,
                #[cfg(test)]
                fail_append_position: false,
            })),
            domain: IoDomain::Legacy,
        }
    }
    pub(super) fn new_owned(file: File, mode: FileMode, info: OwnedFileInfo, root: Arc<DataRoot>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FileObject {
                file, mode, opened_path: info.path.clone(), position: FilePosition::Known(0),
                ownership: OwnershipState::Managed(info),
                #[cfg(test)] fail_append_position: false,
            })),
            domain: IoDomain::Managed(root),
        }
    }
    fn lock(&self) -> Result<FileObjectGuard<'_>, FsError> {
        let domain = match &self.domain {
            IoDomain::Legacy => GuardDomain::Legacy,
            IoDomain::Managed(root) => GuardDomain::Managed { _guard: root.namespace_guard()? },
        };
        let file = self.inner.lock().map_err(|_| FsError::FileObjectPoisoned)?;
        Ok(FileObjectGuard { object: file, _domain: domain })
    }
    pub fn info(&self) -> Result<FileInfo, FsError> {
        let object = self.lock()?;
        Ok(FileInfo {
            opened_path: object.opened_path.clone(),
            mode: object.mode,
            position: object.position,
            length: object.file.metadata()?.len(),
            ownership: object.ownership.clone(),
        })
    }
    /// One bounded read. EOF yields an empty vector; short reads are allowed.
    pub fn read(&self, maximum: usize) -> Result<Vec<u8>, FsError> {
        let mut object = self.lock()?;
        let offset = object.offset()?;
        let bytes = object.read_at(offset, maximum)?;
        object.position = FilePosition::Known(offset + bytes.len() as u64);
        Ok(bytes)
    }
    /// Streaming write, not a transactional replacement. Returns bytes written.
    pub fn write(&self, bytes: &[u8]) -> Result<usize, FsError> {
        let mut object = self.lock()?;
        object.require_write()?;
        bounded(bytes.len())?;
        if object.mode.behavior() == FileBehavior::Append {
            object.file.seek(SeekFrom::End(0))?;
            let count = object.file.write(bytes)?;
            object.position = FilePosition::Unconfirmed;
            match object.append_position() {
                Ok(position) => object.position = FilePosition::Known(position),
                Err(error) => {
                    return Err(FsError::WritePositionUnconfirmed {
                        bytes: count,
                        error,
                    })
                }
            }
            return Ok(count);
        }
        let offset = object.offset()?;
        checked_range(offset, bytes.len())?;
        object.file.seek(SeekFrom::Start(offset))?;
        let count = object.file.write(bytes)?;
        object.position = FilePosition::Known(offset + count as u64);
        Ok(count)
    }
    pub fn seek(&self, position: SeekFrom) -> Result<u64, FsError> {
        let mut object = self.lock()?;
        let offset = match position {
            SeekFrom::Start(offset) => offset,
            SeekFrom::Current(delta) => object
                .offset()?
                .checked_add_signed(delta)
                .ok_or(FsError::InvalidOffset)?,
            SeekFrom::End(delta) => object
                .file
                .metadata()?
                .len()
                .checked_add_signed(delta)
                .ok_or(FsError::InvalidOffset)?,
        };
        // Ask the host before accepting a range it cannot represent.
        object.file.seek(SeekFrom::Start(offset))?;
        object.position = FilePosition::Known(offset);
        Ok(offset)
    }
    pub fn read_at(&self, offset: u64, maximum: usize) -> Result<Vec<u8>, FsError> {
        self.lock()?.read_at(offset, maximum)
    }
    pub fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<usize, FsError> {
        let mut object = self.lock()?;
        object.require_write()?;
        if object.mode.behavior() == FileBehavior::Append {
            return Err(FsError::AppendPositionedWrite);
        }
        bounded(bytes.len())?;
        checked_range(offset, bytes.len())?;
        object.file.seek(SeekFrom::Start(offset))?;
        Ok(object.file.write(bytes)?)
    }
    /// Does not change logical position. Host backend extends with zero bytes.
    pub fn truncate(&self, length: u64) -> Result<(), FsError> {
        let object = self.lock()?;
        object.require_write()?;
        object.file.set_len(length)?;
        Ok(())
    }
    pub fn sync_data(&self) -> Result<(), FsError> {
        let object = self.lock()?;
        object.require_write()?;
        object.file.sync_data()?;
        Ok(())
    }
    pub fn sync_all(&self) -> Result<(), FsError> {
        let object = self.lock()?;
        object.require_write()?;
        object.file.sync_all()?;
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn references(&self) -> usize {
        Arc::strong_count(&self.inner)
    }
    #[cfg(test)]
    pub(crate) fn fail_append_position(&self) {
        self.inner.lock().unwrap().fail_append_position = true;
    }
}

impl FileObject {
    fn offset(&self) -> Result<u64, FsError> {
        match self.position {
            FilePosition::Known(offset) => Ok(offset),
            FilePosition::Unconfirmed => Err(FsError::CursorUnconfirmed),
        }
    }
    fn require_write(&self) -> Result<(), FsError> {
        if !self.mode.writable() {
            return Err(FsError::HandleWriteDenied);
        }
        Ok(())
    }
    fn append_position(&mut self) -> std::io::Result<u64> {
        #[cfg(test)]
        if self.fail_append_position {
            self.fail_append_position = false;
            return Err(std::io::Error::other("injected append-position failure"));
        }
        self.file.stream_position()
    }
    fn read_at(&mut self, offset: u64, maximum: usize) -> Result<Vec<u8>, FsError> {
        if !self.mode.readable() {
            return Err(FsError::HandleReadDenied);
        }
        bounded(maximum)?;
        checked_range(offset, maximum)?;
        self.file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(maximum)
            .map_err(|_| FsError::AllocationFailed)?;
        bytes.resize(maximum, 0);
        let count = self.file.read(&mut bytes)?;
        bytes.truncate(count);
        Ok(bytes)
    }
}
fn bounded(count: usize) -> Result<(), FsError> {
    if count > ROOTFS_DEFAULTS.max_file_io_bytes {
        return Err(FsError::IoRequestTooLarge(
            ROOTFS_DEFAULTS.max_file_io_bytes,
        ));
    }
    Ok(())
}
fn checked_range(offset: u64, count: usize) -> Result<(), FsError> {
    offset
        .checked_add(count as u64)
        .ok_or(FsError::InvalidOffset)?;
    Ok(())
}
