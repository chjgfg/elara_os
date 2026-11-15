//! Managed Host names share one inode, policy, generation and quota charge.
use super::fixture;
use crate::filesystem::{
    vfs::{self, LockMode},
    *,
};
use crate::users::Account;
use std::{assert, assert_eq, fs, matches};
fn admin() -> Account {
    Account {
        identity: Default::default(),
        name: "root".into(),
        uid: 0,
        home: "/root".into(),
    }
}
fn rw() -> FileMode {
    FileMode::new(
        FileAccess::ReadWrite,
        FileCreation::Existing,
        FileBehavior::Preserve,
    )
    .unwrap()
}
fn info(root: &DataRoot, path: &str) -> OwnedFileInfo {
    let OwnershipState::Managed(info) = root.owned_file_info(path).unwrap() else {
        panic!("not managed")
    };
    info
}
#[test]
fn host_hardlinks_share_data_policy_locks_generation_and_last_unlink_lifetime() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.create_owned_file(&user, "/root/a", b"old", VirtualMode::new(0o600).unwrap())
        .unwrap();
    root.hardlink_as(&user, "/root/a", "/root/b").unwrap();
    assert_eq!(info(&root, "/root/a").id, info(&root, "/root/b").id);
    let old = root.open_file_as(&user, "/root/a", rw()).unwrap();
    let alias = root.open_file_as(&user, "/root/b", rw()).unwrap();
    assert_eq!(
        old.compare_identity(&alias).unwrap(),
        ObjectBinding::SameObject
    );
    old.lock(LockMode::Exclusive).unwrap();
    assert!(matches!(
        alias.read_at(0, 3),
        Err(FsError::Vfs(vfs::Error::LockConflict))
    ));
    old.unlock().unwrap();
    alias.write_at(0, b"OLD").unwrap();
    assert_eq!(root.read_file_as(&user, "/root/a", 16).unwrap(), b"OLD");
    root.chmod_as(&user, "/root/b", VirtualMode::new(0o640).unwrap())
        .unwrap();
    assert_eq!(info(&root, "/root/a").mode.bits(), 0o640);
    root.replace_owned_file(&user, "/root/b", b"new").unwrap();
    assert_eq!(info(&root, "/root/a").storage_generation, 2);
    assert_eq!(info(&root, "/root/b").storage_generation, 2);
    assert_eq!(root.read_file_as(&user, "/root/a", 16).unwrap(), b"new");
    assert_eq!(old.read_at(0, 3).unwrap(), b"OLD");
    let live = root.open_file_as(&user, "/root/b", rw()).unwrap();
    assert_eq!(
        old.compare_identity(&live).unwrap(),
        ObjectBinding::DifferentObject
    );
    root.remove_owned_file(&user, "/root/a").unwrap();
    assert_eq!(live.read_at(0, 3).unwrap(), b"new");
    root.rename_owned_file(&user, "/root/b", "/root/c").unwrap();
    assert_eq!(root.read_file_as(&user, "/root/c", 16).unwrap(), b"new");
    root.remove_owned_file(&user, "/root/c").unwrap();
    assert_eq!(live.read_at(0, 3).unwrap(), b"new");
    assert!(!path.join("root/a").exists());
    assert!(!path.join("root/c").exists());
}
#[test]
fn host_hardlinks_restart_preserves_v6_and_same_bytes_replacement_is_not_adopted() {
    let path = fixture();
    let user = admin();
    let root = DataRoot::open(&path).unwrap();
    root.create_owned_file(&user, "/root/a", b"keep", VirtualMode::new(0o600).unwrap())
        .unwrap();
    root.hardlink_as(&user, "/root/a", "/root/b").unwrap();
    let catalog = fs::read(path.join("var/.elara-objects/catalog")).unwrap();
    assert!(catalog.starts_with(
        b"ELARA_OWNERSHIP_CATALOG_V6
"
    ));
    drop(root);
    let root = DataRoot::open(&path).unwrap();
    assert_eq!(root.read_file_as(&user, "/root/b", 16).unwrap(), b"keep");
    assert_eq!(info(&root, "/root/a").id, info(&root, "/root/b").id);
    fs::rename(path.join("root/b"), path.join("root/moved-original")).unwrap();
    fs::write(path.join("root/b"), b"keep").unwrap();
    assert!(root.read_file_as(&user, "/root/b", 16).is_err());
    assert!(root
        .replace_owned_file(&user, "/root/a", b"changed")
        .is_err());
    assert_eq!(fs::read(path.join("root/b")).unwrap(), b"keep");
    assert_eq!(fs::read(path.join("root/a")).unwrap(), b"keep");
    assert_eq!(
        fs::read(path.join("var/.elara-objects/catalog")).unwrap(),
        catalog
    );
}
#[test]
fn host_hardlinks_resource_usage_counts_inode_once_and_rejects_domain_escape() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.register_resource_as(
        &user,
        "/opt/app",
        ResourceKind::Application,
        ResourceLimits {
            bytes: 4,
            objects: 2,
        },
    )
    .unwrap();
    root.register_resource_as(
        &user,
        "/opt/other",
        ResourceKind::Application,
        ResourceLimits {
            bytes: 4,
            objects: 2,
        },
    )
    .unwrap();
    root.create_owned_file(
        &user,
        "/opt/app/a",
        b"data",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    root.hardlink_as(&user, "/opt/app/a", "/opt/app/b").unwrap();
    let (_, usage) = root.resource_info_as(&user, "/opt/app").unwrap();
    assert_eq!(
        usage,
        ResourceUsage {
            bytes: 4,
            objects: 2
        }
    );
    assert!(matches!(
        root.hardlink_as(&user, "/opt/app/a", "/opt/other/x"),
        Err(FsError::OwnershipClassMismatch)
    ));
    assert!(!path.join("opt/other/x").exists());
    root.remove_owned_file(&user, "/opt/app/a").unwrap();
    assert_eq!(root.resource_info_as(&user, "/opt/app").unwrap().1, usage);
    root.remove_owned_file(&user, "/opt/app/b").unwrap();
    assert_eq!(
        root.resource_info_as(&user, "/opt/app").unwrap().1,
        ResourceUsage {
            bytes: 0,
            objects: 1
        }
    );
}

