//! Managed links are virtual inode metadata, never host symlinks or reparse points.
use super::fixture;
use crate::filesystem::{vfs, *};
use crate::users::Account;
use std::{assert, assert_eq, fs, matches};
fn admin() -> Account {
    Account {
        identity: Default::default(),
        uid: 0,
        name: "root".into(),
        home: "/root".into(),
    }
}
#[test]
fn host_symlinks_follow_relative_absolute_dangling_and_preserve_unlink_target() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.create_owned_file(
        &user,
        "/root/data",
        b"original",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    root.symlink_as(&user, "data", "/root/relative").unwrap();
    root.symlink_as(&user, "/root/relative", "/root/absolute")
        .unwrap();
    assert_eq!(root.readlink_as(&user, "/root/relative").unwrap(), "data");
    assert_eq!(
        root.read_file_as(&user, "/root/absolute", 32).unwrap(),
        b"original"
    );
    let fd = root
        .open_file_as(&user, "/root/absolute", FileMode::READ)
        .unwrap();
    root.replace_owned_file(&user, "/root/relative", b"new")
        .unwrap();
    assert_eq!(root.read_file_as(&user, "/root/data", 32).unwrap(), b"new");
    assert_eq!(fd.read_at(0, 8).unwrap(), b"original");
    root.rename_as(&user, "/root/relative", "/root/moved")
        .unwrap();
    assert_eq!(root.readlink_as(&user, "/root/moved").unwrap(), "data");
    root.remove_file_as(&user, "/root/moved").unwrap();
    assert_eq!(root.read_file_as(&user, "/root/data", 32).unwrap(), b"new");
    root.symlink_as(&user, "born", "/root/dangling").unwrap();
    assert!(root.read_file_as(&user, "/root/dangling", 8).is_err());
    let mode = FileMode::new(
        FileAccess::ReadWrite,
        FileCreation::IfMissing,
        FileBehavior::Preserve,
    )
    .unwrap();
    let born = root.open_file_as(&user, "/root/dangling", mode).unwrap();
    born.write(b"born").unwrap();
    assert_eq!(root.read_file_as(&user, "/root/born", 8).unwrap(), b"born");
    assert!(fs::symlink_metadata(path.join("root/dangling"))
        .unwrap()
        .is_file());
}
#[test]
fn host_symlinks_directory_parents_acl_execution_and_restart_share_resolver() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.create_directory_as(&user, "/root/dir", CreationOptions::default())
        .unwrap();
    root.symlink_as(&user, "dir", "/root/linkdir").unwrap();
    root.create_owned_file(
        &user,
        "/root/linkdir/image",
        b"echo ok",
        VirtualMode::new(0o700).unwrap(),
    )
    .unwrap();
    assert_eq!(
        root.read_file_as(&user, "/root/dir/image", 32).unwrap(),
        b"echo ok"
    );
    root.symlink_as(&user, "linkdir/image", "/root/exec")
        .unwrap();
    root.load_executable_as(&user, "/root/exec", 32).unwrap();
    root.chmod_as(&user, "/root/exec", VirtualMode::new(0o600).unwrap())
        .unwrap();
    assert!(root.load_executable_as(&user, "/root/exec", 32).is_err());
    assert_eq!(
        root.working_directory_as(&user, "/root/linkdir")
            .unwrap()
            .path(),
        "/root/dir"
    );
    let reopened = DataRoot::open(&path).unwrap();
    assert_eq!(
        reopened.read_file_as(&user, "/root/exec", 32).unwrap(),
        b"echo ok"
    );
    assert_eq!(
        reopened.readlink_as(&user, "/root/exec").unwrap(),
        "linkdir/image"
    );
    reopened.remove_file_as(&user, "/root/exec").unwrap();
    assert_eq!(fs::read(path.join("root/dir/image")).unwrap(), b"echo ok");
}
#[test]
fn host_symlinks_loops_host_escape_unknown_substitution_and_target_policy_are_rejected() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.symlink_as(&user, "b", "/root/a").unwrap();
    root.symlink_as(&user, "a", "/root/b").unwrap();
    assert!(matches!(
        root.read_file_as(&user, "/root/a", 8),
        Err(FsError::Vfs(vfs::Error::LinkLoop))
    ));
    assert!(root.symlink_as(&user, "C:/Windows", "/root/host").is_err());
    assert!(!path.join("root/host").exists());
    root.symlink_as(&user, "/etc/passwd", "/root/readonly")
        .unwrap();
    let passwd = fs::read(path.join("etc/passwd")).ok();
    assert!(root
        .replace_owned_file(&user, "/root/readonly", b"bad")
        .is_err());
    assert_eq!(fs::read(path.join("etc/passwd")).ok(), passwd);
    root.create_owned_file(
        &user,
        "/root/secret",
        b"secret",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    root.symlink_as(&user, "/root/secret", "/tmp/public")
        .unwrap();
    let guest = Account {
        identity: Default::default(),
        uid: 1000,
        name: "guest".into(),
        home: "/home/guest".into(),
    };
    assert!(root.read_file_as(&guest, "/tmp/public", 16).is_err());
    fs::rename(path.join("tmp/public"), path.join("tmp/original-link")).unwrap();
    fs::write(path.join("tmp/public"), b"same-or-not-authoritative").unwrap();
    assert!(root.read_file_as(&user, "/tmp/public", 16).is_err());
    assert_eq!(
        fs::read(path.join("tmp/public")).unwrap(),
        b"same-or-not-authoritative"
    );
}

