//! Cross-domain Host leases, with explicit before/after lock controls.
use super::fixture;
use crate::filesystem::{
    vfs::{self, LockMode, ShareMode},
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
fn conflict<T>(result: Result<T, FsError>) {
    assert!(matches!(
        result,
        Err(FsError::Vfs(vfs::Error::LockConflict))
    ));
}
#[test]
fn host_locks_gate_all_fd_and_path_io_without_hijacking_identity_probe() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let other = DataRoot::open(&path).unwrap();
    let user = admin();
    root.create_owned_file(&user, "/root/a", b"old", VirtualMode::new(0o700).unwrap())
        .unwrap();
    let a = root.open_file_as(&user, "/root/a", rw()).unwrap();
    let b = other.open_file_as(&user, "/root/a", rw()).unwrap();
    assert_eq!(b.read_at(0, 3).unwrap(), b"old");
    a.lock(LockMode::Exclusive).unwrap();
    assert_eq!(a.compare_identity(&b).unwrap(), ObjectBinding::SameObject);
    assert_eq!(a.read_at(0, 3).unwrap(), b"old");
    conflict(b.read(3));
    conflict(b.read_at(0, 3));
    conflict(b.write(b"x"));
    conflict(b.write_at(0, b"x"));
    conflict(b.truncate(0));
    conflict(b.sync_data());
    conflict(b.sync_all());
    conflict(other.read_file_as(&user, "/root/a", 32));
    conflict(other.read_owned_file(&user, "/root/a", 32));
    conflict(other.append_owned_file(&user, "/root/a", b"x"));
    conflict(other.replace_owned_file(&user, "/root/a", b"new"));
    conflict(other.copy_file_as(&user, "/root/a", "/root/copy"));
    conflict(other.load_executable_as(&user, "/root/a", 32));
    let truncate = FileMode::new(
        FileAccess::ReadWrite,
        FileCreation::Existing,
        FileBehavior::Truncate,
    )
    .unwrap();
    conflict(other.open_file_as(&user, "/root/a", truncate));
    assert_eq!(b.info().unwrap().position, FilePosition::Known(0));
    assert_eq!(fs::read(path.join("root/a")).unwrap(), b"old");
    assert!(!path.join("root/copy").exists());
    let duplicate = a.clone();
    drop(a);
    conflict(b.read_at(0, 3));
    duplicate.unlock().unwrap();
    assert_eq!(b.read_at(0, 3).unwrap(), b"old");
    duplicate.lock(LockMode::Shared).unwrap();
    b.lock(LockMode::Shared).unwrap();
    assert_eq!(b.read_at(0, 3).unwrap(), b"old");
    conflict(b.write(b"x"));
    conflict(b.lock(LockMode::Exclusive));
    // A failed upgrade must retain B's shared lock.
    duplicate.unlock().unwrap();
    conflict(duplicate.write(b"x"));
    b.unlock().unwrap();
    duplicate.lock(LockMode::Exclusive).unwrap();
    drop(duplicate);
    assert_eq!(b.write_at(0, b"new").unwrap(), 3);
    drop(b);
    assert!(!fs::read_dir(path.join("var/.elara-objects"))
        .unwrap()
        .any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".lease")));
}
#[test]
fn host_locks_exclusive_share_and_retired_generations_are_separate() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.create_owned_file(&user, "/root/a", b"old", VirtualMode::new(0o600).unwrap())
        .unwrap();
    let a = root
        .open_file_shared_with_options(
            &user,
            "/root/a",
            rw(),
            ShareMode::Exclusive,
            CreationOptions::default(),
        )
        .unwrap();
    assert!(matches!(
        root.open_file_as(&user, "/root/a", FileMode::READ),
        Err(FsError::Vfs(vfs::Error::Busy))
    ));
    assert!(matches!(
        root.replace_owned_file(&user, "/root/a", b"new"),
        Err(FsError::Vfs(vfs::Error::Busy))
    ));
    let d = a.clone();
    drop(a);
    assert!(matches!(
        root.read_file_as(&user, "/root/a", 32),
        Err(FsError::Vfs(vfs::Error::Busy))
    ));
    drop(d);
    let old = root.open_file_as(&user, "/root/a", rw()).unwrap();
    root.replace_owned_file(&user, "/root/a", b"new").unwrap();
    let new = root.open_file_as(&user, "/root/a", rw()).unwrap();
    old.lock(LockMode::Exclusive).unwrap();
    assert_eq!(new.read_at(0, 3).unwrap(), b"new");
    assert_eq!(old.read_at(0, 3).unwrap(), b"old");
    root.rename_owned_file(&user, "/root/a", "/root/moved")
        .unwrap();
    new.lock(LockMode::Exclusive).unwrap();
    conflict(root.read_file_as(&user, "/root/moved", 32));
    root.remove_owned_file(&user, "/root/moved").unwrap();
    assert_eq!(new.read_at(0, 3).unwrap(), b"new");
}
#[test]
fn host_locks_block_image_and_secret_reads_use_the_same_gate() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.mount_memory_as(
        &user,
        "/mnt/a",
        vfs::Limits {
            bytes: 1024 * 1024,
            inodes: 128,
        },
    )
    .unwrap();
    root.create_block_image_as(&user, "/mnt/a", "/root/disk", 32, 2)
        .unwrap();
    let disk = root.open_file_as(&user, "/root/disk", rw()).unwrap();
    disk.lock(LockMode::Exclusive).unwrap();
    conflict(root.block_image_stats_as(&user, "/root/disk", 2));
    conflict(root.load_block_image_as(&user, "/root/disk", "/mnt/b", 2));
    conflict(root.save_block_image_as(&user, "/mnt/a", "/root/disk", 2));
    conflict(root.repair_block_image_as(&user, "/root/disk", 2));
    disk.unlock().unwrap();
    root.block_image_stats_as(&user, "/root/disk", 2).unwrap();
    root.create_image_key_as(&user, "/root/key").unwrap();
    let key = root.open_file_as(&user, "/root/key", rw()).unwrap();
    key.lock(LockMode::Exclusive).unwrap();
    conflict(root.seal_volume_as(&user, "/mnt/a", "/root/sealed", "/root/key"));
    assert!(!path.join("root/sealed").exists());
    key.unlock().unwrap();
    root.seal_volume_as(&user, "/mnt/a", "/root/sealed", "/root/key")
        .unwrap();
}

