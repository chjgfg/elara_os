//! Virtual naming authority, independent of target data permissions.
use super::fixture;
use crate::filesystem::{CreationOptions, DataRoot, FsError, VirtualMode};
use crate::users::Account;
use std::{assert, assert_eq, format, fs, matches};
fn user(uid: u32) -> Account {
    Account {
        identity: Default::default(),
        name: format!("u{uid}"),
        uid,
        home: format!("/home/u{uid}"),
    }
}
fn mode(bits: u16) -> VirtualMode {
    VirtualMode::new(bits).unwrap()
}
fn shared(root: &DataRoot, bits: u16) {
    root.create_directory_as(&user(1000), "/tmp/shared", CreationOptions::default())
        .unwrap();
    root.set_access_as(&user(1000), "/tmp/shared", Some(mode(bits)), None, None)
        .unwrap();
}
#[test]
fn nonsticky_file_names_follow_parent_access_not_target_data_mode() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    shared(&root, 0o777);
    root.create_owned_file(&user(1001), "/tmp/shared/f", b"private", mode(0))
        .unwrap();
    assert!(matches!(
        root.read_file_as(&user(1002), "/tmp/shared/f", 32),
        Err(FsError::OwnershipDenied)
    ));
    root.rename_file_as(&user(1002), "/tmp/shared/f", "/tmp/shared/moved")
        .unwrap();
    assert_eq!(fs::read(path.join("tmp/shared/moved")).unwrap(), b"private");
    root.remove_file_as(&user(1002), "/tmp/shared/moved")
        .unwrap();
    assert!(!path.join("tmp/shared/moved").exists());
}
#[test]
fn nonsticky_directory_names_follow_parent_access_not_target_owner() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    shared(&root, 0o777);
    root.create_directory_as(&user(1001), "/tmp/shared/sub", CreationOptions::default())
        .unwrap();
    root.rename_directory_as(&user(1002), "/tmp/shared/sub", "/tmp/shared/moved")
        .unwrap();
    root.remove_directory_as(&user(1002), "/tmp/shared/moved")
        .unwrap();
    assert!(!path.join("tmp/shared/moved").exists());
}

