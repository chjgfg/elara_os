//! Directory/mask/group/ACL/task invariants, including host-native observations.
use super::fixture;
use crate::filesystem::access::AccessFault;
use crate::filesystem::access_model::AccessCatalog;
use crate::filesystem::{
    Acl, CleanupStatus, CreationOptions, DataRoot, DeleteCondition, FileMode, FsError,
    OwnershipState, Umask, VirtualMode,
};
use crate::users::Account;
use std::{assert, assert_eq, assert_ne, fs, matches};
fn user(uid: u32) -> Account {
    Account {
        identity: Default::default(),
        name: std::format!("u{uid}"),
        uid,
        home: std::format!("/home/u{uid}"),
    }
}
fn opts(mask: u16) -> CreationOptions<'static> {
    CreationOptions {
        umask: Umask::new(mask).unwrap(),
        scope: None,
    }
}
fn mode(bits: u16) -> VirtualMode {
    VirtualMode::new(bits).unwrap()
}
fn info(root: &DataRoot, path: &str) -> crate::filesystem::OwnedFileInfo {
    let OwnershipState::Managed(info) = root.owned_file_info(path).unwrap() else {
        panic!("not managed")
    };
    info
}
#[test]
fn directory_search_listing_and_parent_mutation_are_distinct_permissions() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    let bob = user(1001);
    root.create_directory_as(&alice, "/tmp/d", opts(0o077))
        .unwrap();
    root.create_owned_file(&alice, "/tmp/d/f", b"visible", mode(0o644))
        .unwrap();
    assert!(matches!(
        root.read_file_as(&bob, "/tmp/d/f", 32),
        Err(FsError::OwnershipDenied)
    ));
    root.set_access_as(&alice, "/tmp/d", Some(mode(0o711)), None, None)
        .unwrap();
    assert_eq!(root.read_file_as(&bob, "/tmp/d/f", 32).unwrap(), b"visible");
    assert!(matches!(
        root.list_as(&bob, "/tmp/d"),
        Err(FsError::OwnershipDenied)
    ));
    assert!(matches!(
        root.create_file_as(&bob, "/tmp/d/new"),
        Err(FsError::OwnershipDenied)
    ));
    root.set_access_as(&alice, "/tmp/d", Some(mode(0o755)), None, None)
        .unwrap();
    assert_eq!(root.list_as(&bob, "/tmp/d").unwrap().len(), 1);
    root.set_access_as(&alice, "/tmp/d", Some(mode(0o777)), None, None)
        .unwrap();
    root.create_file_as(&bob, "/tmp/d/new").unwrap();
    assert!(matches!(
        root.open_directory("/tmp/d"),
        Err(FsError::OwnershipContextRequired)
    ));
    assert!(matches!(
        root.list("/tmp/d"),
        Err(FsError::OwnershipContextRequired)
    ));
    assert!(matches!(
        root.read_file("/tmp/d/f", 32),
        Err(FsError::OwnershipContextRequired)
    ));
}
#[test]
fn directory_rename_updates_all_catalog_descendants_and_preserves_ids() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    root.create_directory_as(&alice, "/tmp/tree", opts(0o077))
        .unwrap();
    root.create_directory_as(&alice, "/tmp/tree/sub", opts(0o077))
        .unwrap();
    root.create_owned_file(&alice, "/tmp/tree/sub/f", b"data", mode(0o600))
        .unwrap();
    let old = info(&root, "/tmp/tree/sub/f");
    let dir = root
        .directory_info_as(&alice, "/tmp/tree/sub")
        .unwrap()
        .unwrap();
    assert!(matches!(
        root.rename("/tmp/tree", "/tmp/moved"),
        Err(FsError::OwnershipContextRequired)
    ));
    root.rename_directory_as(&alice, "/tmp/tree", "/tmp/moved")
        .unwrap();
    let new = info(&root, "/tmp/moved/sub/f");
    assert_eq!(new.id, old.id);
    assert_eq!(new.storage_generation, old.storage_generation);
    assert_eq!(
        root.directory_info_as(&alice, "/tmp/moved/sub")
            .unwrap()
            .unwrap()
            .id,
        dir.id
    );
    assert_eq!(fs::read(path.join("tmp/moved/sub/f")).unwrap(), b"data");
    assert!(!path.join("tmp/tree").exists());
    assert!(matches!(
        root.rename_directory_as(&alice, "/tmp/moved", "/tmp/moved/bad"),
        Err(FsError::InvalidPath)
    ));
}
#[test]
fn unknown_directories_are_not_adopted_and_replaced_markers_do_not_inherit_authority() {
    let path = fixture();
    fs::create_dir(path.join("tmp/legacy")).unwrap();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    assert!(matches!(
        root.create_directory_as(&alice, "/tmp/legacy", opts(0o077)),
        Err(FsError::AlreadyExists)
    ));
    assert!(matches!(
        root.remove_directory_as(&alice, "/tmp/legacy"),
        Err(FsError::LegacyOwnershipUnknown)
    ));
    root.create_directory_as(&alice, "/tmp/d", opts(0o077))
        .unwrap();
    let marker = path.join("tmp/d/.elara-dir-owner");
    let bytes = fs::read(&marker).unwrap();
    fs::remove_file(&marker).unwrap();
    fs::write(&marker, bytes).unwrap();
    assert!(matches!(
        root.list_as(&alice, "/tmp/d"),
        Err(FsError::OwnershipBindingUnconfirmed)
    ));
    assert!(root.remove_directory_as(&alice, "/tmp/d").is_err());
    assert!(marker.exists());
}
#[test]
fn directory_mutations_recover_after_each_namespace_and_catalog_checkpoint() {
    for fault in [
        AccessFault::AfterNamespace,
        AccessFault::AfterFiles,
        AccessFault::AfterCatalog,
    ] {
        let path = fixture();
        let mut root = DataRoot::open(&path).unwrap();
        let alice = user(1000);
        root.access_fault = fault;
        assert!(matches!(
            root.create_directory_as(&alice, "/tmp/d", opts(0o077)),
            Err(FsError::AccessCommitPending(_))
        ));
        root.access_fault = AccessFault::None;
        assert!(root.directory_info_as(&alice, "/tmp/d").unwrap().is_some());
        root.create_owned_file(&alice, "/tmp/d/f", b"keep", mode(0o600))
            .unwrap();
        let id = info(&root, "/tmp/d/f").id;
        root.access_fault = fault;
        assert!(matches!(
            root.rename_directory_as(&alice, "/tmp/d", "/tmp/e"),
            Err(FsError::AccessCommitPending(_))
        ));
        root.access_fault = AccessFault::None;
        assert_eq!(info(&root, "/tmp/e/f").id, id);
        root.remove_file_as(&alice, "/tmp/e/f").unwrap();
        root.access_fault = fault;
        assert!(matches!(
            root.remove_directory_as(&alice, "/tmp/e"),
            Err(FsError::AccessCommitPending(_))
        ));
        root.access_fault = AccessFault::None;
        root.recover_transactions().unwrap();
        assert!(!path.join("tmp/e").exists());
        assert!(!path.join("var/.elara-objects/access.pending").exists());
    }
}
#[test]
fn directory_capability_keeps_grant_after_chmod_and_blocks_windows_move() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    let bob = user(1001);
    root.create_directory_as(&alice, "/tmp/d", opts(0o022))
        .unwrap();
    let mut cursor = root.open_directory_as(&bob, "/tmp/d").unwrap();
    root.set_access_as(&alice, "/tmp/d", Some(mode(0o700)), None, None)
        .unwrap();
    assert!(root.open_directory_as(&bob, "/tmp/d").is_err());
    assert!(cursor.read(256).unwrap().is_empty());
    cursor.refresh().unwrap();
    #[cfg(windows)]
    assert!(matches!(
        root.rename_directory_as(&alice, "/tmp/d", "/tmp/e"),
        Err(FsError::AccessCommitPending(_))
    ));
    drop(cursor);
    root.recover_transactions().unwrap();
}
#[test]
fn umask_changes_only_new_objects_and_zero_mode_does_not_revoke_creation_fd() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    for (mask, file_mode, directory_mode) in [
        (0o077, 0o600, 0o700),
        (0o022, 0o644, 0o755),
        (0o002, 0o664, 0o775),
    ] {
        let f = std::format!("/tmp/f{mask}");
        let d = std::format!("/tmp/d{mask}");
        root.create_file_with_options(&alice, &f, b"", opts(mask))
            .unwrap();
        root.create_directory_as(&alice, &d, opts(mask)).unwrap();
        assert_eq!(info(&root, &f).mode.bits(), file_mode);
        assert_eq!(
            root.directory_info_as(&alice, &d)
                .unwrap()
                .unwrap()
                .mode
                .bits(),
            directory_mode
        );
    }
    let file = root
        .open_file_with_options(&alice, "/tmp/zero", FileMode::CREATE, opts(0o777))
        .unwrap();
    assert_eq!(info(&root, "/tmp/zero").mode.bits(), 0);
    assert_eq!(file.write(b"grant").unwrap(), 5);
    assert!(matches!(
        root.open_file_as(&alice, "/tmp/zero", FileMode::READ),
        Err(FsError::OwnershipDenied)
    ));
    assert!(Umask::new(0o1000).is_err());
}
#[test]
fn groups_and_acl_enforce_mask_named_user_precedence_and_group_union() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let admin = user(0);
    let alice = user(1000);
    let bob = user(1001);
    root.group_add_as(&admin, 5000, "team").unwrap();
    root.group_add_as(&admin, 5001, "otherteam").unwrap();
    root.group_member_as(&admin, 5000, 1001, true).unwrap();
    root.group_member_as(&admin, 5001, 1001, true).unwrap();
    assert!(matches!(
        root.group_member_as(&alice, 5000, 1000, true),
        Err(FsError::OwnershipDenied)
    ));
    root.group_member_as(&admin, 5000, 1000, true).unwrap();
    root.primary_group_as(&admin, 1000, 5000).unwrap();
    root.create_file_with_options(&alice, "/tmp/shared", b"shared", opts(0o007))
        .unwrap();
    assert_eq!(info(&root, "/tmp/shared").owner.gid, 5000);
    assert_eq!(
        root.read_file_as(&bob, "/tmp/shared", 32).unwrap(),
        b"shared"
    );
    let mut acl = Acl::default();
    acl.users.insert(1001, 0);
    acl.mask = Some(7);
    root.set_access_as(&alice, "/tmp/shared", None, None, Some(acl))
        .unwrap();
    assert!(matches!(
        root.read_file_as(&bob, "/tmp/shared", 32),
        Err(FsError::OwnershipDenied)
    ));
    let mut acl = Acl {
        group: Some(0),
        mask: Some(6),
        ..Acl::default()
    };
    acl.groups.insert(5000, 4);
    acl.groups.insert(5001, 2);
    root.set_access_as(&alice, "/tmp/shared", None, None, Some(acl))
        .unwrap();
    root.append_file_as(&bob, "/tmp/shared", b"!").unwrap();
    root.set_access_as(&alice, "/tmp/shared", Some(mode(0o640)), None, None)
        .unwrap();
    assert_eq!(root.acl_as(&alice, "/tmp/shared").unwrap().mask, Some(4));
    assert!(matches!(
        root.append_file_as(&bob, "/tmp/shared", b"bad"),
        Err(FsError::OwnershipDenied)
    ));
    root.group_member_as(&admin, 5000, 1001, false).unwrap();
    root.group_member_as(&admin, 5001, 1001, false).unwrap();
    assert!(root.read_file_as(&bob, "/tmp/shared", 32).is_err());
    let reopened = DataRoot::open(&path).unwrap();
    assert_eq!(reopened.credentials_as(&alice).unwrap().gid, 5000);
}
#[test]
fn acl_future_opens_are_revoked_but_already_granted_capabilities_are_not() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    let bob = user(1001);
    root.create_owned_file(&alice, "/tmp/f", b"x", mode(0o600))
        .unwrap();
    let mut acl = Acl::default();
    acl.users.insert(1001, 6);
    root.set_access_as(&alice, "/tmp/f", None, None, Some(acl))
        .unwrap();
    let file = root.open_file_as(&bob, "/tmp/f", FileMode::READ).unwrap();
    root.set_access_as(&alice, "/tmp/f", Some(mode(0o600)), None, None)
        .unwrap();
    assert!(root.open_file_as(&bob, "/tmp/f", FileMode::READ).is_err());
    assert_eq!(file.read(8).unwrap(), b"x");
}
#[test]
fn task_exit_cleans_exact_registered_objects_but_keeps_persistent_and_unknown_bytes() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    fs::create_dir(path.join("home/u1000")).unwrap();
    fs::write(path.join("tmp/unknown"), b"not ours").unwrap();
    let task = root.begin_task_as(&alice).unwrap();
    let options = CreationOptions {
        umask: Umask::default(),
        scope: Some(&task),
    };
    root.create_directory_as(&alice, "/tmp/task", options)
        .unwrap();
    root.create_file_with_options(&alice, "/tmp/task/f", b"gone", options)
        .unwrap();
    root.create_file_with_options(&alice, "/home/u1000/keep", b"keep", options)
        .unwrap();
    root.create_owned_file(&alice, "/tmp/old-v1", b"persistent API", mode(0o600))
        .unwrap();
    assert_eq!(
        info(&root, "/tmp/task/f").delete_condition,
        DeleteCondition::TaskExit(task.id())
    );
    assert_eq!(root.recover_tasks().unwrap().live, 1);
    // Cleanup is lifecycle authority, not an operation by a chmod-restricted user.
    root.set_access_as(&alice, "/tmp/task", Some(mode(0)), None, None)
        .unwrap();
    let report = task.finish().unwrap();
    assert_eq!(
        (report.files, report.directories, report.deferred),
        (1, 1, 0)
    );
    assert!(!path.join("tmp/task").exists());
    assert_eq!(fs::read(path.join("home/u1000/keep")).unwrap(), b"keep");
    assert_eq!(fs::read(path.join("tmp/unknown")).unwrap(), b"not ours");
    assert!(path.join("tmp/old-v1").exists());
    assert!(root.load_access_catalog().unwrap().tasks.is_empty());
}
#[test]
fn task_cleanup_preserves_replaced_files_and_foreign_directory_children() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    let task = root.begin_task_as(&alice).unwrap();
    let options = CreationOptions {
        umask: Umask::default(),
        scope: Some(&task),
    };
    root.create_directory_as(&alice, "/tmp/task", options)
        .unwrap();
    root.create_file_with_options(&alice, "/tmp/task/f", b"ours", options)
        .unwrap();
    fs::remove_file(path.join("tmp/task/f")).unwrap();
    fs::write(path.join("tmp/task/f"), b"replacement").unwrap();
    fs::write(path.join("tmp/task/extra"), b"foreign").unwrap();
    assert!(matches!(
        task.finish().unwrap().cleanup(),
        CleanupStatus::Deferred(_)
    ));
    assert_eq!(fs::read(path.join("tmp/task/f")).unwrap(), b"replacement");
    assert_eq!(fs::read(path.join("tmp/task/extra")).unwrap(), b"foreign");
    assert_ne!(root.recover_tasks().unwrap().deferred, 0);
}
#[test]
fn scope_mismatch_is_rejected_before_bytes_or_namespace_changes_and_panic_reaps() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let other = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    let bob = user(1001);
    let task = root.begin_task_as(&alice).unwrap();
    let options = CreationOptions {
        umask: Umask::default(),
        scope: Some(&task),
    };
    assert!(matches!(
        other.create_file_with_options(&alice, "/tmp/bad", b"bad", options),
        Err(FsError::TaskScopeMismatch)
    ));
    assert!(matches!(
        root.create_file_with_options(&bob, "/tmp/bad", b"bad", options),
        Err(FsError::TaskScopeMismatch)
    ));
    assert!(!path.join("tmp/bad").exists());
    task.finish().unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let task = root.begin_task_as(&alice).unwrap();
        root.create_file_with_options(
            &alice,
            "/tmp/panic",
            b"gone",
            CreationOptions {
                umask: Umask::default(),
                scope: Some(&task),
            },
        )
        .unwrap();
        panic!("injected task panic");
    }));
    assert!(result.is_err());
    assert!(!path.join("tmp/panic").exists());
}
#[test]
fn policy_and_v1_v2_catalogs_roundtrip_and_reject_noncanonical_invalid_modes() {
    let mut catalog = AccessCatalog::default();
    let bytes = catalog.encode().unwrap();
    assert_eq!(AccessCatalog::decode(&bytes).unwrap(), catalog);
    let mut extra = bytes.clone();
    extra.push(b' ');
    assert!(AccessCatalog::decode(&extra).is_err());
    catalog.next_dir = 2;
    catalog.directories.insert(
        "/tmp/d".into(),
        crate::filesystem::DirectoryInfo {
            id: 1,
            path: "/tmp/d".into(),
            owner: crate::filesystem::FileOwner {
                uid: 1000,
                gid: 1000,
            },
            mode: mode(0o700),
            acl: Acl::default(),
            delete_condition: DeleteCondition::OwnerRequest,
        },
    ).unwrap();
    let valid = catalog.encode().unwrap();
    let invalid = String::from_utf8(valid)
        .unwrap()
        .replace("\"mode\":448", "\"mode\":512");
    assert!(AccessCatalog::decode(invalid.as_bytes()).is_err());
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    root.create_owned_file(&alice, "/tmp/v1", b"keep", mode(0o600))
        .unwrap();
    assert!(fs::read_to_string(path.join("var/.elara-objects/catalog"))
        .unwrap()
        .starts_with("ELARA_OWNERSHIP_CATALOG_V1\n"));
    let task = root.begin_task_as(&alice).unwrap();
    root.create_file_with_options(
        &alice,
        "/tmp/v2",
        b"gone",
        CreationOptions {
            umask: Umask::default(),
            scope: Some(&task),
        },
    )
    .unwrap();
    let bytes = fs::read(path.join("var/.elara-objects/catalog")).unwrap();
    assert_eq!(
        crate::filesystem::ownership_model::Catalog::decode(&bytes)
            .unwrap()
            .encode()
            .unwrap(),
        bytes
    );
    assert!(bytes.starts_with(b"ELARA_OWNERSHIP_CATALOG_V2\n"));
    task.finish().unwrap();
    assert!(path.join("tmp/v1").exists());
    // Sticky is now supported for directories; old catalog headers still
    // cannot carry it (the forged V1 catalog assertion above stays intact).
    assert_eq!(
        serde_json::from_str::<VirtualMode>("512").unwrap().bits(),
        0o1000
    );
    assert_eq!(
        serde_json::from_str::<VirtualMode>("2048").unwrap().bits(),
        0o4000
    );
    catalog.directories.inode_mut("/tmp/d").unwrap().mode = mode(0o4700);
    assert!(catalog.encode().is_err());
}
#[test]
#[cfg(windows)]
fn unicode_directory_aliases_cannot_bypass_marker_permissions() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let alice = user(1000);
    let bob = user(1001);
    root.create_directory_as(&alice, "/tmp/Äprivate", opts(0o077))
        .unwrap();
    // Unknown legacy child still remains behind its managed parent's search gate.
    fs::write(path.join("tmp/Äprivate/legacy"), b"hidden").unwrap();
    assert!(matches!(
        root.list_as(&bob, "/tmp/äprivate"),
        Err(FsError::OwnershipDenied)
    ));
    assert!(matches!(
        root.read_file_as(&bob, "/tmp/äprivate/legacy", 32),
        Err(FsError::OwnershipDenied)
    ));
    assert!(matches!(
        root.list("/tmp/äprivate"),
        Err(FsError::OwnershipContextRequired)
    ));
    assert!(matches!(
        root.create_file_as(&bob, "/tmp/äprivate/new"),
        Err(FsError::OwnershipDenied)
    ));
    assert!(!path.join("tmp/Äprivate/new").exists());
}