#[test]
fn host_locks_recover_abandoned_tokens_but_preserve_unknown_or_missing_held_markers() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.create_owned_file(&user, "/root/a", b"data", VirtualMode::new(0o600).unwrap())
        .unwrap();
    let OwnershipState::Managed(info) = root.owned_file_info("/root/a").unwrap() else {
        panic!("managed");
    };
    let register = || {
        let guard = root.namespace_guard().unwrap();
        root.register_host_open_locked(&info, ShareMode::Exclusive, false, &guard)
            .unwrap()
    };
    let (id, token) = register();
    let marker = path
        .join("var/.elara-objects")
        .join(std::format!("open-{id}.lease"))
        .join("payload");
    assert!(matches!(
        root.read_file_as(&user, "/root/a", 16),
        Err(FsError::Vfs(vfs::Error::Busy))
    ));
    drop(token); // process-death equivalent: no OpenLease destructor, record remains Held
    assert!(marker.exists());
    assert_eq!(root.read_file_as(&user, "/root/a", 16).unwrap(), b"data");
    assert!(!marker.exists());
    let (id, token) = register();
    drop(token);
    let marker = path
        .join("var/.elara-objects")
        .join(std::format!("open-{id}.lease"))
        .join("payload");
    let expected = fs::read(&marker).unwrap();
    fs::write(&marker, b"unknown").unwrap();
    assert!(matches!(
        root.read_file_as(&user, "/root/a", 16),
        Err(FsError::OwnershipBindingUnconfirmed)
    ));
    assert_eq!(fs::read(&marker).unwrap(), b"unknown");
    fs::write(&marker, &expected).unwrap(); // repair only our deliberately corrupted fixture
    root.read_file_as(&user, "/root/a", 16).unwrap();
    let (id, token) = register();
    drop(token);
    let marker = path
        .join("var/.elara-objects")
        .join(std::format!("open-{id}.lease"))
        .join("payload");
    fs::remove_file(&marker).unwrap();
    assert!(matches!(
        root.read_file_as(&user, "/root/a", 16),
        Err(FsError::OwnershipBindingUnconfirmed)
    ));
    assert_eq!(fs::read(path.join("root/a")).unwrap(), b"data");
}

