//! Filesystem facade. Each responsibility is implemented in its own module.
mod backend;
mod binding;
mod diagnostics;
mod directory;
mod handles;
mod log_store;
mod metadata;
mod modes;
mod namespace;
mod objects;
mod ownership;
mod ownership_model;
mod open;
mod owned_access;
mod policy;
mod recovery;
mod transaction;
mod traversal;
mod types;

pub use backend::DataRoot;
pub use binding::ObjectBinding;
pub use diagnostics::{PhysicalUsage, Usage};
pub use directory::{DirectoryCursor, DirectoryEntry};
pub use handles::{FileHandle, HandleTable};
pub use log_store::{LogBudget, LogCommit, LogDataDurability, LogMode, LogPolicy, LogStats, LogWriteProgress, MirrorStatus};
pub use metadata::{FileStat, ObjectKind, Timestamp};
pub use modes::{FileAccess, FileBehavior, FileCreation, FileMode};
pub use namespace::RenameReceipt;
pub use objects::{FileInfo, FilePosition, OpenFile};
pub use ownership::OwnedCommit;
pub use ownership_model::{ObjectId, FileOwner, VirtualMode, OwnedFileInfo, OwnedStorageClass, OwnershipState, DeleteCondition};
pub use traversal::{WalkEntry, WalkOptions};
pub use types::{
    CleanupStatus, CommitReceipt, DirectoryDurability, Entry, EntryKind, FsError, RecoveryOutcome,
};

#[cfg(test)]
#[path = "../tests/filesystem/mod.rs"]
mod tests;
