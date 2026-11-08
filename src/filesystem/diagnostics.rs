//! Read-only logical usage. Physical allocation, native hard-link deduplication
//! and private protocol storage are deliberately not fabricated by this report.
use super::{DataRoot, FsError, ObjectKind, WalkEntry, WalkOptions};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhysicalUsage {
    UnavailableOnHostBackend,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    pub files: u64,
    pub directories: u64,
    pub blocked_links: u64,
    pub blocked_names: u64,
    pub other: u64,
    pub logical_file_bytes: u64,
    pub physical_usage: PhysicalUsage,
}
impl DataRoot {
    /// Includes the requested root entry. No partial result on traversal failure.
    pub fn usage(&self, path: &str, options: WalkOptions) -> Result<Usage, FsError> {
        Usage::from_entries(&self.walk(path, options)?)
    }

    pub fn usage_checked<F>(
        &self,
        path: &str,
        options: WalkOptions,
        check: F,
    ) -> Result<Usage, FsError>
    where
        F: FnMut(&str) -> Result<(), FsError>,
    {
        Usage::from_entries(&self.walk_checked(path, options, check)?)
    }
}

impl Usage {
    pub(super) fn from_entries(entries: &[WalkEntry]) -> Result<Self, FsError> {
        let mut usage = Usage {
            files: 0,
            directories: 0,
            blocked_links: 0,
            blocked_names: 0,
            other: 0,
            logical_file_bytes: 0,
            physical_usage: PhysicalUsage::UnavailableOnHostBackend,
        };
        for entry in entries {
            match entry {
                WalkEntry::BlockedName { .. } => usage.blocked_names += 1,
                WalkEntry::Accessible { stat, .. } => match stat.kind {
                    ObjectKind::File => {
                        usage.files += 1;
                        usage.logical_file_bytes = usage
                            .logical_file_bytes
                            .checked_add(stat.length)
                            .ok_or(FsError::UsageOverflow)?;
                    }
                    ObjectKind::Directory => usage.directories += 1,
                    ObjectKind::BlockedLink => usage.blocked_links += 1,
                    ObjectKind::Other => usage.other += 1,
                },
            }
        }
        Ok(usage)
    }
}
