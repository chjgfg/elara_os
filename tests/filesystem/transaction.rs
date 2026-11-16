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


#[test]
fn shared_transaction_owner_is_a_real_link_and_recovery_keeps_the_seed() {
    use crate::filesystem::transaction::{self, MarkerPublication, REPLACE_OWNER, TX_MAGIC};
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    let store = transaction::ensure_store(&data).unwrap();
    let _lock = transaction::lock_store(&store).unwrap();
    store.create_dir("txn-800-1").unwrap();
    let first = store.open_dir("txn-800-1").unwrap();
    assert_eq!(transaction::write_transaction_owner(&store, &first).unwrap(), MarkerPublication::Seeded);
    store.create_dir("txn-800-2").unwrap();
    let second = store.open_dir("txn-800-2").unwrap();
    assert_eq!(transaction::write_transaction_owner(&store, &second).unwrap(), MarkerPublication::Shared);
    // Adversarial alias probe: copying identical bytes must not pass this test.
    fs::write(root.join(store_path()).join(REPLACE_OWNER), b"alias-probe").unwrap();
    assert_eq!(fs::read(root.join(store_path()).join("txn-800-2").join(TX_OWNER)).unwrap(), b"alias-probe");
    fs::write(root.join(store_path()).join(REPLACE_OWNER), TX_MAGIC).unwrap();
    drop(first);
    drop(second);
    drop(_lock);
    assert_eq!(data.recover_transactions().unwrap(), RecoveryOutcome::Inspected { discarded: 2, preserved: 0 });
    assert_eq!(fs::read(root.join(store_path()).join(REPLACE_OWNER)).unwrap(), TX_MAGIC);
    assert_eq!(data.recover_transactions().unwrap(), RecoveryOutcome::Inspected { discarded: 0, preserved: 0 });
}

#[test]
fn shared_transaction_owner_fallback_is_explicit_and_independent() {
    use crate::filesystem::transaction::{self, MarkerPublication, REPLACE_OWNER, TX_MAGIC};
    use std::io;
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    let store = transaction::ensure_store(&data).unwrap();
    let _lock = transaction::lock_store(&store).unwrap();
    for (index, seeded) in [false, true].into_iter().enumerate() {
        if seeded { fs::write(root.join(store_path()).join(REPLACE_OWNER), TX_MAGIC).unwrap(); }
        let name = std::format!("txn-801-{index}");
        store.create_dir(&name).unwrap();
        let stage = store.open_dir(&name).unwrap();
        let result = transaction::write_transaction_owner_with(&store, &stage, |_, _, _, _| Err(io::ErrorKind::Unsupported.into())).unwrap();
        assert_eq!(result, MarkerPublication::Independent { link_error: io::ErrorKind::Unsupported });
        assert_eq!(fs::read(root.join(store_path()).join(&name).join(TX_OWNER)).unwrap(), TX_MAGIC);
        if seeded {
            fs::write(root.join(store_path()).join(REPLACE_OWNER), b"not-an-alias").unwrap();
            assert_eq!(fs::read(root.join(store_path()).join(&name).join(TX_OWNER)).unwrap(), TX_MAGIC);
            fs::write(root.join(store_path()).join(REPLACE_OWNER), TX_MAGIC).unwrap();
        } else { assert!(!root.join(store_path()).join(REPLACE_OWNER).exists()); }
    }
}

#[test]
fn shared_transaction_owner_refuses_unknown_seed_before_payload_mutation() {
    use crate::filesystem::transaction::{self, REPLACE_OWNER};
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    let store = transaction::ensure_store(&data).unwrap();
    fs::write(root.join("home/value.txt"), b"old").unwrap();
    let seed = root.join(store_path()).join(REPLACE_OWNER);
    for bytes in [b"".as_slice(), b"unknown-owner"] {
        fs::write(&seed, bytes).unwrap();
        assert!(data.replace_file("/home/value.txt", b"new").is_err());
        assert_eq!(fs::read(root.join("home/value.txt")).unwrap(), b"old");
        assert_eq!(fs::read(&seed).unwrap(), bytes);
        assert!(data.recover_transactions().is_err());
    }
    // A same-named directory is not an immutable marker either.
    fs::remove_file(&seed).unwrap();
    store.create_dir(REPLACE_OWNER).unwrap();
    assert!(data.replace_file("/home/value.txt", b"new").is_err());
    assert!(seed.is_dir());
}

#[test]
fn shared_transaction_owner_never_overwrites_a_preexisting_stage_owner() {
    use crate::filesystem::transaction::{self, REPLACE_OWNER, TX_MAGIC};
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    let store = transaction::ensure_store(&data).unwrap();
    let _lock = transaction::lock_store(&store).unwrap();
    fs::write(root.join(store_path()).join(REPLACE_OWNER), TX_MAGIC).unwrap();
    store.create_dir("txn-802-1").unwrap();
    let stage = store.open_dir("txn-802-1").unwrap();
    fs::write(root.join(store_path()).join("txn-802-1").join(TX_OWNER), b"unknown").unwrap();
    assert!(transaction::write_transaction_owner(&store, &stage).is_err());
    assert_eq!(fs::read(root.join(store_path()).join("txn-802-1").join(TX_OWNER)).unwrap(), b"unknown");
    assert_eq!(fs::read(root.join(store_path()).join(REPLACE_OWNER)).unwrap(), TX_MAGIC);
}
