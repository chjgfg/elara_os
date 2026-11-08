//! General namespace operations, not shell business logic. Namespace mutators
//! serialize with first-party create/delete/open and transaction recovery.
use super::metadata::sync_relative_directory;
use super::transaction::Publication;
use super::{CommitReceipt, DataRoot, DirectoryDurability, FsError};
use crate::config::ROOTFS_DEFAULTS;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenameReceipt {
    pub source_directory: DirectoryDurability,
    pub destination_directory: DirectoryDurability,
}

fn is_same_or_descendant(destination: &Path, source: &Path) -> bool {
    let source_len = source.components().count();
    if destination.components().count() < source_len { return false; }
    destination.components().zip(source.components()).all(|(left, right)| {
        #[cfg(windows)]
        { left.as_os_str().to_string_lossy().eq_ignore_ascii_case(&right.as_os_str().to_string_lossy()) }
        #[cfg(not(windows))]
        { left == right }
    })
}

impl DataRoot {
    /// Existing destinations are refused under the shared namespace lock.
    /// This is not protection from hostile host programs bypassing that lock.
    /// Rename failure never falls back to a copy/delete operation.
    pub fn rename(&self, source: &str, destination: &str) -> Result<RenameReceipt, FsError> {
        self.resolve_existing(source)?;
        self.resolve_new(destination)?;
        self.ensure_write_allowed(destination)?;
        let _lock = self.namespace_guard()?;
        let (source, metadata) = self.resolve_existing(source)?;
        let destination = self.resolve_new(destination)?;
        if !metadata.is_file() && !metadata.is_dir() {
            return Err(FsError::NotFile);
        }
        if metadata.is_dir() && is_same_or_descendant(&destination, &source) {
            return Err(FsError::InvalidPath);
        }
        self.dir
            .rename(&source, &self.dir, &destination)
            .map_err(|error| {
                if super::types::host_path_is_busy(&error) {
                    return FsError::BusyPath(error);
                }
                FsError::AtomicRenameUnavailable(error)
            })?;
        Ok(RenameReceipt {
            source_directory: sync_relative_directory(
                self,
                source.parent().unwrap_or(Path::new(".")),
            ),
            destination_directory: sync_relative_directory(
                self,
                destination.parent().unwrap_or(Path::new(".")),
            ),
        })
    }

    /// Copy observed bytes to a NEW ordinary file. No permission/timestamp clone.
    /// A private synchronized payload is atomically linked into the destination;
    /// an occupied target cannot be overwritten, even by a concurrent host writer.
    /// Unsupported links/cross-device publication fail without copy fallback.
    pub fn copy_file(&self, source: &str, destination: &str) -> Result<CommitReceipt, FsError> {
        let contents = self.read_file(source, ROOTFS_DEFAULTS.max_transaction_bytes as u64)?;
        self.publish_bytes(destination, &contents, Publication::CreateExclusive)
    }
}
