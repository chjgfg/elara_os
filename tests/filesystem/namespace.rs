use super::fixture;
use crate::config::ROOTFS_DEFAULTS;
use crate::filesystem::types::TransactionFault;
use crate::filesystem::{DataRoot, FileMode, FsError};
use std::sync::{Arc, Barrier};
use std::{assert, assert_eq, fs, matches, vec};

#[test]
fn rename_moves_files_and_trees_but_open_handles_keep_their_identity() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    data.create_directory("/home/tree").unwrap();
    data.create_file("/home/tree/a").unwrap();
    data.write_file("/home/tree/a", b"keep").unwrap();
    let file = data.open_file("/home/tree/a", FileMode::READ).unwrap();
    data.rename("/home/tree/a", "/home/tree/b").unwrap();
    assert_eq!(file.read(16).unwrap(), b"keep");
    assert_eq!(file.info().unwrap().opened_path, "/home/tree/a");
    drop(file);
    data.rename("/home/tree", "/tmp/tree").unwrap();
    assert!(!root.join("home/tree").exists());
    assert_eq!(data.read_file("/tmp/tree/b", 16).unwrap(), b"keep");
}
#[cfg(windows)]
#[test]
fn moving_a_tree_with_an_open_descendant_does_not_fall_back_to_copy_delete() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    data.create_directory("/home/tree").unwrap();
    data.create_file("/home/tree/value").unwrap();
    data.write_file("/home/tree/value", b"keep").unwrap();
    let file = data.open_file("/home/tree/value", FileMode::READ).unwrap();
    assert!(matches!(
        data.rename("/home/tree", "/tmp/tree"),
        Err(FsError::AtomicRenameUnavailable(_))
    ));
    assert!(!root.join("tmp/tree").exists());
    assert_eq!(data.read_file("/home/tree/value", 16).unwrap(), b"keep");
    assert_eq!(file.read(16).unwrap(), b"keep");
    drop(file);
    data.rename("/home/tree", "/tmp/tree").unwrap();
    assert!(!root.join("home/tree").exists());
    assert_eq!(data.read_file("/tmp/tree/value", 16).unwrap(), b"keep");
}

#[test]
fn rename_refuses_overwrite_protected_system_and_descendant_destinations() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    fs::write(root.join("home/a"), b"A").unwrap();
    fs::write(root.join("home/b"), b"B").unwrap();
    fs::write(root.join("usr/system"), b"system").unwrap();
    assert!(matches!(
        data.rename("/home/a", "/home/b"),
        Err(FsError::AlreadyExists)
    ));
    assert!(matches!(
        data.rename("/home/a", "/usr/new"),
        Err(FsError::ReadOnlySystemPath)
    ));
    assert!(matches!(
        data.rename("/usr/system", "/home/new"),
        Err(FsError::ReadOnlySystemPath)
    ));
    assert!(matches!(
        data.rename("/home", "/tmp/home"),
        Err(FsError::ProtectedPath)
    ));
    data.create_directory("/home/tree").unwrap();
    assert!(matches!(
        data.rename("/home/tree", "/home/tree/child"),
        Err(FsError::InvalidPath)
    ));
    #[cfg(windows)]
    assert!(matches!(data.rename("/home/tree", "/HOME/TREE/child"), Err(FsError::InvalidPath)));
    assert_eq!(data.read_file("/home/a", 8).unwrap(), b"A");
    assert_eq!(data.read_file("/home/b", 8).unwrap(), b"B");
}
#[test]
fn copy_publishes_an_independent_new_file_and_never_overwrites_targets() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    fs::write(root.join("usr/source"), b"source").unwrap();
    let receipt = data.copy_file("/usr/source", "/home/copy").unwrap();
    assert_eq!(receipt.bytes, 6);
    assert_eq!(data.read_file("/home/copy", 16).unwrap(), b"source");
    assert!(matches!(
        data.copy_file("/usr/source", "/home/copy"),
        Err(FsError::AlreadyExists)
    ));
    data.write_file("/home/copy", b"changed").unwrap();
    assert_eq!(data.read_file("/usr/source", 16).unwrap(), b"source");
    assert!(matches!(
        data.copy_file("/usr/source", "/usr/new"),
        Err(FsError::ReadOnlySystemPath)
    ));
    assert!(matches!(
        data.copy_file("/home", "/home/bad"),
        Err(FsError::NotFile)
    ));
    assert!(!root.join("home/bad").exists());
}
#[test]
fn copy_failure_never_publishes_partial_payload_or_copies_as_fallback() {
    let root = fixture();
    let mut data = DataRoot::open(&root).unwrap();
    fs::write(root.join("home/source"), b"complete-content").unwrap();
    data.transaction_fault = TransactionFault::PartialStage;
    assert!(data.copy_file("/home/source", "/home/partial").is_err());
    assert!(!root.join("home/partial").exists());
    data.transaction_fault = TransactionFault::RenameUnsupported;
    assert!(matches!(
        data.copy_file("/home/source", "/home/no-fallback"),
        Err(FsError::AtomicPublishUnavailable(_))
    ));
    assert!(!root.join("home/no-fallback").exists());
    data.transaction_fault = TransactionFault::None;
    fs::write(
        root.join("home/large"),
        vec![b'x'; ROOTFS_DEFAULTS.max_transaction_bytes + 1],
    )
    .unwrap();
    assert!(matches!(
        data.copy_file("/home/large", "/home/large-copy"),
        Err(FsError::TooLarge(_))
    ));
    assert!(!root.join("home/large-copy").exists());
    assert_eq!(
        data.read_file("/home/source", 32).unwrap(),
        b"complete-content"
    );
}
#[test]
fn competing_renames_cannot_overwrite_another_first_party_destination() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    data.create_file("/home/a").unwrap();
    data.write_file("/home/a", b"A").unwrap();
    data.create_file("/home/b").unwrap();
    data.write_file("/home/b", b"B").unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = ["/home/a", "/home/b"]
        .into_iter()
        .map(|source| {
            let root = root.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let data = DataRoot::open(&root).unwrap();
                barrier.wait();
                data.rename(source, "/home/winner")
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(FsError::AlreadyExists)))
            .count(),
        1
    );
    assert_eq!(
        root.join("home/a").exists() as usize + root.join("home/b").exists() as usize,
        1
    );
    let winner = data.read_file("/home/winner", 8).unwrap();
    assert!(winner == b"A" || winner == b"B");
}
#[test]
fn competing_copies_publish_one_complete_destination_not_mixed_contents() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    data.create_file("/home/a").unwrap();
    data.write_file("/home/a", &[b'A'; 128]).unwrap();
    data.create_file("/home/b").unwrap();
    data.write_file("/home/b", &[b'B'; 128]).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = ["/home/a", "/home/b"]
        .into_iter()
        .map(|source| {
            let root = root.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let data = DataRoot::open(&root).unwrap();
                barrier.wait();
                data.copy_file(source, "/home/winner")
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(FsError::AlreadyExists)))
            .count(),
        1
    );
    let winner = data.read_file("/home/winner", 256).unwrap();
    assert!(winner == [b'A'; 128] || winner == [b'B'; 128]);
}