#[test]
fn host_locks_pending_and_retiring_recovery_is_exact_and_ids_never_reuse() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.create_owned_file(&user, "/root/a", b"data", VirtualMode::new(0o600).unwrap())
        .unwrap();
    let base = path.join("var/.elara-objects");
    let unknown = base.join("open-9000.lease");
    fs::write(&unknown, b"outside protocol").unwrap();
    let catalog = base.join("open.catalog");
    for (phase, has_marker) in [
        ("Pending", false),
        ("Pending", true),
        ("Retiring", false),
        ("Retiring", true),
    ] {
        let OwnershipState::Managed(info) = root.owned_file_info("/root/a").unwrap() else {
            panic!("managed")
        };
        let guard = root.namespace_guard().unwrap();
        let (id, token) = root
            .register_host_open_locked(&info, ShareMode::Shared, false, &guard)
            .unwrap();
        drop(guard);
        drop(token);
        let bytes = fs::read(&catalog).unwrap();
        let prefix = b"ELARA_OPEN_CATALOG_V4\n";
        let mut data: serde_json::Value =
            serde_json::from_slice(bytes.strip_prefix(prefix).unwrap()).unwrap();
        data["rows"][id.to_string()]["phase"] = phase.into();
        let mut encoded = prefix.to_vec();
        encoded.extend(serde_json::to_vec(&data).unwrap());
        fs::write(&catalog, &encoded).unwrap();
        let lease = base.join(std::format!("open-{id}.lease"));
        if !has_marker {
            for name in ["payload", "witness", "transaction.owner"] {
                fs::remove_file(lease.join(name)).unwrap();
            }
            fs::remove_dir(&lease).unwrap();
        }
        assert_eq!(root.read_file_as(&user, "/root/a", 16).unwrap(), b"data");
        assert!(!lease.exists());
        let bytes = fs::read(&catalog).unwrap();
        let data: serde_json::Value =
            serde_json::from_slice(bytes.strip_prefix(prefix).unwrap()).unwrap();
        assert!(data["rows"].as_object().unwrap().is_empty());
        assert_eq!(data["next"], id + 1);
        assert_eq!(fs::read(&unknown).unwrap(), b"outside protocol");
    }
}

