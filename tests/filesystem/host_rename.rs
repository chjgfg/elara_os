//! Replacement preserves object capabilities, not the old destination name.
use super::fixture;
use crate::{filesystem::*, users::Account};
use std::{assert, assert_eq};
fn admin() -> Account {
    Account {
        identity: Default::default(),
        uid: 0,
        name: "root".into(),
        home: "/root".into(),
    }
}
fn create(root: &DataRoot, user: &Account, path: &str, bytes: &[u8]) {
    root.create_owned_file(user, path, bytes, VirtualMode::new(0o600).unwrap())
        .unwrap();
}
#[test]
fn host_rename_replace_preserves_both_alias_sets_and_old_destination_fd() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    create(&root, &user, "/root/source", b"source");
    create(&root, &user, "/root/dest", b"victim");
    root.hardlink_as(&user, "/root/source", "/root/source-alias")
        .unwrap();
    root.hardlink_as(&user, "/root/dest", "/root/victim-alias")
        .unwrap();
    let old = root
        .open_file_as(&user, "/root/dest", FileMode::READ)
        .unwrap();
    root.rename_replace_as(&user, "/root/source", "/root/dest")
        .unwrap();
    assert!(!path.join("root/source").exists());
    assert_eq!(
        root.read_file_as(&user, "/root/dest", 16).unwrap(),
        b"source"
    );
    assert_eq!(
        root.read_file_as(&user, "/root/source-alias", 16).unwrap(),
        b"source"
    );
    assert_eq!(
        root.read_file_as(&user, "/root/victim-alias", 16).unwrap(),
        b"victim"
    );
    assert_eq!(old.read_at(0, 16).unwrap(), b"victim");
    root.remove_file_as(&user, "/root/victim-alias").unwrap();
    assert_eq!(old.read_at(0, 16).unwrap(), b"victim");
    let reopened = DataRoot::open(&path).unwrap();
    assert_eq!(
        reopened.read_file_as(&user, "/root/dest", 16).unwrap(),
        b"source"
    );
}
#[test]
fn host_rename_replace_same_inode_is_noop_and_links_are_not_followed() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    create(&root, &user, "/root/a", b"data");
    root.hardlink_as(&user, "/root/a", "/root/b").unwrap();
    let before = std::fs::read(path.join("var/.elara-objects/catalog")).unwrap();
    root.rename_replace_as(&user, "/root/a", "/root/b").unwrap();
    assert!(path.join("root/a").exists());
    assert_eq!(
        std::fs::read(path.join("var/.elara-objects/catalog")).unwrap(),
        before
    );
    root.symlink_as(&user, "a", "/root/link").unwrap();
    create(&root, &user, "/root/dest", b"old");
    root.rename_replace_as(&user, "/root/link", "/root/dest")
        .unwrap();
    assert_eq!(root.readlink_as(&user, "/root/dest").unwrap(), "a");
    assert_eq!(root.read_file_as(&user, "/root/a", 16).unwrap(), b"data");
    create(&root, &user, "/root/regular", b"new");
    root.rename_replace_as(&user, "/root/regular", "/root/dest")
        .unwrap();
    assert_eq!(root.read_file_as(&user, "/root/dest", 16).unwrap(), b"new");
    assert_eq!(root.read_file_as(&user, "/root/a", 16).unwrap(), b"data");
}
#[test]
fn host_rename_replace_faults_recover_each_phase_and_keep_unlinked_fd() {
    use crate::filesystem::ownership::OwnershipFault;
    use std::matches;
    for fault in [
        OwnershipFault::AfterStorage,
        OwnershipFault::AfterPublicName,
        OwnershipFault::AfterCatalog,
        OwnershipFault::AfterOldPayloadRemoval,
    ] {
        let path = fixture();
        let root = DataRoot::open(&path).unwrap();
        let user = admin();
        create(&root, &user, "/root/source", b"new");
        create(&root, &user, "/root/dest", b"old");
        let fd = root
            .open_file_as(&user, "/root/dest", FileMode::READ)
            .unwrap();
        assert!(matches!(
            root.rename_replace_owned_with_fault(&user, "/root/source", "/root/dest", fault),
            Err(FsError::OwnershipCommitPending { .. })
        ));
        let pending = std::fs::read(path.join("var/.elara-objects/pending")).unwrap();
        assert!(pending.starts_with(b"ELARA_OWNERSHIP_INTENT_V3\n"));
        if fault == OwnershipFault::AfterStorage {
            assert_eq!(std::fs::read(path.join("root/dest")).unwrap(), b"old");
            assert!(path.join("root/source").exists());
        } else {
            assert_eq!(std::fs::read(path.join("root/dest")).unwrap(), b"new");
            assert!(!path.join("root/source").exists());
        }
        let reopened = DataRoot::open(&path).unwrap();
        reopened.recover_transactions().unwrap();
        reopened.recover_transactions().unwrap();
        assert_eq!(
            reopened.read_file_as(&user, "/root/dest", 16).unwrap(),
            b"new"
        );
        assert_eq!(fd.read_at(0, 16).unwrap(), b"old");
        assert!(!path.join("root/source").exists());
        assert!(!path.join("var/.elara-objects/pending").exists());
    }
}
#[test]
fn host_rename_replace_recovery_preserves_foreign_same_byte_destination() {
    use crate::filesystem::ownership::OwnershipFault;
    use std::{fs, matches};
    for fault in [
        OwnershipFault::AfterStorage,
        OwnershipFault::AfterPublicName,
        OwnershipFault::AfterCatalog,
    ] {
        let path = fixture();
        let root = DataRoot::open(&path).unwrap();
        let user = admin();
        create(&root, &user, "/root/source", b"new");
        create(&root, &user, "/root/dest", b"old");
        assert!(matches!(
            root.rename_replace_owned_with_fault(&user, "/root/source", "/root/dest", fault),
            Err(FsError::OwnershipCommitPending { .. })
        ));
        let bytes = fs::read(path.join("root/dest")).unwrap();
        fs::rename(path.join("root/dest"), path.join("root/saved")).unwrap();
        fs::write(path.join("root/dest"), &bytes).unwrap();
        let catalog = fs::read(path.join("var/.elara-objects/catalog")).unwrap();
        assert!(root.recover_transactions().is_err());
        assert_eq!(fs::read(path.join("root/dest")).unwrap(), bytes);
        assert_eq!(
            fs::read(path.join("var/.elara-objects/catalog")).unwrap(),
            catalog
        );
        assert!(path.join("var/.elara-objects/pending").exists());
        // Restore only objects created by this test, retaining the rejected file.
        fs::rename(path.join("root/dest"), path.join("root/foreign")).unwrap();
        fs::rename(path.join("root/saved"), path.join("root/dest")).unwrap();
        root.recover_transactions().unwrap();
        assert_eq!(root.read_file_as(&user, "/root/dest", 16).unwrap(), b"new");
    }
}
#[test]
fn host_rename_replace_sticky_checks_victim_and_cross_domain_never_changes_names() {
    use std::{fs, matches};
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    let a = Account {
        identity: Default::default(),
        uid: 1001,
        name: "a".into(),
        home: "/home/a".into(),
    };
    let b = Account {
        identity: Default::default(),
        uid: 1002,
        name: "b".into(),
        home: "/home/b".into(),
    };
    root.create_directory_as(&user, "/tmp/shared", CreationOptions::default())
        .unwrap();
    root.set_access_as(
        &user,
        "/tmp/shared",
        Some(VirtualMode::new(0o1777).unwrap()),
        None,
        None,
    )
    .unwrap();
    create(&root, &a, "/tmp/shared/source", b"a");
    create(&root, &b, "/tmp/shared/dest", b"b");
    let before = fs::read(path.join("var/.elara-objects/catalog")).unwrap();
    assert!(matches!(
        root.rename_replace_as(&a, "/tmp/shared/source", "/tmp/shared/dest"),
        Err(FsError::OwnershipDenied)
    ));
    assert_eq!(
        fs::read(path.join("var/.elara-objects/catalog")).unwrap(),
        before
    );
    assert_eq!(fs::read(path.join("tmp/shared/source")).unwrap(), b"a");
    assert_eq!(fs::read(path.join("tmp/shared/dest")).unwrap(), b"b");
    root.set_access_as(
        &user,
        "/tmp/shared",
        Some(VirtualMode::new(0o777).unwrap()),
        None,
        None,
    )
    .unwrap();
    root.rename_replace_as(&a, "/tmp/shared/source", "/tmp/shared/dest")
        .unwrap();
    fs::write(path.join("tmp/unknown"), b"keep").unwrap();
    assert!(root
        .rename_replace_as(&user, "/tmp/shared/dest", "/tmp/unknown")
        .is_err());
    assert_eq!(fs::read(path.join("tmp/unknown")).unwrap(), b"keep");
    root.mount_memory_as(
        &user,
        "/mnt/a",
        vfs::Limits {
            bytes: 4096,
            inodes: 16,
        },
    )
    .unwrap();
    assert!(matches!(
        root.rename_replace_as(&user, "/tmp/shared/dest", "/mnt/a/other"),
        Err(FsError::Vfs(vfs::Error::CrossDevice))
    ));
    root.register_resource_as(
        &user,
        "/opt/app",
        ResourceKind::Application,
        ResourceLimits {
            bytes: 4096,
            objects: 16,
        },
    )
    .unwrap();
    root.register_application_location_as(
        &user,
        "/opt/app",
        "/root/app",
        ResourceKind::ApplicationData,
        ResourceLimits {
            bytes: 4096,
            objects: 16,
        },
    )
    .unwrap();
    create(&root, &user, "/root/app/a", b"app");
    create(&root, &user, "/root/plain", b"plain");
    assert!(root
        .rename_replace_as(&user, "/root/app/a", "/root/plain")
        .is_err());
    assert_eq!(root.read_file_as(&user, "/root/app/a", 16).unwrap(), b"app");
    assert_eq!(
        root.read_file_as(&user, "/root/plain", 16).unwrap(),
        b"plain"
    );
}
#[test]
fn host_rename_directories_replace_empty_target_and_preserve_descendant_identity() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    for d in ["/root/source", "/root/source/nested", "/root/dest"] {
        root.create_directory_as(&user, d, CreationOptions::default())
            .unwrap();
    }
    create(&root, &user, "/root/source/nested/data", b"payload");
    root.symlink_as(&user, "nested/data", "/root/source/link")
        .unwrap();
    let before = root
        .directory_info_as(&user, "/root/source")
        .unwrap()
        .unwrap();
    let fd = root
        .open_file_as(&user, "/root/source/nested/data", FileMode::READ)
        .unwrap();
    root.rename_replace_as(&user, "/root/source", "/root/dest")
        .unwrap();
    assert!(!path.join("root/source").exists());
    assert_eq!(
        root.directory_info_as(&user, "/root/dest")
            .unwrap()
            .unwrap()
            .id,
        before.id
    );
    assert_eq!(
        root.read_file_as(&user, "/root/dest/link", 16).unwrap(),
        b"payload"
    );
    assert_eq!(fd.read_at(0, 16).unwrap(), b"payload");
    root.rename_replace_as(&user, "/root/dest", "/root/dest")
        .unwrap();
    root.create_directory_as(&user, "/root/nonempty", CreationOptions::default())
        .unwrap();
    create(&root, &user, "/root/nonempty/data", b"keep");
    assert!(root
        .rename_replace_as(&user, "/root/dest", "/root/nonempty")
        .is_err());
    assert!(root
        .rename_replace_as(&user, "/root/dest", "/root/dest/nested")
        .is_err());
    assert!(root
        .rename_replace_as(&user, "/root/nonempty/data", "/root/dest")
        .is_err());
    assert!(root
        .rename_replace_as(&user, "/root/dest", "/root/nonempty/data")
        .is_err());
    assert_eq!(
        root.read_file_as(&user, "/root/nonempty/data", 16).unwrap(),
        b"keep"
    );
    let reopened = DataRoot::open(&path).unwrap();
    assert_eq!(
        reopened.read_file_as(&user, "/root/dest/link", 16).unwrap(),
        b"payload"
    );
}

