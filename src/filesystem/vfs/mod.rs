//! Internal filesystems; inode identity is separate from directory names.
//! No host mounts, device access, or ambient-path IO is performed here.
mod archive;
pub use archive::VolumeAccess;
mod model;
mod registry;
mod resolution;
mod volume;
pub use model::{Error, InodeKind, InodeStat, Limits, LockMode, ShareMode, Stats};
pub use registry::{MountInfo, MountKind, Registry, Route};
pub use volume::{DirectoryAnchor, File, Volume};

mod image;
pub use image::{ImageStats, Loaded, Recovery};

pub(crate) use volume::Directory;

mod pseudo;
pub use pseudo::PseudoKind;

pub mod block_store;
mod block_volume;
mod data;

pub(super) mod sealed;

mod overlay;

mod dedup;
pub use dedup::DedupStats;
