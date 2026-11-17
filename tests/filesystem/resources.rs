use super::fixture;
use crate::filesystem::*;
use crate::users::Account;
use std::{assert, assert_eq, matches};
fn admin() -> Account {
    Account {
        identity: Default::default(),
        name: "root".into(),
        uid: 0,
        home: "/root".into(),
    }
}
fn limits(bytes: u64, objects: u64) -> ResourceLimits {
    ResourceLimits { bytes, objects }
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
    match root.owned_file_info(path).unwrap() {
        OwnershipState::Managed(info) => info,
        other => panic!("{other:?}"),
    }
}
#[test]
fn resources_registration_and_catalogs_roundtrip_without_adopting_existing_data() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    assert!(matches!(
        root.create_file_as(&user, "/opt/unregistered"),
        Err(FsError::OwnershipContextRequired)
    ));
    std::fs::create_dir(path.join("opt/existing")).unwrap();
    std::fs::write(path.join("opt/existing/unknown"), b"keep").unwrap();
    assert!(matches!(
        root.register_resource_as(
            &user,
            "/opt/existing",
            ResourceKind::Application,
            limits(16, 8)
        ),
        Err(FsError::AlreadyExists)
    ));
    root.register_resource_as(&user, "/opt/app", ResourceKind::Application, limits(16, 8))
        .unwrap();
    let (record, usage) = root.resource_info_as(&user, "/opt/app").unwrap();
    assert_eq!(
        usage,
        ResourceUsage {
            bytes: 0,
            objects: 1
        }
    );
    root.create_owned_file(
        &user,
        "/opt/app/file",
        b"payload",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    assert_eq!(
        info(&root, "/opt/app/file").delete_condition,
        DeleteCondition::ResourceDelete(record.id)
    );
    assert_eq!(
        info(&root, "/opt/app/file").storage_class,
        OwnedStorageClass::Application
    );
    let access = root.load_access_catalog().unwrap();
    let encoded = access.encode().unwrap();
    assert!(encoded.starts_with(b"ELARA_ACCESS_CATALOG_V6\n"));
    assert_eq!(
        super::super::access_model::AccessCatalog::decode(&encoded)
            .unwrap()
            .encode()
            .unwrap(),
        encoded
    );
    let store = root.existing_owner_store().unwrap().unwrap();
    let mut files = root.load_owner_catalog(&store).unwrap();
    let encoded = files.encode().unwrap();
    assert!(encoded.starts_with(b"ELARA_OWNERSHIP_CATALOG_V5\n"));
    assert_eq!(
        super::super::ownership_model::Catalog::decode(&encoded)
            .unwrap()
            .encode()
            .unwrap(),
        encoded
    );
    files
        .entries
        .inode_mut("/opt/app/file")
        .unwrap()
        .delete_condition = DeleteCondition::OwnerRequest;
    assert!(files.encode().is_err());
    drop(root);
    let reopened = DataRoot::open(&path).unwrap();
    assert_eq!(
        reopened.resource_info_as(&user, "/opt/app").unwrap().0,
        record
    );
    assert_eq!(
        reopened.read_file_as(&user, "/opt/app/file", 16).unwrap(),
        b"payload"
    );
    assert_eq!(
        std::fs::read(path.join("opt/existing/unknown")).unwrap(),
        b"keep"
    );
}
#[test]
fn resources_all_growth_channels_enforce_quota_before_bytes_or_privilege_change() {
    let root = DataRoot::open(&fixture()).unwrap();
    let user = admin();
    root.register_resource_as(&user, "/opt/app", ResourceKind::Application, limits(8, 8))
        .unwrap();
    root.create_owned_file(
        &user,
        "/opt/app/f",
        b"12345678",
        VirtualMode::new(0o4755).unwrap(),
    )
    .unwrap();
    let file = root.open_file_as(&user, "/opt/app/f", rw()).unwrap();
    assert!(matches!(
        file.write_at(8, b"X"),
        Err(FsError::ResourceQuota)
    ));
    file.seek(std::io::SeekFrom::End(0)).unwrap();
    assert!(matches!(file.write(b"X"), Err(FsError::ResourceQuota)));
    assert!(matches!(file.truncate(9), Err(FsError::ResourceQuota)));
    assert!(matches!(
        root.append_owned_file(&user, "/opt/app/f", b"X"),
        Err(FsError::ResourceQuota)
    ));
    assert!(matches!(
        root.replace_owned_file(&user, "/opt/app/f", b"123456789"),
        Err(FsError::ResourceQuota)
    ));
    assert!(matches!(
        root.create_owned_file(
            &user,
            "/opt/app/extra",
            b"X",
            VirtualMode::new(0o600).unwrap()
        ),
        Err(FsError::ResourceQuota)
    ));
    let append = FileMode::new(
        FileAccess::WriteOnly,
        FileCreation::Existing,
        FileBehavior::Append,
    )
    .unwrap();
    let other = root.open_file_as(&user, "/opt/app/f", append).unwrap();
    assert!(matches!(other.write(b"X"), Err(FsError::ResourceQuota)));
    drop(other);
    assert_eq!(file.read_at(0, 16).unwrap(), b"12345678");
    assert_eq!(info(&root, "/opt/app/f").mode.bits(), 0o4755);
    assert!(!root.root().join("opt/app/extra").exists());
    // Negative control: same old FD can grow after raising this resource quota.
    root.resource_quota_as(&user, "/opt/app", limits(16, 8))
        .unwrap();
    file.truncate(9).unwrap();
    file.write_at(15, b"X").unwrap();
    assert_eq!(
        root.resource_info_as(&user, "/opt/app").unwrap().1,
        ResourceUsage {
            bytes: 16,
            objects: 2
        }
    );
    assert_eq!(info(&root, "/opt/app/f").mode.bits(), 0o755);
}
#[test]
fn resources_object_quota_and_cross_resource_rename_are_atomic() {
    let root = DataRoot::open(&fixture()).unwrap();
    let user = admin();
    root.register_resource_as(&user, "/opt/a", ResourceKind::Application, limits(16, 2))
        .unwrap();
    root.register_resource_as(&user, "/srv/b", ResourceKind::Application, limits(16, 8))
        .unwrap();
    root.create_file_as(&user, "/opt/a/f").unwrap();
    let before = root.load_access_catalog().unwrap().encode().unwrap();
    assert!(matches!(
        root.create_directory_as(&user, "/opt/a/d", CreationOptions::default()),
        Err(FsError::ResourceQuota)
    ));
    assert_eq!(
        root.load_access_catalog().unwrap().encode().unwrap(),
        before
    );
    assert!(!root.root().join("opt/a/d").exists());
    assert!(matches!(
        root.rename_as(&user, "/opt/a/f", "/srv/b/f"),
        Err(FsError::OwnershipClassMismatch)
    ));
    assert!(matches!(
        root.rename_as(&user, "/opt/a", "/opt/moved"),
        Err(FsError::OwnershipClassMismatch) | Err(FsError::ServiceOwnedPath)
    ));
    assert!(matches!(
        root.create_file("/var/cache/raw"),
        Err(FsError::OwnershipContextRequired)
    ));
    root.rename_as(&user, "/opt/a/f", "/opt/a/renamed").unwrap();
    root.remove_file_as(&user, "/opt/a/renamed").unwrap();
    root.create_directory_as(&user, "/opt/a/d", CreationOptions::default())
        .unwrap();
    assert_eq!(root.resource_info_as(&user, "/opt/a").unwrap().1.objects, 2);
    assert!(root
        .resource_quota_as(&user, "/opt/a", limits(16, 1))
        .is_err());
}
#[test]
fn resources_uninstall_preserves_unknown_names_and_resumes_after_restart() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.register_resource_as(&user, "/opt/a", ResourceKind::Application, limits(64, 8))
        .unwrap();
    root.create_owned_file(
        &user,
        "/opt/a/known",
        b"known",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    std::fs::write(path.join("opt/a/unknown"), b"keep").unwrap();
    let result = root.remove_resource_as(&user, "/opt/a").unwrap();
    assert_eq!(result.files, 1);
    assert_eq!(result.deferred, 1);
    assert_eq!(
        root.resource_info_as(&user, "/opt/a").unwrap().0.state,
        ResourceState::Removing
    );
    assert!(matches!(
        root.create_file_as(&user, "/opt/a/new"),
        Err(FsError::ResourceInactive)
    ));
    drop(root);
    let root = DataRoot::open(&path).unwrap();
    assert_eq!(root.recover_resources().unwrap().deferred, 1);
    assert_eq!(std::fs::read(path.join("opt/a/unknown")).unwrap(), b"keep");
    // Move our fixture's unknown data out rather than deleting/adopting it.
    std::fs::rename(path.join("opt/a/unknown"), path.join("home/preserved")).unwrap();
    assert_eq!(root.recover_resources().unwrap().resources, 1);
    assert!(root.resources_as(&user).unwrap().is_empty());
    assert!(!path.join("opt/a").exists());
    assert_eq!(std::fs::read(path.join("home/preserved")).unwrap(), b"keep");
}
#[test]
fn resources_live_fd_cannot_write_after_removal_begins() {
    let root = DataRoot::open(&fixture()).unwrap();
    let user = admin();
    root.register_resource_as(&user, "/opt/a", ResourceKind::Application, limits(64, 8))
        .unwrap();
    root.create_owned_file(&user, "/opt/a/f", b"keep", VirtualMode::new(0o600).unwrap())
        .unwrap();
    let file = root.open_file_as(&user, "/opt/a/f", rw()).unwrap();
    // Deterministically exercise Removing before any platform-dependent unlink.
    {
        let guard = root.namespace_guard().unwrap();
        let old = root.load_access_catalog().unwrap();
        let mut new = old.clone();
        new.resources.values_mut().next().unwrap().state = ResourceState::Removing;
        new.bump().unwrap();
        root.commit_policy_locked(old, new, None, &guard).unwrap();
    }
    assert!(matches!(
        file.write_at(0, b"X"),
        Err(FsError::ResourceInactive)
    ));
    let report = root.remove_resource_as(&user, "/opt/a").unwrap();
    if report.deferred == 0 {
        assert_eq!(
            report,
            ResourceRecovery {
                resources: 1,
                files: 1,
                directories: 1,
                deferred: 0
            }
        );
        assert!(matches!(
            file.write_at(0, b"X"),
            Err(FsError::ResourceDetached)
        ));
    } else {
        assert_eq!(
            root.resource_info_as(&user, "/opt/a").unwrap().0.state,
            ResourceState::Removing
        );
        assert!(matches!(
            file.write_at(0, b"X"),
            Err(FsError::ResourceInactive)
        ));
    }
    assert_eq!(file.read_at(0, 4).unwrap(), b"keep");
    drop(file);
    root.recover_resources().unwrap();
    assert!(root.resources_as(&user).unwrap().is_empty());
}
#[test]
fn resources_cache_clear_retains_registration_and_unknown_contents() {
    let root = DataRoot::open(&fixture()).unwrap();
    let user = admin();
    root.register_resource_as(&user, "/var/cache/a", ResourceKind::Cache, limits(32, 8))
        .unwrap();
    root.create_directory_as(&user, "/var/cache/a/d", CreationOptions::default())
        .unwrap();
    root.create_owned_file(
        &user,
        "/var/cache/a/d/f",
        b"cache",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    std::fs::write(root.root().join("var/cache/a/unknown"), b"keep").unwrap();
    let id = root.resource_info_as(&user, "/var/cache/a").unwrap().0.id;
    let report = root.clear_cache_as(&user, "/var/cache/a").unwrap();
    assert_eq!(
        report,
        ResourceRecovery {
            resources: 1,
            files: 1,
            directories: 1,
            deferred: 0
        }
    );
    let (record, usage) = root.resource_info_as(&user, "/var/cache/a").unwrap();
    assert_eq!(record.id, id);
    assert_eq!(record.state, ResourceState::Active);
    assert_eq!(
        usage,
        ResourceUsage {
            bytes: 0,
            objects: 1
        }
    );
    root.create_owned_file(
        &user,
        "/var/cache/a/new",
        b"again",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(root.root().join("var/cache/a/unknown")).unwrap(),
        b"keep"
    );
}
#[test]
fn resources_registration_recovers_the_joint_directory_intent() {
    let path = fixture();
    let mut root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.access_fault = super::super::access::AccessFault::AfterNamespace;
    assert!(matches!(
        root.register_resource_as(
            &user,
            "/opt/recover",
            ResourceKind::Application,
            limits(32, 8)
        ),
        Err(FsError::AccessCommitPending(_))
    ));
    drop(root);
    let root = DataRoot::open(&path).unwrap();
    let (record, usage) = root.resource_info_as(&user, "/opt/recover").unwrap();
    assert_eq!(record.state, ResourceState::Active);
    assert_eq!(usage.objects, 1);
    assert_eq!(
        root.directory_info_as(&user, "/opt/recover")
            .unwrap()
            .unwrap()
            .delete_condition,
        DeleteCondition::ResourceDelete(record.id)
    );
    assert!(root
        .register_resource_as(
            &user,
            "/opt/recover",
            ResourceKind::Application,
            limits(32, 8)
        )
        .is_err());
}

#[test]
fn resources_replaced_generation_and_reused_name_never_revive_old_fd() {
    let root = DataRoot::open(&fixture()).unwrap();
    let user = admin();
    root.register_resource_as(&user, "/opt/a", ResourceKind::Application, limits(16, 8))
        .unwrap();
    root.create_owned_file(&user, "/opt/a/f", b"old", VirtualMode::new(0o600).unwrap())
        .unwrap();
    let old = root.open_file_as(&user, "/opt/a/f", rw()).unwrap();
    root.replace_owned_file(&user, "/opt/a/f", b"new").unwrap();
    assert!(matches!(
        old.write_at(0, b"X"),
        Err(FsError::ResourceDetached)
    ));
    assert_eq!(old.read_at(0, 3).unwrap(), b"old");
    assert_eq!(root.read_file_as(&user, "/opt/a/f", 16).unwrap(), b"new");
    drop(old);
    let before = root.resource_info_as(&user, "/opt/a").unwrap().0.id;
    root.remove_resource_as(&user, "/opt/a").unwrap();
    root.register_resource_as(&user, "/opt/a", ResourceKind::Application, limits(16, 8))
        .unwrap();
    assert!(root.resource_info_as(&user, "/opt/a").unwrap().0.id.value() > before.value());
}
#[test]
fn resources_reaper_preserves_substituted_file_and_recovers_only_registered_binding() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.register_resource_as(&user, "/opt/a", ResourceKind::Application, limits(32, 8))
        .unwrap();
    root.create_owned_file(
        &user,
        "/opt/a/f",
        b"owned",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    std::fs::rename(path.join("opt/a/f"), path.join("home/registered-original")).unwrap();
    std::fs::write(path.join("opt/a/f"), b"substitute").unwrap();
    let report = root.remove_resource_as(&user, "/opt/a").unwrap();
    assert_eq!(report.files, 0);
    assert_eq!(report.deferred, 2);
    assert_eq!(std::fs::read(path.join("opt/a/f")).unwrap(), b"substitute");
    std::fs::rename(path.join("opt/a/f"), path.join("home/substitute-preserved")).unwrap();
    std::fs::rename(path.join("home/registered-original"), path.join("opt/a/f")).unwrap();
    let report = root.recover_resources().unwrap();
    assert_eq!(
        report,
        ResourceRecovery {
            resources: 1,
            files: 1,
            directories: 1,
            deferred: 0
        }
    );
    assert_eq!(
        std::fs::read(path.join("home/substitute-preserved")).unwrap(),
        b"substitute"
    );
}

#[test]
fn resources_concurrent_domains_cannot_overcommit_the_same_byte_budget() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.register_resource_as(&user, "/opt/a", ResourceKind::Application, limits(1, 8))
        .unwrap();
    root.create_file_as(&user, "/opt/a/left").unwrap();
    root.create_file_as(&user, "/opt/a/right").unwrap();
    let other = DataRoot::open(&path).unwrap();
    let left = root.open_file_as(&user, "/opt/a/left", rw()).unwrap();
    let right = other.open_file_as(&user, "/opt/a/right", rw()).unwrap();
    let barrier = std::sync::Barrier::new(2);
    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            left.write(b"L")
        });
        let b = scope.spawn(|| {
            barrier.wait();
            right.write(b"R")
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    match (&a, &b) {
        (Ok(1), Err(FsError::ResourceQuota)) | (Err(FsError::ResourceQuota), Ok(1)) => {}
        _ => panic!("one write must win and one must hit the quota: {a:?} {b:?}"),
    }
    assert_eq!(
        root.resource_info_as(&user, "/opt/a").unwrap().1,
        ResourceUsage {
            bytes: 1,
            objects: 3
        }
    );
    assert_eq!(
        left.read_at(0, 2).unwrap().len() + right.read_at(0, 2).unwrap().len(),
        1
    );
}

#[test]
fn resources_cache_eviction_restart_blocks_writes_and_preserves_registration_id() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.register_resource_as(&user, "/var/cache/a", ResourceKind::Cache, limits(16, 8))
        .unwrap();
    root.create_owned_file(
        &user,
        "/var/cache/a/f",
        b"cache",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    let id = root.resource_info_as(&user, "/var/cache/a").unwrap().0.id;
    let file = root.open_file_as(&user, "/var/cache/a/f", rw()).unwrap();
    {
        let guard = root.namespace_guard().unwrap();
        let old = root.load_access_catalog().unwrap();
        let mut new = old.clone();
        new.resources.get_mut(&id).unwrap().state = ResourceState::Evicting;
        new.bump().unwrap();
        root.commit_policy_locked(old, new, None, &guard).unwrap();
    }
    assert!(matches!(file.truncate(0), Err(FsError::ResourceInactive)));
    assert!(matches!(
        root.create_file_as(&user, "/var/cache/a/new"),
        Err(FsError::ResourceInactive)
    ));
    drop(file);
    drop(root);
    let root = DataRoot::open(&path).unwrap();
    assert_eq!(
        root.recover_resources().unwrap(),
        ResourceRecovery {
            resources: 1,
            files: 1,
            directories: 0,
            deferred: 0
        }
    );
    let (record, usage) = root.resource_info_as(&user, "/var/cache/a").unwrap();
    assert_eq!(record.id, id);
    assert_eq!(record.state, ResourceState::Active);
    assert_eq!(
        usage,
        ResourceUsage {
            bytes: 0,
            objects: 1
        }
    );
    root.create_file_as(&user, "/var/cache/a/new").unwrap();
}

#[test]
fn resources_application_locations_roundtrip_and_uninstall_as_one_group() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.register_resource_as(&user, "/opt/a", ResourceKind::Application, limits(32, 8))
        .unwrap();
    let app = root.resource_info_as(&user, "/opt/a").unwrap().0.id;
    for (path, kind) in [
        ("/var/cache/a", ResourceKind::Cache),
        ("/root/a-data", ResourceKind::ApplicationData),
    ] {
        root.register_application_location_as(&user, "/opt/a", path, kind, limits(8, 8))
            .unwrap();
        let record = root.resource_info_as(&user, path).unwrap().0;
        assert_eq!(record.association, ResourceAssociation::Application(app));
    }
    root.create_owned_file(
        &user,
        "/root/a-data/f",
        b"data",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    root.create_owned_file(
        &user,
        "/root/unrelated",
        b"keep",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    assert_eq!(
        info(&root, "/root/unrelated").delete_condition,
        DeleteCondition::OwnerRequest
    );
    let data = root.open_file_as(&user, "/root/a-data/f", rw()).unwrap();
    assert!(matches!(data.truncate(9), Err(FsError::ResourceQuota)));
    let encoded = root.load_access_catalog().unwrap().encode().unwrap();
    assert!(encoded.starts_with(b"ELARA_ACCESS_CATALOG_V7\n"));
    assert_eq!(
        super::super::access_model::AccessCatalog::decode(&encoded)
            .unwrap()
            .encode()
            .unwrap(),
        encoded
    );
    drop(data);
    drop(root);
    let root = DataRoot::open(&path).unwrap();
    let report = root.remove_resource_as(&user, "/opt/a").unwrap();
    assert_eq!(
        report,
        ResourceRecovery {
            resources: 3,
            files: 1,
            directories: 3,
            deferred: 0
        }
    );
    assert!(root.resources_as(&user).unwrap().is_empty());
    assert_eq!(
        root.read_file_as(&user, "/root/unrelated", 16).unwrap(),
        b"keep"
    );
}

#[test]
fn resources_application_uninstall_freezes_children_and_resumes_without_adoption() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.register_resource_as(&user, "/opt/a", ResourceKind::Application, limits(32, 8))
        .unwrap();
    root.register_application_location_as(
        &user,
        "/opt/a",
        "/var/cache/a",
        ResourceKind::Cache,
        limits(32, 8),
    )
    .unwrap();
    root.create_owned_file(
        &user,
        "/opt/a/program",
        b"code",
        VirtualMode::new(0o700).unwrap(),
    )
    .unwrap();
    let file = root.open_file_as(&user, "/opt/a/program", rw()).unwrap();
    std::fs::write(path.join("var/cache/a/unknown"), b"keep").unwrap();
    let report = root.remove_resource_as(&user, "/opt/a").unwrap();
    assert_eq!(report.deferred, 1);
    assert_eq!(report.resources, 0);
    assert!(root
        .resources_as(&user)
        .unwrap()
        .iter()
        .all(|r| r.state == ResourceState::Removing));
    assert!(matches!(
        file.write_at(0, b"X"),
        Err(FsError::ResourceInactive)
    ));
    assert!(matches!(
        root.create_file_as(&user, "/var/cache/a/new"),
        Err(FsError::ResourceInactive)
    ));
    assert!(matches!(
        root.register_application_location_as(
            &user,
            "/opt/a",
            "/root/new",
            ResourceKind::ApplicationData,
            limits(8, 8)
        ),
        Err(FsError::ResourceInactive)
    ));
    assert!(!path.join("root/new").exists());
    drop(file);
    drop(root);
    let root = DataRoot::open(&path).unwrap();
    assert_eq!(root.recover_resources().unwrap().deferred, 1);
    assert_eq!(
        std::fs::read(path.join("var/cache/a/unknown")).unwrap(),
        b"keep"
    );
    std::fs::rename(
        path.join("var/cache/a/unknown"),
        path.join("home/preserved-app-cache"),
    )
    .unwrap();
    let report = root.recover_resources().unwrap();
    assert_eq!(
        report,
        ResourceRecovery {
            resources: 2,
            files: 1,
            directories: 2,
            deferred: 0
        }
    );
    root.register_resource_as(&user, "/opt/a", ResourceKind::Application, limits(8, 8))
        .unwrap();
    assert_eq!(root.resources_as(&user).unwrap().len(), 1);
    assert_eq!(
        std::fs::read(path.join("home/preserved-app-cache")).unwrap(),
        b"keep"
    );
}

#[test]
fn resources_application_association_rejects_missing_parent_and_forged_catalog() {
    let root = DataRoot::open(&fixture()).unwrap();
    let user = admin();
    assert!(matches!(
        root.register_application_location_as(
            &user,
            "/opt/missing",
            "/root/a",
            ResourceKind::ApplicationData,
            limits(8, 8)
        ),
        Err(FsError::NotFound)
    ));
    assert!(!root.root().join("root/a").exists());
    root.register_resource_as(&user, "/opt/a", ResourceKind::Application, limits(8, 8))
        .unwrap();
    root.register_application_location_as(
        &user,
        "/opt/a",
        "/root/data",
        ResourceKind::ApplicationData,
        limits(8, 8),
    )
    .unwrap();
    let mut catalog = root.load_access_catalog().unwrap();
    let data = catalog
        .resources
        .values_mut()
        .find(|r| r.kind == ResourceKind::ApplicationData)
        .unwrap();
    data.association = ResourceAssociation::Independent;
    assert!(catalog.encode().is_err());
    let mut catalog = root.load_access_catalog().unwrap();
    catalog
        .resources
        .values_mut()
        .find(|r| r.kind == ResourceKind::Application)
        .unwrap()
        .state = ResourceState::Removing;
    assert!(catalog.encode().is_err()); // freezing the parent alone is never a valid commit
}

#[test]
fn resources_application_uninstall_supersedes_in_progress_cache_eviction() {
    let root = DataRoot::open(&fixture()).unwrap();
    let user = admin();
    root.register_resource_as(&user, "/opt/a", ResourceKind::Application, limits(8, 8))
        .unwrap();
    root.register_application_location_as(
        &user,
        "/opt/a",
        "/var/cache/a",
        ResourceKind::Cache,
        limits(8, 8),
    )
    .unwrap();
    let id = root.resource_info_as(&user, "/var/cache/a").unwrap().0.id;
    {
        let guard = root.namespace_guard().unwrap();
        let old = root.load_access_catalog().unwrap();
        let mut new = old.clone();
        new.resources.get_mut(&id).unwrap().state = ResourceState::Evicting;
        new.bump().unwrap();
        root.commit_policy_locked(old, new, None, &guard).unwrap();
    }
    assert_eq!(
        root.remove_resource_as(&user, "/opt/a").unwrap().resources,
        2
    );
    assert!(root.resources_as(&user).unwrap().is_empty());
}

#[test]
fn resources_associated_data_blocks_raw_and_cross_boundary_mutations() {
    let root = DataRoot::open(&fixture()).unwrap();
    let user = admin();
    root.register_resource_as(&user, "/opt/a", ResourceKind::Application, limits(8, 8))
        .unwrap();
    let path = if cfg!(windows) {
        "/ROOT/data"
    } else {
        "/root/data"
    };
    root.register_application_location_as(
        &user,
        "/opt/a",
        path,
        ResourceKind::ApplicationData,
        limits(8, 8),
    )
    .unwrap();
    root.create_file_as(&user, "/root/data/owned").unwrap();
    root.create_file_as(&user, "/root/ordinary").unwrap();
    assert!(matches!(
        root.create_file("/root/data/raw"),
        Err(FsError::OwnershipContextRequired)
    ));
    assert!(matches!(
        root.rename_as(&user, "/root/data/owned", "/root/escaped"),
        Err(FsError::OwnershipClassMismatch)
    ));
    assert!(matches!(
        root.rename_as(&user, "/root/ordinary", "/root/data/adopted"),
        Err(FsError::OwnershipClassMismatch)
    ));
    assert!(root.root().join("root/ordinary").exists());
    assert!(!root.root().join("root/escaped").exists());
    root.remove_resource_as(&user, "/opt/a").unwrap();
    assert!(root.root().join("root/ordinary").exists());
}

#[test]
fn resources_registered_location_pins_ordinary_ancestor_rename() {
    let root = DataRoot::open(&fixture()).unwrap();
    let user = admin();
    root.create_directory_as(&user, "/root/container", CreationOptions::default())
        .unwrap();
    // Negative control: the same ordinary directory is movable before registration.
    root.rename_as(&user, "/root/container", "/root/moved")
        .unwrap();
    root.rename_as(&user, "/root/moved", "/root/container")
        .unwrap();
    root.register_resource_as(&user, "/opt/a", ResourceKind::Application, limits(8, 8))
        .unwrap();
    root.register_application_location_as(
        &user,
        "/opt/a",
        "/root/container/data",
        ResourceKind::ApplicationData,
        limits(8, 8),
    )
    .unwrap();
    let before = root.load_access_catalog().unwrap().encode().unwrap();
    assert!(matches!(
        root.rename_as(&user, "/root/container", "/root/moved"),
        Err(FsError::ServiceOwnedPath)
    ));
    assert_eq!(
        root.load_access_catalog().unwrap().encode().unwrap(),
        before
    );
    assert!(root.root().join("root/container/data").exists());
    assert!(!root.root().join("root/moved").exists());
    root.remove_resource_as(&user, "/opt/a").unwrap();
    root.rename_as(&user, "/root/container", "/root/moved")
        .unwrap();
}

#[test]
fn sealed_checkpoint_keygen_rejects_resource_lifetime_before_creating_key() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.register_resource_as(
        &user,
        "/opt/keyapp",
        ResourceKind::Application,
        limits(1024, 8),
    )
    .unwrap();
    root.register_application_location_as(
        &user,
        "/opt/keyapp",
        "/root/keydata",
        ResourceKind::ApplicationData,
        limits(1024, 8),
    )
    .unwrap();
    let before = root.resource_info_as(&user, "/root/keydata").unwrap();
    assert!(matches!(
        root.create_image_key_as(&user, "/root/keydata/key"),
        Err(FsError::OwnershipClassMismatch)
    ));
    assert!(!path.join("root/keydata/key").exists());
    assert!(matches!(
        root.owned_file_info("/root/keydata/key").unwrap(),
        OwnershipState::LegacyUnknown
    ));
    assert_eq!(
        root.resource_info_as(&user, "/root/keydata").unwrap(),
        before
    );
    root.create_image_key_as(&user, "/root/key").unwrap();
    assert_eq!(root.read_file_as(&user, "/root/key", 33).unwrap().len(), 32);
    assert_eq!(
        info(&root, "/root/key").delete_condition,
        DeleteCondition::OwnerRequest
    );
}

