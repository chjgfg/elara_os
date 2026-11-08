use super::fixture;
use crate::filesystem::{DataRoot, FsError};
use std::{assert, assert_eq, fs, matches};

#[test]
fn system_paths_are_read_only_through_every_mutation_api() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    fs::write(root.join("usr/existing.txt"), b"original").unwrap();
    fs::create_dir(root.join("usr/empty-dir")).unwrap();
    for path in [
        "/usr/new.txt",
        "/bin/new.txt",
        "/boot/new.txt",
        "/sys/new.txt",
    ] {
        assert!(
            matches!(data.create_file(path), Err(FsError::ReadOnlySystemPath)),
            "{path}"
        );
    }
    assert!(matches!(
        data.create_directory("/usr/new-dir"),
        Err(FsError::ReadOnlySystemPath)
    ));
    assert!(matches!(
        data.write_file("/usr/existing.txt", b"changed"),
        Err(FsError::ReadOnlySystemPath)
    ));
    assert!(matches!(
        data.append_file("/usr/existing.txt", b"extra"),
        Err(FsError::ReadOnlySystemPath)
    ));
    assert!(matches!(
        data.remove_file("/usr/existing.txt"),
        Err(FsError::ReadOnlySystemPath)
    ));
    assert!(matches!(
        data.remove_directory("/usr/empty-dir"),
        Err(FsError::ReadOnlySystemPath)
    ));
    assert_eq!(
        fs::read(root.join("usr/existing.txt")).unwrap(),
        b"original"
    );
    assert!(!root.join("usr/new.txt").exists());
    data.create_file("/home/allowed.txt").unwrap();
    data.create_file("/tmp/allowed.txt").unwrap();
}

#[test]
fn removing_the_write_gate_makes_the_counterexample_succeed() {
    let root = fixture();
    let mut data = DataRoot::open(&root).unwrap();
    assert!(matches!(
        data.create_file("/usr/counter.txt"),
        Err(FsError::ReadOnlySystemPath)
    ));
    data.bypass_write_policy = true;
    data.create_file("/usr/counter.txt").unwrap();
    assert!(root.join("usr/counter.txt").is_file());
}

#[test]
fn unclassified_paths_and_windows_aliases_do_not_become_writable() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    assert!(matches!(
        data.create_file("/orphan.txt"),
        Err(FsError::UnclassifiedWritePath)
    ));
    assert!(!root.join("orphan.txt").exists());
    #[cfg(windows)]
    assert!(matches!(
        data.create_file("/USR/alias.txt"),
        Err(FsError::ReadOnlySystemPath)
    ));
}