#[test]
fn host_locks_capacity_rejects_create_and_truncate_before_side_effects() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.create_owned_file(&user, "/root/a", b"data", VirtualMode::new(0o600).unwrap())
        .unwrap();
    root.create_directory_as(&user, "/root/cwd-full", CreationOptions::default()).unwrap();
    let mut opens = Vec::new();
    for _ in 0..256 {
        opens.push(root.open_file_as(&user, "/root/a", FileMode::READ).unwrap());
    }
    assert!(matches!(
        root.open_file_as(&user, "/root/a", FileMode::READ),
        Err(FsError::OwnershipCapacity)
    ));
    assert!(matches!(
        root.open_file_as(&user, "/root/new", FileMode::CREATE),
        Err(FsError::OwnershipCapacity)
    ));
    let before = fs::read(path.join("var/.elara-objects/open.catalog")).unwrap();
    assert!(matches!(root.working_directory_as(&user, "/root/cwd-full"), Err(FsError::OwnershipCapacity)));
    assert_eq!(fs::read(path.join("var/.elara-objects/open.catalog")).unwrap(), before);
    let truncate = FileMode::new(
        FileAccess::ReadWrite,
        FileCreation::Existing,
        FileBehavior::Truncate,
    )
    .unwrap();
    assert!(matches!(
        root.open_file_as(&user, "/root/a", truncate),
        Err(FsError::OwnershipCapacity)
    ));
    assert!(!path.join("root/new").exists());
    assert_eq!(fs::read(path.join("root/a")).unwrap(), b"data");
    let duplicate = opens[0].clone();
    opens.remove(0);
    assert!(matches!(
        root.open_file_as(&user, "/root/a", FileMode::READ),
        Err(FsError::OwnershipCapacity)
    ));
    drop(duplicate);
    let pin = root.working_directory_as(&user, "/root/cwd-full").unwrap();
    assert!(matches!(root.open_file_as(&user, "/root/new", FileMode::CREATE), Err(FsError::OwnershipCapacity)));
    drop(pin);
    root.open_file_as(&user, "/root/new", FileMode::CREATE)
        .unwrap();
    drop(opens);
}

#[test]
fn host_locks_same_bytes_substituted_witness_never_retires_live_or_dead_original() {
    for live in [true, false] {
        for component in ["payload", "witness"] {
            let path = fixture();
            let root = DataRoot::open(&path).unwrap();
            let user = admin();
            root.create_owned_file(
                &user,
                "/root/a",
                b"protected",
                VirtualMode::new(0o600).unwrap(),
            )
            .unwrap();
            let OwnershipState::Managed(info) = root.owned_file_info("/root/a").unwrap() else {
                panic!("managed")
            };
            let guard = root.namespace_guard().unwrap();
            let (id, token) = root
                .register_host_open_locked(&info, ShareMode::Shared, false, &guard)
                .unwrap();
            root.set_host_lock_locked(&info, id, Some(LockMode::Exclusive), &guard)
                .unwrap();
            drop(guard);
            let token = if live {
                Some(token)
            } else {
                drop(token);
                None
            };
            let base = path.join("var/.elara-objects");
            let replaced = base.join(std::format!("open-{id}.lease")).join(component);
            let kept = base.join("retained-original-witness");
            let bytes = std::format!(
                "ELARA_OPEN_LEASE_V3\n{id} {} {}\n",
                info.id.value(),
                info.storage_generation
            )
            .into_bytes();
            fs::rename(&replaced, &kept).unwrap();
            fs::write(&replaced, &bytes).unwrap();
            let catalog_before = fs::read(base.join("open.catalog")).unwrap();
            assert!(matches!(
                root.read_file_as(&user, "/root/a", 32),
                Err(FsError::OwnershipBindingUnconfirmed)
            ));
            assert_eq!(fs::read(&replaced).unwrap(), bytes);
            assert_eq!(fs::read(base.join("open.catalog")).unwrap(), catalog_before);
            assert!(kept.exists());
            drop(token);
            assert!(matches!(
                root.read_file_as(&user, "/root/a", 32),
                Err(FsError::OwnershipBindingUnconfirmed)
            ));
            fs::remove_file(&replaced).unwrap();
            fs::rename(&kept, &replaced).unwrap();
            assert_eq!(
                root.read_file_as(&user, "/root/a", 32).unwrap(),
                b"protected"
            );
            assert!(!base.join(std::format!("open-{id}.lease")).exists());
        }
    }
}