#[test]
fn runtime_state_requires_scope_and_reaps_only_registered_objects() {
    use crate::filesystem::OwnedStorageClass;
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let admin = user(0);
    fs::write(path.join("run/unknown"), b"foreign").unwrap();
    assert!(matches!(
        root.create_file_with_options(&admin, "/run/no-scope", b"bad", opts(0o077)),
        Err(FsError::TaskScopeMismatch)
    ));
    assert!(matches!(
        root.create_directory_as(&admin, "/run/no-dir", opts(0o077)),
        Err(FsError::TaskScopeMismatch)
    ));
    assert!(matches!(
        root.create_file("/run/raw"),
        Err(FsError::OwnershipContextRequired)
    ));
    assert!(!path.join("run/no-scope").exists());
    assert!(!path.join("run/no-dir").exists());
    assert!(!path.join("run/raw").exists());
    let task = root.begin_task_as(&admin).unwrap();
    let options = CreationOptions {
        umask: Umask::default(),
        scope: Some(&task),
    };
    root.create_directory_as(&admin, "/run/task", options)
        .unwrap();
    root.create_file_with_options(&admin, "/run/task/state", b"ready", options)
        .unwrap();
    let state = info(&root, "/run/task/state");
    assert_eq!(state.storage_class, OwnedStorageClass::RuntimeState);
    assert_eq!(state.delete_condition, DeleteCondition::TaskExit(task.id()));
    let fd = root
        .open_file_as(&admin, "/run/task/state", FileMode::READ)
        .unwrap();
    assert_eq!(fd.read(16).unwrap(), b"ready");
    drop(fd);
    let before = fs::read(path.join("var/.elara-objects/catalog")).unwrap();
    assert!(before.starts_with(b"ELARA_OWNERSHIP_CATALOG_V4"));
    let decoded = crate::filesystem::ownership_model::Catalog::decode(&before).unwrap();
    assert_eq!(decoded.encode().unwrap(), before);
    let access = fs::read(path.join("var/.elara-objects/access.catalog")).unwrap();
    assert!(access.starts_with(b"ELARA_ACCESS_CATALOG_V5"));
    assert_eq!(
        AccessCatalog::decode(&access).unwrap().encode().unwrap(),
        access
    );
    assert!(root
        .rename_as(&admin, "/run/task/state", "/root/escaped")
        .is_err());
    assert!(root
        .rename_directory_as(&admin, "/run/task", "/tmp/escaped")
        .is_err());
    root.rename_file_as(&admin, "/run/task/state", "/run/task/renamed")
        .unwrap();
    let other = root.begin_task_as(&admin).unwrap();
    root.create_file_with_options(
        &admin,
        "/run/live",
        b"live",
        CreationOptions {
            umask: Umask::default(),
            scope: Some(&other),
        },
    )
    .unwrap();
    assert_eq!(root.recover_tasks().unwrap().live, 2);
    let report = task.finish().unwrap();
    assert_eq!(
        (report.files, report.directories, report.deferred),
        (1, 1, 0)
    );
    assert!(!path.join("run/task").exists());
    assert!(path.join("run/live").exists());
    assert_eq!(fs::read(path.join("run/unknown")).unwrap(), b"foreign");
    other.finish().unwrap();
}
#[test]
fn runtime_state_recovery_defers_replacements_and_rejects_forged_catalogs() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let admin = user(0);
    let task = root.begin_task_as(&admin).unwrap();
    let options = CreationOptions {
        umask: Umask::default(),
        scope: Some(&task),
    };
    root.create_directory_as(&admin, "/var/run", options)
        .unwrap();
    root.create_file_with_options(&admin, "/var/run/state", b"owned", options)
        .unwrap();
    let mut catalog = crate::filesystem::ownership_model::Catalog::decode(
        &fs::read(path.join("var/.elara-objects/catalog")).unwrap(),
    )
    .unwrap();
    catalog
        .entries
        .inode_mut("/var/run/state")
        .unwrap()
        .delete_condition = DeleteCondition::OwnerRequest;
    assert!(catalog.encode().is_err());
    let other = DataRoot::open(&path).unwrap();
    assert!(matches!(
        other.create_file_with_options(&admin, "/run/wrong", b"bad", options),
        Err(FsError::TaskScopeMismatch)
    ));
    fs::rename(path.join("var/run/state"), path.join("var/run/original")).unwrap();
    fs::write(path.join("var/run/state"), b"replacement").unwrap();
    assert!(task.finish().unwrap().deferred > 0);
    assert!(root.recover_tasks().unwrap().deferred > 0);
    assert_eq!(
        fs::read(path.join("var/run/state")).unwrap(),
        b"replacement"
    );
    assert_eq!(fs::read(path.join("var/run/original")).unwrap(), b"owned");
}

