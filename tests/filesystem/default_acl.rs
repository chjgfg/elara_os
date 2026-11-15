//! Default inheritance, birth-policy recovery, canonical compatibility and controls.
use super::fixture;
use crate::filesystem::access::AccessFault;
use crate::filesystem::access_model::AccessCatalog;
use crate::filesystem::ownership::OwnershipFault;
use crate::filesystem::{
    Acl, CreationOptions, DataRoot, DefaultAcl, DeleteCondition, DirectoryInfo, FileMode,
    FileOwner, FsError, OwnershipState, Umask, VirtualMode,
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
fn opts(mask: u16) -> CreationOptions<'static> {
    CreationOptions {
        umask: Umask::new(mask).unwrap(),
        scope: None,
    }
}
fn template() -> DefaultAcl {
    let mut default = DefaultAcl::from_mode(mode(0o700));
    default.acl.users.insert(1001, 7);
    default.acl.mask = Some(7);
    default
}
fn info(root: &DataRoot, path: &str) -> crate::filesystem::OwnedFileInfo {
    let OwnershipState::Managed(info) = root.owned_file_info(path).unwrap() else {
        panic!("not managed")
    };
    info
}
fn parent(root: &DataRoot, alice: &Account) {
    root.create_directory_as(alice, "/tmp/d", opts(0)).unwrap();
    root.set_default_acl_as(alice, "/tmp/d", Some(template()))
        .unwrap();
}
#[test]
fn inheritance_clips_every_requested_mode_and_preserves_named_entries() {
    for default in [template(), DefaultAcl::from_mode(mode(0o754))] {
        for requested in 0..=0o777 {
            let (actual, acl) = default.inherit(mode(requested)).unwrap();
            assert_eq!(actual.bits() & !requested, 0);
            assert_eq!(
                (actual.bits() >> 6) & 7,
                u16::from(default.owner) & ((requested >> 6) & 7)
            );
            assert_eq!(
                actual.bits() & 7,
                u16::from(default.other) & (requested & 7)
            );
            assert_eq!(
                (actual.bits() >> 3) & 7,
                u16::from(default.acl.mask.or(default.acl.group).unwrap()) & ((requested >> 3) & 7)
            );
            assert_eq!(acl.users, default.acl.users);
            assert_eq!(acl.groups, default.acl.groups);
            if default.acl.mask.is_some() {
                assert_eq!(acl.group, default.acl.group);
            }
        }
    }
    let owner = FileOwner {
        uid: 1000,
        gid: 1000,
    };
    let actor = AccessCatalog::default().credentials(1001);
    let (actual, mut acl) = template().inherit(mode(0o666)).unwrap();
    assert!(!acl.permits(owner, actual, &actor, 1));
    acl.mask = Some(7); // Control: omit requested-mode clipping of the mask only.
    assert!(acl.permits(owner, actual, &actor, 1));
}
#[test]
fn old_access_catalog_bytes_roundtrip_and_default_ids_and_records_are_strict() {
    let json = r#"{"revision":1,"next_dir":1,"next_task":1,"directories":{},"groups":{},"primary":{},"file_acl":{},"tasks":{}}"#;
    let legacy = std::format!(
        "ELARA_ACCESS_CATALOG_V1{}{}{}",
        char::from(10),
        json,
        char::from(10)
    )
    .into_bytes();
    assert_eq!(
        AccessCatalog::decode(&legacy).unwrap().encode().unwrap(),
        legacy
    );
    let mut catalog = AccessCatalog::default();
    catalog.default_acl.insert(1, template());
    assert!(catalog.encode().is_err());
    catalog.next_dir = 2;
    catalog.directories.insert(
        "/tmp/d".into(),
        DirectoryInfo {
            id: 1,
            path: "/tmp/d".into(),
            owner: FileOwner {
                uid: 1000,
                gid: 1000,
            },
            mode: mode(0o700),
            acl: Acl::default(),
            delete_condition: DeleteCondition::OwnerRequest,
        },
    ).unwrap();
    let bytes = catalog.encode().unwrap();
    assert!(bytes.starts_with(b"ELARA_ACCESS_CATALOG_V2"));
    assert_eq!(AccessCatalog::decode(&bytes).unwrap(), catalog);
    let mut wrong_version = bytes.clone();
    let index = wrong_version.iter().position(|b| *b == 10).unwrap() - 1;
    wrong_version[index] = b'1';
    assert!(AccessCatalog::decode(&wrong_version).is_err());
    let default = catalog.default_acl.get_mut(&1).unwrap();
    default.acl.mask = None;
    assert!(catalog.encode().is_err());
    let mut invalid = template();
    invalid.acl.group = None;
    assert!(invalid.validate().is_err());
    let mut invalid = template();
    invalid.owner = 8;
    assert!(invalid.validate().is_err());
    let mut invalid = template();
    invalid.acl.users.insert(1002, 8);
    assert!(invalid.validate().is_err());
}
#[test]
fn default_overrides_umask_files_clip_execute_and_directories_pass_template_to_children() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    let bob = user(1001);
    parent(&root, &alice);
    let permissions = fs::metadata(path.join("tmp/d")).unwrap().permissions();
    root.create_file_with_options(&alice, "/tmp/d/f", b"inherited", opts(0o777))
        .unwrap();
    assert_eq!(info(&root, "/tmp/d/f").mode.bits(), 0o660);
    let acl = root.acl_as(&alice, "/tmp/d/f").unwrap();
    assert_eq!(acl.mask, Some(6));
    assert_eq!(acl.users.get(&1001), Some(&7));
    assert_eq!(acl.group, Some(0));
    assert_eq!(
        root.read_file_as(&bob, "/tmp/d/f", 32).unwrap(),
        b"inherited"
    );
    root.create_directory_as(&alice, "/tmp/d/sub", opts(0o777))
        .unwrap();
    assert_eq!(
        root.directory_info_as(&alice, "/tmp/d/sub")
            .unwrap()
            .unwrap()
            .mode
            .bits(),
        0o770
    );
    assert_eq!(
        root.default_acl_as(&alice, "/tmp/d/sub").unwrap(),
        Some(template())
    );
    root.create_file_with_options(&alice, "/tmp/d/sub/f", b"grandchild", opts(0o777))
        .unwrap();
    assert_eq!(
        root.read_file_as(&bob, "/tmp/d/sub/f", 32).unwrap(),
        b"grandchild"
    );
    assert_eq!(
        fs::metadata(path.join("tmp/d")).unwrap().permissions(),
        permissions
    );
    assert!(matches!(
        root.default_acl_as(&alice, "/tmp/d/f"),
        Err(FsError::NotDirectory)
    ));
}