#[test]
fn resources_registration_publish_denial_keeps_intent_and_recovers_exact_id() {
    let path = fixture();
    let mut root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.access_fault = super::super::access::AccessFault::BeforeDirectoryPublish;
    let failure = root.register_resource_as(
        &user,
        "/var/cache/publish",
        ResourceKind::Cache,
        limits(16, 8),
    );
    assert!(matches!(failure,
        Err(FsError::AccessCommitPending(error)) if matches!(*error,
            FsError::AtomicRenameUnavailable(ref io) if io.kind() == std::io::ErrorKind::PermissionDenied)
    ));
    assert!(!path.join("var/cache/publish").exists());
    let store = path.join("var/.elara-objects");
    assert!(store.join("access.pending").is_file());
    assert!(store.join("directory-1.stage/.elara-dir-owner").is_file());
    assert!(root.load_access_catalog().unwrap().resources.is_empty());
    drop(root);
    let root = DataRoot::open(&path).unwrap();
    let (record, usage) = root.resource_info_as(&user, "/var/cache/publish").unwrap();
    assert_eq!(record.id.value(), 1);
    assert_eq!(record.state, ResourceState::Active);
    assert_eq!(
        usage,
        ResourceUsage {
            bytes: 0,
            objects: 1
        }
    );
    assert!(!store.join("access.pending").exists());
    assert!(!store.join("directory-1.stage").exists());
    root.create_owned_file(
        &user,
        "/var/cache/publish/f",
        b"ok",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    assert_eq!(
        root.read_file_as(&user, "/var/cache/publish/f", 8).unwrap(),
        b"ok"
    );
    drop(root);
    let root = DataRoot::open(&path).unwrap();
    assert_eq!(
        root.resource_info_as(&user, "/var/cache/publish")
            .unwrap()
            .0
            .id,
        record.id
    );
}


#[test]
fn application_domain_confines_paths_links_admin_and_root_family() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.register_resource_as(&user, "/opt/app", ResourceKind::Application, limits(8192, 32)).unwrap();
    root.register_application_location_as(&user, "/opt/app", "/root/app-data", ResourceKind::ApplicationData, limits(8192, 32)).unwrap();
    root.create_owned_file(&user, "/opt/app/image", b"echo app", VirtualMode::new(0o700).unwrap()).unwrap();
    root.create_owned_file(&user, "/root/secret", b"secret", VirtualMode::new(0o600).unwrap()).unwrap();
    root.symlink_as(&user, "/root/secret", "/opt/app/escape").unwrap();
    let actor = root.application_account_as(&user, "/opt/app").unwrap();
    assert!(!actor.is_admin());
    assert_eq!(root.read_file_as(&actor, "/opt/app/image", 64).unwrap(), b"echo app");
    assert!(root.read_file_as(&actor, "/root/secret", 64).is_err());
    assert!(root.read_file_as(&actor, "/opt/app/escape", 64).is_err());
    assert!(root.list_as(&actor, "/root").is_err());
    assert!(root.create_file_as(&actor, "/opt/app/write").is_err());
    root.create_file_as(&actor, "/root/app-data/allowed").unwrap();
    assert!(root.create_file_as(&actor, "/root/app-data-other").is_err());
    assert!(root.resources_as(&actor).is_err());
    assert!(root.upgrade_catalog_as(&actor).is_err());
    assert!(root.application_account_as(&actor, "/opt/app").is_err());
    let other = DataRoot::open(&fixture()).unwrap();
    assert!(other.credentials_as(&actor).is_err());
    assert!(other.read_file_as(&actor, "/opt/app/image", 64).is_err());
    root.resource_quota_as(&user, "/opt/app", limits(16384, 32)).unwrap();
    assert!(root.read_file_as(&actor, "/opt/app/image", 64).is_err(), "changed resource snapshot must revoke the old domain");
}