#[test]
fn host_hardlinks_link_faults_recover_and_foreign_destination_is_preserved() {
    use crate::filesystem::ownership::OwnershipFault;
    for fault in [
        OwnershipFault::AfterStorage,
        OwnershipFault::AfterPublicName,
        OwnershipFault::AfterCatalog,
    ] {
        let path = fixture();
        let root = DataRoot::open(&path).unwrap();
        let user = admin();
        root.create_owned_file(&user, "/root/a", b"data", VirtualMode::new(0o600).unwrap())
            .unwrap();
        assert!(matches!(
            root.hardlink_owned_file_with_fault(&user, "/root/a", "/root/b", fault),
            Err(FsError::OwnershipCommitPending { .. })
        ));
        root.recover_transactions().unwrap();
        root.recover_transactions().unwrap();
        assert_eq!(info(&root, "/root/a").id, info(&root, "/root/b").id);
        assert_eq!(root.read_file_as(&user, "/root/b", 16).unwrap(), b"data");
        assert!(!path.join("var/.elara-objects/pending").exists());
    }
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.create_owned_file(&user, "/root/a", b"data", VirtualMode::new(0o600).unwrap())
        .unwrap();
    assert!(matches!(
        root.hardlink_owned_file_with_fault(
            &user,
            "/root/a",
            "/root/b",
            OwnershipFault::AfterStorage
        ),
        Err(FsError::OwnershipCommitPending { .. })
    ));
    fs::write(path.join("root/b"), b"data").unwrap();
    let pending = fs::read(path.join("var/.elara-objects/pending")).unwrap();
    assert!(root.recover_transactions().is_err());
    assert_eq!(fs::read(path.join("root/b")).unwrap(), b"data");
    assert_eq!(
        fs::read(path.join("var/.elara-objects/pending")).unwrap(),
        pending
    );
    fs::rename(path.join("root/b"), path.join("root/foreign-saved")).unwrap();
    root.recover_transactions().unwrap();
    assert_eq!(info(&root, "/root/a").id, info(&root, "/root/b").id);
    assert_eq!(fs::read(path.join("root/foreign-saved")).unwrap(), b"data");
}
#[test]
fn host_hardlinks_replace_faults_recover_partial_aliases_and_keep_old_descriptors() {
    use crate::filesystem::ownership::OwnershipFault;
    for fault in [
        OwnershipFault::AfterStorage,
        OwnershipFault::AfterFirstPublicName,
        OwnershipFault::AfterPublicName,
        OwnershipFault::AfterCatalog,
        OwnershipFault::AfterOldPayloadRemoval,
    ] {
        let path = fixture();
        let root = DataRoot::open(&path).unwrap();
        let user = admin();
        root.create_owned_file(&user, "/root/a", b"old", VirtualMode::new(0o600).unwrap())
            .unwrap();
        root.hardlink_as(&user, "/root/a", "/root/b").unwrap();
        let old = root.open_file_as(&user, "/root/a", FileMode::READ).unwrap();
        assert!(matches!(
            root.replace_owned_file_with_fault(&user, "/root/b", b"new", fault),
            Err(FsError::OwnershipCommitPending { .. })
        ));
        if fault == OwnershipFault::AfterFirstPublicName {
            assert_eq!(fs::read(path.join("root/a")).unwrap(), b"new");
            assert_eq!(fs::read(path.join("root/b")).unwrap(), b"old");
        }
        let reopened = DataRoot::open(&path).unwrap();
        reopened.recover_transactions().unwrap();
        reopened.recover_transactions().unwrap();
        assert_eq!(reopened.read_file_as(&user, "/root/a", 16).unwrap(), b"new");
        assert_eq!(reopened.read_file_as(&user, "/root/b", 16).unwrap(), b"new");
        assert_eq!(old.read_at(0, 3).unwrap(), b"old");
        assert_eq!(info(&reopened, "/root/a").storage_generation, 2);
        assert!(!path.join("var/.elara-objects/pending").exists());
    }
}

