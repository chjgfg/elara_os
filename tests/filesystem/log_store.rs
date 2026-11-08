//! Managed log protocol tests, including pre/post-commit fault contrasts.
use super::fixture;
use crate::filesystem::log_store::LogFault;
use crate::filesystem::{CleanupStatus, DataRoot, FsError, LogMode, LogPolicy, MirrorStatus};
use crate::logging::Logger;
use std::{assert, assert_eq, format, fs, matches, thread};

fn setup() -> (std::path::PathBuf, DataRoot, LogPolicy) {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let policy = LogPolicy::new(128, 2).unwrap();
    root.initialize_log(policy).unwrap();
    (path, root, policy)
}

#[test]
fn protocol_publication_rejects_a_guard_from_another_root() {
    let (_, left, _) = setup();
    let (_, right, _) = setup();
    let guard = left.namespace_guard().unwrap();
    let error = right.publish_protocol_locked(
        std::path::Path::new("home/wrong-lock"),
        b"bad",
        true,
        &guard,
    );
    assert!(matches!(error, Err(FsError::InvalidRoot)));
    assert!(!right.root().join("home/wrong-lock").exists());
}

#[test]
fn rotation_bounds_retained_generations_and_preserves_order() {
    let (_, root, policy) = setup();
    let mut rotations = 0;
    for number in 0..100 {
        let receipt = root
            .append_log(
                policy,
                format!("entry-{number:03}-xxxxxxxxxxxxxxxx\n").as_bytes(),
            )
            .unwrap();
        rotations += usize::from(receipt.rotated);
        assert_eq!(receipt.mirror, MirrorStatus::Current);
        assert_eq!(receipt.cleanup, CleanupStatus::Complete);
        let stats = root.log_stats(policy).unwrap();
        assert!(stats.archives <= 2);
        assert!(stats.active_bytes <= 128);
        assert!(stats.retained_body_bytes <= 3 * 128);
        assert_eq!(stats.peak_body_bound, 5 * 128);
        assert_eq!(stats.peak_named_bytes_bound, policy.peak_named_bytes());
    }
    assert!(rotations > 10);
    let tail = root.log_tail(policy, 3).unwrap();
    assert_eq!(
        tail,
        [
            "entry-097-xxxxxxxxxxxxxxxx",
            "entry-098-xxxxxxxxxxxxxxxx",
            "entry-099-xxxxxxxxxxxxxxxx"
        ]
    );
    assert_eq!(root.log_stats(policy).unwrap().mode, LogMode::Managed);
}

#[test]
fn precommit_fault_keeps_old_records_and_recovery_is_idempotent() {
    let (_, root, policy) = setup();
    root.append_log(policy, b"first\n").unwrap();
    assert!(root
        .append_log_with_fault(policy, b"not-committed\n", LogFault::AfterGeneration)
        .is_err());
    assert_eq!(root.log_tail(policy, 10).unwrap(), ["first"]);
    root.initialize_log(policy).unwrap();
    root.initialize_log(policy).unwrap();
    assert_eq!(root.log_tail(policy, 10).unwrap(), ["first"]);
    root.append_log(policy, b"next\n").unwrap();
    assert_eq!(root.log_tail(policy, 10).unwrap(), ["first", "next"]);
}

#[test]
fn postcommit_fault_reports_committed_and_repairs_compatibility_view() {
    let (path, root, policy) = setup();
    root.append_log(policy, b"first\n").unwrap();
    let receipt = root
        .append_log_with_fault(policy, b"second\n", LogFault::AfterManifest)
        .unwrap();
    assert!(matches!(receipt.mirror, MirrorStatus::Deferred(_)));
    assert_eq!(root.log_tail(policy, 10).unwrap(), ["first", "second"]);
    assert_eq!(
        fs::read(path.join("var/log/elara.log")).unwrap(),
        b"first\n"
    );
    root.initialize_log(policy).unwrap();
    root.initialize_log(policy).unwrap();
    assert_eq!(
        fs::read(path.join("var/log/elara.log")).unwrap(),
        b"second\n"
    );
    assert_eq!(root.log_tail(policy, 10).unwrap(), ["first", "second"]);
}

