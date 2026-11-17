//! Filesystem facade. Each responsibility is implemented in its own module.
mod access;
mod access_model;
mod backend;
mod binding;
mod diagnostics;
mod directory;
mod execution;
mod handles;
mod inode_table;
mod directory_model;
mod host_boundary;
mod host_locks;
mod host_symlinks;
mod host_directory;
mod host_cwd;
mod log_store;
mod metadata;
mod modes;
mod namespace;
mod namespace_access;
mod namespace_resolution;
mod application;
pub use application::ApplicationDomain;
mod namespace_catalog;
pub use namespace_catalog::CatalogUpgrade;
mod objects;
mod open;
mod owned_access;
mod ownership;
mod ownership_model;
mod policy;
mod privilege;
mod recovery;
mod resource_model;
mod block_images;
mod sealed_images;
mod resources;
mod scope;
mod task_status;
mod runtime_jobs;
mod runtime_executions;
pub use runtime_jobs::RuntimeJobState;
pub(crate) use runtime_jobs::{JobObservation, ObservationKind};
pub use task_status::{TaskLeaseState, TaskLeaseStatus};
mod system_providers;
mod transaction;
mod traversal;
mod types;
pub mod vfs;
mod vfs_bridge;
pub use resource_model::{
    ResourceAssociation, ResourceId, ResourceKind, ResourceLimits, ResourceRecord, ResourceState,
};
pub use resources::{ResourceRecovery, ResourceUsage};

pub use access_model::{Acl, Credentials, DefaultAcl, DirectoryInfo, Group, TaskScopeId, Umask};
pub use backend::DataRoot;
pub use binding::ObjectBinding;
pub use diagnostics::{PhysicalUsage, Usage};
pub use directory::{DirectoryCursor, DirectoryEntry};
pub use execution::{ExecutableFile, ExecutableImage};
pub use handles::{FileHandle, HandleTable};
pub(crate) use host_boundary::validate_root_location;
pub use log_store::{
    LogBudget, LogCommit, LogDataDurability, LogMode, LogPolicy, LogStats, LogWriteProgress,
    MirrorStatus,
};
pub use metadata::{FileStat, InodeAccess, InodeIdentity, InodeMetadata, InodeTimes, LinkCount, ObjectKind, Timestamp};
pub use modes::{FileAccess, FileBehavior, FileCreation, FileMode};
pub use namespace::RenameReceipt;
pub use objects::{FileInfo, FilePosition, OpenFile};
pub use ownership::{CommitObject, OwnedCommit};
pub use ownership_model::{
    DeleteCondition, FileOwner, HostFileKind, ObjectId, OwnedFileInfo, OwnedStorageClass, OwnershipState,
    VirtualMode,
};
pub use scope::{CreationOptions, TaskRecovery, TaskScope};
pub use traversal::{WalkEntry, WalkOptions};
pub use types::{
    CleanupStatus, CommitReceipt, DirectoryDurability, Entry, EntryKind, FsError, RecoveryOutcome,
};
pub use vfs_bridge::WorkingDirectory;

#[cfg(test)]
#[path = "../tests/filesystem/mod.rs"]
mod tests;