#[test]
fn runtime_state_preserves_legacy_owner_directory_catalog_without_adopting_it() {
    let mut catalog = AccessCatalog {
        next_dir: 2,
        ..AccessCatalog::default()
    };
    catalog.directories.insert(
        "/run/legacy".into(),
        crate::filesystem::DirectoryInfo {
            id: 1,
            path: "/run/legacy".into(),
            owner: crate::filesystem::FileOwner { uid: 0, gid: 0 },
            mode: mode(0o700),
            acl: Acl::default(),
            delete_condition: DeleteCondition::OwnerRequest,
        },
    ).unwrap();
    let bytes = catalog.encode().unwrap();
    assert!(bytes.starts_with(b"ELARA_ACCESS_CATALOG_V1"));
    let loaded = AccessCatalog::decode(&bytes).unwrap();
    assert_eq!(loaded, catalog);
    assert_eq!(
        loaded.directories.get("/run/legacy").unwrap().delete_condition,
        DeleteCondition::OwnerRequest
    );
    assert!(loaded.tasks.is_empty());
}

#[test]
fn host_cwd_search_only_clone_and_independent_root_block_retarget() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let owner = user(1000);
    let guest = user(1001);
    root.create_directory_as(&owner, "/tmp/cwd-parent", opts(0)).unwrap();
    root.create_directory_as(&owner, "/tmp/cwd-parent/leaf", opts(0)).unwrap();
    root.set_access_as(&owner, "/tmp/cwd-parent/leaf", Some(mode(0o111)), None, None).unwrap();
    assert!(matches!(root.list_as(&guest, "/tmp/cwd-parent/leaf"), Err(FsError::OwnershipDenied)));
    let pin = root.working_directory_as(&guest, "/tmp/cwd-parent/leaf").unwrap();
    let copy = pin.clone();
    drop(pin);
    let independent = DataRoot::open(&path).unwrap();
    assert!(matches!(independent.remove_directory_as(&owner, "/tmp/cwd-parent/leaf"), Err(FsError::Vfs(crate::filesystem::vfs::Error::Busy))));
    assert!(matches!(independent.rename_directory_as(&owner, "/tmp/cwd-parent", "/tmp/cwd-moved"), Err(FsError::Vfs(crate::filesystem::vfs::Error::Busy))));
    drop(copy);
    independent.remove_directory_as(&owner, "/tmp/cwd-parent/leaf").unwrap();
    independent.create_directory_as(&owner, "/tmp/cwd-parent/leaf", opts(0)).unwrap();
    independent.remove_directory_as(&owner, "/tmp/cwd-parent/leaf").unwrap();
    independent.remove_directory_as(&owner, "/tmp/cwd-parent").unwrap();
}