#[test]
fn cleanup_fault_keeps_committed_records_and_recovery_retires_only_owned_generations() {
    let (path, root, policy) = setup();
    root.append_log(policy, b"first\n").unwrap();
    let receipt = root
        .append_log_with_fault(policy, b"second\n", LogFault::BeforeCleanup)
        .unwrap();
    assert!(matches!(receipt.cleanup, CleanupStatus::Deferred(_)));
    fs::write(
        path.join("var/log/.elara-logs/generation-999"),
        b"unowned content",
    )
    .unwrap();
    fs::write(path.join("var/log/.elara-logs/unrelated"), b"keep me").unwrap();
    root.initialize_log(policy).unwrap();
    assert_eq!(root.log_tail(policy, 10).unwrap(), ["first", "second"]);
    assert_eq!(
        fs::read(path.join("var/log/.elara-logs/generation-999")).unwrap(),
        b"unowned content"
    );
    assert_eq!(
        fs::read(path.join("var/log/.elara-logs/unrelated")).unwrap(),
        b"keep me"
    );
}

#[test]
fn removing_hash_guard_accepts_a_same_length_structurally_valid_corruption() {
    let (path, root, policy) = setup();
    root.append_log(policy, b"first\n").unwrap();
    let id = root.log_stats(policy).unwrap().generation;
    let payload = path.join(format!("var/log/.elara-logs/generation-{id}/payload"));
    fs::write(&payload, b"other\n").unwrap();
    assert!(matches!(
        root.log_tail(policy, 10),
        Err(FsError::CorruptLogStore)
    ));
    assert_eq!(
        root.log_active_without_hash_guard(policy).unwrap(),
        b"other\n"
    );
    assert_eq!(fs::read(payload).unwrap(), b"other\n");
}

#[test]
fn failed_rotation_never_prunes_archives_referenced_by_the_old_manifest() {
    let (_, root, policy) = setup();
    for i in 0..20 {
        root.append_log(
            policy,
            format!("record-{i:02}-xxxxxxxxxxxxxxxxxxxx\n").as_bytes(),
        )
        .unwrap();
    }
    assert_eq!(root.log_stats(policy).unwrap().archives, 2);
    let before = root.log_tail(policy, 100).unwrap();
    assert!(root
        .append_log_with_fault(policy, b"unpublished\n", LogFault::AfterGeneration)
        .is_err());
    assert_eq!(root.log_tail(policy, 100).unwrap(), before);
    root.initialize_log(policy).unwrap();
    assert_eq!(root.log_tail(policy, 100).unwrap(), before);
}

#[test]
fn malformed_active_generation_is_not_silently_overwritten() {
    let (path, root, policy) = setup();
    root.append_log(policy, b"first\n").unwrap();
    let generation = root.log_stats(policy).unwrap().generation;
    let target = path.join(format!(
        "var/log/.elara-logs/generation-{generation}/payload"
    ));
    fs::write(&target, b"unknown data").unwrap();
    assert!(matches!(
        root.append_log(policy, b"next\n"),
        Err(FsError::CorruptLogStore)
    ));
    assert_eq!(fs::read(target).unwrap(), b"unknown data");
}

#[test]
fn capability_cache_does_not_hide_a_corrupted_store_owner_marker() {
    let (path, root, policy) = setup();
    root.append_log(policy, b"trusted\n").unwrap();
    fs::write(
        path.join("var/log/.elara-logs/store.owner"),
        b"unknown owner",
    )
    .unwrap();
    assert!(matches!(
        root.append_log(policy, b"refused\n"),
        Err(FsError::UnownedLogStore)
    ));
    assert_eq!(
        fs::read(path.join("var/log/.elara-logs/store.owner")).unwrap(),
        b"unknown owner"
    );
}