#[test]
fn host_hardlinks_acl_privilege_directory_move_and_task_lifetime_are_shared() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    let task = root.begin_task_as(&user).unwrap();
    let options = CreationOptions {
        scope: Some(&task),
        ..CreationOptions::default()
    };
    root.create_directory_as(&user, "/tmp/d", options).unwrap();
    root.create_file_with_options(&user, "/tmp/d/a", b"data", options)
        .unwrap();
    root.hardlink_as(&user, "/tmp/d/a", "/tmp/d/b").unwrap();
    let mut acl = Acl::default();
    acl.users.insert(1001, 4);
    acl.mask = Some(4);
    root.set_access_as(
        &user,
        "/tmp/d/a",
        Some(VirtualMode::new(0o4640).unwrap()),
        None,
        Some(acl),
    )
    .unwrap();
    assert_eq!(
        root.acl_as(&user, "/tmp/d/b").unwrap().users.get(&1001),
        Some(&4)
    );
    assert!(info(&root, "/tmp/d/b").mode.is_setuid());
    root.append_owned_file(&user, "/tmp/d/b", b"!").unwrap();
    assert!(!info(&root, "/tmp/d/a").mode.is_setuid());
    assert!(!info(&root, "/tmp/d/b").mode.is_setuid());
    root.rename_directory_as(&user, "/tmp/d", "/tmp/moved")
        .unwrap();
    assert_eq!(
        info(&root, "/tmp/moved/a").id,
        info(&root, "/tmp/moved/b").id
    );
    let fd = root
        .open_file_as(&user, "/tmp/moved/b", FileMode::READ)
        .unwrap();
    let report = task.finish().unwrap();
    assert_eq!(report.deferred, 0);
    assert!(!path.join("tmp/moved").exists());
    assert_eq!(fd.read_at(0, 8).unwrap(), b"data!");
}
#[test]
fn host_hardlinks_reject_foreign_legacy_existing_and_cross_backend_names() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.create_owned_file(&user, "/tmp/a", b"data", VirtualMode::new(0o644).unwrap())
        .unwrap();
    let guest = Account {
        identity: Default::default(),
        name: "guest".into(),
        uid: 1000,
        home: "/home/guest".into(),
    };
    assert!(matches!(
        root.hardlink_as(&guest, "/tmp/a", "/tmp/stolen"),
        Err(FsError::OwnershipDenied)
    ));
    assert!(!path.join("tmp/stolen").exists());
    fs::write(path.join("tmp/legacy"), b"unknown").unwrap();
    assert!(matches!(
        root.hardlink_as(&user, "/tmp/legacy", "/tmp/adopted"),
        Err(FsError::LegacyOwnershipUnknown)
    ));
    assert!(root.hardlink_as(&user, "/tmp/a", "/tmp/legacy").is_err());
    assert_eq!(fs::read(path.join("tmp/legacy")).unwrap(), b"unknown");
    root.mount_memory_as(
        &user,
        "/mnt/volume",
        vfs::Limits {
            bytes: 4096,
            inodes: 16,
        },
    )
    .unwrap();
    assert!(matches!(
        root.hardlink_as(&user, "/tmp/a", "/mnt/volume/b"),
        Err(FsError::Vfs(vfs::Error::CrossDevice))
    ));
    assert!(matches!(
        root.hardlink_as(&user, "/tmp/a", "/root/b"),
        Err(FsError::OwnershipClassMismatch)
    ));
}