#[test]
fn host_rename_directories_recover_retirement_namespace_and_both_catalogs() {
    use crate::filesystem::access::AccessFault;
    use std::{fs, matches};
    for fault in [AccessFault::AfterReplaceRetirement, AccessFault::AfterNamespace,
        AccessFault::AfterFiles, AccessFault::AfterCatalog] {
        let path = fixture(); let mut root = DataRoot::open(&path).unwrap(); let user = admin();
        for name in ["/root/source", "/root/dest"] {
            root.create_directory_as(&user, name, CreationOptions::default()).unwrap();
        }
        create(&root, &user, "/root/source/data", b"live");
        let fd = root.open_file_as(&user, "/root/source/data", FileMode::READ).unwrap();
        let id = root.directory_info_as(&user, "/root/source").unwrap().unwrap().id;
        root.access_fault = fault;
        assert!(matches!(root.rename_replace_as(&user, "/root/source", "/root/dest"), Err(FsError::AccessCommitPending(_))));
        if fault == AccessFault::AfterReplaceRetirement {
            assert!(path.join("root/source").exists());
            assert!(!path.join("root/dest").exists());
        } else {
            assert!(!path.join("root/source").exists());
            assert_eq!(fs::read(path.join("root/dest/data")).unwrap(), b"live");
        }
        let reopened = DataRoot::open(&path).unwrap();
        reopened.recover_transactions().unwrap(); reopened.recover_transactions().unwrap();
        assert_eq!(reopened.directory_info_as(&user, "/root/dest").unwrap().unwrap().id, id);
        assert_eq!(reopened.read_file_as(&user, "/root/dest/data", 16).unwrap(), b"live");
        assert_eq!(fd.read_at(0, 16).unwrap(), b"live");
        assert!(!path.join("var/.elara-objects/access.pending").exists());
    }
}
#[test]
fn host_rename_directories_retirement_gap_refuses_foreign_destination() {
    use crate::filesystem::access::AccessFault;
    use std::{fs, matches};
    let path = fixture(); let mut root = DataRoot::open(&path).unwrap(); let user = admin();
    for name in ["/root/source", "/root/dest"] {
        root.create_directory_as(&user, name, CreationOptions::default()).unwrap();
    }
    create(&root, &user, "/root/source/data", b"live");
    root.access_fault = AccessFault::AfterReplaceRetirement;
    assert!(matches!(root.rename_replace_as(&user, "/root/source", "/root/dest"), Err(FsError::AccessCommitPending(_))));
    fs::create_dir(path.join("root/dest")).unwrap();
    fs::write(path.join("root/dest/foreign"), b"keep").unwrap();
    let before = fs::read(path.join("var/.elara-objects/access.catalog")).unwrap();
    assert!(root.recover_transactions().is_err());
    assert_eq!(fs::read(path.join("var/.elara-objects/access.catalog")).unwrap(), before);
    assert_eq!(fs::read(path.join("root/dest/foreign")).unwrap(), b"keep");
    assert_eq!(fs::read(path.join("root/source/data")).unwrap(), b"live");
    fs::rename(path.join("root/dest"), path.join("root/foreign-dir")).unwrap();
    root.recover_transactions().unwrap();
    assert_eq!(fs::read(path.join("root/dest/data")).unwrap(), b"live");
    assert_eq!(fs::read(path.join("root/foreign-dir/foreign")).unwrap(), b"keep");
}
