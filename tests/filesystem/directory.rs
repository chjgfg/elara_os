use super::fixture;
use crate::config::ROOTFS_DEFAULTS;
use crate::filesystem::{DataRoot, FsError, ObjectKind};
use std::{assert, assert_eq, format, fs, matches};

#[test]
fn directory_pages_are_sorted_seekable_bounded_and_rewindable() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    data.create_file("/home/c").unwrap();
    data.create_directory("/home/b").unwrap();
    data.create_file("/home/a").unwrap();
    let mut cursor = data.open_directory("/home").unwrap();
    assert_eq!(cursor.len(), 3);
    assert!(!cursor.is_empty());
    assert_eq!(cursor.read(0).unwrap().len(), 0);
    let page = cursor.read(2).unwrap();
    assert_eq!(page[0].name, "a");
    assert_eq!(page[0].kind, ObjectKind::File);
    assert_eq!(page[1].name, "b");
    assert_eq!(page[1].kind, ObjectKind::Directory);
    assert_eq!(cursor.position(), 2);
    assert!(matches!(
        cursor.read(ROOTFS_DEFAULTS.max_directory_page + 1),
        Err(FsError::DirectoryPageTooLarge)
    ));
    assert_eq!(cursor.position(), 2);
    assert_eq!(cursor.read(8).unwrap()[0].name, "c");
    assert!(cursor.read(1).unwrap().is_empty());
    assert!(matches!(cursor.seek(4), Err(FsError::InvalidOffset)));
    cursor.seek(1).unwrap();
    assert_eq!(cursor.read(1).unwrap()[0].name, "b");
    cursor.rewind();
    assert_eq!(cursor.read(1).unwrap()[0].name, "a");
}
#[cfg(unix)]
#[test]
fn pinned_directory_refresh_does_not_follow_its_reused_old_name() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    data.create_directory("/home/old").unwrap();
    data.create_file("/home/old/a").unwrap();
    let mut cursor = data.open_directory("/home/old").unwrap();
    data.rename("/home/old", "/home/moved").unwrap();
    data.create_directory("/home/old").unwrap();
    data.create_file("/home/old/wrong").unwrap();
    data.create_file("/home/moved/b").unwrap();
    assert_eq!(cursor.len(), 1, "old snapshot must not change silently");
    cursor.refresh().unwrap();
    assert_eq!(cursor.position(), 0);
    let page = cursor.read(8).unwrap();
    assert_eq!(page.len(), 2);
    assert_eq!(page[0].name, "a");
    assert_eq!(page[1].name, "b");
}
#[cfg(windows)]
#[test]
fn pinned_directory_blocks_rename_and_closing_it_releases_the_host_guard() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    data.create_directory("/home/old").unwrap();
    data.create_file("/home/old/a").unwrap();
    let mut cursor = data.open_directory("/home/old").unwrap();
    assert!(matches!(
        data.rename("/home/old", "/home/moved"),
        Err(FsError::BusyPath(_))
    ));
    assert!(root.join("home/old/a").is_file());
    assert!(!root.join("home/moved").exists());
    data.create_file("/home/old/b").unwrap();
    assert_eq!(cursor.len(), 1);
    cursor.refresh().unwrap();
    assert_eq!(cursor.position(), 0);
    let page = cursor.read(8).unwrap();
    assert_eq!(page.len(), 2);
    assert_eq!(page[0].name, "a");
    assert_eq!(page[1].name, "b");
    drop(cursor);
    // Counterexample: removing the pinned handle permits the same rename.
    data.rename("/home/old", "/home/moved").unwrap();
    assert!(!root.join("home/old").exists());
    assert!(root.join("home/moved/a").is_file());
    assert!(root.join("home/moved/b").is_file());
}

#[cfg(windows)]
#[test]
fn empty_open_directory_is_busy_for_remove_and_drop_releases_it() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    data.create_directory("/home/empty").unwrap();
    let cursor = data.open_directory("/home/empty").unwrap();
    assert!(matches!(data.remove_directory("/home/empty"), Err(FsError::BusyPath(_))));
    assert!(root.join("home/empty").is_dir());
    drop(cursor);
    data.remove_directory("/home/empty").unwrap();
    assert!(!root.join("home/empty").exists());
}

#[test]
fn directory_listing_hides_protocol_names_and_refuses_non_directories() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    data.create_file("/home/a").unwrap();
    let mut cursor = data.open_directory("/").unwrap();
    assert!(!cursor
        .read(256)
        .unwrap()
        .iter()
        .any(|entry| entry.name == ROOTFS_DEFAULTS.marker_filename));
    let mut temporary = data.open_directory("/tmp").unwrap();
    assert!(!temporary
        .read(256)
        .unwrap()
        .iter()
        .any(|entry| entry.name == ROOTFS_DEFAULTS.transaction_directory));
    assert!(!data
        .list("/tmp")
        .unwrap()
        .iter()
        .any(|entry| entry.name == ROOTFS_DEFAULTS.transaction_directory));
    assert!(matches!(
        data.open_directory("/home/a"),
        Err(FsError::NotDirectory)
    ));
    assert!(matches!(
        data.open_directory("C:/Windows"),
        Err(FsError::InvalidPath)
    ));
}
#[test]
fn oversized_refresh_preserves_the_previous_snapshot_and_position() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    fs::write(root.join("home/a"), b"").unwrap();
    let mut cursor = data.open_directory("/home").unwrap();
    cursor.read(1).unwrap();
    for index in 0..ROOTFS_DEFAULTS.max_directory_entries {
        fs::write(root.join(format!("home/file-{index}")), b"").unwrap();
    }
    assert!(matches!(
        cursor.refresh(),
        Err(FsError::DirectorySnapshotTooLarge)
    ));
    assert_eq!(cursor.len(), 1);
    assert_eq!(cursor.position(), 1);
    assert!(matches!(
        data.open_directory("/home"),
        Err(FsError::DirectorySnapshotTooLarge)
    ));
}
