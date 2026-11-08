//! Bounded, iterative, read-only traversal. This is an observed host tree, not
//! an atomic filesystem snapshot; native links are reported and never followed.
use super::{DataRoot, FileStat, FsError, ObjectKind};
use crate::config::ROOTFS_DEFAULTS;
use crate::virtual_path::VirtualPath;
use std::ffi::OsString;
use std::{format, vec};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalkOptions {
    max_entries: usize,
    max_depth: usize,
}
impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            max_entries: ROOTFS_DEFAULTS.max_walk_entries,
            max_depth: ROOTFS_DEFAULTS.max_walk_depth,
        }
    }
}
impl WalkOptions {
    pub fn new(max_entries: usize, max_depth: usize) -> Result<Self, FsError> {
        if max_entries == 0
            || max_entries > ROOTFS_DEFAULTS.max_walk_entries
            || max_depth > ROOTFS_DEFAULTS.max_walk_depth
        {
            return Err(FsError::InvalidWalkLimit);
        }
        Ok(Self {
            max_entries,
            max_depth,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WalkEntry {
    Accessible {
        path: String,
        stat: FileStat,
        depth: usize,
    },
    /// A host-only name cannot be converted into a valid virtual path. Keep the
    /// literal name instead of a lossy conversion that could alias another file.
    BlockedName {
        parent: String,
        name: OsString,
        depth: usize,
    },
}
impl DataRoot {
    pub fn walk(&self, path: &str, options: WalkOptions) -> Result<Vec<WalkEntry>, FsError> {
        self.walk_checked(path, options, |_| Ok(()))
    }

    /// Check every virtual path before inspecting it or enumerating its children.
    /// A rejected entry aborts the whole observation; no partial tree is returned.
    pub fn walk_checked<F>(
        &self,
        path: &str,
        options: WalkOptions,
        mut check: F,
    ) -> Result<Vec<WalkEntry>, FsError>
    where
        F: FnMut(&str) -> Result<(), FsError>,
    {
        let path = VirtualPath::strict(path)
            .map_err(|_| FsError::InvalidPath)?
            .to_string();
        let mut pending = vec![(path, 0usize)];
        let mut observed = Vec::new();
        while let Some((path, depth)) = pending.pop() {
            if depth > options.max_depth {
                return Err(FsError::WalkLimitExceeded);
            }
            check(&path)?;
            let stat = self.stat(&path)?;
            let descend = stat.kind == ObjectKind::Directory;
            push(
                &mut observed,
                WalkEntry::Accessible {
                    path: path.clone(),
                    stat,
                    depth,
                },
                options,
            )?;
            if !descend {
                continue;
            }
            let mut directory = self.open_directory(&path)?;
            let mut children = Vec::new();
            loop {
                let entries = directory.read(ROOTFS_DEFAULTS.max_directory_page)?;
                if entries.is_empty() {
                    break;
                }
                for entry in entries {
                    if depth >= options.max_depth {
                        return Err(FsError::WalkLimitExceeded);
                    }
                    let child = entry.name.to_str().map(|name| {
                        if path == "/" {
                            format!("/{name}")
                        } else {
                            format!("{path}/{name}")
                        }
                    });
                    match child {
                        Some(child) if VirtualPath::strict(&child).is_ok() => {
                            if pending.len() + children.len() + observed.len()
                                >= options.max_entries
                            {
                                return Err(FsError::WalkLimitExceeded);
                            }
                            children
                                .try_reserve(1)
                                .map_err(|_| FsError::AllocationFailed)?;
                            children.push((child, depth + 1));
                        }
                        _ => push(
                            &mut observed,
                            WalkEntry::BlockedName {
                                parent: path.clone(),
                                name: entry.name,
                                depth: depth + 1,
                            },
                            options,
                        )?,
                    }
                }
            }
            pending
                .try_reserve(children.len())
                .map_err(|_| FsError::AllocationFailed)?;
            pending.extend(children.into_iter().rev());
        }
        Ok(observed)
    }
}
fn push(
    entries: &mut Vec<WalkEntry>,
    entry: WalkEntry,
    options: WalkOptions,
) -> Result<(), FsError> {
    if entries.len() >= options.max_entries {
        return Err(FsError::WalkLimitExceeded);
    }
    entries
        .try_reserve(1)
        .map_err(|_| FsError::AllocationFailed)?;
    entries.push(entry);
    Ok(())
}