#[test]
fn host_symlinks_budget_is_40_and_dot_components_resolve_after_links() {
    use std::format;
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.create_owned_file(
        &user,
        "/root/data",
        b"root",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    for n in (1..=40).rev() {
        let target = if n == 40 {
            "data".to_owned()
        } else {
            format!("link{}", n + 1)
        };
        root.symlink_as(&user, &target, &format!("/root/link{n}"))
            .unwrap();
    }
    assert_eq!(root.read_file_as(&user, "/root/link1", 8).unwrap(), b"root");
    root.symlink_as(&user, "link1", "/root/too-many").unwrap();
    assert!(matches!(
        root.read_file_as(&user, "/root/too-many", 8),
        Err(FsError::Vfs(vfs::Error::LinkLoop))
    ));
    for dir in ["/root/outer", "/root/outer/inner"] {
        root.create_directory_as(&user, dir, CreationOptions::default())
            .unwrap();
    }
    root.create_owned_file(
        &user,
        "/root/outer/data",
        b"outer",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    root.symlink_as(&user, "outer/inner", "/root/jump").unwrap();
    root.symlink_as(&user, "jump/../data", "/root/dot").unwrap();
    assert_eq!(root.read_file_as(&user, "/root/dot", 8).unwrap(), b"outer");
    for (name, target) in [
        ("not-dir", "data/../data"),
        ("trailing", "data/"),
        ("above", "../../../../root/data"),
    ] {
        let link = format!("/root/{name}");
        root.symlink_as(&user, target, &link).unwrap();
        assert!(root.read_file_as(&user, &link, 8).is_err());
    }
}
#[test]
fn host_symlinks_search_permissions_are_not_erased_by_parent_components() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    let guest = Account {
        identity: Default::default(),
        uid: 1000,
        name: "guest".into(),
        home: "/home/guest".into(),
    };
    root.create_directory_as(&user, "/tmp/private", CreationOptions::default())
        .unwrap();
    root.set_access_as(
        &user,
        "/tmp/private",
        Some(VirtualMode::new(0o700).unwrap()),
        None,
        None,
    )
    .unwrap();
    root.create_owned_file(
        &user,
        "/tmp/public-data",
        b"public",
        VirtualMode::new(0o644).unwrap(),
    )
    .unwrap();
    root.symlink_as(&user, "private/../public-data", "/tmp/indirect")
        .unwrap();
    assert_eq!(
        root.read_file_as(&guest, "/tmp/public-data", 8).unwrap(),
        b"public"
    );
    assert!(root.read_file_as(&guest, "/tmp/indirect", 8).is_err());
    root.set_access_as(
        &user,
        "/tmp/private",
        Some(VirtualMode::new(0o711).unwrap()),
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        root.read_file_as(&guest, "/tmp/indirect", 8).unwrap(),
        b"public"
    );
    root.mount_memory_as(
        &user,
        "/mnt/ram",
        vfs::Limits {
            bytes: 4096,
            inodes: 16,
        },
    )
    .unwrap();
    root.symlink_as(&user, "/mnt/ram/data", "/tmp/cross")
        .unwrap();
    assert!(matches!(
        root.read_file_as(&user, "/tmp/cross", 8),
        Err(FsError::Vfs(vfs::Error::NotFound))
    ));
    root.create_owned_file(&user, "/mnt/ram/data", b"cross", VirtualMode::new(0o600).unwrap()).unwrap();
    assert_eq!(root.read_file_as(&user, "/tmp/cross", 8).unwrap(), b"cross");
}
#[test]
fn host_symlinks_stat_cursor_refresh_and_nofollow_operations_observe_link_inode() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.create_directory_as(&user, "/root/d", CreationOptions::default())
        .unwrap();
    root.create_owned_file(
        &user,
        "/root/d/data",
        b"secret",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    root.symlink_as(&user, "data", "/root/d/link").unwrap();
    let stat = root.stat_as(&user, "/root/d/link").unwrap();
    assert_eq!(stat.kind, ObjectKind::Symlink);
    assert_eq!(stat.length, 4);
    let mut cursor = root.open_directory_as(&user, "/root/d").unwrap();
    assert!(cursor
        .read(16)
        .unwrap()
        .iter()
        .any(|e| e.name == "link" && e.kind == ObjectKind::Symlink));
    root.symlink_as(&user, "missing", "/root/d/dangling")
        .unwrap();
    cursor.refresh().unwrap();
    assert!(cursor
        .read(16)
        .unwrap()
        .iter()
        .any(|e| e.name == "dangling" && e.kind == ObjectKind::Symlink));
    let exclusive = FileMode::new(
        FileAccess::WriteOnly,
        FileCreation::Exclusive,
        FileBehavior::Preserve,
    )
    .unwrap();
    assert!(matches!(
        root.open_file_as(&user, "/root/d/dangling", exclusive),
        Err(FsError::AlreadyExists)
    ));
    assert!(root
        .hardlink_as(&user, "/root/d/link", "/root/d/alias")
        .is_err());
    assert!(!path.join("root/d/alias").exists());
    root.symlink_as(&user, "d", "/root/parent").unwrap();
    root.rename_as(&user, "/root/parent/link", "/root/parent/renamed")
        .unwrap();
    assert_eq!(root.readlink_as(&user, "/root/d/renamed").unwrap(), "data");
    assert!(matches!(
        root.ownership_as(&user, "/root/parent/renamed").unwrap(),
        OwnershipState::Managed(OwnedFileInfo {
            kind: HostFileKind::Symlink { .. },
            ..
        })
    ));
    root.remove_file_as(&user, "/root/parent/renamed").unwrap();
    assert_eq!(
        root.read_file_as(&user, "/root/d/data", 8).unwrap(),
        b"secret"
    );
    fs::rename(path.join("root/d/dangling"), path.join("root/d/saved-link")).unwrap();
    fs::write(path.join("root/d/dangling"), b"missing").unwrap();
    assert!(cursor.refresh().is_err());
    assert!(root.readlink_as(&user, "/root/d/dangling").is_err());
    assert_eq!(fs::read(path.join("root/d/dangling")).unwrap(), b"missing");
}
#[test]
fn host_symlinks_creation_faults_recover_target_metadata_and_preserve_foreign_name() {
    use crate::filesystem::{access_model::CreationMode, ownership::OwnershipFault};
    for fault in [
        OwnershipFault::AfterStorage,
        OwnershipFault::AfterPublicName,
        OwnershipFault::AfterCatalog,
    ] {
        let path = fixture();
        let root = DataRoot::open(&path).unwrap();
        let user = admin();
        root.create_owned_file(
            &user,
            "/root/data",
            b"payload",
            VirtualMode::new(0o600).unwrap(),
        )
        .unwrap();
        {
            let guard = root.namespace_guard().unwrap();
            assert!(matches!(
                root.create_owned_node_configured_locked(
                    &user,
                    "/root/link",
                    b"data",
                    (
                        CreationMode::Exact(VirtualMode::new(0o777).unwrap()),
                        fault,
                        CreationOptions::default(),
                        HostFileKind::Symlink {
                            target: "data".into()
                        },
                    ),
                    &guard
                ),
                Err(FsError::OwnershipCommitPending { .. })
            ));
        }
        let reopened = DataRoot::open(&path).unwrap();
        reopened.recover_transactions().unwrap();
        reopened.recover_transactions().unwrap();
        assert_eq!(reopened.readlink_as(&user, "/root/link").unwrap(), "data");
        assert_eq!(
            reopened.read_file_as(&user, "/root/link", 16).unwrap(),
            b"payload"
        );
        assert!(!path.join("var/.elara-objects/pending").exists());
    }
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    {
        let guard = root.namespace_guard().unwrap();
        assert!(matches!(
            root.create_owned_node_configured_locked(
                &user,
                "/root/link",
                b"data",
                (
                    CreationMode::Exact(VirtualMode::new(0o777).unwrap()),
                    OwnershipFault::AfterStorage,
                    CreationOptions::default(),
                    HostFileKind::Symlink {
                        target: "data".into()
                    },
                ),
                &guard
            ),
            Err(FsError::OwnershipCommitPending { .. })
        ));
    }
    fs::write(path.join("root/link"), b"data").unwrap();
    assert!(root.recover_transactions().is_err());
    assert_eq!(fs::read(path.join("root/link")).unwrap(), b"data");
    assert!(path.join("var/.elara-objects/pending").exists());
}

#[test]
fn host_symlinks_image_io_obeys_target_binding_lock_and_resource_lifetime() {
    let path = fixture(); let root = DataRoot::open(&path).unwrap(); let user = admin();
    root.mount_memory_as(&user, "/mnt/a", vfs::Limits { bytes: 65536, inodes: 64 }).unwrap();
    root.create_owned_file(&user, "/mnt/a/data", b"image", VirtualMode::new(0o600).unwrap()).unwrap();
    root.create_block_image_as(&user, "/mnt/a", "/root/disk", 32, 2).unwrap();
    root.symlink_as(&user, "disk", "/root/disk-link").unwrap();
    root.block_image_stats_as(&user, "/root/disk-link", 2).unwrap();
    let mode = FileMode::new(FileAccess::ReadWrite, FileCreation::Existing, FileBehavior::Preserve).unwrap();
    let disk = root.open_file_as(&user, "/root/disk", mode).unwrap();
    disk.lock(vfs::LockMode::Exclusive).unwrap();
    assert!(matches!(root.block_image_stats_as(&user, "/root/disk-link", 2), Err(FsError::Vfs(vfs::Error::LockConflict))));
    disk.unlock().unwrap();
    root.save_block_image_as(&user, "/mnt/a", "/root/disk-link", 2).unwrap();
    root.load_block_image_as(&user, "/root/disk-link", "/mnt/b", 2).unwrap();
    assert_eq!(root.read_file_as(&user, "/mnt/b/data", 16).unwrap(), b"image");
    root.create_image_key_as(&user, "/root/key").unwrap();
    root.symlink_as(&user, "key", "/root/key-link").unwrap();
    let key = root.open_file_as(&user, "/root/key", mode).unwrap();
    key.lock(vfs::LockMode::Exclusive).unwrap();
    assert!(matches!(root.seal_volume_as(&user, "/mnt/a", "/root/sealed", "/root/key-link"), Err(FsError::Vfs(vfs::Error::LockConflict))));
    assert!(!path.join("root/sealed").exists()); key.unlock().unwrap();
    root.seal_volume_as(&user, "/mnt/a", "/root/sealed", "/root/key-link").unwrap();
    root.register_resource_as(&user, "/opt/app", ResourceKind::Application, ResourceLimits { bytes: 4096, objects: 8 }).unwrap();
    root.register_application_location_as(&user, "/opt/app", "/root/app-data", ResourceKind::ApplicationData, ResourceLimits { bytes: 4096, objects: 8 }).unwrap();
    root.symlink_as(&user, "app-data", "/root/app-link").unwrap();
    assert!(matches!(root.create_image_key_as(&user, "/root/app-link/key"), Err(FsError::OwnershipClassMismatch)));
    assert!(!path.join("root/app-data/key").exists());
    assert!(matches!(root.create_block_image_as(&user, "/mnt/a", "/root/app-link/disk", 32, 2), Err(FsError::OwnershipClassMismatch)));
    assert!(!path.join("root/app-data/disk").exists());
}

#[test]
fn cross_backend_links_read_write_names_and_cwd_preserve_target_domains() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let user = admin();
    root.mount_memory_as(&user, "/mnt/cross", vfs::Limits { bytes: 65536, inodes: 64 }).unwrap();
    root.create_directory_as(&user, "/root/host", Default::default()).unwrap();
    root.create_directory_as(&user, "/mnt/cross/dir", Default::default()).unwrap();
    root.create_owned_file(&user, "/root/host/data", b"host", VirtualMode::new(0o600).unwrap()).unwrap();
    root.create_owned_file(&user, "/mnt/cross/dir/data", b"volume", VirtualMode::new(0o600).unwrap()).unwrap();
    root.symlink_as(&user, "/mnt/cross/dir", "/root/to-volume").unwrap();
    root.symlink_global_as(&user, "/root/host", "/mnt/cross/to-host").unwrap();
    assert_eq!(root.read_file_as(&user, "/root/to-volume/data", 32).unwrap(), b"volume");
    assert_eq!(root.read_file_as(&user, "/mnt/cross/to-host/data", 32).unwrap(), b"host");
    root.replace_owned_file(&user, "/mnt/cross/to-host/data", b"changed").unwrap();
    root.hardlink_as(&user, "/root/to-volume/data", "/root/to-volume/alias").unwrap();
    root.rename_as(&user, "/root/to-volume/alias", "/root/to-volume/moved").unwrap();
    assert_eq!(root.read_file_as(&user, "/mnt/cross/dir/moved", 32).unwrap(), b"volume");
    let cwd = root.working_directory_as(&user, "/mnt/cross/to-host").unwrap();
    assert_eq!(cwd.path(), "/root/host");
    assert!(matches!(root.rename_as(&user, "/root/host", "/root/moved-host"), Err(FsError::Vfs(vfs::Error::Busy))));
    assert!(matches!(root.hardlink_as(&user, "/root/host/data", "/root/to-volume/cross"), Err(FsError::Vfs(vfs::Error::CrossDevice))));
    assert!(matches!(root.rename_as(&user, "/root/host/data", "/root/to-volume/cross"), Err(FsError::Vfs(vfs::Error::CrossDevice))));
    root.remove_file_as(&user, "/root/to-volume").unwrap();
    assert_eq!(root.read_file_as(&user, "/mnt/cross/dir/data", 32).unwrap(), b"volume");
    assert_eq!(root.read_file_as(&user, "/root/host/data", 32).unwrap(), b"changed");
}

