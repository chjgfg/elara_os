//! Migration checks use production gates and separately exercise interrupted states.
use super::*;
use crate::filesystem::{access::AccessFault, ownership::OwnershipFault, *};
use std::{assert, assert_eq, format, fs, matches, path::PathBuf, vec};
fn admin() -> Account {
    Account {
        identity: Default::default(),
        name: "root".into(),
        uid: 0,
        home: "/root".into(),
    }
}
fn setup(directory: bool) -> (PathBuf, DataRoot) {
    let path = crate::test_support::marked_root("joint-catalog");
    let root = DataRoot::open(&path).unwrap();
    root.create_owned_file(
        &admin(),
        "/tmp/file",
        b"original",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    if directory {
        root.create_directory_as(&admin(), "/tmp/dir", CreationOptions::default())
            .unwrap();
    }
    (path, root)
}
fn single_authority(path: &Path) {
    let store = path.join(store_path());
    assert_eq!(
        fs::read(store.join(STORE_OWNER)).unwrap(),
        JOINT_STORE_MAGIC
    );
    assert!(store.join(JOINT_NAME).is_file());
    for name in [FILE_CATALOG, ACCESS_CATALOG, MIGRATION_NAME] {
        assert!(!store.join(name).exists(), "{name}");
    }
}
fn interrupt(root: &DataRoot, fault: CatalogFault) {
    let guard = root.namespace_guard().unwrap();
    assert!(root.upgrade_catalog_locked(&guard, fault).is_err());
}
#[test]
fn joint_catalog_each_checkpoint_recovers_missing_and_present_access_exactly() {
    for directory in [false, true] {
        for fault in [
            CatalogFault::AfterIntent,
            CatalogFault::AfterJoint,
            CatalogFault::AfterMarker,
            CatalogFault::AfterFilesRetired,
            CatalogFault::AfterAccessRetired,
        ] {
            let (path, root) = setup(directory);
            let store = root.existing_owner_store().unwrap().unwrap();
            let files = root.load_owner_catalog(&store).unwrap().encode().unwrap();
            let access = root.load_access_catalog().unwrap().encode().unwrap();
            interrupt(&root, fault);
            let reopened = DataRoot::open(&path).unwrap();
            reopened.recover_transactions().unwrap();
            reopened.recover_transactions().unwrap();
            let (f, a) = reopened.joint_catalogs(&store).unwrap();
            assert_eq!(f.encode().unwrap(), files);
            assert_eq!(a.encode().unwrap(), access);
            assert_eq!(
                reopened.read_file_as(&admin(), "/tmp/file", 64).unwrap(),
                b"original"
            );
            reopened.upgrade_catalog_as(&admin()).unwrap();
            single_authority(&path);
        }
    }
}
#[test]
fn joint_catalog_tampering_never_retires_unknown_bytes() {
    for fault in [
        CatalogFault::AfterIntent,
        CatalogFault::AfterJoint,
        CatalogFault::AfterMarker,
    ] {
        for name in [
            FILE_CATALOG,
            ACCESS_CATALOG,
            JOINT_NAME,
            STORE_OWNER,
            MIGRATION_NAME,
        ] {
            let (path, root) = setup(true);
            interrupt(&root, fault);
            let store = path.join(store_path());
            fs::write(store.join(name), b"foreign").unwrap();
            let before: Vec<_> = [
                FILE_CATALOG,
                ACCESS_CATALOG,
                JOINT_NAME,
                STORE_OWNER,
                MIGRATION_NAME,
            ]
            .iter()
            .map(|n| (n, fs::read(store.join(n)).ok()))
            .collect();
            assert!(root.recover_transactions().is_err(), "{name}");
            for (n, bytes) in before {
                assert_eq!(fs::read(store.join(n)).ok(), bytes, "{n}");
            }
        }
    }
}
#[test]
fn joint_catalog_rejects_orphan_and_legacy_reappearance_without_adoption() {
    let (path, root) = setup(true);
    let store = path.join(store_path());
    fs::write(store.join(JOINT_NAME), b"foreign").unwrap();
    assert!(root.upgrade_catalog_as(&admin()).is_err());
    assert!(!store.join(MIGRATION_NAME).exists());
    assert_eq!(fs::read(store.join(JOINT_NAME)).unwrap(), b"foreign");
    // This test created the foreign file; removing it is not recovery behavior.
    fs::remove_file(store.join(JOINT_NAME)).unwrap();
    root.upgrade_catalog_as(&admin()).unwrap();
    let bytes = fs::read(store.join(JOINT_NAME)).unwrap();
    fs::write(store.join(FILE_CATALOG), b"second authority").unwrap();
    assert!(root.create_file_as(&admin(), "/tmp/rejected").is_err());
    assert!(!path.join("tmp/rejected").exists());
    assert_eq!(fs::read(store.join(JOINT_NAME)).unwrap(), bytes);
    assert_eq!(
        fs::read(store.join(FILE_CATALOG)).unwrap(),
        b"second authority"
    );
}
#[test]
fn joint_catalog_missing_access_is_not_a_present_default_catalog() {
    let (path, root) = setup(false);
    interrupt(&root, CatalogFault::AfterIntent);
    let store = path.join(store_path());
    fs::write(
        store.join(ACCESS_CATALOG),
        AccessCatalog::default().encode().unwrap(),
    )
    .unwrap();
    assert!(root.recover_transactions().is_err());
    assert!(store.join(FILE_CATALOG).exists());
    assert!(store.join(MIGRATION_NAME).exists());
    assert!(!store.join(JOINT_NAME).exists());
}
#[test]
fn joint_catalog_root_only_and_v1_reader_rejects_v2() {
    let (path, root) = setup(true);
    let mut user = admin();
    user.uid = 1000;
    let store = root.existing_owner_store().unwrap().unwrap();
    assert!(matches!(
        root.upgrade_catalog_as(&user),
        Err(FsError::OwnershipDenied)
    ));
    assert!(super::super::transaction::read_magic(
        &store,
        STORE_OWNER,
        b"ELARA_OWNERSHIP_STORE_V1\n"
    )
    .unwrap());
    assert!(!exists(&store, MIGRATION_NAME).unwrap());
    let durability = crate::filesystem::metadata::sync_relative_directory(&root, &store_path());
    assert_eq!(
        root.upgrade_catalog_as(&admin()).unwrap(),
        CatalogUpgrade::Migrated {
            directory_durability: durability
        }
    );
    assert_eq!(
        root.upgrade_catalog_as(&admin()).unwrap(),
        CatalogUpgrade::AlreadyCurrent {
            directory_durability: durability
        }
    );
    assert!(!super::super::transaction::read_magic(
        &store,
        STORE_OWNER,
        b"ELARA_OWNERSHIP_STORE_V1\n"
    )
    .unwrap());
    single_authority(&path);
}
#[test]
fn joint_catalog_codec_is_bounded_exact_and_rejects_truncation() {
    let files = Catalog::empty().encode().unwrap();
    let access = AccessCatalog::default().encode().unwrap();
    let migration = Migration {
        files: &files,
        access: LegacyAccess::Present(&access),
    };
    let bytes = migration.encode().unwrap();
    assert_eq!(
        Migration::decode(&bytes).unwrap().joint().unwrap(),
        migration.joint().unwrap()
    );
    for end in 0..bytes.len() {
        assert!(Migration::decode(&bytes[..end]).is_err());
    }
    let mut extra = bytes.clone();
    extra.push(0);
    assert!(Migration::decode(&extra).is_err());
    let mut huge = bytes;
    huge[MIGRATION_MAGIC.len() + 1..MIGRATION_MAGIC.len() + 9].fill(255);
    assert!(Migration::decode(&huge).is_err());
    assert!(frame(JOINT_MAGIC, &vec![0; MAX_CATALOG_BYTES + 1], &[]).is_err());
    let full = frame(
        JOINT_MAGIC,
        &vec![0; MAX_CATALOG_BYTES],
        &vec![0; MAX_ACCESS_BYTES],
    )
    .unwrap();
    assert!(full.len() < ROOTFS_DEFAULTS.max_transaction_bytes);
}
#[test]
fn joint_catalog_pending_v1_operations_finish_before_migration() {
    for fault in [
        OwnershipFault::AfterStorage,
        OwnershipFault::AfterPublicName,
        OwnershipFault::AfterCatalog,
    ] {
        let (path, root) = setup(true);
        assert!(matches!(
            root.replace_owned_file_with_fault(&admin(), "/tmp/file", b"new", fault),
            Err(FsError::OwnershipCommitPending { .. })
        ));
        root.upgrade_catalog_as(&admin()).unwrap();
        assert_eq!(
            root.read_file_as(&admin(), "/tmp/file", 64).unwrap(),
            b"new"
        );
        single_authority(&path);
    }
    for fault in [
        AccessFault::AfterNamespace,
        AccessFault::AfterFiles,
        AccessFault::AfterCatalog,
    ] {
        let (path, mut root) = setup(true);
        root.create_owned_file(
            &admin(),
            "/tmp/dir/child",
            b"child",
            VirtualMode::new(0o600).unwrap(),
        )
        .unwrap();
        root.access_fault = fault;
        assert!(matches!(
            root.rename_directory_as(&admin(), "/tmp/dir", "/tmp/moved"),
            Err(FsError::AccessCommitPending(_))
        ));
        root.access_fault = AccessFault::None;
        root.upgrade_catalog_as(&admin()).unwrap();
        assert_eq!(
            root.read_file_as(&admin(), "/tmp/moved/child", 64).unwrap(),
            b"child"
        );
        single_authority(&path);
    }
}
#[test]
fn joint_catalog_v2_file_and_directory_transactions_recover_each_checkpoint() {
    for fault in [
        OwnershipFault::AfterStorage,
        OwnershipFault::AfterPublicName,
        OwnershipFault::AfterCreationAcl,
        OwnershipFault::AfterCatalog,
    ] {
        let (path, root) = setup(true);
        root.upgrade_catalog_as(&admin()).unwrap();
        assert!(root
            .create_owned_file_with_fault(
                &admin(),
                "/tmp/new",
                b"new",
                VirtualMode::new(0o600).unwrap(),
                fault
            )
            .is_err());
        root.recover_transactions().unwrap();
        assert_eq!(root.read_file_as(&admin(), "/tmp/new", 64).unwrap(), b"new");
        single_authority(&path);
    }
    for fault in [
        AccessFault::AfterNamespace,
        AccessFault::AfterFiles,
        AccessFault::AfterCatalog,
    ] {
        let (path, mut root) = setup(true);
        root.create_owned_file(
            &admin(),
            "/tmp/dir/child",
            b"child",
            VirtualMode::new(0o600).unwrap(),
        )
        .unwrap();
        root.upgrade_catalog_as(&admin()).unwrap();
        root.access_fault = fault;
        assert!(matches!(
            root.rename_directory_as(&admin(), "/tmp/dir", "/tmp/moved"),
            Err(FsError::AccessCommitPending(_))
        ));
        root.access_fault = AccessFault::None;
        root.recover_transactions().unwrap();
        assert_eq!(
            root.read_file_as(&admin(), "/tmp/moved/child", 64).unwrap(),
            b"child"
        );
        root.remove_file_as(&admin(), "/tmp/moved/child").unwrap();
        root.remove_directory_as(&admin(), "/tmp/moved").unwrap();
        single_authority(&path);
    }
}
#[test]
fn joint_catalog_active_handles_cwd_and_equal_numeric_ids_remain_distinct() {
    let (path, root) = setup(true);
    let actor = admin();
    let OwnershipState::Managed(before) = root.owned_file_info("/tmp/file").unwrap() else {
        panic!()
    };
    let directory = root.directory_info_as(&actor, "/tmp/dir").unwrap().unwrap();
    assert_eq!(before.id.value(), directory.id);
    let fd = root
        .open_file_as(&actor, "/tmp/file", FileMode::READ)
        .unwrap();
    let cwd = root.working_directory_as(&actor, "/tmp/dir").unwrap();
    root.upgrade_catalog_as(&actor).unwrap();
    assert_eq!(
        root.owned_file_info("/tmp/file").unwrap(),
        OwnershipState::Managed(before)
    );
    assert_eq!(
        root.directory_info_as(&actor, "/tmp/dir").unwrap().unwrap(),
        directory
    );
    cwd.validate_as(&root, &actor).unwrap();
    assert!(root.remove_directory_as(&actor, "/tmp/dir").is_err());
    root.remove_owned_file(&actor, "/tmp/file").unwrap();
    assert_eq!(fd.read_at(0, 64).unwrap(), b"original");
    drop(cwd);
    root.remove_directory_as(&actor, "/tmp/dir").unwrap();
    single_authority(&path);
}
#[test]
fn joint_catalog_policy_links_replacement_and_resource_task_lifecycle() {
    let (path, root) = setup(true);
    let actor = admin();
    let scope = root.begin_task_as(&actor).unwrap();
    root.create_file_with_options(
        &actor,
        "/tmp/scoped",
        b"task",
        CreationOptions {
            scope: Some(&scope),
            ..Default::default()
        },
    )
    .unwrap();
    let limits = ResourceLimits {
        bytes: 4096,
        objects: 16,
    };
    root.register_resource_as(&actor, "/opt/app", ResourceKind::Application, limits)
        .unwrap();
    root.register_application_location_as(
        &actor,
        "/opt/app",
        "/var/cache/app",
        ResourceKind::Cache,
        limits,
    )
    .unwrap();
    root.upgrade_catalog_as(&actor).unwrap();
    root.hardlink_as(&actor, "/tmp/file", "/tmp/alias").unwrap();
    root.symlink_as(&actor, "file", "/tmp/link").unwrap();
    root.set_access_as(
        &actor,
        "/tmp/file",
        Some(VirtualMode::new(0o640).unwrap()),
        None,
        None,
    )
    .unwrap();
    root.create_owned_file(
        &actor,
        "/tmp/victim",
        b"victim",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    let old = root
        .open_file_as(&actor, "/tmp/victim", FileMode::READ)
        .unwrap();
    root.rename_replace_as(&actor, "/tmp/file", "/tmp/victim")
        .unwrap();
    assert_eq!(old.read_at(0, 64).unwrap(), b"victim");
    assert_eq!(
        root.read_file_as(&actor, "/tmp/victim", 64).unwrap(),
        b"original"
    );
    root.create_owned_file(
        &actor,
        "/var/cache/app/data",
        b"cache",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    assert_eq!(
        root.resource_info_as(&actor, "/var/cache/app")
            .unwrap()
            .1
            .bytes,
        5
    );
    assert_eq!(scope.finish().unwrap().deferred, 0);
    assert!(!path.join("tmp/scoped").exists());
    single_authority(&path);
}
#[test]
fn joint_catalog_all_seven_file_versions_migrate_without_renumbering() {
    for version in 1..=7 {
        let (path, root) = setup(true);
        let actor = admin();
        let mut scope = None;
        match version {
            2 | 4 => {
                let task = root.begin_task_as(&actor).unwrap();
                let name = if version == 2 {
                    "/tmp/task"
                } else {
                    "/run/task"
                };
                root.create_file_with_options(
                    &actor,
                    name,
                    b"task",
                    CreationOptions {
                        scope: Some(&task),
                        ..Default::default()
                    },
                )
                .unwrap();
                scope = Some(task);
            }
            3 => {
                root.chmod_as(&actor, "/tmp/file", VirtualMode::new(0o4600).unwrap())
                    .unwrap();
            }
            5 => {
                root.register_resource_as(
                    &actor,
                    "/opt/app",
                    ResourceKind::Application,
                    ResourceLimits {
                        bytes: 1024,
                        objects: 8,
                    },
                )
                .unwrap();
                root.create_file_as(&actor, "/opt/app/file").unwrap();
            }
            6 => {
                root.hardlink_as(&actor, "/tmp/file", "/tmp/alias").unwrap();
            }
            7 => {
                root.symlink_as(&actor, "file", "/tmp/link").unwrap();
            }
            _ => {}
        }
        let store = root.existing_owner_store().unwrap().unwrap();
        let before = root.load_owner_catalog(&store).unwrap().encode().unwrap();
        assert!(before.starts_with(format!("ELARA_OWNERSHIP_CATALOG_V{version}\n").as_bytes()));
        root.upgrade_catalog_as(&actor).unwrap();
        assert_eq!(
            root.load_owner_catalog(&store).unwrap().encode().unwrap(),
            before
        );
        assert_eq!(
            root.read_file_as(&actor, "/tmp/file", 64).unwrap(),
            b"original"
        );
        if let Some(task) = scope {
            assert_eq!(task.finish().unwrap().deferred, 0);
        }
        single_authority(&path);
    }
}
#[test]
fn joint_catalog_all_seven_access_versions_migrate_without_policy_loss() {
    for version in 1..=7 {
        let (path, root) = setup(true);
        let actor = admin();
        let mut scope = None;
        match version {
            2 => {
                root.set_default_acl_as(
                    &actor,
                    "/tmp/dir",
                    Some(DefaultAcl::from_mode(VirtualMode::new(0o750).unwrap())),
                )
                .unwrap();
            }
            3 | 4 => {
                let bits = if version == 3 { 0o2700 } else { 0o1700 };
                root.set_access_as(
                    &actor,
                    "/tmp/dir",
                    Some(VirtualMode::new(bits).unwrap()),
                    None,
                    None,
                )
                .unwrap();
            }
            5 => {
                let task = root.begin_task_as(&actor).unwrap();
                root.create_directory_as(
                    &actor,
                    "/run/task",
                    CreationOptions {
                        scope: Some(&task),
                        ..Default::default()
                    },
                )
                .unwrap();
                scope = Some(task);
            }
            6 | 7 => {
                let limits = ResourceLimits {
                    bytes: 1024,
                    objects: 8,
                };
                root.register_resource_as(&actor, "/opt/app", ResourceKind::Application, limits)
                    .unwrap();
                if version == 7 {
                    root.register_application_location_as(
                        &actor,
                        "/opt/app",
                        "/var/cache/app",
                        ResourceKind::Cache,
                        limits,
                    )
                    .unwrap();
                }
            }
            _ => {}
        }
        let before = root.load_access_catalog().unwrap().encode().unwrap();
        assert!(before.starts_with(format!("ELARA_ACCESS_CATALOG_V{version}\n").as_bytes()));
        root.upgrade_catalog_as(&actor).unwrap();
        assert_eq!(
            root.load_access_catalog().unwrap().encode().unwrap(),
            before
        );
        if let Some(task) = scope {
            assert_eq!(task.finish().unwrap().deferred, 0);
        }
        single_authority(&path);
    }
}
#[test]
fn joint_catalog_capacity_and_invalid_task_pair_fail_before_intent() {
    let (path, root) = setup(true);
    let store = path.join(store_path());
    let before = fs::read(store.join(FILE_CATALOG)).unwrap();
    let oversized = vec![b'x'; MAX_CATALOG_BYTES + 1];
    fs::write(store.join(FILE_CATALOG), &oversized).unwrap();
    assert!(matches!(
        root.upgrade_catalog_as(&admin()),
        Err(FsError::OwnershipCapacity)
    ));
    assert!(!store.join(MIGRATION_NAME).exists());
    assert!(!store.join(JOINT_NAME).exists());
    assert_eq!(fs::read(store.join(FILE_CATALOG)).unwrap(), oversized);
    fs::write(store.join(FILE_CATALOG), before).unwrap();
    let task = root.begin_task_as(&admin()).unwrap();
    root.create_file_with_options(
        &admin(),
        "/tmp/task",
        b"task",
        CreationOptions {
            scope: Some(&task),
            ..Default::default()
        },
    )
    .unwrap();
    let access = fs::read(store.join(ACCESS_CATALOG)).unwrap();
    let mut invalid = AccessCatalog::decode(&access).unwrap();
    invalid.tasks.clear();
    fs::write(store.join(ACCESS_CATALOG), invalid.encode().unwrap()).unwrap();
    assert!(matches!(
        root.upgrade_catalog_as(&admin()),
        Err(FsError::InvalidOwnershipRecord)
    ));
    assert!(!store.join(MIGRATION_NAME).exists());
    fs::write(store.join(ACCESS_CATALOG), access).unwrap();
    task.finish().unwrap();
}
#[test]
fn joint_catalog_v2_missing_authority_does_not_recreate_v1() {
    let (path, root) = setup(true);
    root.upgrade_catalog_as(&admin()).unwrap();
    let store = path.join(store_path());
    fs::remove_file(store.join(JOINT_NAME)).unwrap();
    assert!(root.upgrade_catalog_as(&admin()).is_err());
    assert!(root.create_file_as(&admin(), "/tmp/rejected").is_err());
    assert!(!path.join("tmp/rejected").exists());
    assert!(!store.join(FILE_CATALOG).exists());
    assert!(!store.join(ACCESS_CATALOG).exists());
    assert_eq!(
        fs::read(store.join(STORE_OWNER)).unwrap(),
        JOINT_STORE_MAGIC
    );
}
#[test]
fn joint_catalog_crash_child() {
    let Some(path) = std::env::var_os("ELARA_TEST_CATALOG_CRASH_ROOT") else {
        return;
    };
    let root = DataRoot::open(Path::new(&path)).unwrap();
    let index: usize = std::env::var("ELARA_TEST_CATALOG_CRASH_PHASE")
        .unwrap()
        .parse()
        .unwrap();
    let faults = [
        CatalogFault::AfterIntent,
        CatalogFault::AfterJoint,
        CatalogFault::AfterMarker,
        CatalogFault::AfterFilesRetired,
        CatalogFault::AfterAccessRetired,
    ];
    let guard = root.namespace_guard().unwrap();
    assert!(root.upgrade_catalog_locked(&guard, faults[index]).is_err());
    // No Rust drops: the OS releases the process lock and handles.
    std::process::exit(86);
}
#[test]
fn joint_catalog_abrupt_process_exit_recovers_every_published_phase() {
    for index in 0..5 {
        let (path, root) = setup(true);
        let before = root.owned_file_info("/tmp/file").unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "filesystem::namespace_catalog::tests::joint_catalog_crash_child",
                "--nocapture",
            ])
            .env("ELARA_TEST_CATALOG_CRASH_ROOT", &path)
            .env("ELARA_TEST_CATALOG_CRASH_PHASE", index.to_string())
            .output()
            .unwrap();
        assert_eq!(child.status.code(), Some(86), "{child:?}");
        let reopened = DataRoot::open(&path).unwrap();
        reopened.recover_transactions().unwrap();
        reopened.recover_transactions().unwrap();
        assert_eq!(reopened.owned_file_info("/tmp/file").unwrap(), before);
        assert_eq!(
            reopened.read_file_as(&admin(), "/tmp/file", 64).unwrap(),
            b"original"
        );
        single_authority(&path);
    }
}
