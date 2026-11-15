use crate::config::ROOTFS_DEFAULTS;
use crate::filesystem::{DataRoot, FsError};
use cap_std::{ambient_authority, fs::{Dir, Metadata, OpenOptions}};
use std::io::{Read, Write};
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
use std::path::Path;
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

/// Compatibility wrapper; startup should retain the returned capability via prepare_open.
pub fn prepare(root: &Path) -> Result<InitState, FsError> {
    prepare_open(root).map(|(_, state)| state)
}

/// Only bootstrap touches the host parent, to create the explicitly selected
/// root itself. Missing ancestors are refused, never created outside that root.
pub fn prepare_open(root: &Path) -> Result<(DataRoot, InitState), FsError> {
    prepare_open_checked(root, |_, _| {})
}

fn inspect_capability_path(dir: &Dir, path: &Path) -> Result<(), FsError> {
    let mut relative = std::path::PathBuf::new();
    for component in path.components() {
        relative.push(component.as_os_str());
        match dir.symlink_metadata(&relative) {
            Ok(metadata) if cap_linked(&metadata) => return Err(FsError::Link(relative)),
            Ok(metadata) if !metadata.is_dir() => return Err(FsError::NotDirectory),
            Ok(_) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn cap_linked(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use cap_std::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 { return true; }
    }
    metadata.file_type().is_symlink()
}

fn prepare_open_checked(root: &Path, mut before_create: impl FnMut(&Dir, &str)) -> Result<(DataRoot, InitState), FsError> {
    crate::filesystem::validate_root_location(root)?;
    inspect_ancestors(root)?;
    let parent = root.parent().ok_or(FsError::InvalidRoot)?;
    if !parent.is_dir() { return Err(FsError::NotDirectory); }
    let parent_path = parent.canonicalize()?;
    inspect_ancestors(&parent_path)?;
    let name = root.file_name().ok_or(FsError::InvalidRoot)?;
    let resolved_root = parent_path.join(name);
    crate::filesystem::validate_root_location(&resolved_root)?;
    let parent = Dir::open_ambient_dir(&parent_path, ambient_authority())?;
    let existed = match parent.symlink_metadata(name) {
        Ok(metadata) if cap_linked(&metadata) => return Err(FsError::Link(root.to_owned())),
        Ok(metadata) if !metadata.is_dir() => return Err(FsError::NotDirectory),
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    if !existed { parent.create_dir(name)?; }
    let dir = parent.open_dir(name)?;
    let root = resolved_root;
    let marker = Path::new(ROOTFS_DEFAULTS.marker_filename);
    let owned = match dir.symlink_metadata(marker) {
        Ok(metadata) if cap_linked(&metadata) => return Err(FsError::Link(root.join(marker))),
        Ok(metadata) if !metadata.is_file() => return Err(FsError::UnmarkedRoot),
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    if owned {
        let mut bytes = Vec::new();
        dir.open(marker)?.take((ROOTFS_DEFAULTS.marker_contents.len() + 1) as u64).read_to_end(&mut bytes)?;
        if bytes != ROOTFS_DEFAULTS.marker_contents { return Err(FsError::UnmarkedRoot); }
    } else if dir.entries()?.next().is_some() { return Err(FsError::UnmarkedRoot); }
    let mut created = 0;
    for relative in ROOTFS_DEFAULTS.directories {
        if relative.is_empty() || relative.split('/').any(|part| part.is_empty()
            || !part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))) {
            return Err(FsError::InvalidPath);
        }
        inspect_capability_path(&dir, Path::new(relative))?;
        if !dir.exists(relative) { created += 1; }
    }
    if !owned {
        let mut file = dir.open_with(marker, OpenOptions::new().write(true).create_new(true))?;
        file.write_all(ROOTFS_DEFAULTS.marker_contents)?;
        file.sync_all()?;
    }
    for relative in ROOTFS_DEFAULTS.directories {
        before_create(&dir, relative);
        dir.create_dir_all(relative)?;
        inspect_capability_path(&dir, Path::new(relative))?;
    }
    let data = DataRoot::from_granted_directory(root, dir)?;
    data.recover_transactions()?;
    let state = if !existed { InitState::CreatedNewRoot(created) }
        else if !owned { InitState::ClaimedEmptyRoot(created) }
        else if created != 0 { InitState::CompletedOwnedRoot(created) }
        else { InitState::AlreadyReady };
    Ok((data, state))
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

    #[test]
    fn missing_host_parent_is_refused_without_creating_ancestors() {
        let parent = fresh_path(); let root = parent.join("data");
        assert!(matches!(prepare(&root), Err(FsError::NotDirectory)));
        assert!(!parent.exists());
    }

    #[test]
    fn raw_dot_root_is_refused_before_creating_any_directory() {
        let parent = fresh_path(); fs::create_dir(&parent).unwrap();
        let target = PathBuf::from(std::format!("{}/./data", parent.display()));
        assert!(matches!(prepare(&target), Err(FsError::InvalidRoot)));
        assert!(!parent.join("data").exists());
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 0);
    }

    #[test]
    fn startup_retains_its_granted_root_and_marker_reads_are_bounded() {
        let path = fresh_path();
        let (root, state) = super::prepare_open(&path).unwrap();
        assert!(matches!(state, InitState::CreatedNewRoot(_)));
        root.create_file("/tmp/inside").unwrap();
        assert!(path.join("tmp/inside").is_file());
        drop(root);
        fs::write(path.join(ROOTFS_DEFAULTS.marker_filename), [b'x'; 4096]).unwrap();
        assert!(matches!(prepare(&path), Err(FsError::UnmarkedRoot)));
        assert!(matches!(crate::filesystem::DataRoot::open(&path), Err(FsError::UnmarkedRoot)));
        assert_eq!(fs::read(path.join("tmp/inside")).unwrap(), b"");
    }

    #[test]
    fn initialization_parent_swap_after_preflight_cannot_write_outside_capability() {
        let path = fresh_path(); prepare(&path).unwrap();
        let outside = fresh_path(); fs::create_dir(&outside).unwrap();
        fs::write(outside.join("sentinel"), b"outside").unwrap();
        let result = super::prepare_open_checked(&path, |_, relative| {
            if relative != "usr/bin" { return; }
            // Parent usr has already passed the preceding create/inspect step.
            fs::rename(path.join("usr"), path.join("usr.saved")).unwrap();
            #[cfg(windows)] {
                let status = std::process::Command::new("cmd.exe")
                    .args(["/d", "/c", "mklink", "/J"])
                    .arg(path.join("usr")).arg(&outside).status().unwrap();
                assert!(status.success());
            }
            #[cfg(unix)] std::os::unix::fs::symlink(&outside, path.join("usr")).unwrap();
            // Control: ambient traversal sees outside, so rechecking strings
            // alone would not confine a subsequent create_dir_all.
            assert_eq!(fs::read(path.join("usr/sentinel")).unwrap(), b"outside");
        });
        assert!(result.is_err());
        assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"outside");
        assert!(!outside.join("bin").exists()); assert!(!outside.join("lib").exists());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
        // Positive removal control, confined to this synthetic target fixture:
        // replace the capability create with ambient IO and an external bin
        // really is created. The production path above created nothing there.
        fs::create_dir_all(path.join("usr/bin")).unwrap();
        assert!(outside.join("bin").is_dir());
        assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"outside");
    }
}
