//! Atomic replacement transactions. Staging is confined to a marked private
//! store under virtual /tmp. No copy-overwrite fallback is permitted.
use super::backend::{linked, DataRoot};
use super::types::FsError;
use crate::config::ROOTFS_DEFAULTS;
use cap_std::fs::{Dir, OpenOptions};
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{format, fs};

pub(super) const STORE_MARKER: &str = "store.owner";
pub(super) const TX_OWNER: &str = "transaction.owner";
pub(super) const PAYLOAD: &str = "payload";
pub(super) const TX_MAGIC: &[u8] = b"ELARA_REPLACE_TRANSACTION_V1\n";
pub(super) const LOG_GENERATION_TX_MAGIC: &[u8] = b"ELARA_LOG_GENERATION_TX_V2\n";
static NEXT: AtomicU64 = AtomicU64::new(0);

use super::{CleanupStatus, CommitReceipt};

pub(super) fn store_path() -> PathBuf {
    Path::new("tmp").join(ROOTFS_DEFAULTS.transaction_directory)
}

pub(super) fn read_magic(dir: &Dir, name: &str, expected: &[u8]) -> io::Result<bool> {
    let mut bytes = Vec::new();
    dir.open(name)?
        .take(expected.len() as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes == expected)
}

pub(super) fn validate_store(data: &DataRoot) -> Result<Dir, FsError> {
    let path = store_path();
    data.inspect_relative(&path)?;
    let store = data.dir.open_dir(path)?;
    let metadata = match store.symlink_metadata(STORE_MARKER) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(FsError::UnownedTransactionStore)
        }
        Err(error) => return Err(error.into()),
    };
    if linked(&metadata)
        || !metadata.is_file()
        || !read_magic(
            &store,
            STORE_MARKER,
            ROOTFS_DEFAULTS.transaction_store_magic,
        )?
    {
        return Err(FsError::UnownedTransactionStore);
    }
    Ok(store)
}

pub(super) fn ensure_store(data: &DataRoot) -> Result<Dir, FsError> {
    data.inspect_relative(Path::new("tmp"))?;
    let path = store_path();
    match data.dir.create_dir(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    data.inspect_relative(&path)?;
    let store = data.dir.open_dir(&path)?;
    match store.symlink_metadata(STORE_MARKER) {
        Ok(_) => validate_store(data),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if store.entries()?.next().is_some() {
                return Err(FsError::UnownedTransactionStore);
            }
            let mut marker = store.open_with(
                STORE_MARKER,
                OpenOptions::new().write(true).create_new(true),
            )?;
            marker.write_all(ROOTFS_DEFAULTS.transaction_store_magic)?;
            marker.sync_all()?;
            drop(marker);
            validate_store(data)
        }
        Err(error) => Err(error.into()),
    }
}

