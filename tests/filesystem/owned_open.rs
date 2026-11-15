//! Account-aware opens grant a stable object capability, not a mutable name.
use super::fixture;
use crate::filesystem::{
    DataRoot, FileAccess, FileBehavior, FileCreation, FileMode, FilePosition, FsError, HandleTable,
    OwnedFileInfo, OwnershipState, VirtualMode,
};
use crate::users::Account;
use std::io::SeekFrom;
use std::{assert, assert_eq, assert_ne, fs, matches};

fn alice() -> Account {
    Account {
        identity: Default::default(),
        name: "alice".into(),
        uid: 1000,
        home: "/home/alice".into(),
    }
}
fn bob() -> Account {
    Account {
        identity: Default::default(),
        name: "bob".into(),
        uid: 1001,
        home: "/home/bob".into(),
    }
}
fn root_user() -> Account {
    Account {
        identity: Default::default(),
        name: "root".into(),
        uid: 0,
        home: "/root".into(),
    }
}
fn managed(state: OwnershipState) -> OwnedFileInfo {
    let OwnershipState::Managed(info) = state else {
        panic!("expected managed object")
    };
    info
}
fn rw(behavior: FileBehavior) -> FileMode {
    FileMode::new(FileAccess::ReadWrite, FileCreation::Existing, behavior).unwrap()
}

#[test]
fn every_valid_mode_preserves_creation_and_access_contracts() {
    for access in [
        FileAccess::ReadOnly,
        FileAccess::WriteOnly,
        FileAccess::ReadWrite,
    ] {
        for creation in [
            FileCreation::Existing,
            FileCreation::IfMissing,
            FileCreation::Exclusive,
        ] {
            for behavior in [
                FileBehavior::Preserve,
                FileBehavior::Truncate,
                FileBehavior::Append,
            ] {
                let Ok(mode) = FileMode::new(access, creation, behavior) else {
                    continue;
                };
                for exists in [false, true] {
                    let path = fixture();
                    let data = DataRoot::open(&path).unwrap();
                    if exists {
                        data.create_owned_file(
                            &alice(),
                            "/tmp/mode",
                            b"abc",
                            VirtualMode::new(0o600).unwrap(),
                        )
                        .unwrap();
                    }
                    let result = data.open_file_as(&alice(), "/tmp/mode", mode);
                    if exists && creation == FileCreation::Exclusive {
                        assert!(matches!(result, Err(FsError::AlreadyExists)));
                        assert_eq!(fs::read(path.join("tmp/mode")).unwrap(), b"abc");
                        continue;
                    }
                    if !exists && creation == FileCreation::Existing {
                        assert!(matches!(result, Err(FsError::NotFound)));
                        assert!(!path.join("tmp/mode").exists());
                        continue;
                    }
                    let file = result.unwrap();
                    let info = managed(file.info().unwrap().ownership);
                    assert_eq!(info.owner.uid, alice().uid);
                    assert_eq!(info.mode.bits(), 0o600);
                    if access == FileAccess::ReadOnly {
                        assert!(matches!(file.write(b"!"), Err(FsError::HandleWriteDenied)));
                    } else {
                        assert_eq!(file.write(b"!").unwrap(), 1);
                    }
                    if access == FileAccess::WriteOnly {
                        assert!(matches!(file.read_at(0, 8), Err(FsError::HandleReadDenied)));
                    }
                    let expected: &[u8] = if access == FileAccess::ReadOnly {
                        b"abc"
                    } else if !exists || behavior == FileBehavior::Truncate {
                        b"!"
                    } else if behavior == FileBehavior::Append {
                        b"abc!"
                    } else {
                        b"!bc"
                    };
                    assert_eq!(fs::read(path.join("tmp/mode")).unwrap(), expected);
                }
            }
        }
    }
}