#[test]
fn noncanonical_generation_directory_names_are_preserved_even_when_empty() {
    let (path, root, policy) = setup();
    root.append_log(policy, b"trusted\n").unwrap();
    let next = root.log_stats(policy).unwrap().generation + 1;
    assert!(root
        .append_log_with_fault(policy, b"unpublished\n", LogFault::AfterGeneration)
        .is_err());
    let alias = path.join(format!("var/log/.elara-logs/generation-{next:04}"));
    fs::create_dir(&alias).unwrap();
    root.initialize_log(policy).unwrap();
    assert!(alias.is_dir());
    assert_eq!(root.log_tail(policy, 10).unwrap(), ["trusted"]);
}

#[test]
fn partially_prepared_generation_is_staged_only_in_tmp_and_recovered_by_its_v2_owner() {
    let (path, root, policy) = setup();
    root.append_log(policy, b"trusted\n").unwrap();
    let next = root.log_stats(policy).unwrap().generation + 1;
    assert!(root
        .append_log_with_fault(policy, b"not-published\n", LogFault::PartialGeneration)
        .is_err());
    assert!(!path
        .join(format!("var/log/.elara-logs/generation-{next}"))
        .exists());
    let staged = root
        .dir
        .read_dir(crate::filesystem::transaction::store_path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_str().is_some_and(|name| name.starts_with("txn-")))
        .count();
    assert!(staged > 0);
    assert_eq!(root.log_tail(policy, 10).unwrap(), ["trusted"]);
    root.initialize_log(policy).unwrap();
    let staged = root
        .dir
        .read_dir(crate::filesystem::transaction::store_path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_str().is_some_and(|name| name.starts_with("txn-")))
        .count();
    assert_eq!(staged, 0);
    assert_eq!(root.log_tail(policy, 10).unwrap(), ["trusted"]);
}

#[test]
fn unknown_store_is_never_claimed_or_modified() {
    let path = fixture();
    fs::create_dir(path.join("var/log/.elara-logs")).unwrap();
    fs::write(path.join("var/log/.elara-logs/keep"), b"unknown").unwrap();
    let root = DataRoot::open(&path).unwrap();
    assert!(matches!(Logger::open(&root), Err(FsError::UnownedLogStore)));
    assert_eq!(
        fs::read(path.join("var/log/.elara-logs/keep")).unwrap(),
        b"unknown"
    );
    assert!(!path.join("var/log/.elara-logs/store.owner").exists());
}

#[test]
fn unowned_large_legacy_log_remains_readonly_but_has_a_bounded_tail() {
    let path = fixture();
    let mut bytes = b"old record\n".repeat(200_000);
    bytes.extend_from_slice("last 星澜\n".as_bytes());
    fs::write(path.join("var/log/elara.log"), &bytes).unwrap();
    let root = DataRoot::open(&path).unwrap();
    let logger = Logger::open(&root).unwrap();
    assert_eq!(logger.stats(&root).unwrap().mode, LogMode::LegacyReadOnly);
    assert_eq!(logger.stats(&root).unwrap().active_bytes, bytes.len());
    assert_eq!(logger.stats(&root).unwrap().peak_named_bytes_bound, 0);
    assert!(matches!(
        logger.record(&root, "root", "refused"),
        Err(FsError::LegacyLogReadOnly)
    ));
    assert_eq!(logger.tail(&root, 1).unwrap(), ["last 星澜"]);
    assert_eq!(fs::read(path.join("var/log/elara.log")).unwrap(), bytes);
    assert!(!path.join("var/log/.elara-logs").exists());
}

#[test]
fn private_store_and_managed_view_cannot_be_written_through_ordinary_apis() {
    let (_, root, policy) = setup();
    assert!(matches!(
        root.read_file("/var/log/.elara-logs/manifest", 1024),
        Err(FsError::InvalidPath)
    ));
    assert!(matches!(
        root.append_file("/var/log/elara.log", b"attack"),
        Err(FsError::ServiceOwnedPath)
    ));
    assert!(matches!(
        root.write_file("/var/log/elara.log", b"attack"),
        Err(FsError::ServiceOwnedPath)
    ));
    assert!(matches!(
        root.remove_file("/var/log/elara.log"),
        Err(FsError::ServiceOwnedPath)
    ));
    assert!(root
        .list("/var/log")
        .unwrap()
        .iter()
        .all(|entry| entry.name != ".elara-logs"));
    assert!(root.log_tail(policy, 10).unwrap().is_empty());
}