#[test]
fn default_changes_affect_future_children_only_and_clear_restores_umask() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    let bob = user(1001);
    parent(&root, &alice);
    let old_parent = root.directory_info_as(&alice, "/tmp/d").unwrap().unwrap();
    root.create_file_with_options(&alice, "/tmp/d/old", b"grant", opts(0o077))
        .unwrap();
    let granted = root
        .open_file_as(&bob, "/tmp/d/old", FileMode::READ)
        .unwrap();
    root.create_directory_as(&alice, "/tmp/d/sub", opts(0o077))
        .unwrap();
    root.set_default_acl_as(&alice, "/tmp/d", None).unwrap();
    assert_eq!(
        root.directory_info_as(&alice, "/tmp/d").unwrap().unwrap(),
        old_parent
    );
    assert_eq!(root.default_acl_as(&alice, "/tmp/d").unwrap(), None);
    assert_eq!(
        root.default_acl_as(&alice, "/tmp/d/sub").unwrap(),
        Some(template())
    );
    root.create_file_with_options(&alice, "/tmp/d/new", b"private", opts(0o077))
        .unwrap();
    assert_eq!(info(&root, "/tmp/d/new").mode.bits(), 0o600);
    assert!(matches!(
        root.read_file_as(&bob, "/tmp/d/new", 32),
        Err(FsError::OwnershipDenied)
    ));
    assert_eq!(root.read_file_as(&bob, "/tmp/d/old", 32).unwrap(), b"grant");
    assert_eq!(granted.read(32).unwrap(), b"grant");
}
#[test]
fn only_owner_can_set_defaults_and_files_and_system_classes_are_refused() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    let bob = user(1001);
    parent(&root, &alice);
    assert!(matches!(
        root.set_default_acl_as(&bob, "/tmp/d", None),
        Err(FsError::OwnershipDenied)
    ));
    root.create_file_with_options(&alice, "/tmp/d/f", b"keep", opts(0o077))
        .unwrap();
    assert!(matches!(
        root.set_default_acl_as(&alice, "/tmp/d/f", Some(template())),
        Err(FsError::NotDirectory)
    ));
    // /etc is Configuration, not SystemReadOnly; it is an unowned layout directory.
    assert!(matches!(
        root.set_default_acl_as(&user(0), "/etc", Some(template())),
        Err(FsError::LegacyOwnershipUnknown)
    ));
    assert!(matches!(
        root.default_acl_as(&user(0), "/etc"),
        Err(FsError::LegacyOwnershipUnknown)
    ));
    assert!(matches!(
        root.set_default_acl_as(&user(0), "/usr", Some(template())),
        Err(FsError::ReadOnlySystemPath)
    ));
    fs::create_dir(path.join("tmp/legacy")).unwrap();
    assert!(matches!(
        root.default_acl_as(&alice, "/tmp/legacy"),
        Err(FsError::LegacyOwnershipUnknown)
    ));
    assert!(matches!(
        root.set_default_acl_as(&alice, "/tmp/legacy", Some(template())),
        Err(FsError::LegacyOwnershipUnknown)
    ));
    assert_eq!(fs::read_dir(path.join("tmp/legacy")).unwrap().count(), 0);
    assert_eq!(
        root.default_acl_as(&alice, "/tmp/d").unwrap(),
        Some(template())
    );
    assert_eq!(fs::read(path.join("tmp/d/f")).unwrap(), b"keep");
}
#[test]
fn zero_inherited_mode_keeps_creation_fd_without_granting_future_open() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    parent(&root, &alice);
    root.set_default_acl_as(&alice, "/tmp/d", Some(DefaultAcl::from_mode(mode(0))))
        .unwrap();
    let file = root
        .open_file_with_options(&alice, "/tmp/d/zero", FileMode::CREATE, opts(0))
        .unwrap();
    assert_eq!(info(&root, "/tmp/d/zero").mode.bits(), 0);
    assert_eq!(file.write(b"grant").unwrap(), 5);
    assert!(matches!(
        root.open_file_as(&alice, "/tmp/d/zero", FileMode::READ),
        Err(FsError::OwnershipDenied)
    ));
    assert_eq!(fs::read(path.join("tmp/d/zero")).unwrap(), b"grant");
}
#[test]
fn removing_inherited_acl_guard_exposes_named_user_deny_despite_other_grant() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    let bob = user(1001);
    parent(&root, &alice);
    let mut default = DefaultAcl::from_mode(mode(0o777));
    default.acl.users.insert(1001, 0);
    default.acl.mask = Some(7);
    root.set_default_acl_as(&alice, "/tmp/d", Some(default))
        .unwrap();
    root.create_file_with_options(&alice, "/tmp/d/f", b"protected", opts(0o077))
        .unwrap();
    assert!(matches!(
        root.read_file_as(&bob, "/tmp/d/f", 32),
        Err(FsError::OwnershipDenied)
    ));
    assert_eq!(
        root.read_without_object_mode_guard(&bob, "/tmp/d/f", 32)
            .unwrap(),
        b"protected"
    );
    // Fixture-only necessity control: remove just the inherited ACL record.
    let id = info(&root, "/tmp/d/f").id.value();
    let mut catalog = root.load_access_catalog().unwrap();
    catalog.file_acl.remove(&id);
    catalog.bump().unwrap();
    fs::write(
        path.join("var/.elara-objects/access.catalog"),
        catalog.encode().unwrap(),
    )
    .unwrap();
    assert_eq!(
        root.read_file_as(&bob, "/tmp/d/f", 32).unwrap(),
        b"protected"
    );
}
#[test]
fn directory_rename_and_removal_preserve_then_retire_default_identity() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    parent(&root, &alice);
    root.create_directory_as(&alice, "/tmp/d/sub", opts(0o077))
        .unwrap();
    let id = root
        .directory_info_as(&alice, "/tmp/d/sub")
        .unwrap()
        .unwrap()
        .id;
    root.rename_directory_as(&alice, "/tmp/d", "/tmp/moved")
        .unwrap();
    assert_eq!(
        root.directory_info_as(&alice, "/tmp/moved/sub")
            .unwrap()
            .unwrap()
            .id,
        id
    );
    assert_eq!(
        root.default_acl_as(&alice, "/tmp/moved/sub").unwrap(),
        Some(template())
    );
    root.remove_directory_as(&alice, "/tmp/moved/sub").unwrap();
    root.remove_directory_as(&alice, "/tmp/moved").unwrap();
    assert!(root.load_access_catalog().unwrap().default_acl.is_empty());
    assert!(fs::read(path.join("var/.elara-objects/access.catalog"))
        .unwrap()
        .starts_with(b"ELARA_ACCESS_CATALOG_V1"));
}

