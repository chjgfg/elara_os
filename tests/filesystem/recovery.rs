use super::fixture;
use crate::config::ROOTFS_DEFAULTS;
use crate::filesystem::transaction::{store_path, PAYLOAD, TX_MAGIC, TX_OWNER};
use crate::filesystem::{DataRoot, FsError, RecoveryOutcome};
use std::{assert, assert_eq, fs, matches};

#[test]
fn recovery_discards_only_owned_staging_and_never_replays_destination_changes() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    data.create_file("/home/value.txt").unwrap();
    data.write_file("/home/value.txt", b"committed-value")
        .unwrap();
    let abandoned = root.join(store_path()).join("txn-999999-1");
    fs::create_dir(&abandoned).unwrap();
    fs::write(abandoned.join(TX_OWNER), TX_MAGIC).unwrap();
    fs::write(abandoned.join(PAYLOAD), b"uncommitted-value").unwrap();
    fs::write(root.join("tmp/user-owned.txt"), b"do-not-delete").unwrap();
    assert_eq!(
        data.recover_transactions().unwrap(),
        RecoveryOutcome::Inspected {
            discarded: 1,
            preserved: 0
        }
    );
    assert!(!abandoned.exists());
    assert_eq!(
        fs::read(root.join("home/value.txt")).unwrap(),
        b"committed-value"
    );
    assert_eq!(
        fs::read(root.join("tmp/user-owned.txt")).unwrap(),
        b"do-not-delete"
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
fn startup_recovery_is_active_and_not_merely_an_unused_api() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    data.create_file("/home/value.txt").unwrap();
    data.write_file("/home/value.txt", b"old").unwrap();
    let abandoned = root.join(store_path()).join("txn-999999-2");
    fs::create_dir(&abandoned).unwrap();
    fs::write(abandoned.join(TX_OWNER), TX_MAGIC).unwrap();
    fs::write(abandoned.join(PAYLOAD), b"new").unwrap();
    assert!(abandoned.exists());
    crate::initialization::prepare(&root).unwrap();
    assert!(
        !abandoned.exists(),
        "the initialization path must invoke recovery"
    );
    assert_eq!(fs::read(root.join("home/value.txt")).unwrap(), b"old");
}

#[test]
fn unmarked_store_and_unknown_transaction_contents_are_not_deleted() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    let store = root.join(store_path());
    fs::create_dir(&store).unwrap();
    fs::write(store.join("keep.txt"), b"outside-ownership-protocol").unwrap();
    assert!(matches!(
        data.recover_transactions(),
        Err(FsError::UnownedTransactionStore)
    ));
    assert_eq!(
        fs::read(store.join("keep.txt")).unwrap(),
        b"outside-ownership-protocol"
    );
}

#[test]
fn transaction_namespace_is_reserved_from_normal_virtual_paths() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    let path = std::format!("/tmp/{}", ROOTFS_DEFAULTS.transaction_directory);
    assert!(matches!(
        data.create_directory(&path),
        Err(FsError::InvalidPath)
    ));
    assert!(matches!(data.list(&path), Err(FsError::InvalidPath)));
    assert_eq!(
        data.recover_transactions().unwrap(),
        RecoveryOutcome::NoStore
    );
}