#[test]
fn host_locks_legacy_active_catalog_is_not_adopted_and_empty_catalog_preserves_next() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.create_owned_file(&user, "/root/a", b"old", VirtualMode::new(0o600).unwrap())
        .unwrap();
    let catalog = path.join("var/.elara-objects/open.catalog");
    let active=b"ELARA_OPEN_CATALOG_V1\n{\"next\":2,\"rows\":{\"1\":{\"object\":1,\"generation\":1,\"exclusive\":true,\"lock\":\"Exclusive\",\"phase\":\"Held\"}}}";
    fs::write(&catalog, active).unwrap();
    assert!(matches!(
        root.open_file_as(&user, "/root/a", FileMode::READ),
        Err(FsError::OwnershipBindingUnconfirmed)
    ));
    assert_eq!(fs::read(&catalog).unwrap(), active);
    fs::write(
        &catalog,
        b"ELARA_OPEN_CATALOG_V1\n{\"next\":42,\"rows\":{}}",
    )
    .unwrap();
    let file = root.open_file_as(&user, "/root/a", FileMode::READ).unwrap();
    assert!(path
        .join("var/.elara-objects/open-42.lease/payload")
        .exists());
    assert!(fs::read(&catalog)
        .unwrap()
        .starts_with(b"ELARA_OPEN_CATALOG_V4\n"));
    assert_eq!(file.read(3).unwrap(), b"old");
}

#[test]
fn host_locks_container_stage_recovery_preserves_unrecognized_children() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    for foreign in [false, true] {
        let guard = root.namespace_guard().unwrap();
        let (store, name) = root.create_protocol_stage(&guard).unwrap();
        let stage = store.open_dir(&name).unwrap();
        let write = |name: &str, bytes: &[u8]| {
            use std::io::Write;
            let mut file = stage
                .open_with(
                    name,
                    cap_std::fs::OpenOptions::new().write(true).create_new(true),
                )
                .unwrap();
            file.write_all(bytes).unwrap();
        };
        write(
            "transaction.owner",
            crate::filesystem::host_locks::CONTAINER_MAGIC,
        );
        write("payload", b"incomplete unpublished token");
        if foreign {
            write("unrecognized", b"must survive");
        } else {
            stage.hard_link("payload", &stage, "witness").unwrap();
        }
        drop(stage);
        drop(guard);
        root.recover_transactions().unwrap();
        assert_eq!(store.symlink_metadata(&name).is_ok(), foreign);
        if foreign {
            use std::io::Read;
            let mut bytes = Vec::new();
            store
                .open_dir(&name)
                .unwrap()
                .open("unrecognized")
                .unwrap()
                .read_to_end(&mut bytes)
                .unwrap();
            assert_eq!(bytes, b"must survive");
        }
    }
}

#[test]
fn host_locks_io_probes_only_self_and_conflicts_but_startup_inspects_all() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    for name in ["/root/a", "/root/b"] {
        root.create_owned_file(&user, name, b"data", VirtualMode::new(0o600).unwrap())
            .unwrap();
    }
    let OwnershipState::Managed(info) = root.owned_file_info("/root/a").unwrap() else {
        panic!("managed")
    };
    let guard = root.namespace_guard().unwrap();
    let (id, token) = root
        .register_host_open_locked(&info, ShareMode::Shared, false, &guard)
        .unwrap();
    root.set_host_lock_locked(&info, id, Some(LockMode::Shared), &guard)
        .unwrap();
    drop(guard);
    let lease = path
        .join("var/.elara-objects")
        .join(std::format!("open-{id}.lease"));
    let replaced = lease.join("witness");
    let kept = path.join("var/.elara-objects/retained-witness");
    fs::rename(&replaced, &kept).unwrap();
    let bytes = std::format!(
        "ELARA_OPEN_LEASE_V3\n{id} {} {}\n",
        info.id.value(),
        info.storage_generation
    )
    .into_bytes();
    fs::write(&replaced, &bytes).unwrap();
    // Shared lock does not deny reads. A corrupted irrelevant witness must not
    // cause a hidden global scan; the write and explicit full recovery do inspect it.
    assert_eq!(root.read_file_as(&user, "/root/a", 16).unwrap(), b"data");
    assert_eq!(root.read_file_as(&user, "/root/b", 16).unwrap(), b"data");
    assert!(matches!(
        root.append_owned_file(&user, "/root/a", b"x"),
        Err(FsError::OwnershipBindingUnconfirmed)
    ));
    assert!(matches!(
        root.recover_transactions(),
        Err(FsError::OwnershipBindingUnconfirmed)
    ));
    let guard = root.namespace_guard().unwrap();
    assert!(matches!(
        root.set_host_lock_locked(&info, id, None, &guard),
        Err(FsError::OwnershipBindingUnconfirmed)
    ));
    drop(guard);
    drop(token);
    fs::remove_file(&replaced).unwrap();
    fs::rename(&kept, &replaced).unwrap();
    root.append_owned_file(&user, "/root/a", b"x").unwrap();
    assert!(!lease.exists());
    assert_eq!(root.read_file_as(&user, "/root/a", 16).unwrap(), b"datax");
}