#[test]
fn sticky_file_and_directory_names_require_target_parent_owner_or_root() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    shared(&root, 0o1777);
    for uid in [1002, 1001, 1000, 0] {
        let actor = user(uid);
        let file = format!("/tmp/shared/f{uid}");
        let moved = format!("/tmp/shared/m{uid}");
        root.create_owned_file(&user(1001), &file, b"private", mode(0))
            .unwrap();
        let result = root.rename_file_as(&actor, &file, &moved);
        if uid == 1002 {
            assert!(matches!(result, Err(FsError::OwnershipDenied)));
            assert!(matches!(
                root.remove_file_as(&actor, &file),
                Err(FsError::OwnershipDenied)
            ));
            assert_eq!(
                fs::read(path.join(file.trim_start_matches('/'))).unwrap(),
                b"private"
            );
        } else {
            result.unwrap();
            root.remove_file_as(&actor, &moved).unwrap();
        }
        let dir = format!("/tmp/shared/d{uid}");
        let next = format!("/tmp/shared/n{uid}");
        root.create_directory_as(&user(1001), &dir, CreationOptions::default())
            .unwrap();
        let result = root.rename_directory_as(&actor, &dir, &next);
        if uid == 1002 {
            assert!(matches!(result, Err(FsError::OwnershipDenied)));
            assert!(matches!(
                root.remove_directory_as(&actor, &dir),
                Err(FsError::OwnershipDenied)
            ));
            assert!(path.join(dir.trim_start_matches('/')).is_dir());
        } else {
            result.unwrap();
            root.remove_directory_as(&actor, &next).unwrap();
        }
    }
}
#[test]
fn clearing_only_sticky_allows_previously_denied_name_without_granting_data_access() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    shared(&root, 0o1777);
    root.create_owned_file(&user(1001), "/tmp/shared/f", b"secret", mode(0))
        .unwrap();
    let id = root.owned_file_info("/tmp/shared/f").unwrap();
    assert!(matches!(
        root.remove_file_as(&user(1002), "/tmp/shared/f"),
        Err(FsError::OwnershipDenied)
    ));
    root.set_access_as(&user(1000), "/tmp/shared", Some(mode(0o777)), None, None)
        .unwrap();
    assert_eq!(root.owned_file_info("/tmp/shared/f").unwrap(), id);
    assert!(matches!(
        root.read_file_as(&user(1002), "/tmp/shared/f", 32),
        Err(FsError::OwnershipDenied)
    ));
    root.remove_file_as(&user(1002), "/tmp/shared/f").unwrap();
    assert!(!path.join("tmp/shared/f").exists());
}
#[test]
fn owning_target_does_not_bypass_parent_write_or_search_permissions() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    shared(&root, 0o1777);
    root.create_owned_file(&user(1001), "/tmp/shared/f", b"keep", mode(0o600))
        .unwrap();
    for bits in [0o1755, 0o1766] {
        root.set_access_as(&user(1000), "/tmp/shared", Some(mode(bits)), None, None)
            .unwrap();
        assert!(matches!(
            root.remove_file_as(&user(1001), "/tmp/shared/f"),
            Err(FsError::OwnershipDenied)
        ));
        assert!(matches!(
            root.rename_file_as(&user(1001), "/tmp/shared/f", "/tmp/shared/m"),
            Err(FsError::OwnershipDenied)
        ));
    }
    assert_eq!(fs::read(path.join("tmp/shared/f")).unwrap(), b"keep");
}
#[test]
fn rename_checks_destination_parent_and_never_replaces_an_existing_name() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    shared(&root, 0o1777);
    root.create_directory_as(&user(1000), "/tmp/dest", CreationOptions::default())
        .unwrap();
    root.set_access_as(&user(1000), "/tmp/dest", Some(mode(0o755)), None, None)
        .unwrap();
    root.create_owned_file(&user(1001), "/tmp/shared/f", b"source", mode(0o600))
        .unwrap();
    assert!(matches!(
        root.rename_file_as(&user(1001), "/tmp/shared/f", "/tmp/dest/f"),
        Err(FsError::OwnershipDenied)
    ));
    root.set_access_as(&user(1000), "/tmp/dest", Some(mode(0o1777)), None, None)
        .unwrap();
    root.create_owned_file(&user(1002), "/tmp/dest/existing", b"target", mode(0o600))
        .unwrap();
    assert!(matches!(
        root.rename_file_as(&user(1001), "/tmp/shared/f", "/tmp/dest/existing"),
        Err(FsError::AlreadyExists)
    ));
    root.rename_file_as(&user(1001), "/tmp/shared/f", "/tmp/dest/new")
        .unwrap();
    assert_eq!(fs::read(path.join("tmp/dest/new")).unwrap(), b"source");
    assert_eq!(fs::read(path.join("tmp/dest/existing")).unwrap(), b"target");
}

