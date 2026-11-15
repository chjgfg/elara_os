//! Directory setgid and sticky guards, never host permissions.
use super::fixture;
use crate::filesystem::access::AccessFault;
use crate::filesystem::access_model::AccessCatalog;
use crate::filesystem::ownership::OwnershipFault;
use crate::filesystem::{
    Acl, CreationOptions, DataRoot, DefaultAcl, FileMode, FsError, OwnershipState, Umask,
    VirtualMode,
};
use crate::users::Account;
use std::{assert, assert_eq, fs, matches};
fn user(uid: u32) -> Account {
    Account {
        identity: Default::default(),
        name: std::format!("u{uid}"),
        uid,
        home: std::format!("/home/u{uid}"),
    }
}
fn mode(bits: u16) -> VirtualMode {
    VirtualMode::new(bits).unwrap()
}
fn opts() -> CreationOptions<'static> {
    CreationOptions::default()
}
#[test]
fn sticky_file_name_owner_guard_remains_required() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    let bob = user(1001);
    let charlie = user(1002);
    root.create_directory_as(&alice, "/tmp/shared", opts())
        .unwrap();
    root.set_access_as(&alice, "/tmp/shared", Some(mode(0o1777)), None, None)
        .unwrap();
    root.create_owned_file(&bob, "/tmp/shared/f", b"private", mode(0))
        .unwrap();
    assert!(matches!(
        root.rename_file_as(&charlie, "/tmp/shared/f", "/tmp/shared/moved"),
        Err(FsError::OwnershipDenied)
    ));
    assert!(matches!(
        root.remove_file_as(&charlie, "/tmp/shared/f"),
        Err(FsError::OwnershipDenied)
    ));
    assert_eq!(fs::read(path.join("tmp/shared/f")).unwrap(), b"private");
    assert!(!path.join("tmp/shared/moved").exists());
}
#[test]
fn sticky_directory_name_owner_guard_remains_required() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    let bob = user(1001);
    let charlie = user(1002);
    root.create_directory_as(&alice, "/tmp/shared", opts())
        .unwrap();
    root.set_access_as(&alice, "/tmp/shared", Some(mode(0o1777)), None, None)
        .unwrap();
    root.create_directory_as(&bob, "/tmp/shared/sub", opts())
        .unwrap();
    assert!(matches!(
        root.rename_directory_as(&charlie, "/tmp/shared/sub", "/tmp/shared/moved"),
        Err(FsError::OwnershipDenied)
    ));
    assert!(matches!(
        root.remove_directory_as(&charlie, "/tmp/shared/sub"),
        Err(FsError::OwnershipDenied)
    ));
    assert!(path.join("tmp/shared/sub").exists());
    assert!(!path.join("tmp/shared/moved").exists());
}