#[test]
fn cross_backend_links_share_loop_budget_and_deny_host_permission_escape() {
    let root = DataRoot::open(&fixture()).unwrap();
    let user = admin();
    root.mount_memory_as(&user, "/mnt/cross", vfs::Limits { bytes: 65536, inodes: 64 }).unwrap();
    root.symlink_as(&user, "/mnt/cross/back", "/root/loop").unwrap();
    root.symlink_global_as(&user, "/root/loop", "/mnt/cross/back").unwrap();
    assert!(matches!(root.read_file_as(&user, "/root/loop", 32), Err(FsError::Vfs(vfs::Error::LinkLoop))));
    root.create_owned_file(&user, "/root/secret", b"secret", VirtualMode::new(0o600).unwrap()).unwrap();
    root.symlink_global_as(&user, "/root/secret", "/mnt/cross/secret").unwrap();
    let guest = Account { identity: Default::default(), uid: 1000, name: "guest".into(), home: "/home/guest".into() };
    assert!(root.read_file_as(&guest, "/mnt/cross/secret", 32).is_err());
    assert_eq!(root.readlink_as(&user, "/mnt/cross/back").unwrap(), "/root/loop");
}

#[test]
fn cross_backend_links_exact_40_budget_nofollow_and_dangling_birth() {
    use std::format;
    let root = DataRoot::open(&fixture()).unwrap();
    let user = admin();
    root.mount_memory_as(&user, "/mnt/cross", vfs::Limits { bytes: 65536, inodes: 64 }).unwrap();
    root.create_owned_file(&user, "/root/data", b"limit", VirtualMode::new(0o600).unwrap()).unwrap();
    for n in (0..20).rev() {
        let target = if n == 19 { "/root/data".to_owned() } else { format!("/root/h{}", n + 1) };
        root.symlink_global_as(&user, &target, &format!("/mnt/cross/v{n}")).unwrap();
        root.symlink_as(&user, &format!("/mnt/cross/v{n}"), &format!("/root/h{n}")).unwrap();
    }
    assert_eq!(root.read_file_as(&user, "/root/h0", 32).unwrap(), b"limit");
    root.symlink_as(&user, "/root/h0", "/root/over").unwrap();
    assert!(matches!(root.read_file_as(&user, "/root/over", 32), Err(FsError::Vfs(vfs::Error::LinkLoop))));
    assert_eq!(root.readlink_as(&user, "/root/over").unwrap(), "/root/h0");
    root.symlink_global_as(&user, "/root/new", "/mnt/cross/dangling").unwrap();
    let exclusive = FileMode::new(FileAccess::WriteOnly, FileCreation::Exclusive, FileBehavior::Preserve).unwrap();
    assert!(matches!(root.open_file_as(&user, "/mnt/cross/dangling", exclusive), Err(FsError::Vfs(vfs::Error::Exists))));
    let mode = FileMode::new(FileAccess::ReadWrite, FileCreation::IfMissing, FileBehavior::Preserve).unwrap();
    let fd = root.open_file_as(&user, "/mnt/cross/dangling", mode).unwrap();
    fd.write(b"born").unwrap();
    assert_eq!(root.read_file_as(&user, "/root/new", 32).unwrap(), b"born");
    root.remove_file_as(&user, "/mnt/cross/dangling").unwrap();
    assert_eq!(fd.read_at(0, 32).unwrap(), b"born");
}