#[test]
fn application_domain_rejects_setid_cross_mount_and_other_application() {
    let root = DataRoot::open(&fixture()).unwrap();
    let user = admin();
    for app in ["/opt/a", "/opt/b"] {
        root.register_resource_as(&user, app, ResourceKind::Application, limits(8192, 32)).unwrap();
    }
    root.mount_memory_as(&user, "/mnt/external", vfs::Limits { bytes: 8192, inodes: 32 }).unwrap();
    root.create_file_with_options(&user, "/mnt/external/private", b"mounted-secret", CreationOptions::default()).unwrap();
    root.create_owned_file(&user, "/opt/b/private", b"other-app", VirtualMode::new(0o600).unwrap()).unwrap();
    root.symlink_as(&user, "/mnt/external/private", "/opt/a/mounted").unwrap();
    root.symlink_as(&user, "/opt/b/private", "/opt/a/other").unwrap();
    let actor = root.application_account_as(&user, "/opt/a").unwrap();
    for path in ["/opt/a/mounted", "/opt/a/other", "/opt/b/private"] {
        assert!(root.read_file_as(&actor, path, 64).is_err(), "{path}");
    }
    assert!(root.mount_memory_as(&actor, "/mnt/forbidden", vfs::Limits { bytes: 8192, inodes: 32 }).is_err());
    for (name, bits) in [("suid", 0o4700), ("sgid", 0o2700)] {
        let path = std::format!("/opt/a/{name}");
        root.create_owned_file(&user, &path, b"echo forbidden", VirtualMode::new(bits).unwrap()).unwrap();
        let image = root.load_executable_as(&actor, &path, 64).unwrap();
        assert!(image.execution_account(&root, &actor).is_err(), "{name} must not drop the application domain");
    }
    root.create_owned_file(&user, "/opt/a/plain", b"true", VirtualMode::new(0o700).unwrap()).unwrap();
    let image = root.load_executable_as(&actor, "/opt/a/plain", 64).unwrap();
    assert_eq!(image.execution_account(&root, &actor).unwrap(), actor);
}

