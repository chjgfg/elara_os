use super::fixture;
use crate::filesystem::{
    DataRoot, FileStat, FsError, ObjectKind, PhysicalUsage, Timestamp, Usage, WalkEntry,
    WalkOptions,
};
use std::time::SystemTime;
use std::{assert, assert_eq, fs, matches};

#[test]
fn usage_counts_logical_files_and_directories_and_marks_physical_usage_unknown() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    fs::create_dir(root.join("home/tree")).unwrap();
    fs::write(root.join("home/a"), b"abc").unwrap();
    fs::write(root.join("home/tree/b"), b"de").unwrap();
    let usage = data.usage("/home", WalkOptions::default()).unwrap();
    assert_eq!(usage.files, 2);
    assert_eq!(usage.directories, 2);
    assert_eq!(usage.logical_file_bytes, 5);
    assert_eq!(usage.blocked_links, 0);
    assert_eq!(
        usage.physical_usage,
        PhysicalUsage::UnavailableOnHostBackend
    );
    assert!(!root.join("tmp/.elara-transactions").exists());
    assert!(matches!(
        data.usage("/home", WalkOptions::new(1, 64).unwrap()),
        Err(FsError::WalkLimitExceeded)
    ));
}
#[test]
fn usage_overflow_is_an_error_instead_of_a_wrapped_disk_report() {
    let stat = FileStat {
        kind: ObjectKind::File,
        length: u64::MAX,
        created: Timestamp::Available(SystemTime::UNIX_EPOCH),
        modified: Timestamp::Available(SystemTime::UNIX_EPOCH),
        accessed: Timestamp::Available(SystemTime::UNIX_EPOCH),
        host_readonly: false,
    };
    let entries = [
        WalkEntry::Accessible {
            path: "/home/a".into(),
            stat: stat.clone(),
            depth: 1,
        },
        WalkEntry::Accessible {
            path: "/home/b".into(),
            stat: FileStat { length: 1, ..stat },
            depth: 1,
        },
    ];
    assert!(matches!(
        Usage::from_entries(&entries),
        Err(FsError::UsageOverflow)
    ));
}
