//! Reference-counted open-file descriptions. Logical position is authoritative:
//! positioned IO may move the host cursor but never changes logical position.
use super::transaction::NamespaceGuard;
use super::{DataRoot, FileBehavior, FileMode, FsError, OwnedFileInfo, OwnershipState};
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
    lease: Option<super::host_locks::OpenLease>,
    #[cfg(test)]
    fail_append_position: bool,
}

/// Clones share one description and position; reopening creates another.
/// This object is a granted capability, not a user-identity authorization API.
#[derive(Clone)]
struct HostFile {
    inner: Arc<Mutex<FileObject>>,
    domain: IoDomain,
    application: Option<Arc<crate::users::Account>>,
}
#[derive(Clone)]
enum IoDomain {
    Legacy,
    Managed(Arc<DataRoot>),
}
enum GuardDomain<'a> {
    Legacy,
    Managed {
        root: &'a DataRoot,
        guard: NamespaceGuard<'a>,
    },
}
struct FileObjectGuard<'a> {
    object: MutexGuard<'a, FileObject>,
    _domain: GuardDomain<'a>,
}
impl FileObjectGuard<'_> {
    fn check_io(&self, write: bool) -> Result<(), FsError> {
        if let (GuardDomain::Managed { guard, .. }, Some(lease), OwnershipState::Managed(info)) =
            (&self._domain, &self.object.lease, &self.object.ownership)
        {
            lease
                .root
                .check_host_io_locked(info, Some(lease.id), write, guard)?;
        }
        Ok(())
    }

    fn resource_length(&self, length: u64) -> Result<(), FsError> {
        if let (GuardDomain::Managed { root, .. }, OwnershipState::Managed(info)) =
            (&self._domain, &self.object.ownership)
        {
            root.check_resource_write_locked(info, length, false)?;
        }
        Ok(())
    }
    fn resource_write(&self, offset: u64, count: usize) -> Result<(), FsError> {
        if !std::matches!(&self.object.ownership, OwnershipState::Managed(info) if std::matches!(info.delete_condition, super::DeleteCondition::ResourceDelete(_)))
        {
            return Ok(());
        }
        let old = self.object.file.metadata()?.len();
        let length = if count == 0 {
            old
        } else {
            old.max(
                offset
                    .checked_add(count as u64)
                    .ok_or(FsError::InvalidOffset)?,
            )
        };
        self.resource_length(length)
    }
    fn clear_privilege(&self) -> Result<(), FsError> {
        if let (GuardDomain::Managed { root, guard }, OwnershipState::Managed(info)) =
            (&self._domain, &self.object.ownership)
        {
            root.clear_privilege_locked(info.id, info.storage_generation, guard)?;
        }
        Ok(())
    }
}
impl std::ops::Deref for FileObjectGuard<'_> {
    type Target = FileObject;
    fn deref(&self) -> &FileObject {
        &self.object
    }
}
impl std::ops::DerefMut for FileObjectGuard<'_> {
    fn deref_mut(&mut self) -> &mut FileObject {
        &mut self.object
    }
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