#[test]
fn application_open_handles_and_clones_revoke_without_revoking_admin_handles() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.register_resource_as(&user, "/opt/app", ResourceKind::Application, limits(8192, 32)).unwrap();
    root.register_application_location_as(&user, "/opt/app", "/root/app-data", ResourceKind::ApplicationData, limits(8192, 32)).unwrap();
    root.create_owned_file(&user, "/root/app-data/file", b"kept", VirtualMode::new(0o600).unwrap()).unwrap();
    // Unknown read-only legacy files must not escape the same revocation policy.
    std::fs::write(path.join("opt/app/legacy"), b"legacy").unwrap();
    let actor = root.application_account_as(&user, "/opt/app").unwrap();
    let handle = root.open_file_as(&actor, "/root/app-data/file", rw()).unwrap();
    let clone = handle.clone();
    let legacy = root.open_file_as(&actor, "/opt/app/legacy", FileMode::READ).unwrap();
    let administrative = root.open_file_as(&user, "/root/app-data/file", rw()).unwrap();
    assert_eq!(handle.read_at(0, 4).unwrap(), b"kept");
    assert_eq!(legacy.read_at(0, 6).unwrap(), b"legacy");
    root.resource_quota_as(&user, "/opt/app", limits(16384, 32)).unwrap();
    for revoked in [&handle, &clone] {
        assert!(revoked.read(1).is_err());
        assert!(revoked.read_at(0, 1).is_err());
        assert!(revoked.write(b"x").is_err());
        assert!(revoked.write_at(0, b"x").is_err());
        assert!(revoked.truncate(0).is_err());
        assert!(revoked.seek(std::io::SeekFrom::Start(0)).is_err());
        assert!(revoked.info().is_err());
        assert!(revoked.sync_data().is_err());
        assert!(revoked.sync_all().is_err());
        assert!(revoked.lock(vfs::LockMode::Shared).is_err());
    }
    assert!(legacy.read_at(0, 6).is_err());
    assert_eq!(administrative.read_at(0, 4).unwrap(), b"kept");
    assert_eq!(std::fs::read(path.join("root/app-data/file")).unwrap(), b"kept");
    // A freshly admitted domain receives the changed grant, not the stale one.
    let current = root.application_account_as(&user, "/opt/app").unwrap();
    assert_eq!(root.open_file_as(&current, "/root/app-data/file", rw()).unwrap().read_at(0, 4).unwrap(), b"kept");
}