#[test]
fn birth_permissions_recover_at_storage_acl_name_and_catalog_checkpoints() {
    for fault in [
        OwnershipFault::AfterStorage,
        OwnershipFault::AfterCreationAcl,
        OwnershipFault::AfterPublicName,
        OwnershipFault::AfterCatalog,
    ] {
        let path = fixture();
        let root = DataRoot::open(&path).unwrap();
        let alice = user(1000);
        let bob = user(1001);
        parent(&root, &alice);
        let mut default = DefaultAcl::from_mode(mode(0o777));
        default.acl.users.insert(1001, 0);
        default.acl.mask = Some(7);
        root.set_default_acl_as(&alice, "/tmp/d", Some(default))
            .unwrap();
        assert!(matches!(
            root.create_owned_file_with_fault(&alice, "/tmp/d/f", b"protected", mode(0o666), fault),
            Err(FsError::OwnershipCommitPending { .. })
        ));
        assert!(fs::read(path.join("var/.elara-objects/pending"))
            .unwrap()
            .starts_with(b"ELARA_OWNERSHIP_INTENT_V2"));
        if fault == OwnershipFault::AfterCreationAcl {
            assert!(!path.join("tmp/d/f").exists());
            assert_eq!(
                root.load_access_catalog()
                    .unwrap()
                    .file_acl
                    .get(&1)
                    .unwrap()
                    .users
                    .get(&1001),
                Some(&0)
            );
        }
        if fault == OwnershipFault::AfterPublicName {
            assert_eq!(fs::read(path.join("tmp/d/f")).unwrap(), b"protected");
            assert_eq!(
                root.load_access_catalog()
                    .unwrap()
                    .file_acl
                    .get(&1)
                    .unwrap()
                    .users
                    .get(&1001),
                Some(&0)
            );
        }
        root.recover_transactions().unwrap();
        assert_eq!(
            root.read_file_as(&alice, "/tmp/d/f", 32).unwrap(),
            b"protected"
        );
        assert!(matches!(
            root.read_file_as(&bob, "/tmp/d/f", 32),
            Err(FsError::OwnershipDenied)
        ));
        assert!(!path.join("var/.elara-objects/pending").exists());
        assert!(!path.join("var/.elara-objects/access.pending").exists());
    }
}
#[test]
fn nested_policy_intents_and_directory_birth_restore_the_original_template() {
    for fault in [
        AccessFault::AfterNamespace,
        AccessFault::AfterFiles,
        AccessFault::AfterCatalog,
    ] {
        let path = fixture();
        let mut root = DataRoot::open(&path).unwrap();
        let alice = user(1000);
        parent(&root, &alice);
        let revision = root.load_access_catalog().unwrap().revision;
        root.access_fault = fault;
        assert!(matches!(
            root.create_file_with_options(&alice, "/tmp/d/f", b"recover", opts(0o777)),
            Err(FsError::OwnershipCommitPending { .. })
        ));
        assert!(!path.join("tmp/d/f").exists());
        root.access_fault = AccessFault::None;
        root.recover_transactions().unwrap();
        assert_eq!(root.load_access_catalog().unwrap().revision, revision + 1);
        assert_eq!(
            root.read_file_as(&user(1001), "/tmp/d/f", 32).unwrap(),
            b"recover"
        );
        root.access_fault = fault;
        assert!(matches!(
            root.create_directory_as(&alice, "/tmp/d/sub", opts(0o777)),
            Err(FsError::AccessCommitPending(_))
        ));
        root.access_fault = AccessFault::None;
        root.recover_transactions().unwrap();
        assert_eq!(
            root.default_acl_as(&alice, "/tmp/d/sub").unwrap(),
            Some(template())
        );
        assert_eq!(
            root.directory_info_as(&alice, "/tmp/d/sub")
                .unwrap()
                .unwrap()
                .mode
                .bits(),
            0o770
        );
    }
}
#[test]
fn default_inheritance_preserves_task_conditions_and_unknown_tmp_data() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    parent(&root, &alice);
    fs::write(path.join("tmp/foreign"), b"unknown").unwrap();
    let task = root.begin_task_as(&alice).unwrap();
    let options = CreationOptions {
        umask: Umask::new(0o777).unwrap(),
        scope: Some(&task),
    };
    root.create_directory_as(&alice, "/tmp/d/sub", options)
        .unwrap();
    root.create_file_with_options(&alice, "/tmp/d/sub/f", b"temporary", options)
        .unwrap();
    assert_eq!(
        info(&root, "/tmp/d/sub/f").delete_condition,
        DeleteCondition::TaskExit(task.id())
    );
    let report = task.finish().unwrap();
    assert_eq!(report.files, 1);
    assert_eq!(report.directories, 1);
    assert_eq!(report.deferred, 0);
    assert_eq!(fs::read(path.join("tmp/foreign")).unwrap(), b"unknown");
    let catalog = root.load_access_catalog().unwrap();
    assert_eq!(catalog.default_acl.len(), 1);
    assert!(catalog.file_acl.is_empty());
}
#[test]
fn inherited_acl_capacity_failure_precedes_payload_and_name_writes() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    parent(&root, &alice);
    let mut catalog = root.load_access_catalog().unwrap();
    for id in 2..=4097 {
        catalog.file_acl.insert(id, Acl::default());
    }
    let before = catalog.encode().unwrap();
    fs::write(path.join("var/.elara-objects/access.catalog"), &before).unwrap();
    assert!(matches!(
        root.create_file_with_options(&alice, "/tmp/d/f", b"not staged", opts(0o077)),
        Err(FsError::OwnershipCapacity)
    ));
    assert!(!path.join("tmp/d/f").exists());
    assert!(!path.join("var/.elara-objects/pending").exists());
    assert_eq!(
        fs::read(path.join("var/.elara-objects/access.catalog")).unwrap(),
        before
    );
    assert_eq!(fs::read_dir(path.join("tmp/d")).unwrap().count(), 1);
    assert!(!path.join("var/.elara-objects/payload-1-1").exists());
}