#[test]
fn unregistered_parent_retains_typed_legacy_owner_gate_and_unknown_targets_are_preserved() {
    use crate::filesystem::access_model::NameAuthority;
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    root.create_owned_file(&user(1001), "/tmp/f", b"keep", mode(0))
        .unwrap();
    assert!(matches!(
        root.remove_file_as(&user(1002), "/tmp/f"),
        Err(FsError::OwnershipDenied)
    ));
    let guard = root.namespace_guard().unwrap();
    let target = crate::filesystem::FileOwner {
        uid: 1001,
        gid: 1001,
    };
    assert_eq!(
        root.name_removal_locked(&user(1001), "/tmp/f", target)
            .unwrap(),
        NameAuthority::LegacyTargetOwner
    );
    drop(guard);
    shared(&root, 0o777);
    fs::write(path.join("tmp/shared/unknown"), b"foreign").unwrap();
    fs::create_dir(path.join("tmp/shared/unknown-dir")).unwrap();
    assert!(matches!(
        root.remove_file_as(&user(1000), "/tmp/shared/unknown"),
        Err(FsError::LegacyOwnershipUnknown)
    ));
    assert!(matches!(
        root.remove_directory_as(&user(1000), "/tmp/shared/unknown-dir"),
        Err(FsError::LegacyOwnershipUnknown)
    ));
    assert_eq!(
        fs::read(path.join("tmp/shared/unknown")).unwrap(),
        b"foreign"
    );
    assert!(path.join("tmp/shared/unknown-dir").exists());
}
#[test]
fn sticky_access_v4_roundtrips_and_cannot_hide_under_older_headers() {
    use crate::filesystem::access_model::AccessCatalog;
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    shared(&root, 0o1777);
    let p = path.join("var/.elara-objects/access.catalog");
    let bytes = fs::read(&p).unwrap();
    assert!(bytes.starts_with(b"ELARA_ACCESS_CATALOG_V4"));
    assert_eq!(
        AccessCatalog::decode(&bytes).unwrap().encode().unwrap(),
        bytes
    );
    for version in [b'1', b'2', b'3'] {
        let mut forged = bytes.clone();
        let index = b"ELARA_ACCESS_CATALOG_V".len();
        forged[index] = version;
        assert!(AccessCatalog::decode(&forged).is_err());
    }
    root.set_access_as(&user(1000), "/tmp/shared", Some(mode(0o2777)), None, None)
        .unwrap();
    assert!(fs::read(&p)
        .unwrap()
        .starts_with(b"ELARA_ACCESS_CATALOG_V3"));
    root.set_access_as(&user(1000), "/tmp/shared", Some(mode(0o777)), None, None)
        .unwrap();
    assert!(fs::read(&p)
        .unwrap()
        .starts_with(b"ELARA_ACCESS_CATALOG_V1"));
}
#[test]
fn sticky_is_not_inherited_and_acl_updates_do_not_clear_parent_special_bits() {
    use crate::filesystem::{Acl, DefaultAcl};
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    shared(&root, 0o1777);
    // The private primary group is held by the parent owner, so requesting SGID is valid.
    root.set_access_as(&user(1000), "/tmp/shared", Some(mode(0o3777)), None, None)
        .unwrap();
    root.set_default_acl_as(
        &user(1000),
        "/tmp/shared",
        Some(DefaultAcl::from_mode(mode(0o777))),
    )
    .unwrap();
    root.create_directory_as(&user(1001), "/tmp/shared/sub", CreationOptions::default())
        .unwrap();
    assert_eq!(
        root.directory_info_as(&user(1001), "/tmp/shared/sub")
            .unwrap()
            .unwrap()
            .mode
            .bits(),
        0o2777
    );
    let mut acl = Acl::default();
    acl.users.insert(1002, 7);
    acl.group = Some(7);
    acl.mask = Some(7);
    root.set_access_as(&user(1000), "/tmp/shared", None, None, Some(acl))
        .unwrap();
    assert_eq!(
        root.directory_info_as(&user(1000), "/tmp/shared")
            .unwrap()
            .unwrap()
            .mode
            .bits(),
        0o3777
    );
    let host = fs::metadata(path.join("tmp/shared")).unwrap().permissions();
    root.set_access_as(&user(1000), "/tmp/shared", Some(mode(0o1777)), None, None)
        .unwrap();
    assert_eq!(
        fs::metadata(path.join("tmp/shared")).unwrap().permissions(),
        host
    );
}
#[test]
fn forged_parent_marker_cannot_grant_nonsticky_name_authority() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    shared(&root, 0o777);
    root.create_owned_file(&user(1001), "/tmp/shared/f", b"keep", mode(0))
        .unwrap();
    let marker = path.join("tmp/shared/.elara-dir-owner");
    let magic = fs::read(&marker).unwrap();
    fs::rename(&marker, path.join("tmp/saved-marker")).unwrap();
    fs::write(&marker, magic).unwrap();
    let before = fs::read(path.join("var/.elara-objects/catalog")).unwrap();
    assert!(matches!(
        root.remove_file_as(&user(1002), "/tmp/shared/f"),
        Err(FsError::OwnershipBindingUnconfirmed)
    ));
    assert_eq!(fs::read(path.join("tmp/shared/f")).unwrap(), b"keep");
    assert_eq!(
        fs::read(path.join("var/.elara-objects/catalog")).unwrap(),
        before
    );
}
#[test]
#[cfg(windows)]
fn unicode_parent_alias_keeps_sticky_owner_gate() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    root.create_directory_as(&user(1000), "/tmp/Äshared", CreationOptions::default())
        .unwrap();
    root.set_access_as(&user(1000), "/tmp/äshared", Some(mode(0o1777)), None, None)
        .unwrap();
    root.create_owned_file(&user(1001), "/tmp/äshared/f", b"keep", mode(0))
        .unwrap();
    // File catalog spelling is still exact for Unicode; do not mistake its
    // earlier unknown-name refusal for a sticky authorization decision.
    assert!(matches!(
        root.remove_file_as(&user(1002), "/tmp/Äshared/f"),
        Err(FsError::LegacyOwnershipUnknown)
    ));
    assert_eq!(fs::read(path.join("tmp/Äshared/f")).unwrap(), b"keep");
    assert!(matches!(
        root.remove_file_as(&user(1002), "/tmp/äshared/f"),
        Err(FsError::OwnershipDenied)
    ));
    root.remove_file_as(&user(1000), "/tmp/äshared/f").unwrap();
    assert!(!path.join("tmp/Äshared/f").exists());
}

