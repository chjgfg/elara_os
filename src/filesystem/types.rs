//! Filesystem error and directory-entry types. No IO policy or parser logic.
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::{fmt, write};

#[derive(Debug)]
pub enum FsError {
    Io(io::Error),
    InvalidRoot,
    InvalidPath,
    AccessDenied(String),
    Link(PathBuf),
    UnmarkedRoot,
    NotDirectory,
    NotFile,
    IsDirectory,
    DirectoryNotEmpty,
    NotFound,
    AlreadyExists,
    ProtectedPath,
    ReadOnlySystemPath,
    UnclassifiedWritePath,
    UnownedTransactionStore,
    TransactionLockUnavailable(io::Error),
    AtomicReplaceUnavailable(io::Error),
    AtomicPublishUnavailable(io::Error),
    AtomicRenameUnavailable(io::Error),
    BusyPath(io::Error),
    DirectoryPageTooLarge,
    DirectorySnapshotTooLarge,
    InvalidWalkLimit,
    WalkLimitExceeded,
    UsageOverflow,
    TooLarge(u64),
    IoRequestTooLarge(usize),
    InvalidOpenMode,
    HandleReadDenied,
    HandleWriteDenied,
    AppendPositionedWrite,
    InvalidOffset,
    CursorUnconfirmed,
    InvalidHandle,
    InvalidHandleLimit,
    HandleTableFull,
    HandleSpaceExhausted,
    FileObjectPoisoned,
    AllocationFailed,
    InvalidLogPolicy,
    UnownedLogStore,
    CorruptLogStore,
    LegacyLogReadOnly,
    LogRecordTooLarge,
    LogTailIncomplete,
    LogFollowupUnconfirmed { generation: u64, error: io::ErrorKind },
    LogCleanupUnconfirmed { generation: u64, error: io::ErrorKind },
    LogMaintenanceBlocked { generation: u64, error: io::ErrorKind },
    LogAppendUnconfirmed { progress: super::log_store::LogWriteProgress, error: io::ErrorKind },
    ServiceOwnedPath,
    InvalidOwnershipRecord,
    OwnershipCapacity,
    OwnershipIdentityExhausted,
    UnownedOwnershipStore,
    OwnershipRecoveryBlocked,
    OwnershipContextRequired,
    OwnershipBindingUnconfirmed,
    LegacyOwnershipUnknown,
    OwnershipDenied,
    OwnershipClassMismatch,
    OwnershipCommitPending { object: u64, error: Box<FsError> },
    WritePositionUnconfirmed { bytes: usize, error: io::Error },
}

