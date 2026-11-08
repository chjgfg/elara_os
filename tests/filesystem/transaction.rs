use super::fixture;
use crate::filesystem::transaction::{store_path, PAYLOAD, TX_OWNER};
use crate::filesystem::types::TransactionFault;
use crate::filesystem::{CleanupStatus, DataRoot, DirectoryDurability, FsError, RecoveryOutcome};
use std::{assert, assert_eq, fs, matches};

#[test]
fn replacement_commits_new_bytes_without_user_directory_staging() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    data.create_file("/home/value.txt").unwrap();
    let receipt = data
        .replace_file("/home/value.txt", b"complete-new-value")
        .unwrap();
    assert_eq!(receipt.bytes, 18);
    assert_eq!(receipt.cleanup, CleanupStatus::Complete);
    assert!(matches!(
        receipt.directory_durability,
        DirectoryDurability::Synced | DirectoryDurability::Unconfirmed(_)
    ));
    assert_eq!(
        data.read_file("/home/value.txt", 64).unwrap(),
        b"complete-new-value"
    );
    assert_eq!(fs::read_dir(root.join("home")).unwrap().count(), 1);
    assert_eq!(
        data.recover_transactions().unwrap(),
        RecoveryOutcome::Inspected {
            discarded: 0,
            preserved: 0
        }
    );
}

#[test]
fn partial_staging_failure_preserves_the_old_file_and_removes_owned_staging() {
    let root = fixture();
    let mut data = DataRoot::open(&root).unwrap();
    fs::write(root.join("home/value.txt"), b"old-complete").unwrap();
    data.transaction_fault = TransactionFault::PartialStage;
    assert!(data
        .replace_file("/home/value.txt", b"new-complete")
        .is_err());
    assert_eq!(
        fs::read(root.join("home/value.txt")).unwrap(),
        b"old-complete"
    );
    assert_eq!(
        data.recover_transactions().unwrap(),
        RecoveryOutcome::Inspected {
            discarded: 0,
            preserved: 0
        }
    );
}

#[test]
fn unsupported_atomic_rename_has_no_copy_fallback() {
    let root = fixture();
    let mut data = DataRoot::open(&root).unwrap();
    fs::write(root.join("home/value.txt"), b"original").unwrap();
    data.transaction_fault = TransactionFault::RenameUnsupported;
    let error = data
        .replace_file("/home/value.txt", b"replacement")
        .unwrap_err();
    assert!(
        matches!(error, FsError::AtomicReplaceUnavailable(ref e) if e.kind() == std::io::ErrorKind::CrossesDevices)
    );
    assert_eq!(fs::read(root.join("home/value.txt")).unwrap(), b"original");
}

#[test]
fn transactions_refuse_read_only_paths_and_oversized_payloads_before_mutation() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    fs::write(root.join("usr/value.txt"), b"original").unwrap();
    assert!(matches!(
        data.replace_file("/usr/value.txt", b"new"),
        Err(FsError::ReadOnlySystemPath)
    ));
    let oversized = std::vec![b'x'; crate::config::ROOTFS_DEFAULTS.max_transaction_bytes + 1];
    fs::write(root.join("home/value.txt"), b"old").unwrap();
    assert!(matches!(
        data.replace_file("/home/value.txt", &oversized),
        Err(FsError::TooLarge(_))
    ));
    assert!(!root.join(store_path()).exists());
}

#[test]
fn concurrent_replacements_are_serialized_and_recovery_does_not_remove_active_data() {
    let root = fixture();
    let data = std::sync::Arc::new(DataRoot::open(&root).unwrap());
    data.create_file("/home/value.txt").unwrap();
    data.write_file("/home/value.txt", b"warm-store").unwrap();
    let mut threads = Vec::new();
    for byte in [b'A', b'B'] {
        let data = data.clone();
        threads.push(std::thread::spawn(move || {
            for _ in 0..5 {
                data.replace_file("/home/value.txt", &[byte; 4096]).unwrap();
            }
        }));
    }
    let recovery = {
        let data = data.clone();
        std::thread::spawn(move || {
            for _ in 0..5 {
                data.recover_transactions().unwrap();
            }
        })
    };
    for thread in threads {
        thread.join().unwrap();
    }
    recovery.join().unwrap();
    let bytes = data.read_file("/home/value.txt", 8192).unwrap();
    assert_eq!(bytes.len(), 4096);
    assert!(bytes.iter().all(|byte| *byte == b'A') || bytes.iter().all(|byte| *byte == b'B'));
}

#[test]
fn failure_without_a_valid_transaction_owner_is_preserved_for_inspection() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    data.create_file("/home/value.txt").unwrap();
    data.write_file("/home/value.txt", b"old").unwrap();
    let unknown = root.join(store_path()).join("txn-999999-99");
    fs::create_dir(&unknown).unwrap();
    fs::write(unknown.join(TX_OWNER), b"not-our-transaction").unwrap();
    fs::write(unknown.join(PAYLOAD), b"keep-me").unwrap();
    assert_eq!(
        data.recover_transactions().unwrap(),
        RecoveryOutcome::Inspected {
            discarded: 0,
            preserved: 1
        }
    );
    assert_eq!(fs::read(unknown.join(PAYLOAD)).unwrap(), b"keep-me");
}