#[test]
fn sticky_policy_recovery_keeps_committed_bit_and_task_cleanup_retains_unknown_names() {
    use crate::filesystem::access::AccessFault;
    for fault in [
        AccessFault::AfterNamespace,
        AccessFault::AfterFiles,
        AccessFault::AfterCatalog,
    ] {
        let path = fixture();
        let mut root = DataRoot::open(&path).unwrap();
        shared(&root, 0o777);
        root.access_fault = fault;
        assert!(matches!(
            root.set_access_as(&user(1000), "/tmp/shared", Some(mode(0o1777)), None, None),
            Err(FsError::AccessCommitPending(_))
        ));
        root.access_fault = AccessFault::None;
        root.recover_transactions().unwrap();
        assert_eq!(
            root.directory_info_as(&user(1000), "/tmp/shared")
                .unwrap()
                .unwrap()
                .mode
                .bits(),
            0o1777
        );
        let task = root.begin_task_as(&user(1001)).unwrap();
        let options = CreationOptions {
            scope: Some(&task),
            ..CreationOptions::default()
        };
        root.create_file_with_options(&user(1001), "/tmp/shared/f", b"task", options)
            .unwrap();
        root.create_directory_as(&user(1001), "/tmp/shared/sub", options)
            .unwrap();
        fs::write(path.join("tmp/shared/unknown"), b"foreign").unwrap();
        root.set_access_as(&user(1000), "/tmp/shared", Some(mode(0o1000)), None, None)
            .unwrap();
        let report = task.finish().unwrap();
        assert_eq!(
            (report.files, report.directories, report.deferred),
            (1, 1, 0)
        );
        assert_eq!(
            fs::read(path.join("tmp/shared/unknown")).unwrap(),
            b"foreign"
        );
    }
}
#[test]
fn acl_mask_limits_parent_naming_even_for_target_owner() {
    use crate::filesystem::Acl;
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    shared(&root, 0o1777);
    root.create_owned_file(&user(1001), "/tmp/shared/f", b"keep", mode(0))
        .unwrap();
    let mut acl = Acl::default();
    acl.users.insert(1001, 7);
    acl.group = Some(7);
    acl.mask = Some(1);
    root.set_access_as(&user(1000), "/tmp/shared", None, None, Some(acl.clone()))
        .unwrap();
    assert!(matches!(
        root.remove_file_as(&user(1001), "/tmp/shared/f"),
        Err(FsError::OwnershipDenied)
    ));
    acl.mask = Some(3);
    root.set_access_as(&user(1000), "/tmp/shared", None, None, Some(acl))
        .unwrap();
    root.remove_file_as(&user(1001), "/tmp/shared/f").unwrap();
    assert!(!path.join("tmp/shared/f").exists());
}
#[test]
fn regular_file_sticky_is_rejected_before_names_catalogs_or_bytes_change() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    shared(&root, 0o1777);
    root.create_owned_file(&user(1001), "/tmp/shared/f", b"keep", mode(0o600))
        .unwrap();
    let p = path.join("var/.elara-objects/catalog");
    let before = fs::read(&p).unwrap();
    assert!(matches!(
        root.set_access_as(&user(1001), "/tmp/shared/f", Some(mode(0o1600)), None, None),
        Err(FsError::DirectoryModeOnFile)
    ));
    assert!(matches!(
        root.create_owned_file(&user(1001), "/tmp/shared/bad", b"bad", mode(0o1600)),
        Err(FsError::DirectoryModeOnFile)
    ));
    assert_eq!(fs::read(&p).unwrap(), before);
    assert_eq!(fs::read(path.join("tmp/shared/f")).unwrap(), b"keep");
    assert!(!path.join("tmp/shared/bad").exists());
}