impl HostFile {
    pub(super) fn new(file: File, mode: FileMode, opened_path: String) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FileObject {
                file,
                mode,
                opened_path,
                position: FilePosition::Known(0),
                ownership: OwnershipState::LegacyUnknown,
                lease: None,
                #[cfg(test)]
                fail_append_position: false,
            })),
            domain: IoDomain::Legacy,
            application: None,
        }
    }
    pub(super) fn new_owned(
        file: File,
        mode: FileMode,
        info: OwnedFileInfo,
        lease: super::host_locks::OpenLease,
    ) -> Self {
        let root = lease.root.clone();
        Self {
            inner: Arc::new(Mutex::new(FileObject {
                file,
                mode,
                opened_path: info.path.clone(),
                position: FilePosition::Known(0),
                ownership: OwnershipState::Managed(info),
                lease: Some(lease),
                #[cfg(test)]
                fail_append_position: false,
            })),
            domain: IoDomain::Managed(root),
            application: None,
        }
    }
    fn lock(&self) -> Result<FileObjectGuard<'_>, FsError> {
        let domain = match &self.domain {
            IoDomain::Legacy => GuardDomain::Legacy,
            IoDomain::Managed(root) => GuardDomain::Managed {
                root,
                guard: root.namespace_guard()?,
            },
        };
        if let (Some(actor), GuardDomain::Managed { root, .. }) = (&self.application, &domain) {
            root.validate_application_locked(actor, &root.load_access_catalog()?)?;
        }
        let file = self.inner.lock().map_err(|_| FsError::FileObjectPoisoned)?;
        Ok(FileObjectGuard {
            object: file,
            _domain: domain,
        })
    }
    fn compare_identity(&self, other: &Self) -> Result<super::ObjectBinding, FsError> {
        if Arc::ptr_eq(&self.inner, &other.inner) {
            return Ok(super::ObjectBinding::SameObject);
        }
        // Stable lock ordering avoids AB/BA deadlock for reversed comparisons.
        // Do not reopen a pathname: old descriptors may refer to retired data.
        let (first, second) = if Arc::as_ptr(&self.inner) < Arc::as_ptr(&other.inner) {
            (self, other)
        } else {
            (other, self)
        };
        let a = first
            .inner
            .lock()
            .map_err(|_| FsError::FileObjectPoisoned)?;
        let b = second
            .inner
            .lock()
            .map_err(|_| FsError::FileObjectPoisoned)?;
        let a = match a.file.try_clone() {
            Ok(file) => file.into_std(),
            Err(error) => return Ok(super::ObjectBinding::Unconfirmed(error.kind())),
        };
        let b = match b.file.try_clone() {
            Ok(file) => file.into_std(),
            Err(error) => return Ok(super::ObjectBinding::Unconfirmed(error.kind())),
        };
        Ok(super::binding::controlled_binding(&a, &b))
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
        object.check_io(false)?;
        let bytes = object.read_at(offset, maximum)?;
        object.position = FilePosition::Known(offset + bytes.len() as u64);
        Ok(bytes)
    }
    /// Streaming write, not a transactional replacement. Returns bytes written.
    pub fn write(&self, bytes: &[u8]) -> Result<usize, FsError> {
        let mut object = self.lock()?;
        object.require_write()?;
        object.check_io(true)?;
        bounded(bytes.len())?;
        if object.mode.behavior() == FileBehavior::Append {
            object.resource_write(object.file.metadata()?.len(), bytes.len())?;
            object.file.seek(SeekFrom::End(0))?;
            if !bytes.is_empty() {
                object.clear_privilege()?;
            }
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
        object.resource_write(offset, bytes.len())?;
        object.file.seek(SeekFrom::Start(offset))?;
        if !bytes.is_empty() {
            object.clear_privilege()?;
        }
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
        let mut object = self.lock()?;
        object.check_io(false)?;
        object.read_at(offset, maximum)
    }
    pub fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<usize, FsError> {
        let mut object = self.lock()?;
        object.require_write()?;
        object.check_io(true)?;
        if object.mode.behavior() == FileBehavior::Append {
            return Err(FsError::AppendPositionedWrite);
        }
        bounded(bytes.len())?;
        checked_range(offset, bytes.len())?;
        object.resource_write(offset, bytes.len())?;
        if !bytes.is_empty() {
            object.clear_privilege()?;
        }
        object.file.seek(SeekFrom::Start(offset))?;
        Ok(object.file.write(bytes)?)
    }
    /// Does not change logical position. Host backend extends with zero bytes.
    pub fn truncate(&self, length: u64) -> Result<(), FsError> {
        let object = self.lock()?;
        object.require_write()?;
        object.check_io(true)?;
        object.resource_length(length)?;
        object.clear_privilege()?;
        object.file.set_len(length)?;
        Ok(())
    }
    pub fn sync_data(&self) -> Result<(), FsError> {
        let object = self.lock()?;
        object.require_write()?;
        object.check_io(true)?;
        object.file.sync_data()?;
        Ok(())
    }
    pub fn sync_all(&self) -> Result<(), FsError> {
        let object = self.lock()?;
        object.require_write()?;
        object.check_io(true)?;
        object.file.sync_all()?;
        Ok(())
    }
    fn set_lock(&self, mode: Option<super::vfs::LockMode>) -> Result<(), FsError> {
        let object = self.lock()?;
        if mode == Some(super::vfs::LockMode::Exclusive) && !object.mode.writable() {
            return Err(FsError::HandleWriteDenied);
        }
        match (&object._domain, &object.ownership, &object.lease) {
            (GuardDomain::Managed { root, guard }, OwnershipState::Managed(info), Some(lease)) => {
                root.set_host_lock_locked(info, lease.id, mode, guard)
            }
            _ => Err(super::vfs::Error::Unsupported.into()),
        }
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

#[derive(Clone)]
enum BackendFile {
    Host(HostFile),
    Virtual {
        file: super::vfs::File,
        mode: FileMode,
        path: String,
    },
}
/// A granted open-file capability. Backend selection never silently falls back.
#[derive(Clone)]
pub struct OpenFile {
    backend: BackendFile,
    // Host checks belong inside HostFile::lock to avoid recursive namespace locking.
    virtual_application: Option<(Arc<DataRoot>, Arc<crate::users::Account>)>,
}
impl OpenFile {
    pub(super) fn bind_application(mut self, root: &DataRoot, actor: &crate::users::Account) -> Result<Self, FsError> {
        if actor.application_domain().is_none() { return Ok(self); }
        match &mut self.backend {
            BackendFile::Host(file) => {
                if let IoDomain::Legacy = file.domain {
                    file.domain = IoDomain::Managed(Arc::new(root.clone_file_domain()?));
                }
                file.application = Some(Arc::new(actor.clone()));
            }
            BackendFile::Virtual { .. } => {
                self.virtual_application = Some((Arc::new(root.clone_file_domain()?), Arc::new(actor.clone())));
            }
        }
        Ok(self)
    }
    fn application_guard(&self) -> Result<Option<NamespaceGuard<'_>>, FsError> {
        let Some((root, actor)) = &self.virtual_application else { return Ok(None); };
        let guard = root.namespace_guard()?;
        root.validate_application_locked(actor, &root.load_access_catalog()?)?;
        Ok(Some(guard))
    }

    pub(super) fn new(file: File, mode: FileMode, path: String) -> Self {
        Self {
            backend: BackendFile::Host(HostFile::new(file, mode, path)),
            virtual_application: None,
        }
    }
    pub(super) fn new_owned(
        file: File,
        mode: FileMode,
        info: OwnedFileInfo,
        lease: super::host_locks::OpenLease,
    ) -> Self {
        Self {
            backend: BackendFile::Host(HostFile::new_owned(file, mode, info, lease)),
            virtual_application: None,
        }
    }
    pub(super) fn new_virtual(file: super::vfs::File, mode: FileMode, path: String) -> Self {
        Self {
            backend: BackendFile::Virtual { file, mode, path },
            virtual_application: None,
        }
    }
    /// Compare the opened backing objects, never their possibly stale names.
    /// Host probes may be Unconfirmed (e.g. readonly or externally locked).
    pub fn compare_identity(&self, other: &Self) -> Result<super::ObjectBinding, FsError> {
        use super::ObjectBinding;
        let application_bound = |file: &Self| file.virtual_application.is_some()
            || std::matches!(&file.backend, BackendFile::Host(host) if host.application.is_some());
        if application_bound(self) || application_bound(other) {
            return Err(FsError::OwnershipDenied);
        }
        match (&self.backend, &other.backend) {
            (BackendFile::Host(a), BackendFile::Host(b)) => a.compare_identity(b),
            (BackendFile::Virtual { file: a, .. }, BackendFile::Virtual { file: b, .. }) => Ok(
                if a.volume_id() == b.volume_id() && a.stat()?.inode == b.stat()?.inode {
                    ObjectBinding::SameObject
                } else {
                    ObjectBinding::DifferentObject
                },
            ),
            _ => Ok(ObjectBinding::DifferentObject),
        }
    }
    pub fn info(&self) -> Result<FileInfo, FsError> {
        let _application_guard = self.application_guard()?;
        match &self.backend {
            BackendFile::Host(f) => f.info(),
            BackendFile::Virtual { file, mode, path } => {
                let inode = file.stat()?;
                Ok(FileInfo {
                    opened_path: path.clone(),
                    mode: *mode,
                    position: FilePosition::Known(file.position()?),
                    length: inode.length,
                    ownership: OwnershipState::Virtual {
                        filesystem: file.volume_id(),
                        inode,
                    },
                })
            }
        }
    }
    pub fn read(&self, n: usize) -> Result<Vec<u8>, FsError> {
        let _application_guard = self.application_guard()?;
        match &self.backend {
            BackendFile::Host(f) => f.read(n),
            BackendFile::Virtual { file, .. } => Ok(file.read(n)?),
        }
    }
    pub fn write(&self, b: &[u8]) -> Result<usize, FsError> {
        let _application_guard = self.application_guard()?;
        match &self.backend {
            BackendFile::Host(f) => f.write(b),
            BackendFile::Virtual { file, .. } => Ok(file.write(b)?),
        }
    }
    pub fn read_at(&self, o: u64, n: usize) -> Result<Vec<u8>, FsError> {
        let _application_guard = self.application_guard()?;
        match &self.backend {
            BackendFile::Host(f) => f.read_at(o, n),
            BackendFile::Virtual { file, .. } => Ok(file.read_at(o, n)?),
        }
    }
    pub fn write_at(&self, o: u64, b: &[u8]) -> Result<usize, FsError> {
        let _application_guard = self.application_guard()?;
        match &self.backend {
            BackendFile::Host(f) => f.write_at(o, b),
            BackendFile::Virtual { file, .. } => Ok(file.write_at(o, b)?),
        }
    }
    pub fn seek(&self, p: SeekFrom) -> Result<u64, FsError> {
        let _application_guard = self.application_guard()?;
        match &self.backend {
            BackendFile::Host(f) => f.seek(p),
            BackendFile::Virtual { file, .. } => Ok(file.seek(p)?),
        }
    }
    pub fn truncate(&self, n: u64) -> Result<(), FsError> {
        let _application_guard = self.application_guard()?;
        match &self.backend {
            BackendFile::Host(f) => f.truncate(n),
            BackendFile::Virtual { file, .. } => Ok(file.truncate(n)?),
        }
    }
    pub fn sync_data(&self) -> Result<(), FsError> {
        let _application_guard = self.application_guard()?;
        match &self.backend {
            BackendFile::Host(f) => f.sync_data(),
            BackendFile::Virtual { .. } => Err(super::vfs::Error::Unsupported.into()),
        }
    }
    pub fn sync_all(&self) -> Result<(), FsError> {
        let _application_guard = self.application_guard()?;
        match &self.backend {
            BackendFile::Host(f) => f.sync_all(),
            BackendFile::Virtual { .. } => Err(super::vfs::Error::Unsupported.into()),
        }
    }
    pub fn lock(&self, mode: super::vfs::LockMode) -> Result<(), FsError> {
        let _application_guard = self.application_guard()?;
        match &self.backend {
            BackendFile::Virtual { file, .. } => Ok(file.lock(mode)?),
            BackendFile::Host(file) => file.set_lock(Some(mode)),
        }
    }
    pub fn unlock(&self) -> Result<(), FsError> {
        let _application_guard = self.application_guard()?;
        match &self.backend {
            BackendFile::Virtual { file, .. } => Ok(file.unlock()?),
            BackendFile::Host(file) => file.set_lock(None),
        }
    }
    #[cfg(test)]
    pub(crate) fn references(&self) -> usize {
        match &self.backend {
            BackendFile::Host(f) => f.references(),
            BackendFile::Virtual { file, .. } => file.references(),
        }
    }
    #[cfg(test)]
    pub(crate) fn fail_append_position(&self) {
        if let BackendFile::Host(f) = &self.backend {
            f.fail_append_position();
        }
    }
}