#[test]
fn joint_birth_intent_rejects_unrelated_policy_or_mask_mode_tampering() {
    for unrelated in [true, false] {
        let path = fixture();
        let root = DataRoot::open(&path).unwrap();
        let alice = user(1000);
        parent(&root, &alice);
        fs::write(path.join("tmp/foreign"), b"unknown").unwrap();
        assert!(root
            .create_owned_file_with_fault(
                &alice,
                "/tmp/d/f",
                b"protected",
                mode(0o666),
                OwnershipFault::AfterStorage
            )
            .is_err());
        let pending = path.join("var/.elara-objects/pending");
        let bytes = fs::read(&pending).unwrap();
        let magic_end = bytes.iter().position(|b| *b == 10).unwrap();
        let body = &bytes[magic_end + 1..];
        let header_end = body.iter().position(|b| *b == 10).unwrap();
        let header = std::str::from_utf8(&body[..header_end]).unwrap();
        let fields: Vec<_> = header.split(' ').collect();
        assert_eq!(fields.len(), 5);
        let old_len: usize = fields[0].parse().unwrap();
        let new_len: usize = fields[1].parse().unwrap();
        let access_len: usize = fields[3].parse().unwrap();
        let data = &body[header_end + 1..];
        let offset = old_len + new_len + access_len;
        let mut access = AccessCatalog::decode(&data[offset..]).unwrap();
        if unrelated {
            access.groups.insert(
                5000,
                crate::filesystem::Group {
                    name: "forged".into(),
                    members: Default::default(),
                },
            );
        } else {
            access.file_acl.get_mut(&1).unwrap().mask = Some(7);
        }
        let after = access.encode().unwrap();
        let newline = char::from(10);
        let mut forged = std::format!(
            "ELARA_OWNERSHIP_INTENT_V2{newline}{old_len} {new_len} {} {access_len} {}{newline}",
            fields[2],
            after.len()
        )
        .into_bytes();
        forged.extend_from_slice(&data[..offset]);
        forged.extend_from_slice(&after);
        fs::write(&pending, forged).unwrap();
        assert!(root.recover_transactions().is_err());
        assert!(pending.exists());
        assert!(!path.join("tmp/d/f").exists());
        assert_eq!(fs::read(path.join("tmp/foreign")).unwrap(), b"unknown");
    }
}
#[test]
#[cfg(windows)]
fn unicode_parent_alias_inherits_by_verified_directory_identity_not_path_spelling() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    root.create_directory_as(&alice, "/tmp/Äshared", opts(0))
        .unwrap();
    root.set_default_acl_as(&alice, "/tmp/Äshared", Some(template()))
        .unwrap();
    root.create_file_with_options(&alice, "/tmp/äshared/f", b"inherited", opts(0o777))
        .unwrap();
    assert_eq!(info(&root, "/tmp/äshared/f").mode.bits(), 0o660);
    assert_eq!(
        root.read_file_as(&user(1001), "/tmp/äshared/f", 32)
            .unwrap(),
        b"inherited"
    );
    root.create_directory_as(&alice, "/tmp/äshared/sub", opts(0o777))
        .unwrap();
    assert_eq!(
        root.default_acl_as(&alice, "/tmp/äshared/sub").unwrap(),
        Some(template())
    );
}

#[test]
fn default_policy_changes_require_the_registered_marker_identity() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    parent(&root, &alice);
    let marker = path.join("tmp/d/.elara-dir-owner");
    let magic = fs::read(&marker).unwrap();
    fs::remove_file(&marker).unwrap();
    fs::write(&marker, &magic).unwrap();
    assert_eq!(fs::read(&marker).unwrap(), magic); // Same bytes are not identity.
    let catalog = path.join("var/.elara-objects/access.catalog");
    let before = fs::read(&catalog).unwrap();
    let changed =
        root.set_default_acl_as(&alice, "/tmp/d", Some(DefaultAcl::from_mode(mode(0o777))));
    assert!(
        matches!(changed, Err(FsError::OwnershipBindingUnconfirmed)),
        "setter accepted unbound marker: {changed:?}"
    );
    assert_eq!(fs::read(&catalog).unwrap(), before);
    assert!(matches!(
        root.set_default_acl_as(&alice, "/tmp/d", None),
        Err(FsError::OwnershipBindingUnconfirmed)
    ));
    assert_eq!(fs::read(&catalog).unwrap(), before);
}