fn file_info(root: &DataRoot, path: &str) -> crate::filesystem::OwnedFileInfo {
    let OwnershipState::Managed(info) = root.owned_file_info(path).unwrap() else {
        panic!("not managed")
    };
    info
}
fn shared(root: &DataRoot) {
    let admin = user(0);
    root.group_add_as(&admin, 5000, "team").unwrap();
    root.group_add_as(&admin, 6000, "primary").unwrap();
    root.group_member_as(&admin, 6000, 1001, true).unwrap();
    root.primary_group_as(&admin, 1001, 6000).unwrap();
    root.create_directory_as(&admin, "/tmp/team", opts())
        .unwrap();
    root.set_access_as(&admin, "/tmp/team", Some(mode(0o2777)), Some(5000), None)
        .unwrap();
}
#[test]
fn setgid_overrides_primary_group_and_passes_to_directories_with_default_acl() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    shared(&root);
    let admin = user(0);
    let bob = user(1001);
    let host_before = fs::metadata(path.join("tmp/team")).unwrap().permissions();
    root.set_default_acl_as(
        &admin,
        "/tmp/team",
        Some(DefaultAcl::from_mode(mode(0o770))),
    )
    .unwrap();
    let options = CreationOptions {
        umask: Umask::new(0o777).unwrap(),
        scope: None,
    };
    root.create_file_with_options(&bob, "/tmp/team/f", b"group", options)
        .unwrap();
    root.create_directory_as(&bob, "/tmp/team/sub", options)
        .unwrap();
    root.create_file_with_options(&bob, "/tmp/team/sub/grandchild", b"next", options)
        .unwrap();
    assert_eq!(file_info(&root, "/tmp/team/f").owner.gid, 5000);
    assert_eq!(file_info(&root, "/tmp/team/f").mode.bits(), 0o660);
    let sub = root
        .directory_info_as(&bob, "/tmp/team/sub")
        .unwrap()
        .unwrap();
    assert_eq!(sub.owner.gid, 5000);
    assert_eq!(sub.mode.bits(), 0o2770);
    assert_eq!(file_info(&root, "/tmp/team/sub/grandchild").owner.gid, 5000);
    assert_eq!(root.credentials_as(&bob).unwrap().gid, 6000);
    assert!(!root.credentials_as(&bob).unwrap().groups.contains(&5000));
    assert_eq!(
        fs::metadata(path.join("tmp/team")).unwrap().permissions(),
        host_before
    );
    let reopened = DataRoot::open(&path).unwrap();
    assert_eq!(file_info(&reopened, "/tmp/team/f").owner.gid, 5000);
    assert_eq!(
        reopened
            .directory_info_as(&bob, "/tmp/team/sub")
            .unwrap()
            .unwrap(),
        sub
    );
}
#[test]
fn removing_only_parent_setgid_restores_primary_without_rewriting_existing_objects() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    shared(&root);
    let admin = user(0);
    let bob = user(1001);
    root.create_owned_file(&bob, "/tmp/team/old", b"old", mode(0o640))
        .unwrap();
    root.create_directory_as(&bob, "/tmp/team/old-dir", opts())
        .unwrap();
    assert_eq!(file_info(&root, "/tmp/team/old").owner.gid, 5000);
    root.set_access_as(&admin, "/tmp/team", Some(mode(0o777)), None, None)
        .unwrap();
    root.create_owned_file(&bob, "/tmp/team/new", b"new", mode(0o640))
        .unwrap();
    root.create_directory_as(&bob, "/tmp/team/new-dir", opts())
        .unwrap();
    assert_eq!(file_info(&root, "/tmp/team/new").owner.gid, 6000);
    let new_dir = root
        .directory_info_as(&bob, "/tmp/team/new-dir")
        .unwrap()
        .unwrap();
    assert_eq!(new_dir.owner.gid, 6000);
    assert_eq!(new_dir.mode.bits(), 0o700);
    assert_eq!(file_info(&root, "/tmp/team/old").owner.gid, 5000);
    let old_dir = root
        .directory_info_as(&bob, "/tmp/team/old-dir")
        .unwrap()
        .unwrap();
    assert_eq!(old_dir.owner.gid, 5000);
    assert_eq!(old_dir.mode.bits(), 0o2700);
    assert_eq!(
        root.directory_info_as(&admin, "/tmp/team")
            .unwrap()
            .unwrap()
            .owner
            .gid,
        5000
    );
}
#[test]
fn chmod_requires_owner_and_clears_setgid_for_nonmember_but_keeps_member_or_root() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    shared(&root);
    let admin = user(0);
    let bob = user(1001);
    let stranger = user(1002);
    root.create_directory_as(&bob, "/tmp/team/sub", opts())
        .unwrap();
    assert!(matches!(
        root.set_access_as(&stranger, "/tmp/team/sub", Some(mode(0o2777)), None, None),
        Err(FsError::OwnershipDenied)
    ));
    root.set_access_as(&bob, "/tmp/team/sub", Some(mode(0o2777)), None, None)
        .unwrap();
    assert_eq!(
        root.directory_info_as(&bob, "/tmp/team/sub")
            .unwrap()
            .unwrap()
            .mode
            .bits(),
        0o777
    );
    root.group_member_as(&admin, 5000, bob.uid, true).unwrap();
    root.set_access_as(&bob, "/tmp/team/sub", Some(mode(0o2777)), None, None)
        .unwrap();
    assert_eq!(
        root.directory_info_as(&bob, "/tmp/team/sub")
            .unwrap()
            .unwrap()
            .mode
            .bits(),
        0o2777
    );
    root.group_member_as(&admin, 5000, bob.uid, false).unwrap();
    root.set_access_as(&admin, "/tmp/team/sub", Some(mode(0o2777)), None, None)
        .unwrap();
    assert_eq!(
        root.directory_info_as(&admin, "/tmp/team/sub")
            .unwrap()
            .unwrap()
            .mode
            .bits(),
        0o2777
    );
}

