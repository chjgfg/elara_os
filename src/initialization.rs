use crate::config::ROOTFS_DEFAULTS;
use crate::filesystem::{DataRoot, FsError};
use std::io::Write;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path};
use std::{fmt, fs, matches, write};

#[derive(Debug, PartialEq, Eq)]
pub enum InitState {
    CreatedNewRoot(usize),
    ClaimedEmptyRoot(usize),
    CompletedOwnedRoot(usize),
    AlreadyReady,
}

impl fmt::Display for InitState {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreatedNewRoot(count) => write!(output, "CreatedNewRoot ({count} directories)"),
            Self::ClaimedEmptyRoot(count) => {
                write!(output, "ClaimedEmptyRoot ({count} directories)")
            }
            Self::CompletedOwnedRoot(count) => {
                write!(output, "CompletedOwnedRoot ({count} directories)")
            }
            Self::AlreadyReady => write!(output, "AlreadyReady"),
        }
    }
}

fn inspect_ancestors(path: &Path) -> Result<(), FsError> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(FsError::Link(ancestor.to_owned()));
                }
                #[cfg(windows)]
                if metadata.file_attributes() & 0x400 != 0 {
                    return Err(FsError::Link(ancestor.to_owned()));
                }
                if ancestor != path && !metadata.is_dir() {
                    return Err(FsError::NotDirectory);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub fn prepare(root: &Path) -> Result<InitState, FsError> {
    if !root.is_absolute()
        || root.parent().is_none()
        || root
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(FsError::InvalidRoot);
    }
    #[cfg(windows)]
    if !matches!(root.components().next(), Some(Component::Prefix(prefix))
        if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
    {
        return Err(FsError::InvalidRoot);
    }
    inspect_ancestors(root)?;
    let existed = root.exists();
    if existed && !root.is_dir() {
        return Err(FsError::NotDirectory);
    }
    let marker = root.join(ROOTFS_DEFAULTS.marker_filename);
    inspect_ancestors(&marker)?;
    let owned = marker.exists();
    if owned {
        if fs::read(&marker)?.as_slice() != ROOTFS_DEFAULTS.marker_contents {
            return Err(FsError::UnmarkedRoot);
        }
    } else if existed && fs::read_dir(root)?.next().is_some() {
        return Err(FsError::UnmarkedRoot);
    }
    let mut directories = Vec::new();
    for relative in ROOTFS_DEFAULTS.directories {
        if relative.is_empty()
            || relative.split('/').any(|part| {
                part.is_empty()
                    || !part
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            })
        {
            return Err(FsError::InvalidPath);
        }
        let directory = root.join(relative);
        inspect_ancestors(&directory)?;
        if directory.exists() && !directory.is_dir() {
            return Err(FsError::NotDirectory);
        }
        directories.push(directory);
    }
    fs::create_dir_all(root)?;
    if !owned {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)?;
        file.write_all(ROOTFS_DEFAULTS.marker_contents)?;
    }
    let mut created = 0;
    for directory in directories {
        if !directory.exists() {
            created += 1;
        }
        fs::create_dir_all(&directory)?;
        inspect_ancestors(&directory)?;
    }
    DataRoot::open(root)?;
    Ok(if !existed {
        InitState::CreatedNewRoot(created)
    } else if !owned {
        InitState::ClaimedEmptyRoot(created)
    } else if created != 0 {
        InitState::CompletedOwnedRoot(created)
    } else {
        InitState::AlreadyReady
    })
}

#[cfg(test)]
mod tests {
    use super::{prepare, InitState};
    use crate::config::ROOTFS_DEFAULTS;
    use crate::filesystem::FsError;
    use std::path::PathBuf;
    use std::{assert, assert_eq, fs, matches};

    fn fresh_path() -> PathBuf {
        crate::test_support::fresh_path("init")
    }

    #[test]
    fn fresh_root_is_initialized_and_repeated_start_is_idempotent() {
        let root = fresh_path();
        assert!(matches!(
            prepare(&root).unwrap(),
            InitState::CreatedNewRoot(_)
        ));
        for relative in ROOTFS_DEFAULTS.directories {
            assert!(root.join(relative).is_dir());
        }
        assert_eq!(prepare(&root).unwrap(), InitState::AlreadyReady);
    }

    #[test]
    fn owned_root_is_completed_without_overwriting_data() {
        let root = fresh_path();
        prepare(&root).unwrap();
        fs::write(root.join("root/keep.txt"), b"preserve").unwrap();
        fs::rename(root.join("srv"), root.join("srv.saved")).unwrap();
        assert!(matches!(
            prepare(&root).unwrap(),
            InitState::CompletedOwnedRoot(1)
        ));
        assert_eq!(fs::read(root.join("root/keep.txt")).unwrap(), b"preserve");
    }

    #[test]
    fn nonempty_unowned_directory_is_never_claimed() {
        let root = fresh_path();
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("keep.txt"), b"preserve").unwrap();
        assert!(matches!(prepare(&root), Err(FsError::UnmarkedRoot)));
        assert!(!root.join(ROOTFS_DEFAULTS.marker_filename).exists());
        assert!(!root.join("boot").exists());
        assert_eq!(fs::read(root.join("keep.txt")).unwrap(), b"preserve");
    }

    #[test]
    fn existing_file_collision_is_not_replaced() {
        let root = fresh_path();
        prepare(&root).unwrap();
        fs::rename(root.join("usr"), root.join("usr.saved")).unwrap();
        fs::write(root.join("usr"), b"preserve").unwrap();
        assert!(matches!(prepare(&root), Err(FsError::NotDirectory)));
        assert_eq!(fs::read(root.join("usr")).unwrap(), b"preserve");
    }
}
