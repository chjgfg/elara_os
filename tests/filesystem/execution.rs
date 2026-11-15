//! Execute authorization is distinct from read, with a real guard-removal control.
use super::fixture;
use crate::filesystem::{
    Acl, CreationOptions, DataRoot, FileAccess, FileBehavior, FileCreation, FileMode, FsError,
    VirtualMode,
};
use crate::users::Account;
use std::{assert, assert_eq, format, fs, matches};
fn actor(uid: u32) -> Account {
    Account {
        identity: Default::default(),
        uid,
        name: format!("u{uid}"),
        home: format!("/home/u{uid}"),
    }
}
fn mode(bits: u16) -> VirtualMode {
    VirtualMode::new(bits).unwrap()
}
#[test]
fn execution_guard_is_necessary_and_execute_does_not_grant_read() {
    let root = DataRoot::open(&fixture()).unwrap();
    let owner = actor(1000);
    root.create_owned_file(&owner, "/tmp/image", b"echo image", mode(0o600))
        .unwrap();
    assert!(matches!(
        root.load_executable_as(&owner, "/tmp/image", 64),
        Err(FsError::OwnershipDenied)
    ));
    assert!(matches!(
        root.load_executable_as(&actor(0), "/tmp/image", 64),
        Err(FsError::OwnershipDenied)
    ));
    assert_eq!(
        root.load_without_execute_guard(&owner, "/tmp/image", 64)
            .unwrap()
            .bytes(),
        b"echo image"
    );
    root.chmod_as(&owner, "/tmp/image", mode(0o100)).unwrap();
    let image = root.load_executable_as(&owner, "/tmp/image", 64).unwrap();
    assert_eq!(image.bytes(), b"echo image");
    assert_eq!(image.authorized_as().uid, 1000);
    assert!(matches!(
        root.read_file_as(&owner, "/tmp/image", 64),
        Err(FsError::OwnershipDenied)
    ));
    assert!(matches!(
        root.load_executable_as(&actor(1001), "/tmp/image", 64),
        Err(FsError::OwnershipDenied)
    ));
}
#[test]
fn execution_acl_mask_and_ancestor_search_are_independent_gates() {
    let root = DataRoot::open(&fixture()).unwrap();
    let owner = actor(1000);
    let guest = actor(1001);
    root.create_directory_as(&owner, "/tmp/programs", CreationOptions::default())
        .unwrap();
    root.set_access_as(&owner, "/tmp/programs", Some(mode(0o711)), None, None)
        .unwrap();
    root.create_owned_file(&owner, "/tmp/programs/image", b"true", mode(0o700))
        .unwrap();
    let mut acl = Acl::default();
    acl.users.insert(guest.uid, 1);
    acl.mask = Some(0);
    root.set_access_as(&owner, "/tmp/programs/image", None, None, Some(acl.clone()))
        .unwrap();
    assert!(matches!(
        root.load_executable_as(&guest, "/tmp/programs/image", 64),
        Err(FsError::OwnershipDenied)
    ));
    acl.mask = Some(1);
    root.set_access_as(&owner, "/tmp/programs/image", None, None, Some(acl))
        .unwrap();
    assert_eq!(
        root.load_executable_as(&guest, "/tmp/programs/image", 64)
            .unwrap()
            .bytes(),
        b"true"
    );
    root.set_access_as(&owner, "/tmp/programs", Some(mode(0o700)), None, None)
        .unwrap();
    assert!(matches!(
        root.load_executable_as(&guest, "/tmp/programs/image", 64),
        Err(FsError::OwnershipDenied)
    ));
    assert!(matches!(
        root.load_without_execute_guard(&guest, "/tmp/programs/image", 64),
        Err(FsError::OwnershipDenied)
    ));
}
#[test]
fn execution_snapshot_survives_old_fd_write_replace_rename_and_unlink() {
    let root = DataRoot::open(&fixture()).unwrap();
    let owner = actor(1000);
    root.create_owned_file(&owner, "/tmp/image", b"echo old", mode(0o700))
        .unwrap();
    let fd = root
        .open_file_as(
            &owner,
            "/tmp/image",
            FileMode::new(
                FileAccess::ReadWrite,
                FileCreation::Existing,
                FileBehavior::Preserve,
            )
            .unwrap(),
        )
        .unwrap();
    let image = root.load_executable_as(&owner, "/tmp/image", 64).unwrap();
    fd.write_at(0, b"echo new").unwrap();
    assert_eq!(
        root.load_executable_as(&owner, "/tmp/image", 64)
            .unwrap()
            .bytes(),
        b"echo new"
    );
    root.replace_owned_file(&owner, "/tmp/image", b"echo replacement")
        .unwrap();
    let changed = root.load_executable_as(&owner, "/tmp/image", 64).unwrap();
    let crate::filesystem::ExecutableFile::Host(changed_file) = changed.file() else {
        panic!("host image domain")
    };
    let crate::filesystem::ExecutableFile::Host(original_file) = image.file() else {
        panic!("host image domain")
    };
    assert_eq!(changed_file.id, original_file.id);
    assert!(changed_file.storage_generation > original_file.storage_generation);
    root.rename_file_as(&owner, "/tmp/image", "/tmp/moved")
        .unwrap();
    root.remove_file_as(&owner, "/tmp/moved").unwrap();
    assert_eq!(image.bytes(), b"echo old");
    assert_eq!(changed.bytes(), b"echo replacement");
}
#[test]
fn execution_refuses_unknown_replaced_oversized_and_host_paths() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let owner = actor(1000);
    fs::write(path.join("tmp/unknown"), b"true").unwrap();
    assert!(matches!(
        root.load_executable_as(&owner, "/tmp/unknown", 64),
        Err(FsError::LegacyOwnershipUnknown)
    ));
    root.create_owned_file(&owner, "/tmp/image", b"echo image", mode(0o700))
        .unwrap();
    assert!(matches!(
        root.load_executable_as(&owner, "/tmp/image", 1),
        Err(FsError::TooLarge(1))
    ));
    assert!(matches!(
        root.load_executable_as(&owner, "/tmp/image", u64::MAX),
        Err(FsError::IoRequestTooLarge(_))
    ));
    assert!(root
        .load_executable_as(&owner, "C:/Windows/not-an-elara-image", 64)
        .is_err());
    let before = fs::metadata(path.join("tmp/image")).unwrap().permissions();
    root.load_executable_as(&owner, "/tmp/image", 64).unwrap();
    assert_eq!(
        fs::metadata(path.join("tmp/image")).unwrap().permissions(),
        before
    );
    fs::rename(path.join("tmp/image"), path.join("tmp/original")).unwrap();
    fs::write(path.join("tmp/image"), b"echo image").unwrap();
    assert!(matches!(
        root.load_executable_as(&owner, "/tmp/image", 64),
        Err(FsError::OwnershipBindingUnconfirmed)
    ));
    assert!(matches!(
        root.load_without_execute_guard(&owner, "/tmp/image", 64),
        Err(FsError::OwnershipBindingUnconfirmed)
    ));
    assert_eq!(fs::read(path.join("tmp/original")).unwrap(), b"echo image");
}