#[test]
fn unsupported_special_bits_on_files_fail_without_catalog_payload_or_name_writes() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    shared(&root);
    let admin = user(0);
    root.create_owned_file(&admin, "/tmp/team/f", b"keep", mode(0o600))
        .unwrap();
    let store = path.join("var/.elara-objects");
    let files = fs::read(store.join("catalog")).unwrap();
    let access = fs::read(store.join("access.catalog")).unwrap();
    let count = fs::read_dir(&store).unwrap().count();
    assert!(matches!(
        root.set_access_as(&admin, "/tmp/team/f", Some(mode(0o1600)), None, None),
        Err(FsError::DirectoryModeOnFile)
    ));
    assert!(matches!(
        root.create_owned_file(&admin, "/tmp/team/bad", b"bad", mode(0o1600)),
        Err(FsError::DirectoryModeOnFile)
    ));
    assert_eq!(fs::read(store.join("catalog")).unwrap(), files);
    assert_eq!(fs::read(store.join("access.catalog")).unwrap(), access);
    assert_eq!(fs::read_dir(&store).unwrap().count(), count);
    assert!(!path.join("tmp/team/bad").exists());
    assert_eq!(fs::read(path.join("tmp/team/f")).unwrap(), b"keep");
    for bits in [0o10000, u16::MAX] {
        assert!(VirtualMode::new(bits).is_err());
    }
    assert!(Umask::new(0o2000).is_err());
    let mut bad = crate::filesystem::ownership_model::Catalog::empty();
    assert!(matches!(
        bad.allocate(
            "/tmp/f",
            crate::filesystem::FileOwner { uid: 0, gid: 0 },
            mode(0o1600),
            crate::filesystem::OwnedStorageClass::Temporary
        ),
        Err(FsError::DirectoryModeOnFile)
    ));
    assert!(bad.entries.is_empty());
    assert_eq!(bad.next_id, 1);
}
#[test]
fn access_v3_is_required_for_setgid_and_v1_v2_canonical_bytes_remain_valid() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    shared(&root);
    let admin = user(0);
    root.set_default_acl_as(
        &admin,
        "/tmp/team",
        Some(DefaultAcl::from_mode(mode(0o777))),
    )
    .unwrap();
    let p = path.join("var/.elara-objects/access.catalog");
    let bytes = fs::read(&p).unwrap();
    let v3 = b"ELARA_ACCESS_CATALOG_V3";
    assert!(bytes.starts_with(v3));
    assert_eq!(
        AccessCatalog::decode(&bytes).unwrap().encode().unwrap(),
        bytes
    );
    for header in [b"ELARA_ACCESS_CATALOG_V1", b"ELARA_ACCESS_CATALOG_V2"] {
        let mut forged = bytes.clone();
        forged[..header.len()].copy_from_slice(header);
        assert!(AccessCatalog::decode(&forged).is_err());
    }
    root.set_access_as(&admin, "/tmp/team", Some(mode(0o777)), None, None)
        .unwrap();
    let bytes = fs::read(&p).unwrap();
    assert!(bytes.starts_with(b"ELARA_ACCESS_CATALOG_V2"));
    assert_eq!(
        AccessCatalog::decode(&bytes).unwrap().encode().unwrap(),
        bytes
    );
    root.set_default_acl_as(&admin, "/tmp/team", None).unwrap();
    let bytes = fs::read(&p).unwrap();
    assert!(bytes.starts_with(b"ELARA_ACCESS_CATALOG_V1"));
    assert_eq!(
        AccessCatalog::decode(&bytes).unwrap().encode().unwrap(),
        bytes
    );
}
#[test]
fn access_acl_mask_updates_preserve_directory_setgid_not_host_permissions() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    shared(&root);
    let admin = user(0);
    let before = fs::metadata(path.join("tmp/team")).unwrap().permissions();
    let mut acl = Acl::default();
    acl.users.insert(1001, 7);
    acl.group = Some(1);
    acl.mask = Some(4);
    root.set_access_as(&admin, "/tmp/team", None, None, Some(acl))
        .unwrap();
    assert_eq!(
        root.directory_info_as(&admin, "/tmp/team")
            .unwrap()
            .unwrap()
            .mode
            .bits(),
        0o2747
    );
    assert_eq!(
        fs::metadata(path.join("tmp/team")).unwrap().permissions(),
        before
    );
}
#[test]
fn setgid_directory_and_acl_file_birth_recover_original_gid_and_mode() {
    for fault in [
        AccessFault::AfterNamespace,
        AccessFault::AfterFiles,
        AccessFault::AfterCatalog,
    ] {
        let path = fixture();
        let mut root = DataRoot::open(&path).unwrap();
        shared(&root);
        let bob = user(1001);
        root.access_fault = fault;
        assert!(matches!(
            root.create_directory_as(&bob, "/tmp/team/sub", opts()),
            Err(FsError::AccessCommitPending(_))
        ));
        root.access_fault = AccessFault::None;
        root.recover_transactions().unwrap();
        let sub = root
            .directory_info_as(&bob, "/tmp/team/sub")
            .unwrap()
            .unwrap();
        assert_eq!(sub.owner.gid, 5000);
        assert_eq!(sub.mode.bits(), 0o2700);
    }
    for fault in [
        OwnershipFault::AfterStorage,
        OwnershipFault::AfterCreationAcl,
        OwnershipFault::AfterPublicName,
        OwnershipFault::AfterCatalog,
    ] {
        let path = fixture();
        let root = DataRoot::open(&path).unwrap();
        shared(&root);
        let bob = user(1001);
        let admin = user(0);
        root.set_default_acl_as(
            &admin,
            "/tmp/team",
            Some(DefaultAcl::from_mode(mode(0o777))),
        )
        .unwrap();
        let guard = root.namespace_guard().unwrap();
        assert!(matches!(
            root.create_owned_file_configured_locked(
                &bob,
                "/tmp/team/f",
                b"recover",
                (
                    crate::filesystem::access_model::CreationMode::Requested(mode(0o666)),
                    fault,
                    opts()
                ),
                &guard
            ),
            Err(FsError::OwnershipCommitPending { .. })
        ));
        drop(guard);
        root.recover_transactions().unwrap();
        let info = file_info(&root, "/tmp/team/f");
        assert_eq!(info.owner.gid, 5000);
        assert_eq!(info.mode.bits(), 0o666);
        assert_eq!(
            root.read_file_as(&bob, "/tmp/team/f", 32).unwrap(),
            b"recover"
        );
    }
}
#[test]
fn task_condition_survives_inherited_gid_and_leaves_unknown_contents_untouched() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    shared(&root);
    let bob = user(1001);
    fs::write(path.join("tmp/team/foreign"), b"unknown").unwrap();
    let task = root.begin_task_as(&bob).unwrap();
    let options = CreationOptions {
        umask: Umask::default(),
        scope: Some(&task),
    };
    let file = root
        .open_file_with_options(&bob, "/tmp/team/f", FileMode::CREATE, options)
        .unwrap();
    file.write(b"kept capability").unwrap();
    root.create_directory_as(&bob, "/tmp/team/sub", options)
        .unwrap();
    assert_eq!(file_info(&root, "/tmp/team/f").owner.gid, 5000);
    drop(file);
    let cleanup = task.finish().unwrap();
    assert_eq!(cleanup.files, 1);
    assert_eq!(cleanup.directories, 1);
    assert_eq!(fs::read(path.join("tmp/team/foreign")).unwrap(), b"unknown");
    assert!(!path.join("tmp/team/f").exists());
    assert!(!path.join("tmp/team/sub").exists());
}
#[test]
#[cfg(windows)]
fn unicode_directory_alias_keeps_verified_setgid_and_owner_policy() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let admin = user(0);
    let bob = user(1001);
    root.group_add_as(&admin, 5000, "team").unwrap();
    root.create_directory_as(&admin, "/tmp/Äteam", opts())
        .unwrap();
    root.set_access_as(&admin, "/tmp/äteam", Some(mode(0o2777)), Some(5000), None)
        .unwrap();
    root.create_file_with_options(&bob, "/tmp/äteam/f", b"case", opts())
        .unwrap();
    assert_eq!(file_info(&root, "/tmp/äteam/f").owner.gid, 5000);
    assert_eq!(
        root.directory_info_as(&admin, "/tmp/Äteam")
            .unwrap()
            .unwrap()
            .mode
            .bits(),
        0o2777
    );
    assert!(matches!(
        root.set_access_as(&bob, "/tmp/äteam", Some(mode(0o777)), None, None),
        Err(FsError::OwnershipDenied)
    ));
}