#[test]
fn host_locks_retiring_interrupted_unlinks_finish_without_adopting_unknown_bytes() {
    for remaining in [0, 1, 2] {
        let path = fixture();
        let root = DataRoot::open(&path).unwrap();
        let user = admin();
        root.create_owned_file(&user, "/root/a", b"data", VirtualMode::new(0o600).unwrap())
            .unwrap();
        let OwnershipState::Managed(info) = root.owned_file_info("/root/a").unwrap() else {
            panic!("managed")
        };
        let guard = root.namespace_guard().unwrap();
        let (id, token) = root
            .register_host_open_locked(&info, ShareMode::Shared, false, &guard)
            .unwrap();
        drop(guard);
        drop(token);
        let base = path.join("var/.elara-objects");
        let lease = base.join(std::format!("open-{id}.lease"));
        let catalog = base.join("open.catalog");
        let bytes = fs::read(&catalog).unwrap();
        let prefix = b"ELARA_OPEN_CATALOG_V4\n";
        let mut data: serde_json::Value =
            serde_json::from_slice(bytes.strip_prefix(prefix).unwrap()).unwrap();
        data["rows"][id.to_string()]["phase"] = "Retiring".into();
        let mut encoded = prefix.to_vec();
        encoded.extend(serde_json::to_vec(&data).unwrap());
        fs::write(&catalog, encoded).unwrap();
        for name in ["payload", "witness", "transaction.owner"]
            .iter()
            .skip(remaining)
        {
            fs::remove_file(lease.join(name)).unwrap();
        }
        assert_eq!(root.read_file_as(&user, "/root/a", 16).unwrap(), b"data");
        assert!(!lease.exists());
    }
}

#[test]
fn host_locks_directory_publish_denial_leaves_pending_and_recovers_without_touching_data() {
    use crate::filesystem::host_locks::HostOpenFault;
    for fault in [
        HostOpenFault::BeforeContainerPublish,
        HostOpenFault::AfterContainerCreate,
        HostOpenFault::AfterOwner,
        HostOpenFault::AfterPayload,
        HostOpenFault::AfterWitness,
    ] {
        let path = fixture();
        let mut root = DataRoot::open(&path).unwrap();
        let user = admin();
        root.create_owned_file(
            &user,
            "/root/a",
            b"untouched",
            VirtualMode::new(0o600).unwrap(),
        )
        .unwrap();
        root.host_open_fault = fault;
        let truncate = FileMode::new(
            FileAccess::ReadWrite,
            FileCreation::Existing,
            FileBehavior::Truncate,
        )
        .unwrap();
        assert!(
            matches!(root.open_file_as(&user,"/root/a",truncate),Err(FsError::AtomicPublishUnavailable(e)) if e.kind()==std::io::ErrorKind::PermissionDenied)
        );
        assert_eq!(fs::read(path.join("root/a")).unwrap(), b"untouched");
        let catalog_path = path.join("var/.elara-objects/open.catalog");
        let bytes = fs::read(&catalog_path).unwrap();
        let prefix = b"ELARA_OPEN_CATALOG_V4\n";
        let data: serde_json::Value =
            serde_json::from_slice(bytes.strip_prefix(prefix).unwrap()).unwrap();
        assert_eq!(data["rows"]["1"]["phase"], "Pending");
        assert_eq!(data["next"], 2);
        assert_eq!(
            path.join("var/.elara-objects/open-1.lease").exists(),
            fault != HostOpenFault::BeforeContainerPublish
        );
        drop(root);
        let root = DataRoot::open(&path).unwrap();
        let file = root.open_file_as(&user, "/root/a", FileMode::READ).unwrap();
        assert_eq!(file.read(32).unwrap(), b"untouched");
        let bytes = fs::read(&catalog_path).unwrap();
        let data: serde_json::Value =
            serde_json::from_slice(bytes.strip_prefix(prefix).unwrap()).unwrap();
        assert_eq!(data["rows"].as_object().unwrap().len(), 1);
        assert_eq!(data["rows"]["2"]["phase"], "Held");
        assert_eq!(data["next"], 3);
        drop(file);
        assert!(!path.join("var/.elara-objects/open-2.lease").exists());
    }
}

