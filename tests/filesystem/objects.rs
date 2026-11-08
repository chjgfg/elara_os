use super::fixture;
use crate::config::ROOTFS_DEFAULTS;
use crate::filesystem::{
    DataRoot, FileAccess, FileBehavior, FileCreation, FileMode, FilePosition, FsError,
};
use std::io::SeekFrom;
use std::sync::Arc;
use std::{assert, assert_eq, fs, matches, vec};

#[test]
fn shared_descriptions_share_position_but_reopens_do_not() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    fs::write(root.join("home/value"), b"abcdef").unwrap();
    let first = data.open_file("/home/value", FileMode::READ).unwrap();
    let clone = first.clone();
    let separate = data.open_file("/home/value", FileMode::READ).unwrap();
    assert_eq!(first.read(2).unwrap(), b"ab");
    assert_eq!(clone.read(2).unwrap(), b"cd");
    assert_eq!(first.info().unwrap().position, FilePosition::Known(4));
    assert_eq!(separate.read(2).unwrap(), b"ab");
    assert_eq!(first.read(8).unwrap(), b"ef");
    assert_eq!(first.read(8).unwrap(), b"");
}
#[test]
fn positioned_io_does_not_change_the_shared_logical_position() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    let file = data.open_file("/home/value", FileMode::CREATE).unwrap();
    assert_eq!(file.write(b"abcdef").unwrap(), 6);
    assert_eq!(file.seek(SeekFrom::Start(2)).unwrap(), 2);
    assert_eq!(file.write_at(4, b"XY").unwrap(), 2);
    assert_eq!(file.read_at(0, 8).unwrap(), b"abcdXY");
    assert_eq!(file.info().unwrap().position, FilePosition::Known(2));
    assert_eq!(file.read(2).unwrap(), b"cd");
    assert_eq!(file.write(b"!").unwrap(), 1);
    assert_eq!(data.read_file("/home/value", 16).unwrap(), b"abcd!Y");
}
#[test]
fn seek_truncate_extend_and_sparse_write_keep_exact_position() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    let file = data.open_file("/home/value", FileMode::CREATE).unwrap();
    file.write(b"abcdef").unwrap();
    assert_eq!(file.seek(SeekFrom::End(-2)).unwrap(), 4);
    assert_eq!(file.seek(SeekFrom::Current(-3)).unwrap(), 1);
    assert!(matches!(
        file.seek(SeekFrom::Current(-2)),
        Err(FsError::InvalidOffset)
    ));
    assert_eq!(file.info().unwrap().position, FilePosition::Known(1));
    file.truncate(2).unwrap();
    file.truncate(5).unwrap();
    assert_eq!(file.read_at(0, 8).unwrap(), b"ab\0\0\0");
    file.seek(SeekFrom::Start(7)).unwrap();
    file.write(b"z").unwrap();
    assert_eq!(file.read_at(0, 16).unwrap(), b"ab\0\0\0\0\0z");
    file.truncate(1).unwrap();
    assert_eq!(file.info().unwrap().position, FilePosition::Known(8));
    assert_eq!(file.read(1).unwrap(), b"");
}
#[test]
fn readonly_handles_refuse_every_mutation_and_preserve_data() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    fs::write(root.join("home/value"), b"keep").unwrap();
    let file = data.open_file("/home/value", FileMode::READ).unwrap();
    assert!(matches!(
        file.write(b"bad"),
        Err(FsError::HandleWriteDenied)
    ));
    assert!(matches!(
        file.write_at(0, b"bad"),
        Err(FsError::HandleWriteDenied)
    ));
    assert!(matches!(file.truncate(0), Err(FsError::HandleWriteDenied)));
    assert!(matches!(file.sync_data(), Err(FsError::HandleWriteDenied)));
    assert!(matches!(file.sync_all(), Err(FsError::HandleWriteDenied)));
    assert_eq!(file.info().unwrap().position, FilePosition::Known(0));
    assert_eq!(fs::read(root.join("home/value")).unwrap(), b"keep");
}
#[test]
fn append_goes_to_eof_even_after_seek_and_refuses_positioned_writes() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    fs::write(root.join("home/value"), b"abc").unwrap();
    let mode = FileMode::new(
        FileAccess::ReadWrite,
        FileCreation::Existing,
        FileBehavior::Append,
    )
    .unwrap();
    let file = data.open_file("/home/value", mode).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    assert_eq!(file.write(b"d").unwrap(), 1);
    assert_eq!(file.info().unwrap().position, FilePosition::Known(4));
    assert!(matches!(
        file.write_at(0, b"bad"),
        Err(FsError::AppendPositionedWrite)
    ));
    assert_eq!(file.read_at(0, 16).unwrap(), b"abcd");
    assert_eq!(file.info().unwrap().position, FilePosition::Known(4));
}
#[test]
fn append_followup_failure_reports_written_bytes_and_blocks_unknown_relative_io() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    let mode = FileMode::new(
        FileAccess::ReadWrite,
        FileCreation::Exclusive,
        FileBehavior::Append,
    )
    .unwrap();
    let file = data.open_file("/home/value", mode).unwrap();
    file.fail_append_position();
    assert!(matches!(
        file.write(b"saved"),
        Err(FsError::WritePositionUnconfirmed { bytes: 5, .. })
    ));
    assert_eq!(file.info().unwrap().position, FilePosition::Unconfirmed);
    assert_eq!(file.read_at(0, 16).unwrap(), b"saved");
    assert!(matches!(file.read(1), Err(FsError::CursorUnconfirmed)));
    assert!(matches!(
        file.seek(SeekFrom::Current(0)),
        Err(FsError::CursorUnconfirmed)
    ));
    assert_eq!(file.seek(SeekFrom::Start(0)).unwrap(), 0);
    assert_eq!(file.read(8).unwrap(), b"saved");
}
#[test]
fn oversized_requests_and_overflow_are_rejected_before_io() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    let file = data.open_file("/home/value", FileMode::CREATE).unwrap();
    file.write(b"keep").unwrap();
    assert!(matches!(
        file.read(ROOTFS_DEFAULTS.max_file_io_bytes + 1),
        Err(FsError::IoRequestTooLarge(_))
    ));
    assert!(matches!(
        file.write(&vec![b'x'; ROOTFS_DEFAULTS.max_file_io_bytes + 1]),
        Err(FsError::IoRequestTooLarge(_))
    ));
    assert!(matches!(
        file.read_at(u64::MAX, 1),
        Err(FsError::InvalidOffset)
    ));
    assert!(matches!(
        file.write_at(u64::MAX, b"x"),
        Err(FsError::InvalidOffset)
    ));
    assert_eq!(file.info().unwrap().position, FilePosition::Known(4));
    assert_eq!(file.read_at(0, 16).unwrap(), b"keep");
}
#[test]
fn sync_interfaces_and_handle_identity_survive_atomic_path_replacement() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    fs::write(root.join("home/value"), b"old").unwrap();
    let mode = FileMode::new(
        FileAccess::ReadWrite,
        FileCreation::Existing,
        FileBehavior::Preserve,
    )
    .unwrap();
    let file = data.open_file("/home/value", mode).unwrap();
    file.sync_data().unwrap();
    file.sync_all().unwrap();
    data.replace_file("/home/value", b"new").unwrap();
    assert_eq!(file.read(16).unwrap(), b"old");
    assert_eq!(
        data.open_file("/home/value", FileMode::READ)
            .unwrap()
            .read(16)
            .unwrap(),
        b"new"
    );
}
#[test]
fn concurrent_clones_never_read_the_same_logical_byte_twice() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    fs::write(root.join("home/value"), (0u8..200).collect::<Vec<_>>()).unwrap();
    let file = Arc::new(data.open_file("/home/value", FileMode::READ).unwrap());
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let file = Arc::clone(&file);
            std::thread::spawn(move || {
                let mut seen = Vec::new();
                loop {
                    let bytes = file.read(1).unwrap();
                    if bytes.is_empty() {
                        break;
                    }
                    seen.extend(bytes);
                }
                seen
            })
        })
        .collect();
    let mut seen: Vec<u8> = workers
        .into_iter()
        .flat_map(|worker| worker.join().unwrap())
        .collect();
    seen.sort_unstable();
    assert_eq!(seen, (0u8..200).collect::<Vec<_>>());
    assert_eq!(file.info().unwrap().position, FilePosition::Known(200));
}