#[test]
fn application_virtual_handle_binding_checks_revocation_before_volume_io() {
    let root = DataRoot::open(&fixture()).unwrap();
    let user = admin();
    root.register_resource_as(&user, "/opt/app", ResourceKind::Application, limits(8192, 32)).unwrap();
    root.mount_memory_as(&user, "/mnt/probe", vfs::Limits { bytes: 8192, inodes: 32 }).unwrap();
    root.create_file_with_options(&user, "/mnt/probe/file", b"kept", CreationOptions::default()).unwrap();
    let actor = root.application_account_as(&user, "/opt/app").unwrap();
    // Internal capability binding test; the public path gate denies this mount.
    assert!(root.open_file_as(&actor, "/mnt/probe/file", rw()).is_err());
    let handle = root.open_file_as(&user, "/mnt/probe/file", rw()).unwrap().bind_application(&root, &actor).unwrap();
    assert_eq!(handle.read_at(0, 4).unwrap(), b"kept");
    root.resource_quota_as(&user, "/opt/app", limits(16384, 32)).unwrap();
    assert!(handle.read_at(0, 4).is_err());
    assert!(handle.write_at(0, b"bad").is_err());
    assert!(handle.info().is_err());
    assert_eq!(root.read_file_as(&user, "/mnt/probe/file", 64).unwrap(), b"kept");
}