#[test]
fn vfs_execution_captures_domain_permissions_and_immutable_bytes() {
    let root = DataRoot::open(&fixture()).unwrap();
    let admin = actor(0);
    let guest = actor(1000);
    root.mount_memory_as(
        &admin,
        "/mnt/exec",
        crate::filesystem::vfs::Limits {
            bytes: 65536,
            inodes: 64,
        },
    )
    .unwrap();
    root.create_owned_file(&admin, "/mnt/exec/run", b"echo before", mode(0o600))
        .unwrap();
    assert!(matches!(
        root.load_executable_as(&guest, "/mnt/exec/run", 64),
        Err(FsError::OwnershipDenied)
    ));
    assert_eq!(
        root.load_without_execute_guard(&guest, "/mnt/exec/run", 64)
            .unwrap()
            .bytes(),
        b"echo before"
    );
    root.chmod_as(&admin, "/mnt/exec/run", mode(0o111)).unwrap();
    let image = root
        .load_executable_as(&guest, "/mnt/exec/run", 64)
        .unwrap();
    assert!(matches!(
        image.file(),
        crate::filesystem::ExecutableFile::Virtual { .. }
    ));
    assert!(root.read_file_as(&guest, "/mnt/exec/run", 64).is_err());
    let fd = root
        .open_file_as(
            &admin,
            "/mnt/exec/run",
            FileMode::new(
                FileAccess::ReadWrite,
                FileCreation::Existing,
                FileBehavior::Preserve,
            )
            .unwrap(),
        )
        .unwrap();
    fd.write_at(0, b"echo after!").unwrap();
    root.remove_file_as(&admin, "/mnt/exec/run").unwrap();
    assert_eq!(image.bytes(), b"echo before");
    drop(fd);
    root.unmount_as(&admin, "/mnt/exec").unwrap();
}
