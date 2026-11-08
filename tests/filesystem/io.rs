use super::fixture;
use crate::config::ROOTFS_DEFAULTS;
use crate::filesystem::{backend::virtual_relative, DataRoot, FsError};
use std::path::PathBuf;
use std::{assert, assert_eq, format, fs, matches};

#[test]
fn virtual_paths_are_relative_to_data_root() {
    assert_eq!(
        virtual_relative("/usr/bin").unwrap(),
        PathBuf::from("usr/bin")
    );
    assert_eq!(virtual_relative("/").unwrap(), PathBuf::new());
}

#[test]
fn traversal_host_paths_and_devices_are_rejected() {
    for path in [
        "../",
        "/usr/../etc",
        "C:/Windows",
        "\\\\server\\share",
        "usr/file:stream",
        "NUL",
        "CON.txt",
        "COM¹",
        "usr/end.",
        ".elara-root",
    ] {
        assert!(
            matches!(virtual_relative(path), Err(FsError::InvalidPath)),
            "{path}"
        );
    }
}

#[test]
fn removing_marker_breaks_startup_and_restoring_it_recovers() {
    let root = fixture();
    assert!(DataRoot::open(&root).is_ok());
    fs::rename(
        root.join(ROOTFS_DEFAULTS.marker_filename),
        root.join("marker.saved"),
    )
    .unwrap();
    assert!(matches!(DataRoot::open(&root), Err(FsError::UnmarkedRoot)));
    fs::rename(
        root.join("marker.saved"),
        root.join(ROOTFS_DEFAULTS.marker_filename),
    )
    .unwrap();
    assert!(DataRoot::open(&root).is_ok());
}

#[test]
fn listing_cannot_escape_the_root() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    assert!(data.list("/usr").is_ok());
    assert!(matches!(
        data.list("/usr/../../"),
        Err(FsError::InvalidPath)
    ));
    assert!(matches!(data.list("C:/Windows"), Err(FsError::InvalidPath)));
}

#[test]
fn directory_links_are_refused() {
    let target = fixture();
    let link = target.parent().unwrap().join(format!(
        "link-{}",
        target.file_name().unwrap().to_string_lossy()
    ));
    #[cfg(windows)]
    {
        let status = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .status()
            .unwrap();
        assert!(status.success());
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(matches!(DataRoot::open(&link), Err(FsError::Link(_))));
}

#[test]
fn files_and_directories_are_created_read_and_removed_inside_the_root() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    data.create_directory("/home/elara").unwrap();
    data.create_file("/home/elara/notes.txt").unwrap();
    assert!(root.join("home/elara/notes.txt").is_file());
    assert_eq!(data.read_file("/home/elara/notes.txt", 64).unwrap(), b"");
    fs::write(root.join("home/elara/notes.txt"), "星澜\n").unwrap();
    assert_eq!(
        data.read_file("/home/elara/notes.txt", 64).unwrap(),
        "星澜\n".as_bytes()
    );
    data.remove_file("/home/elara/notes.txt").unwrap();
    assert!(!root.join("home/elara/notes.txt").exists());
}

#[test]
fn creation_refuses_duplicates_missing_parents_and_escapes() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    assert!(matches!(
        data.create_directory("/usr"),
        Err(FsError::AlreadyExists)
    ));
    assert!(matches!(
        data.create_file("/missing/file.txt"),
        Err(FsError::NotDirectory)
    ));
    assert!(matches!(
        data.create_file("/usr/../../escape.txt"),
        Err(FsError::InvalidPath)
    ));
    assert!(matches!(
        data.create_file("C:/Windows/escape.txt"),
        Err(FsError::InvalidPath)
    ));
    assert!(matches!(data.create_file("/"), Err(FsError::ProtectedPath)));
    assert!(matches!(
        data.create_file("/NUL"),
        Err(FsError::InvalidPath)
    ));
    assert!(!root.parent().unwrap().join("escape.txt").exists());
}

#[test]
fn removal_protects_the_root_system_directories_and_marker() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    assert!(matches!(data.remove_file("/"), Err(FsError::ProtectedPath)));
    assert!(matches!(
        data.remove_file("/usr"),
        Err(FsError::ProtectedPath)
    ));
    assert!(matches!(
        data.remove_file("/.elara-root"),
        Err(FsError::InvalidPath)
    ));
    assert!(matches!(
        data.remove_file("/missing.txt"),
        Err(FsError::NotFound)
    ));
    data.create_directory("/home/dir").unwrap();
    assert!(matches!(
        data.remove_file("/home/dir"),
        Err(FsError::IsDirectory)
    ));
    assert!(root.join("usr").is_dir());
    assert!(root.join(ROOTFS_DEFAULTS.marker_filename).is_file());
}

#[test]
fn rmdir_removes_only_empty_unprotected_directories() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    data.create_directory("/home/empty").unwrap();
    data.create_directory("/home/full").unwrap();
    data.create_file("/home/full/keep.txt").unwrap();
    data.create_file("/home/plain.txt").unwrap();
    assert!(matches!(
        data.remove_directory("/"),
        Err(FsError::ProtectedPath)
    ));
    assert!(matches!(
        data.remove_directory("/usr"),
        Err(FsError::ProtectedPath)
    ));
    assert!(matches!(
        data.remove_directory("/home/full"),
        Err(FsError::DirectoryNotEmpty)
    ));
    assert!(matches!(
        data.remove_directory("/home/plain.txt"),
        Err(FsError::NotDirectory)
    ));
    assert!(matches!(
        data.remove_directory("/home/missing"),
        Err(FsError::NotFound)
    ));
    data.remove_directory("/home/empty").unwrap();
    assert!(!root.join("home/empty").exists());
    assert!(root.join("home/full/keep.txt").is_file());
}

#[test]
fn reading_enforces_the_display_limit_and_file_type() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    fs::write(root.join("home/big.txt"), [b'x'; 65]).unwrap();
    assert!(matches!(
        data.read_file("/home/big.txt", 64),
        Err(FsError::TooLarge(64))
    ));
    assert!(matches!(data.read_file("/home", 64), Err(FsError::NotFile)));
}