#[test]
fn rename_dispatch_uses_parent_naming_authority_and_preserves_open_file_grants() {
    use crate::filesystem::FileMode;
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    shared(&root, 0o777);
    root.create_owned_file(&user(1001), "/tmp/shared/f", b"pinned", mode(0o600))
        .unwrap();
    let opened = root
        .open_file_as(&user(1001), "/tmp/shared/f", FileMode::READ)
        .unwrap();
    root.set_access_as(&user(1001), "/tmp/shared/f", Some(mode(0)), None, None)
        .unwrap();
    assert!(matches!(
        root.read_file_as(&user(1002), "/tmp/shared/f", 32),
        Err(FsError::OwnershipDenied)
    ));
    root.rename_as(&user(1002), "/tmp/shared/f", "/tmp/shared/moved")
        .unwrap();
    assert_eq!(opened.read_at(0, 32).unwrap(), b"pinned");
    assert!(matches!(
        root.read_file_as(&user(1002), "/tmp/shared/moved", 32),
        Err(FsError::OwnershipDenied)
    ));
    root.remove_file_as(&user(1002), "/tmp/shared/moved")
        .unwrap();
    assert_eq!(opened.read_at(0, 32).unwrap(), b"pinned");
    assert!(!path.join("tmp/shared/moved").exists());
    root.create_directory_as(&user(1001), "/tmp/shared/sub", CreationOptions::default())
        .unwrap();
    root.set_access_as(&user(1001), "/tmp/shared/sub", Some(mode(0)), None, None)
        .unwrap();
    root.rename_as(&user(1002), "/tmp/shared/sub", "/tmp/shared/next")
        .unwrap();
    root.remove_directory_as(&user(1002), "/tmp/shared/next")
        .unwrap();
}
#[test]
fn rename_dispatch_never_uses_legacy_branch_to_bypass_sticky_or_binding() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    shared(&root, 0o1777);
    root.create_owned_file(&user(1001), "/tmp/shared/f", b"keep", mode(0))
        .unwrap();
    assert!(matches!(
        root.rename_as(&user(1002), "/tmp/shared/f", "/tmp/shared/moved"),
        Err(FsError::OwnershipDenied)
    ));
    assert!(matches!(
        root.rename_as(&user(0), "/tmp/shared/f", "/usr/moved"),
        Err(FsError::ReadOnlySystemPath)
    ));
    root.set_access_as(&user(1000), "/tmp/shared", Some(mode(0o777)), None, None)
        .unwrap();
    fs::rename(path.join("tmp/shared/f"), path.join("tmp/saved-f")).unwrap();
    fs::write(path.join("tmp/shared/f"), b"substitute").unwrap();
    assert!(matches!(
        root.rename_as(&user(1002), "/tmp/shared/f", "/tmp/shared/moved"),
        Err(FsError::OwnershipBindingUnconfirmed)
    ));
    assert_eq!(fs::read(path.join("tmp/shared/f")).unwrap(), b"substitute");
    assert_eq!(fs::read(path.join("tmp/saved-f")).unwrap(), b"keep");
    assert!(!path.join("tmp/shared/moved").exists());
}