impl fmt::Display for FsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "Filesystem error: {error}"),
            Self::InvalidRoot => write!(formatter, "An absolute non-drive-root data directory without traversal is required."),
            Self::InvalidPath => write!(formatter, "Only virtual Unix paths are allowed; traversal, device names and host path syntax are refused."),
            Self::AccessDenied(message) => write!(formatter, "{message}"),
            Self::Link(path) => write!(formatter, "Symbolic link or reparse point refused: {}", path.display()),
            Self::UnmarkedRoot => write!(formatter, "Dedicated data-root marker is missing or invalid."),
            Self::NotDirectory => write!(formatter, "The requested directory does not exist or is not a directory."),
            Self::NotFile => write!(formatter, "The requested path is not a regular file."),
            Self::IsDirectory => write!(formatter, "That path is a directory; use rmdir for empty directories."),
            Self::DirectoryNotEmpty => write!(formatter, "The directory is not empty."),
            Self::NotFound => write!(formatter, "No such file or directory."),
            Self::AlreadyExists => write!(formatter, "The path already exists."),
            Self::ProtectedPath => write!(formatter, "The virtual root and its system directories cannot be removed or replaced."),
            Self::ReadOnlySystemPath => write!(formatter, "System directory is read-only; only an update transaction may change it."),
            Self::UnclassifiedWritePath => write!(formatter, "Write refused: this path has no declared storage class."),
            Self::UnownedTransactionStore => write!(formatter, "Transaction store has no valid ownership marker; it was not modified."),
            Self::TransactionLockUnavailable(error) => write!(formatter, "Transaction lock unavailable; refusing unlocked writes: {error}"),
            Self::AtomicReplaceUnavailable(error) => write!(formatter, "Atomic replacement unavailable; no copy fallback was performed: {error}"),
            Self::AtomicPublishUnavailable(error) => write!(formatter, "Atomic new-file publication unavailable; no copy fallback was performed: {error}"),
            Self::AtomicRenameUnavailable(error) => write!(formatter, "Atomic rename unavailable; no copy/delete fallback was performed: {error}"),
            Self::BusyPath(error) => write!(formatter, "The host backend reports a busy path; no move fallback was performed: {error}"),
            Self::DirectoryPageTooLarge => write!(formatter, "Directory page exceeds the configured entry limit."),
            Self::DirectorySnapshotTooLarge => write!(formatter, "Directory snapshot exceeds the configured entry limit."),
            Self::InvalidWalkLimit => write!(formatter, "Traversal limits must be within configured bounds."),
            Self::WalkLimitExceeded => write!(formatter, "Traversal entry/depth limit exceeded; no complete result is available."),
            Self::UsageOverflow => write!(formatter, "Logical usage overflows the report's byte counter."),
            Self::TooLarge(limit) => write!(formatter, "The file exceeds the {limit}-byte display limit."),
            Self::IoRequestTooLarge(limit) => write!(formatter, "The IO request exceeds the {limit}-byte limit."),
            Self::InvalidOpenMode => write!(formatter, "Read-only opens cannot create, truncate or append."),
            Self::HandleReadDenied => write!(formatter, "The file handle was not opened for reading."),
            Self::HandleWriteDenied => write!(formatter, "The file handle was not opened for writing."),
            Self::AppendPositionedWrite => write!(formatter, "Positioned writes are refused on append handles."),
            Self::InvalidOffset => write!(formatter, "The requested file offset is negative or overflows."),
            Self::CursorUnconfirmed => write!(formatter, "The file position is unconfirmed; seek from start or end before sequential IO."),
            Self::InvalidHandle => write!(formatter, "The file handle is closed, stale or belongs to another table."),
            Self::InvalidHandleLimit => write!(formatter, "The handle-table limit must be within the configured bound."),
            Self::HandleTableFull => write!(formatter, "The file handle table is full."),
            Self::HandleSpaceExhausted => write!(formatter, "Handle identities are exhausted; reuse was refused."),
            Self::FileObjectPoisoned => write!(formatter, "The shared file object is poisoned; access was refused."),
            Self::AllocationFailed => write!(formatter, "The bounded allocation could not be reserved."),
            Self::InvalidLogPolicy => write!(formatter, "Log policy is invalid or differs from the managed store's policy."),
            Self::UnownedLogStore => write!(formatter, "Log store ownership is unknown; its contents were preserved."),
            Self::CorruptLogStore => write!(formatter, "Log store protocol is invalid; no automatic overwrite was performed."),
            Self::LegacyLogReadOnly => write!(formatter, "Existing unowned log is read-only; explicit adoption is required before writes."),
            Self::LogRecordTooLarge => write!(formatter, "One log record exceeds the configured segment bound."),
            Self::LogTailIncomplete => write!(formatter, "The bounded tail cannot provide all requested records."),
            Self::LogFollowupUnconfirmed { generation, error } => write!(formatter, "Log generation {generation} was committed, but its compatibility view is unconfirmed: {error:?}; do not retry the record."),
            Self::LogCleanupUnconfirmed { generation, error } => write!(formatter, "Log generation {generation} was committed, but retirement cleanup is deferred: {error:?}; do not retry the record."),
            Self::LogMaintenanceBlocked { generation, error } => write!(formatter, "No new log record was published; cleanup of generation {generation} is blocked: {error:?}."),
            Self::LogAppendUnconfirmed { progress, error } => write!(formatter, "Log payload progress {progress:?}, record commit unconfirmed: {error:?}; recover before retrying."),
            Self::ServiceOwnedPath => write!(formatter, "This managed path can only be written by its system service."),
            Self::InvalidOwnershipRecord => write!(formatter, "Ownership protocol is invalid; contents were preserved."),
            Self::OwnershipCapacity => write!(formatter, "Ownership catalog or object capacity exceeded."),
            Self::OwnershipIdentityExhausted => write!(formatter, "Persistent object identities or revisions are exhausted; reuse was refused."),
            Self::UnownedOwnershipStore => write!(formatter, "Ownership store marker is missing or invalid; no adoption was performed."),
            Self::OwnershipRecoveryBlocked => write!(formatter, "Ownership recovery is blocked; evidence and unknown data were preserved."),
            Self::OwnershipContextRequired => write!(formatter, "Managed files require an explicit account context."),
            Self::OwnershipBindingUnconfirmed => write!(formatter, "The public name cannot be confirmed as the registered file object."),
            Self::LegacyOwnershipUnknown => write!(formatter, "Legacy file ownership is unknown; automatic adoption was refused."),
            Self::OwnershipDenied => write!(formatter, "Permission denied by the object's virtual owner/mode."),
            Self::OwnershipClassMismatch => write!(formatter, "Moving between storage classes requires an explicit lifecycle change."),
            Self::OwnershipCommitPending { object, error } => write!(formatter, "Object {object} has a pending ownership transaction: {error}; recover before retrying."),
            Self::WritePositionUnconfirmed { bytes, error } => write!(formatter, "{bytes} bytes were written, but the append position is unconfirmed: {error}"),
        }
    }
}

pub(super) fn host_path_is_busy(error: &io::Error) -> bool {
    #[cfg(windows)]
    { std::matches!(error.raw_os_error(), Some(32 | 33)) }
    #[cfg(not(windows))]
    { let _ = error; false }
}

impl std::error::Error for FsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) | Self::BusyPath(error) | Self::TransactionLockUnavailable(error)
            | Self::AtomicReplaceUnavailable(error) | Self::AtomicPublishUnavailable(error)
            | Self::AtomicRenameUnavailable(error) | Self::WritePositionUnconfirmed { error, .. } => Some(error),
            Self::OwnershipCommitPending { error, .. } => Some(error.as_ref()),
            _ => None,
        }
    }
}

impl From<io::Error> for FsError {
    fn from(error: io::Error) -> Self {
        if host_path_is_busy(&error) { Self::BusyPath(error) } else { Self::Io(error) }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryDurability {
    Synced,
    Unconfirmed(io::ErrorKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupStatus {
    Complete,
    Deferred(io::ErrorKind),
}

/// Success means atomic-visible replacement or new-file publication. Directory
/// durability and cleanup remain explicit after the namespace change commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitReceipt {
    pub bytes: usize,
    pub directory_durability: DirectoryDurability,
    pub cleanup: CleanupStatus,
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransactionFault {
    None,
    PartialStage,
    RenameUnsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryOutcome {
    NoStore,
    Inspected { discarded: usize, preserved: usize },
}
