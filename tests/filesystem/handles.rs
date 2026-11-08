use super::fixture;
use crate::filesystem::{DataRoot, FileMode, FilePosition, FsError, HandleTable};
use std::io::SeekFrom;
use std::{assert, assert_eq, assert_ne, matches};

#[test]
fn closed_and_reused_slots_reject_the_old_generation() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    let mut table = HandleTable::with_limit(1).unwrap();
    let old = table.open(&data, "/home/value", FileMode::CREATE).unwrap();
    table.write(old, b"value").unwrap();
    table.close(old).unwrap();
    assert!(matches!(table.close(old), Err(FsError::InvalidHandle)));
    let new = table.open(&data, "/home/value", FileMode::READ).unwrap();
    assert_eq!(new.slot(), old.slot());
    assert_eq!(
        table.resolve_token(new.slot(), new.generation()).unwrap(),
        new
    );
    assert!(matches!(
        table.resolve_token(old.slot(), old.generation()),
        Err(FsError::InvalidHandle)
    ));
    assert!(matches!(
        table.resolve_token(usize::MAX, 1),
        Err(FsError::InvalidHandle)
    ));
    assert_ne!(new.generation(), old.generation());
    assert!(matches!(table.read(old, 8), Err(FsError::InvalidHandle)));
    assert!(matches!(
        table.write(old, b"bad"),
        Err(FsError::InvalidHandle)
    ));
    assert_eq!(table.read(new, 8).unwrap(), b"value");
}
#[test]
fn tokens_from_other_tables_cannot_alias_identical_slot_and_generation() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    let mut first = HandleTable::new().unwrap();
    let mut second = HandleTable::new().unwrap();
    let a = first.open(&data, "/home/a", FileMode::CREATE).unwrap();
    let b = second.open(&data, "/home/b", FileMode::CREATE).unwrap();
    assert_eq!(a.slot(), b.slot());
    assert_eq!(a.generation(), b.generation());
    assert_ne!(a, b);
    assert!(matches!(second.read(a, 1), Err(FsError::InvalidHandle)));
    assert!(matches!(first.close(b), Err(FsError::InvalidHandle)));
}
#[test]
fn duplicate_and_cross_table_transfer_share_position_not_lifetime() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    let mut first = HandleTable::new().unwrap();
    let mut second = HandleTable::new().unwrap();
    let a = first.open(&data, "/home/a", FileMode::CREATE).unwrap();
    first.write(a, b"abcdef").unwrap();
    first.seek(a, SeekFrom::Start(0)).unwrap();
    let duplicate = first.duplicate(a).unwrap();
    let transferred = first.duplicate_to(a, &mut second).unwrap();
    assert_eq!(first.read(a, 2).unwrap(), b"ab");
    assert_eq!(first.read(duplicate, 2).unwrap(), b"cd");
    first.close_all();
    assert_eq!(second.read(transferred, 2).unwrap(), b"ef");
    assert_eq!(
        second.info(transferred).unwrap().position,
        FilePosition::Known(6)
    );
}
#[test]
fn capacity_is_checked_before_create_or_truncate_has_side_effects() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    let mut table = HandleTable::with_limit(1).unwrap();
    let a = table.open(&data, "/home/a", FileMode::CREATE).unwrap();
    table.write(a, b"keep").unwrap();
    assert!(matches!(
        table.open(&data, "/home/never-created", FileMode::CREATE),
        Err(FsError::HandleTableFull)
    ));
    assert!(!root.join("home/never-created").exists());
    let truncating = crate::filesystem::FileMode::new(
        crate::filesystem::FileAccess::ReadWrite,
        crate::filesystem::FileCreation::Existing,
        crate::filesystem::FileBehavior::Truncate,
    )
    .unwrap();
    assert!(matches!(
        table.open(&data, "/home/a", truncating),
        Err(FsError::HandleTableFull)
    ));
    assert_eq!(data.read_file("/home/a", 8).unwrap(), b"keep");
    assert_eq!(table.len(), 1);
    assert_eq!(table.limit(), 1);
    assert!(matches!(
        HandleTable::with_limit(0),
        Err(FsError::InvalidHandleLimit)
    ));
}
#[test]
fn close_all_invalidates_tokens_and_releases_owned_references() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    let file = data.open_file("/home/a", FileMode::CREATE).unwrap();
    let mut table = HandleTable::new().unwrap();
    let a = table.insert(file.clone()).unwrap();
    let b = table.duplicate(a).unwrap();
    assert_eq!(file.references(), 3);
    assert_eq!(table.close_all(), 2);
    assert_eq!(table.close_all(), 0);
    assert!(table.is_empty());
    assert_eq!(file.references(), 1);
    assert!(matches!(table.read(a, 1), Err(FsError::InvalidHandle)));
    assert!(matches!(table.read(b, 1), Err(FsError::InvalidHandle)));
    let c = table.insert(file.clone()).unwrap();
    assert_ne!(a, c);
    assert!(matches!(table.info(a), Err(FsError::InvalidHandle)));
}
#[test]
fn dropping_the_owner_table_releases_all_file_capabilities() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    let file = data.open_file("/home/a", FileMode::CREATE).unwrap();
    {
        let mut table = HandleTable::new().unwrap();
        let handle = table.insert(file.clone()).unwrap();
        table.duplicate(handle).unwrap();
        assert_eq!(file.references(), 3);
    }
    assert_eq!(file.references(), 1);
}
#[test]
fn generation_exhaustion_retires_the_slot_instead_of_wrapping() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    let mut table = HandleTable::with_limit(1).unwrap();
    let old = table.open(&data, "/home/a", FileMode::CREATE).unwrap();
    let last = table.force_last_generation(old);
    table.close(last).unwrap();
    assert!(matches!(
        table.open(&data, "/home/a", FileMode::READ),
        Err(FsError::HandleTableFull)
    ));
    assert!(matches!(table.read(old, 1), Err(FsError::InvalidHandle)));
    assert!(matches!(table.read(last, 1), Err(FsError::InvalidHandle)));
}
#[test]
fn random_io_and_sync_work_through_the_handle_table() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    let mut table = HandleTable::new().unwrap();
    let handle = table.open(&data, "/home/a", FileMode::CREATE).unwrap();
    assert_eq!(table.write(handle, b"abc").unwrap(), 3);
    assert_eq!(table.write_at(handle, 1, b"X").unwrap(), 1);
    assert_eq!(table.read_at(handle, 0, 8).unwrap(), b"aXc");
    table.truncate(handle, 2).unwrap();
    table.sync_data(handle).unwrap();
    table.sync_all(handle).unwrap();
    assert_eq!(table.info(handle).unwrap().length, 2);
    table.close(handle).unwrap();
    assert!(matches!(
        table.sync_all(handle),
        Err(FsError::InvalidHandle)
    ));
}