pub(super) fn lock_store(store: &Dir) -> Result<fs::File, FsError> {
    match store.symlink_metadata("store.lock") {
        Ok(metadata) if linked(&metadata) || !metadata.is_file() => {
            return Err(FsError::UnownedTransactionStore)
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let file = store
        .open_with(
            "store.lock",
            OpenOptions::new().read(true).write(true).create(true),
        )?
        .into_std();
    file.lock().map_err(FsError::TransactionLockUnavailable)?;
    Ok(file)
}

pub(super) fn discard_owned(store: &Dir, name: &OsStr) -> Result<bool, FsError> {
    let Some(text) = name.to_str() else {
        return Ok(false);
    };
    if !text.starts_with("txn-") || !text[4..].chars().all(|c| c.is_ascii_digit() || c == '-') {
        return Ok(false);
    }
    let metadata = store.symlink_metadata(name)?;
    if linked(&metadata) || !metadata.is_dir() {
        return Ok(false);
    }
    let transaction = store.open_dir(name)?;
    let mut entries = Vec::new();
    for result in transaction.entries()? {
        let entry = result?;
        let file_name = entry.file_name();
        if entries.len() >= 3 {
            return Ok(false);
        }
        let metadata = transaction.symlink_metadata(&file_name)?;
        if linked(&metadata) || !metadata.is_file() {
            return Ok(false);
        }
        entries.push(file_name);
    }
    if !entries.is_empty() {
        let replace = read_magic(&transaction, TX_OWNER, TX_MAGIC).unwrap_or(false);
        let log_generation = read_magic(&transaction, TX_OWNER, LOG_GENERATION_TX_MAGIC).unwrap_or(false);
        let object = read_magic(&transaction, TX_OWNER, super::ownership::OWNED_TX_MAGIC).unwrap_or(false);
        if !replace && !log_generation && !object { return Ok(false); }
        if entries.iter().any(|name| name != TX_OWNER && name != PAYLOAD && !(log_generation && name == "journal") && !(object && name == "object.owner")) {
            return Ok(false);
        }
    }
    for entry in entries {
        transaction.remove_file(entry)?;
    }
    drop(transaction);
    store.remove_dir(name)?;
    Ok(true)
}

pub(super) enum Publication {
    ReplaceExisting,
    CreateExclusive,
}

enum ProtocolPayload<'a> {
    Bytes(&'a [u8]),
    Link(&'a Path),
}

pub(super) struct NamespaceGuard<'a> {
    root: &'a DataRoot,
    _file: fs::File,
}
impl<'a> NamespaceGuard<'a> {
    pub(super) fn from_locked(root: &'a DataRoot, file: fs::File) -> Self {
        Self { root, _file: file }
    }
    pub(super) fn require_root(&self, root: &DataRoot) -> Result<(), FsError> {
        if std::ptr::eq(self.root, root) { Ok(()) } else { Err(FsError::InvalidRoot) }
    }
}

impl DataRoot {
    /// Namespace changes and recovery use the same cross-process lock.
    pub(super) fn namespace_guard(&self) -> Result<NamespaceGuard<'_>, FsError> {
        let file = lock_store(&ensure_store(self)?)?;
        let guard = NamespaceGuard::from_locked(self, file);
        self.recover_owner_intent_locked(&guard)?;
        Ok(guard)
    }

    pub(super) fn create_protocol_stage(&self, guard: &NamespaceGuard<'_>) -> Result<(Dir, String), FsError> {
        guard.require_root(self)?;
        let store = validate_store(self)?;
        let name = loop {
            let name = format!("txn-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
            match store.create_dir(&name) {
                Ok(()) => break name,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        };
        Ok((store, name))
    }

    pub fn replace_file(
        &self,
        virtual_path: &str,
        contents: &[u8],
    ) -> Result<CommitReceipt, FsError> {
        self.publish_bytes(virtual_path, contents, Publication::ReplaceExisting)
    }

    pub(super) fn publish_bytes(
        &self,
        virtual_path: &str,
        contents: &[u8],
        publication: Publication,
    ) -> Result<CommitReceipt, FsError> {
        if self.is_protected(virtual_path)? {
            return Err(FsError::ProtectedPath);
        }
        self.ensure_write_allowed(virtual_path)?;
        if contents.len() > ROOTFS_DEFAULTS.max_transaction_bytes {
            return Err(FsError::TooLarge(
                ROOTFS_DEFAULTS.max_transaction_bytes as u64,
            ));
        }
        let target = match publication {
            Publication::ReplaceExisting => {
                let target = self.resolve(virtual_path)?;
                if !self.dir.metadata(&target)?.is_file() {
                    return Err(FsError::NotFile);
                }
                target
            }
            Publication::CreateExclusive => self.resolve_new(virtual_path)?,
        };
        let store = ensure_store(self)?;
        let _lock = lock_store(&store)?;
        let name = loop {
            let candidate = format!(
                "txn-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            match store.create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        };
        let stage = store.open_dir(&name)?;
        let prepared = (|| -> io::Result<()> {
            let mut owner =
                stage.open_with(TX_OWNER, OpenOptions::new().write(true).create_new(true))?;
            owner.write_all(TX_MAGIC)?;
            owner.sync_all()?;
            let mut payload =
                stage.open_with(PAYLOAD, OpenOptions::new().write(true).create_new(true))?;
            #[cfg(test)]
            if self.transaction_fault == super::types::TransactionFault::PartialStage {
                payload.write_all(&contents[..contents.len().min(3)])?;
                return Err(io::Error::other("injected partial staging failure"));
            }
            payload.write_all(contents)?;
            payload.sync_all()?;
            Ok(())
        })();
        if let Err(error) = prepared {
            drop(stage);
            let _ = discard_owned(&store, OsStr::new(&name));
            return Err(error.into());
        }
        // Recheck policy and links after preparation. The actual operation still
        // uses capability handles; a path swap cannot grant host-root access.
        let rechecked = match (match publication {
            Publication::ReplaceExisting => self.resolve(virtual_path),
            Publication::CreateExclusive => self.resolve_new(virtual_path),
        })
        .and_then(|path| {
            self.ensure_write_allowed(virtual_path)?;
            if let Publication::ReplaceExisting = publication {
                if !self.dir.metadata(&path)?.is_file() {
                    return Err(FsError::NotFile);
                }
            }
            Ok(path)
        }) {
            Ok(path) => path,
            Err(error) => {
                drop(stage);
                let _ = discard_owned(&store, OsStr::new(&name));
                return Err(error);
            }
        };
        let replaced = {
            #[cfg(test)]
            if self.transaction_fault == super::types::TransactionFault::RenameUnsupported {
                Err(io::Error::new(
                    io::ErrorKind::CrossesDevices,
                    "injected cross-device rename",
                ))
            } else {
                match publication {
                    Publication::ReplaceExisting => stage.rename(PAYLOAD, &self.dir, &rechecked),
                    Publication::CreateExclusive => stage.hard_link(PAYLOAD, &self.dir, &rechecked),
                }
            }
            #[cfg(not(test))]
            match publication {
                Publication::ReplaceExisting => stage.rename(PAYLOAD, &self.dir, &rechecked),
                Publication::CreateExclusive => stage.hard_link(PAYLOAD, &self.dir, &rechecked),
            }
        };
        drop(stage);
        if let Err(error) = replaced {
            let _ = discard_owned(&store, OsStr::new(&name));
            return Err(match publication {
                Publication::ReplaceExisting => FsError::AtomicReplaceUnavailable(error),
                Publication::CreateExclusive if error.kind() == io::ErrorKind::AlreadyExists => {
                    FsError::AlreadyExists
                }
                Publication::CreateExclusive => FsError::AtomicPublishUnavailable(error),
            });
        }
        let parent = target.parent().unwrap_or(Path::new("."));
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        let directory_durability = super::metadata::sync_relative_directory(self, parent);
        let cleanup = match discard_owned(&store, OsStr::new(&name)) {
            Ok(true) => CleanupStatus::Complete,
            Ok(false) => CleanupStatus::Deferred(io::ErrorKind::InvalidData),
            Err(FsError::Io(error)) => CleanupStatus::Deferred(error.kind()),
            Err(_) => CleanupStatus::Deferred(io::ErrorKind::Other),
        };
        Ok(CommitReceipt {
            bytes: contents.len(),
            directory_durability,
            cleanup,
        })
    }

    /// Private protocol publication under the caller's existing namespace lock.
    /// Target paths originate inside filesystem services, never from user input.
    /// Staging still belongs to the marked /tmp transaction store.
    pub(super) fn publish_protocol_locked(
        &self,
        target: &Path,
        contents: &[u8],
        exclusive: bool,
        guard: &NamespaceGuard<'_>,
    ) -> Result<CommitReceipt, FsError> {
        self.publish_protocol_payload_locked(target, ProtocolPayload::Bytes(contents), exclusive, guard)
    }

    pub(super) fn publish_protocol_link_locked(
        &self, source: &Path, target: &Path, guard: &NamespaceGuard<'_>,
    ) -> Result<CommitReceipt, FsError> {
        self.inspect_relative(source)?;
        self.publish_protocol_payload_locked(target, ProtocolPayload::Link(source), false, guard)
    }

    pub(super) fn publish_protocol_link_exclusive_locked(
        &self, source: &Path, target: &Path, guard: &NamespaceGuard<'_>,
    ) -> Result<CommitReceipt, FsError> {
        self.inspect_relative(source)?;
        self.publish_protocol_payload_locked(target, ProtocolPayload::Link(source), true, guard)
    }

    fn publish_protocol_payload_locked(
        &self, target: &Path, payload: ProtocolPayload<'_>, exclusive: bool, _guard: &NamespaceGuard<'_>,
    ) -> Result<CommitReceipt, FsError> {
        _guard.require_root(self)?;
        let bytes = match &payload {
            ProtocolPayload::Bytes(bytes) => bytes.len(),
            ProtocolPayload::Link(source) => usize::try_from(self.dir.metadata(source)?.len()).map_err(|_| FsError::InvalidOffset)?,
        };
        if bytes > ROOTFS_DEFAULTS.max_transaction_bytes {
            return Err(FsError::TooLarge(ROOTFS_DEFAULTS.max_transaction_bytes as u64));
        }
        let parent = target.parent().ok_or(FsError::InvalidPath)?;
        self.inspect_relative(parent)?;
        match self.dir.symlink_metadata(target) {
            Ok(metadata) if linked(&metadata) => return Err(FsError::Link(self.root.join(target))),
            Ok(metadata) if !metadata.is_file() => return Err(FsError::NotFile),
            Ok(_) if exclusive => return Err(FsError::AlreadyExists),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let store = validate_store(self)?;
        let name = loop {
            let name = format!("txn-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
            match store.create_dir(&name) {
                Ok(()) => break name,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        };
        let stage = store.open_dir(&name)?;
        let prepared = (|| -> io::Result<()> {
            let mut owner = stage.open_with(TX_OWNER, OpenOptions::new().write(true).create_new(true))?;
            owner.write_all(TX_MAGIC)?;
            owner.sync_all()?;
            match &payload {
                ProtocolPayload::Bytes(contents) => {
                    let mut payload = stage.open_with(PAYLOAD, OpenOptions::new().write(true).create_new(true))?;
                    payload.write_all(contents)?;
                    payload.sync_all()?;
                }
                ProtocolPayload::Link(source) => self.dir.hard_link(source, &stage, PAYLOAD)?,
            }
            Ok(())
        })();
        if let Err(error) = prepared {
            drop(stage);
            let _ = discard_owned(&store, OsStr::new(&name));
            return Err(error.into());
        }
        self.inspect_relative(parent)?;
        let published = if exclusive {
            stage.hard_link(PAYLOAD, &self.dir, target)
        } else {
            stage.rename(PAYLOAD, &self.dir, target)
        };
        drop(stage);
        if let Err(error) = published {
            let _ = discard_owned(&store, OsStr::new(&name));
            return Err(FsError::AtomicPublishUnavailable(error));
        }
        let cleanup = match discard_owned(&store, OsStr::new(&name)) {
            Ok(true) => CleanupStatus::Complete,
            _ => CleanupStatus::Deferred(io::ErrorKind::Other),
        };
        Ok(CommitReceipt {
            bytes,
            directory_durability: super::metadata::sync_relative_directory(self, parent),
            cleanup,
        })
    }
}