#[test]
fn host_cwd_replacement_aliases_permissions_and_unknown_directories() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let owner = user(1000);
    let guest = user(1001);
    root.create_directory_as(&owner, "/tmp/cwd-target", opts(0)).unwrap();
    root.create_directory_as(&owner, "/tmp/cwd-source", opts(0)).unwrap();
    root.symlink_as(&owner, "cwd-target", "/tmp/cwd-alias").unwrap();
    let pin = root.working_directory_as(&guest, "/tmp/cwd-alias").unwrap();
    assert_eq!(pin.path(), "/tmp/cwd-target");
    root.remove_file_as(&owner, "/tmp/cwd-alias").unwrap();
    assert!(matches!(root.rename_replace_host_as(&owner, "/tmp/cwd-source", "/tmp/cwd-target"), Err(FsError::Vfs(crate::filesystem::vfs::Error::Busy))));
    assert!(!path.join("var/.elara-objects/access.pending").exists());
    root.set_access_as(&owner, "/tmp/cwd-target", Some(mode(0o700)), None, None).unwrap();
    assert!(matches!(pin.validate_as(&root, &guest), Err(FsError::OwnershipDenied)));
    root.set_access_as(&owner, "/tmp/cwd-target", Some(mode(0o711)), None, None).unwrap();
    pin.validate_as(&root, &guest).unwrap();
    // An ordinary file lease cannot be mistaken for the directory's ID domain.
    root.create_owned_file(&owner, "/tmp/cwd-file", b"payload", mode(0o600)).unwrap();
    let file = root.open_file_as(&owner, "/tmp/cwd-file", FileMode::READ).unwrap();
    assert_eq!(file.read_at(0, 7).unwrap(), b"payload");
    drop(pin);
    root.remove_directory_as(&owner, "/tmp/cwd-target").unwrap();
    root.remove_directory_as(&owner, "/tmp/cwd-source").unwrap();
    fs::create_dir(path.join("tmp/cwd-unknown")).unwrap();
    assert!(matches!(root.working_directory_as(&owner, "/tmp/cwd-unknown"), Err(FsError::LegacyOwnershipUnknown)));
    root.working_directory_as(&guest, "/tmp").unwrap();
}