#[test]
fn application_directory_and_cwd_revoke_without_refreshing_cached_names() {
    let root = DataRoot::open(&fixture()).unwrap();
    let user = admin();
    root.register_resource_as(&user, "/opt/app", ResourceKind::Application, limits(8192, 32)).unwrap();
    root.create_owned_file(&user, "/opt/app/first", b"kept", VirtualMode::new(0o600).unwrap()).unwrap();
    let actor = root.application_account_as(&user, "/opt/app").unwrap();
    let mut cursor = root.open_directory_as(&actor, "/opt/app").unwrap();
    let mut administrative = root.open_directory_as(&user, "/opt/app").unwrap();
    let cwd = root.working_directory_as(&actor, "/opt/app").unwrap();
    cursor.refresh().unwrap();
    assert_eq!(cursor.read(32).unwrap().len(), 1);
    cwd.validate_as(&root, &actor).unwrap();
    root.resource_quota_as(&user, "/opt/app", limits(16384, 32)).unwrap();
    root.create_owned_file(&user, "/opt/app/later", b"new", VirtualMode::new(0o600).unwrap()).unwrap();
    cursor.rewind();
    assert!(cursor.read(32).is_err(), "cached names remain behind the domain gate");
    assert!(cursor.seek(0).is_err());
    assert!(cursor.refresh().is_err(), "revoked refresh must not observe new names");
    assert_eq!(cursor.len(), 1, "failed refresh preserves the old local snapshot");
    assert!(cwd.validate_as(&root, &actor).is_err());
    administrative.refresh().unwrap();
    assert_eq!(administrative.read(32).unwrap().len(), 2);
}