#[test]
fn host_cwd_v3_live_file_lease_migrates_without_id_domain_collision() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.create_owned_file(&user, "/root/cwd-v3-file", b"data", VirtualMode::new(0o600).unwrap()).unwrap();
    root.create_directory_as(&user, "/root/cwd-v3-dir", CreationOptions::default()).unwrap();
    let OwnershipState::Managed(info) = root.owned_file_info("/root/cwd-v3-file").unwrap() else { panic!("managed"); };
    assert_eq!(root.directory_info_as(&user, "/root/cwd-v3-dir").unwrap().unwrap().id, info.id.value());
    let guard = root.namespace_guard().unwrap();
    let (id, token) = root.register_host_open_locked(&info, ShareMode::Exclusive, false, &guard).unwrap();
    drop(guard);
    let catalog = path.join("var/.elara-objects/open.catalog");
    let bytes = fs::read(&catalog).unwrap();
    let offset = bytes.iter().position(|v| *v == 10).unwrap() + 1;
    let mut data: serde_json::Value = serde_json::from_slice(&bytes[offset..]).unwrap();
    data["rows"][id.to_string()].as_object_mut().unwrap().remove("domain");
    let mut v3 = b"ELARA_OPEN_CATALOG_V3".to_vec(); v3.push(10);
    v3.extend(serde_json::to_vec(&data).unwrap());
    fs::write(&catalog, &v3).unwrap();
    assert!(matches!(root.read_file_as(&user, "/root/cwd-v3-file", 16), Err(FsError::Vfs(vfs::Error::Busy))));
    let pin = root.working_directory_as(&user, "/root/cwd-v3-dir").unwrap();
    let bytes = fs::read(&catalog).unwrap();
    assert!(bytes.starts_with(b"ELARA_OPEN_CATALOG_V4"));
    let offset = bytes.iter().position(|v| *v == 10).unwrap() + 1;
    let data: serde_json::Value = serde_json::from_slice(&bytes[offset..]).unwrap();
    let rows = data["rows"].as_object().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[&id.to_string()]["domain"], "File");
    assert!(rows.values().any(|r| r["domain"] == "Directory"));
    assert!(matches!(root.remove_directory_as(&user, "/root/cwd-v3-dir"), Err(FsError::Vfs(vfs::Error::Busy))));
    drop(pin);
    root.remove_directory_as(&user, "/root/cwd-v3-dir").unwrap();
    assert!(matches!(root.read_file_as(&user, "/root/cwd-v3-file", 16), Err(FsError::Vfs(vfs::Error::Busy))));
    drop(token);
    assert_eq!(root.read_file_as(&user, "/root/cwd-v3-file", 16).unwrap(), b"data");
}