#[test]
fn host_cwd_same_bytes_marker_replacement_is_not_identity() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let owner = user(1000);
    root.create_directory_as(&owner, "/tmp/cwd-marked", opts(0)).unwrap();
    let pin = root.working_directory_as(&owner, "/tmp/cwd-marked").unwrap();
    let marker = path.join("tmp/cwd-marked/.elara-dir-owner");
    let bytes = fs::read(&marker).unwrap();
    fs::remove_file(&marker).unwrap();
    fs::write(&marker, &bytes).unwrap();
    assert!(matches!(pin.validate_as(&root, &owner), Err(FsError::OwnershipBindingUnconfirmed)));
}

#[test]
fn host_cwd_incomplete_lease_birth_recovers_before_reacquisition() {
    use crate::filesystem::host_locks::HostOpenFault;
    for fault in [HostOpenFault::BeforeContainerPublish, HostOpenFault::AfterContainerCreate,
        HostOpenFault::AfterOwner, HostOpenFault::AfterPayload, HostOpenFault::AfterWitness] {
        let path = fixture();
        let mut root = DataRoot::open(&path).unwrap();
        let owner = user(1000);
        root.create_directory_as(&owner, "/tmp/cwd-fault", opts(0)).unwrap();
        root.host_open_fault = fault;
        assert!(root.working_directory_as(&owner, "/tmp/cwd-fault").is_err());
        root.host_open_fault = HostOpenFault::None;
        let pin = root.working_directory_as(&owner, "/tmp/cwd-fault").unwrap();
        assert!(matches!(root.remove_directory_as(&owner, "/tmp/cwd-fault"), Err(FsError::Vfs(crate::filesystem::vfs::Error::Busy))));
        drop(pin);
        root.remove_directory_as(&owner, "/tmp/cwd-fault").unwrap();
    }
}

#[test]
fn host_cwd_task_exit_defers_pinned_directory_then_recovers_after_release() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let owner = user(1000);
    let task = root.begin_task_as(&owner).unwrap();
    root.create_directory_as(&owner, "/tmp/cwd-task", CreationOptions { umask: Umask::default(), scope: Some(&task) }).unwrap();
    let pin = root.working_directory_as(&owner, "/tmp/cwd-task").unwrap();
    let report = task.finish().unwrap();
    assert_eq!(report.directories, 0);
    assert_eq!(report.deferred, 1);
    assert!(path.join("tmp/cwd-task").is_dir());
    pin.validate_as(&root, &owner).unwrap();
    drop(pin);
    let report = root.recover_tasks().unwrap();
    assert_eq!(report.directories, 1);
    assert_eq!(report.deferred, 0);
    assert!(!path.join("tmp/cwd-task").exists());
}