#[test]
fn application_uninstall_revokes_live_handles_cursors_and_cwd() {
    let root = DataRoot::open(&fixture()).unwrap();
    let user = admin();
    root.register_resource_as(&user, "/opt/app", ResourceKind::Application, limits(8192, 32)).unwrap();
    root.create_owned_file(&user, "/opt/app/file", b"kept", VirtualMode::new(0o600).unwrap()).unwrap();
    let actor = root.application_account_as(&user, "/opt/app").unwrap();
    let file = root.open_file_as(&actor, "/opt/app/file", FileMode::READ).unwrap();
    let clone = file.clone();
    let administrative = root.open_file_as(&user, "/opt/app/file", FileMode::READ).unwrap();
    let mut cursor = root.open_directory_as(&actor, "/opt/app").unwrap();
    let cwd = root.working_directory_as(&actor, "/opt/app").unwrap();
    assert!(file.compare_identity(&clone).is_err(), "application identity probes are not granted");
    assert!(administrative.compare_identity(&file).is_err());
    assert!(file.compare_identity(&administrative).is_err());
    assert_eq!(administrative.compare_identity(&administrative).unwrap(), ObjectBinding::SameObject);
    root.remove_resource_as(&user, "/opt/app").unwrap();
    assert!(file.read_at(0, 4).is_err());
    assert!(clone.info().is_err());
    assert!(cursor.read(32).is_err());
    assert!(cursor.refresh().is_err());
    assert!(cwd.validate_as(&root, &actor).is_err());
    assert!(root.application_account_as(&user, "/opt/app").is_err());
}