#[test]
fn removing_service_write_guard_allows_the_counterexample() {
    let (path, mut root, policy) = setup();
    root.append_log(policy, b"trusted\n").unwrap();
    assert!(matches!(
        root.append_file("/var/log/elara.log", b"forged\n"),
        Err(FsError::ServiceOwnedPath)
    ));
    root.bypass_write_policy = true;
    root.append_file("/var/log/elara.log", b"forged\n").unwrap();
    assert_eq!(
        fs::read(path.join("var/log/elara.log")).unwrap(),
        b"trusted\nforged\n"
    );
    // The compatibility view is not the authority; the committed generation is.
    assert_eq!(root.log_tail(policy, 10).unwrap(), ["trusted"]);
}

#[test]
fn independent_loggers_serialize_updates_without_losing_records() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    Logger::open(&root).unwrap();
    let mut children = Vec::new();
    for worker in 0..3 {
        let path = path.clone();
        children.push(thread::spawn(move || {
            let root = DataRoot::open(&path).unwrap();
            let logger = Logger::open(&root).unwrap();
            for number in 0..10 {
                logger
                    .record(&root, "root", &format!("worker-{worker}-{number}"))
                    .unwrap();
            }
        }));
    }
    for child in children {
        child.join().unwrap();
    }
    let logger = Logger::open(&root).unwrap();
    let tail = logger.tail(&root, 100).unwrap();
    assert_eq!(tail.len(), 30);
    for worker in 0..3 {
        for number in 0..10 {
            assert_eq!(
                tail.iter()
                    .filter(|line| line.ends_with(&format!("worker-{worker}-{number}")))
                    .count(),
                1
            );
        }
    }
}

#[test]
fn partial_append_is_not_a_committed_record_and_recovery_trims_only_verified_suffix() {
    let (path, root, policy) = setup();
    root.append_log(policy, b"first\n").unwrap();
    let generation = root.log_stats(policy).unwrap().generation;
    let error = root.append_log_with_fault(policy, b"second-record\n", LogFault::PartialAppend);
    assert!(matches!(error, Err(FsError::LogAppendUnconfirmed { .. })));
    assert_eq!(root.log_tail(policy, 10).unwrap(), ["first"]);
    let payload = path.join(format!(
        "var/log/.elara-logs/generation-{generation}/payload"
    ));
    assert!(fs::read(&payload).unwrap().len() > b"first\n".len());
    root.initialize_log(policy).unwrap();
    root.initialize_log(policy).unwrap();
    assert_eq!(fs::read(payload).unwrap(), b"first\n");
    root.append_log(policy, b"after-recovery\n").unwrap();
    assert_eq!(
        root.log_tail(policy, 10).unwrap(),
        ["first", "after-recovery"]
    );
}

#[test]
fn buffered_append_explicitly_does_not_promise_fsync() {
    let (_, root, policy) = setup();
    let receipt = root.append_log(policy, b"one\n").unwrap();
    assert_eq!(
        receipt.data_durability,
        crate::filesystem::LogDataDurability::HostBuffered
    );
    assert!(!receipt.rotated);
    assert!(matches!(
        receipt.directory_durability,
        crate::filesystem::DirectoryDurability::Unconfirmed(_)
    ));
}

#[test]
fn oversized_records_and_policy_changes_are_rejected_before_publication() {
    let (_, root, policy) = setup();
    let before = root.log_stats(policy).unwrap();
    assert!(matches!(
        root.append_log(policy, &b"x".repeat(129)),
        Err(FsError::LogRecordTooLarge)
    ));
    assert!(matches!(
        root.append_log(policy, b"two\nlines\n"),
        Err(FsError::CorruptLogStore)
    ));
    assert_eq!(root.log_stats(policy).unwrap(), before);
    assert!(matches!(
        root.initialize_log(LogPolicy::default()),
        Err(FsError::InvalidLogPolicy)
    ));
}