#[test]
fn rejected_opens_do_not_truncate_or_adopt_legacy_and_system_data() {
    let path = fixture();
    fs::write(path.join("tmp/legacy"), b"legacy").unwrap();
    fs::write(path.join("usr/system"), b"system").unwrap();
    let data = DataRoot::open(&path).unwrap();
    data.create_owned_file(
        &alice(),
        "/tmp/private",
        b"keep",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    for mode in [
        FileMode::READ,
        rw(FileBehavior::Preserve),
        rw(FileBehavior::Truncate),
        rw(FileBehavior::Append),
    ] {
        assert!(matches!(
            data.open_file_as(&bob(), "/tmp/private", mode),
            Err(FsError::OwnershipDenied)
        ));
    }
    assert!(matches!(
        data.open_file_as(&root_user(), "/usr/system", rw(FileBehavior::Truncate)),
        Err(FsError::ReadOnlySystemPath)
    ));
    assert!(matches!(
        data.open_file_as(&alice(), "/tmp/legacy", rw(FileBehavior::Truncate)),
        Err(FsError::LegacyOwnershipUnknown)
    ));
    assert_eq!(
        data.open_file_as(&alice(), "/tmp/legacy", FileMode::READ)
            .unwrap()
            .read(16)
            .unwrap(),
        b"legacy"
    );
    assert!(matches!(
        data.open_file_as(&alice(), "/tmp", FileMode::READ),
        Err(FsError::IsDirectory)
    ));
    assert!(matches!(
        data.open_file_as(&alice(), "/tmp", rw(FileBehavior::Truncate)),
        Err(FsError::IsDirectory)
    ));
    assert_eq!(fs::read(path.join("tmp/private")).unwrap(), b"keep");
    assert_eq!(fs::read(path.join("tmp/legacy")).unwrap(), b"legacy");
    assert_eq!(fs::read(path.join("usr/system")).unwrap(), b"system");
    assert_eq!(
        data.owned_file_info("/tmp/legacy").unwrap(),
        OwnershipState::LegacyUnknown
    );
}

#[test]
fn duplicate_shares_offset_reopen_does_not_and_rename_keeps_the_grant() {
    let path = fixture();
    let data = DataRoot::open(&path).unwrap();
    data.create_owned_file(
        &alice(),
        "/tmp/source",
        b"abcdef",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    let file = data
        .open_file_as(&alice(), "/tmp/source", rw(FileBehavior::Preserve))
        .unwrap();
    let original = file.info().unwrap();
    let duplicate = file.clone();
    let separate = data
        .open_file_as(&alice(), "/tmp/source", FileMode::READ)
        .unwrap();
    assert_eq!(file.read(2).unwrap(), b"ab");
    assert_eq!(duplicate.read(2).unwrap(), b"cd");
    assert_eq!(separate.read(2).unwrap(), b"ab");
    data.rename_owned_file(&alice(), "/tmp/source", "/tmp/moved")
        .unwrap();
    assert_eq!(duplicate.read(2).unwrap(), b"ef");
    assert_eq!(file.info().unwrap().opened_path, "/tmp/source");
    assert_eq!(file.info().unwrap().ownership, original.ownership);
    let after = managed(data.owned_file_info("/tmp/moved").unwrap());
    assert_eq!(after.id, managed(original.ownership).id);
    assert_eq!(file.write_at(0, b"XY").unwrap(), 2);
    assert_eq!(file.info().unwrap().position, FilePosition::Known(6));
    assert_eq!(
        data.read_file_as(&alice(), "/tmp/moved", 16).unwrap(),
        b"XYcdef"
    );
}

#[test]
fn replacement_and_removal_preserve_old_open_generation_not_new_name_bytes() {
    let path = fixture();
    let data = DataRoot::open(&path).unwrap();
    data.create_owned_file(
        &alice(),
        "/tmp/value",
        b"old",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    let old = data
        .open_file_as(&alice(), "/tmp/value", rw(FileBehavior::Preserve))
        .unwrap();
    let before = managed(old.info().unwrap().ownership);
    data.replace_owned_file(&alice(), "/tmp/value", b"new")
        .unwrap();
    let new = data
        .open_file_as(&alice(), "/tmp/value", FileMode::READ)
        .unwrap();
    let after = managed(new.info().unwrap().ownership);
    assert_eq!(before.id, after.id);
    assert_eq!(after.storage_generation, before.storage_generation + 1);
    assert_eq!(old.read_at(0, 8).unwrap(), b"old");
    assert_eq!(old.write_at(0, b"OLD").unwrap(), 3);
    old.sync_all().unwrap();
    assert_eq!(new.read_at(0, 8).unwrap(), b"new");
    assert_eq!(
        old.info().unwrap().ownership,
        OwnershipState::Managed(before)
    );
    data.remove_owned_file(&alice(), "/tmp/value").unwrap();
    assert!(!path.join("tmp/value").exists());
    assert_eq!(new.read_at(0, 8).unwrap(), b"new");
    assert_eq!(old.read_at(0, 8).unwrap(), b"OLD");
}

#[test]
fn chmod_changes_future_opens_not_existing_capabilities() {
    let path = fixture();
    let data = DataRoot::open(&path).unwrap();
    data.create_owned_file(
        &alice(),
        "/tmp/value",
        b"keep",
        VirtualMode::new(0o666).unwrap(),
    )
    .unwrap();
    let granted = data
        .open_file_as(&bob(), "/tmp/value", rw(FileBehavior::Preserve))
        .unwrap();
    data.chmod_owned_file(&alice(), "/tmp/value", VirtualMode::new(0o600).unwrap())
        .unwrap();
    assert!(matches!(
        data.open_file_as(&bob(), "/tmp/value", FileMode::READ),
        Err(FsError::OwnershipDenied)
    ));
    assert_eq!(granted.write_at(0, b"KE").unwrap(), 2);
    assert_eq!(
        managed(granted.info().unwrap().ownership).mode.bits(),
        0o666
    );
    let read = data
        .open_file_as(&alice(), "/tmp/value", FileMode::READ)
        .unwrap();
    assert!(matches!(
        read.write(b"bad"),
        Err(FsError::HandleWriteDenied)
    ));
    assert!(matches!(
        read.write_at(0, b"bad"),
        Err(FsError::HandleWriteDenied)
    ));
    assert!(matches!(read.truncate(0), Err(FsError::HandleWriteDenied)));
    assert!(matches!(read.sync_data(), Err(FsError::HandleWriteDenied)));
    assert_eq!(read.read(8).unwrap(), b"KEep");
}

#[test]
fn table_capacity_precedes_create_and_truncate_and_transfer_keeps_identity() {
    let path = fixture();
    let data = DataRoot::open(&path).unwrap();
    let mut table = HandleTable::with_limit(1).unwrap();
    let first = table
        .open_as(&data, &alice(), "/tmp/value", FileMode::CREATE)
        .unwrap();
    table.write(first, b"abcdef").unwrap();
    table.seek(first, SeekFrom::Start(0)).unwrap();
    let before = table.info(first).unwrap().ownership;
    assert!(matches!(
        table.open_as(&data, &alice(), "/tmp/missing", FileMode::CREATE),
        Err(FsError::HandleTableFull)
    ));
    assert!(matches!(
        table.open_as(&data, &alice(), "/tmp/value", rw(FileBehavior::Truncate)),
        Err(FsError::HandleTableFull)
    ));
    assert!(!path.join("tmp/missing").exists());
    assert_eq!(fs::read(path.join("tmp/value")).unwrap(), b"abcdef");
    let mut target = HandleTable::new().unwrap();
    let transferred = table.duplicate_to(first, &mut target).unwrap();
    assert_eq!(table.read(first, 2).unwrap(), b"ab");
    assert_eq!(target.read(transferred, 2).unwrap(), b"cd");
    assert_eq!(table.close_all(), 1);
    assert_eq!(target.read(transferred, 2).unwrap(), b"ef");
    assert_eq!(target.info(transferred).unwrap().ownership, before);
    assert!(matches!(table.read(first, 1), Err(FsError::InvalidHandle)));
    let reopened = table
        .open_as(&data, &alice(), "/tmp/value", FileMode::READ)
        .unwrap();
    assert_ne!(reopened.generation(), first.generation());
    assert_eq!(table.read(reopened, 2).unwrap(), b"ab");
}

#[test]
fn alias_and_host_name_replacement_cannot_grant_another_users_bytes() {
    let path = fixture();
    let data = DataRoot::open(&path).unwrap();
    data.create_owned_file(
        &alice(),
        "/tmp/private",
        b"secret",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    fs::hard_link(path.join("tmp/private"), path.join("tmp/alias")).unwrap();
    assert!(matches!(
        data.open_file_as(&bob(), "/tmp/alias", FileMode::READ),
        Err(FsError::OwnershipBindingUnconfirmed)
    ));
    fs::remove_file(path.join("tmp/private")).unwrap();
    fs::write(path.join("tmp/private"), b"secret").unwrap();
    assert!(matches!(
        data.open_file_as(&alice(), "/tmp/private", FileMode::READ),
        Err(FsError::OwnershipBindingUnconfirmed)
    ));
}

#[test]
fn copied_directory_capability_never_reopens_the_ambient_path_label() {
    let path = fixture();
    let mut data = DataRoot::open(&path).unwrap();
    data.create_owned_file(
        &alice(),
        "/tmp/value",
        b"keep",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    // Deliberately invalidate only the diagnostic host label, not the granted Dir.
    data.root = path.join("nonexistent-root-label");
    assert!(!data.root.exists());
    let file = data
        .open_file_as(&alice(), "/tmp/value", rw(FileBehavior::Preserve))
        .unwrap();
    drop(data);
    assert_eq!(file.read(8).unwrap(), b"keep");
    assert_eq!(file.write_at(0, b"KE").unwrap(), 2);
    assert_eq!(fs::read(path.join("tmp/value")).unwrap(), b"KEep");
}

#[test]
fn concurrent_managed_clones_share_one_offset_under_the_namespace_domain() {
    let path = fixture();
    let data = DataRoot::open(&path).unwrap();
    let bytes: Vec<_> = (0u8..80).collect();
    data.create_owned_file(
        &alice(),
        "/tmp/value",
        &bytes,
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    let file = data
        .open_file_as(&alice(), "/tmp/value", FileMode::READ)
        .unwrap();
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let file = file.clone();
            std::thread::spawn(move || {
                let mut seen = Vec::new();
                loop {
                    let byte = file.read(1).unwrap();
                    if byte.is_empty() {
                        break;
                    }
                    seen.extend(byte);
                }
                seen
            })
        })
        .collect();
    let mut seen: Vec<_> = workers
        .into_iter()
        .flat_map(|worker| worker.join().unwrap())
        .collect();
    seen.sort_unstable();
    assert_eq!(seen, bytes);
    assert_eq!(file.info().unwrap().position, FilePosition::Known(80));
}

#[test]
fn directory_rename_cannot_move_managed_descendants_without_their_catalog() {
    let path = fixture();
    let data = DataRoot::open(&path).unwrap();
    data.create_directory("/tmp/tree").unwrap();
    data.create_owned_file(
        &alice(),
        "/tmp/tree/value",
        b"keep",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    let before = data.owned_file_info("/tmp/tree/value").unwrap();
    assert!(matches!(
        data.rename("/tmp/tree", "/tmp/moved"),
        Err(FsError::OwnershipContextRequired)
    ));
    assert_eq!(data.owned_file_info("/tmp/tree/value").unwrap(), before);
    assert_eq!(
        data.read_file_as(&alice(), "/tmp/tree/value", 8).unwrap(),
        b"keep"
    );
    assert!(!path.join("tmp/moved").exists());
}