#[test]
fn application_generation_prevents_quota_and_cache_aba() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.register_resource_as(&user, "/opt/app", ResourceKind::Application, limits(8192, 32)).unwrap();
    root.register_application_location_as(&user, "/opt/app", "/var/cache/app", ResourceKind::Cache, limits(8192, 32)).unwrap();
    root.create_owned_file(&user, "/opt/app/image", b"kept", VirtualMode::new(0o600).unwrap()).unwrap();
    let actor = root.application_account_as(&user, "/opt/app").unwrap();
    let handle = root.open_file_as(&actor, "/opt/app/image", FileMode::READ).unwrap();
    root.resource_quota_as(&user, "/opt/app", limits(16384, 32)).unwrap();
    root.resource_quota_as(&user, "/opt/app", limits(8192, 32)).unwrap();
    assert!(root.read_file_as(&actor, "/opt/app/image", 32).is_err(), "quota ABA must not revive the old domain");
    assert!(handle.read_at(0, 4).is_err());
    let current = root.application_account_as(&user, "/opt/app").unwrap();
    assert_eq!(root.read_file_as(&current, "/opt/app/image", 32).unwrap(), b"kept");
    root.clear_cache_as(&user, "/var/cache/app").unwrap();
    assert_eq!(root.resource_info_as(&user, "/var/cache/app").unwrap().0.state, ResourceState::Active);
    assert!(root.read_file_as(&current, "/opt/app/image", 32).is_err(), "cache state ABA must not revive the old domain");
    let catalog = root.load_access_catalog().unwrap();
    let bytes = catalog.encode().unwrap();
    assert!(bytes.starts_with(b"ELARA_ACCESS_CATALOG_V8
"));
    assert_eq!(super::super::access_model::AccessCatalog::decode(&bytes).unwrap(), catalog);
    assert!(catalog.resources.values().all(|r| r.generation == 2));
    let reopened = DataRoot::open(&path).unwrap();
    assert_eq!(reopened.load_access_catalog().unwrap().encode().unwrap(), bytes);
    let fresh = reopened.application_account_as(&user, "/opt/app").unwrap();
    assert_eq!(reopened.read_file_as(&fresh, "/opt/app/image", 32).unwrap(), b"kept");
}

#[test]
fn application_generation_refuses_overflow_without_publishing() {
    let root = DataRoot::open(&fixture()).unwrap();
    let user = admin();
    root.register_resource_as(&user, "/opt/app", ResourceKind::Application, limits(8192, 32)).unwrap();
    let original = root.load_access_catalog().unwrap();
    let mut old = original.clone();
    old.resources.values_mut().next().unwrap().generation = u64::MAX;
    let mut new = old.clone();
    new.resources.values_mut().next().unwrap().limits.bytes = 16384;
    assert!(matches!(super::super::resource_model::advance_generations(&old, &mut new), Err(FsError::OwnershipIdentityExhausted)));
    assert_eq!(root.load_access_catalog().unwrap(), original);
}

#[test]
fn application_retained_handle_rejects_copied_resource_marker() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.register_resource_as(&user, "/opt/app", ResourceKind::Application, limits(8192, 32)).unwrap();
    root.create_owned_file(&user, "/opt/app/file", b"kept", VirtualMode::new(0o600).unwrap()).unwrap();
    let actor = root.application_account_as(&user, "/opt/app").unwrap();
    let file = root.open_file_as(&actor, "/opt/app/file", FileMode::READ).unwrap();
    assert_eq!(file.read_at(0, 4).unwrap(), b"kept");
    // Test fixture only: equal marker bytes on another object are not authority.
    let marker = path.join("opt/app/.elara-dir-owner");
    let bytes = std::fs::read(&marker).unwrap();
    std::fs::rename(&marker, path.join("opt/app/original-marker")).unwrap();
    std::fs::write(&marker, &bytes).unwrap();
    assert!(file.read_at(0, 4).is_err(), "copied marker must not authorize retained IO");
    assert_eq!(std::fs::read(path.join("opt/app/file")).unwrap(), b"kept");
    assert_eq!(std::fs::read(path.join("opt/app/original-marker")).unwrap(), bytes);
}
