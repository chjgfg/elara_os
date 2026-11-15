use crate::filesystem::{
    vfs::*, Credentials, FileAccess, FileBehavior, FileCreation, FileMode, FileOwner, VirtualMode,
};
use std::collections::BTreeSet;
use std::{assert, assert_eq, assert_ne, matches};
fn actor(uid: u32) -> Credentials {
    Credentials {
        uid,
        gid: uid,
        groups: BTreeSet::from([uid]),
    }
}
fn volume() -> std::sync::Arc<Volume> {
    Volume::memory(
        FileOwner { uid: 0, gid: 0 },
        Limits {
            bytes: 65536,
            inodes: 64,
        },
    )
    .unwrap()
}
fn mode(n: u16) -> VirtualMode {
    VirtualMode::new(n).unwrap()
}
fn rw() -> FileMode {
    FileMode::new(
        FileAccess::ReadWrite,
        FileCreation::Existing,
        FileBehavior::Preserve,
    )
    .unwrap()
}
#[test]
fn inode_names_links_and_open_unlinked_lifetime() {
    let v = volume();
    let a = actor(0);
    let id = v.create(&a, "/first", b"abc", mode(0o600)).unwrap();
    v.link(&a, "/first", "/second").unwrap();
    assert_eq!(v.stat(&a, "/second", true).unwrap().inode, id);
    assert_eq!(v.stat(&a, "/first", true).unwrap().links, 2);
    let f = v.open(&a, "/second", rw(), ShareMode::Shared).unwrap();
    f.write_at(1, b"XY").unwrap();
    let other = v
        .open(&a, "/first", FileMode::READ, ShareMode::Shared)
        .unwrap();
    assert_eq!(other.read(9).unwrap(), b"aXY");
    v.unlink(&a, "/first", false).unwrap();
    v.unlink(&a, "/second", false).unwrap();
    assert_eq!(f.stat().unwrap().links, 0);
    assert_eq!(f.read_at(0, 9).unwrap(), b"aXY");
    assert_eq!(v.stats().unwrap().inodes, 2);
    drop(other);
    drop(f);
    assert_eq!(v.stats().unwrap().inodes, 1);
}
#[test]
fn relative_absolute_dangling_cycle_and_escape_links() {
    let v = volume();
    let a = actor(0);
    v.mkdir(&a, "/dir", mode(0o755)).unwrap();
    v.create(&a, "/file", b"data", mode(0o644)).unwrap();
    v.symlink(&a, "../file", "/dir/relative").unwrap();
    v.symlink(&a, "/file", "/absolute").unwrap();
    for p in ["/dir/relative", "/absolute"] {
        assert_eq!(
            v.open(&a, p, FileMode::READ, ShareMode::Shared)
                .unwrap()
                .read(9)
                .unwrap(),
            b"data"
        );
    }
    assert_eq!(
        v.stat(&a, "/absolute", false).unwrap().kind,
        InodeKind::Symlink
    );
    assert_eq!(v.readlink(&a, "/dir/relative").unwrap(), "../file");
    v.symlink(&a, "/missing", "/dangling").unwrap();
    assert_eq!(v.stat(&a, "/dangling", true), Err(Error::NotFound));
    v.symlink(&a, "/loop", "/loop").unwrap();
    assert_eq!(v.stat(&a, "/loop", true), Err(Error::LinkLoop));
    v.symlink(&a, "../../outside", "/dir/escape").unwrap();
    assert_eq!(v.stat(&a, "/dir/escape", true), Err(Error::InvalidPath));
}
#[test]
fn rename_replaces_name_without_retargeting_old_open_file() {
    let v = volume();
    let a = actor(0);
    v.create(&a, "/old", b"old", mode(0o600)).unwrap();
    v.create(&a, "/new", b"new", mode(0o600)).unwrap();
    let old = v
        .open(&a, "/old", FileMode::READ, ShareMode::Shared)
        .unwrap();
    assert_eq!(v.rename(&a, "/new", "/old", false), Err(Error::Exists));
    v.rename(&a, "/new", "/old", true).unwrap();
    assert_eq!(old.read(9).unwrap(), b"old");
    assert_eq!(
        v.open(&a, "/old", FileMode::READ, ShareMode::Shared)
            .unwrap()
            .read(9)
            .unwrap(),
        b"new"
    );
    v.mkdir(&a, "/a", mode(0o700)).unwrap();
    v.mkdir(&a, "/a/b", mode(0o700)).unwrap();
    assert_eq!(v.rename(&a, "/a", "/a/b/a", true), Err(Error::InvalidPath));
}
#[test]
fn locks_are_shared_by_dup_and_enforced_on_other_opens() {
    let v = volume();
    let a = actor(0);
    v.create(&a, "/f", b"keep", mode(0o600)).unwrap();
    let f = v.open(&a, "/f", rw(), ShareMode::Shared).unwrap();
    let other = v.open(&a, "/f", rw(), ShareMode::Shared).unwrap();
    f.lock(LockMode::Shared).unwrap();
    other.lock(LockMode::Shared).unwrap();
    assert_eq!(other.write(b"bad"), Err(Error::LockConflict));
    assert_eq!(f.lock(LockMode::Exclusive), Err(Error::LockConflict));
    other.unlock().unwrap();
    f.lock(LockMode::Exclusive).unwrap();
    assert_eq!(other.read(4), Err(Error::LockConflict));
    let dup = f.clone();
    drop(f);
    assert_eq!(other.read(4), Err(Error::LockConflict));
    drop(dup);
    assert_eq!(other.read(4).unwrap(), b"keep");
    assert!(matches!(
        v.open(&a, "/f", rw(), ShareMode::Exclusive),
        Err(Error::Busy)
    ));
}
#[test]
fn quota_failure_rolls_back_bytes_names_and_special_bits() {
    let v = Volume::memory(
        FileOwner { uid: 0, gid: 0 },
        Limits {
            bytes: 8,
            inodes: 4,
        },
    )
    .unwrap();
    let a = actor(0);
    v.create(&a, "/f", b"old", mode(0o6700)).unwrap();
    let f = v.open(&a, "/f", rw(), ShareMode::Shared).unwrap();
    assert_eq!(f.write_at(0, b"too-large"), Err(Error::Quota));
    assert_eq!(f.read_at(0, 9).unwrap(), b"old");
    assert_eq!(f.stat().unwrap().mode.bits(), 0o6700);
    f.write_at(0, b"new").unwrap();
    assert_eq!(f.stat().unwrap().mode.bits(), 0o700);
    v.quota(
        &a,
        0,
        Limits {
            bytes: 3,
            inodes: 2,
        },
    )
    .unwrap();
    assert_eq!(v.create(&a, "/extra", b"x", mode(0o600)), Err(Error::Quota));
    assert_eq!(v.stat(&a, "/extra", false), Err(Error::NotFound));
}
#[test]
fn sticky_search_permissions_and_handle_capabilities() {
    let v = volume();
    let root = actor(0);
    let guest = actor(10);
    let stranger = actor(11);
    v.mkdir(&root, "/shared", mode(0o1777)).unwrap();
    v.create(&guest, "/shared/f", b"data", mode(0o600)).unwrap();
    let f = v
        .open(&guest, "/shared/f", rw(), ShareMode::Shared)
        .unwrap();
    assert_eq!(v.unlink(&stranger, "/shared/f", false), Err(Error::Denied));
    v.chmod(&guest, "/shared/f", mode(0)).unwrap();
    assert!(matches!(
        v.open(&guest, "/shared/f", FileMode::READ, ShareMode::Shared),
        Err(Error::Denied)
    ));
    f.write_at(0, b"keep").unwrap();
    assert_eq!(f.read_at(0, 4).unwrap(), b"keep");
    v.chmod(&root, "/shared", mode(0o1700)).unwrap();
    assert_eq!(v.stat(&guest, "/shared/f", false), Err(Error::Denied));
}
#[test]
fn snapshots_are_independent_and_encoding_roundtrips_aliases() {
    let v = volume();
    let a = actor(0);
    v.create(&a, "/f", b"before", mode(0o600)).unwrap();
    v.link(&a, "/f", "/alias").unwrap();
    let snapshot = v.snapshot(&a, true).unwrap();
    let clone = v.snapshot(&a, false).unwrap();
    v.open(&a, "/f", rw(), ShareMode::Shared)
        .unwrap()
        .write_at(0, b"after!")
        .unwrap();
    assert_eq!(
        snapshot
            .open(&a, "/f", FileMode::READ, ShareMode::Shared)
            .unwrap()
            .read(9)
            .unwrap(),
        b"before"
    );
    assert!(matches!(
        snapshot.open(&a, "/f", rw(), ShareMode::Shared),
        Err(Error::ReadOnly)
    ));
    clone.unlink(&a, "/alias", false).unwrap();
    assert_eq!(v.stat(&a, "/f", true).unwrap().links, 2);
    let bytes = v.encode(&a).unwrap();
    let restored = Volume::decode(&bytes, false).unwrap();
    assert_eq!(restored.encode(&a).unwrap(), bytes);
    assert_eq!(
        restored.stat(&a, "/f", true).unwrap().inode,
        restored.stat(&a, "/alias", true).unwrap().inode
    );
}
#[test]
fn mounts_are_pinned_and_cross_volume_operations_never_copy_delete() {
    let registry = Registry::default();
    let a = actor(0);
    registry.mount(&a, "/mnt/a", volume()).unwrap();
    registry.mount(&a, "/mnt/b", volume()).unwrap();
    let Route::Mounted { volume, path, .. } = registry.route("/mnt/a").unwrap() else {
        panic!("mount")
    };
    assert_eq!(path, "/");
    volume.create(&a, "/f", b"keep", mode(0o600)).unwrap();
    let f = volume
        .open(&a, "/f", FileMode::READ, ShareMode::Shared)
        .unwrap();
    assert_eq!(
        registry.rename(&a, "/mnt/a/f", "/mnt/b/f", true),
        Err(Error::CrossDevice)
    );
    assert_eq!(registry.unmount(&a, "/mnt/a"), Err(Error::Busy));
    drop(volume);
    assert_eq!(registry.unmount(&a, "/mnt/a"), Err(Error::Busy));
    drop(f);
    registry.unmount(&a, "/mnt/a").unwrap();
    assert!(matches!(registry.route("/mnt/alias").unwrap(), Route::Host));
}

#[test]
fn image_roundtrip_compression_and_corruption_recovery() {
    let v = volume();
    let a = actor(0);
    v.create(&a, "/zeros", &[0; 16000], mode(0o600)).unwrap();
    v.link(&a, "/zeros", "/alias").unwrap();
    let (bytes, stats) = v.image(&a).unwrap();
    assert!(stats.image_bytes < stats.logical_bytes, "{stats:?}");
    let loaded = Volume::load_image(&bytes).unwrap();
    assert_eq!(loaded.recovery, Recovery::Clean);
    assert_eq!(loaded.volume.encode(&a).unwrap(), v.encode(&a).unwrap());
    let mut corrupt = bytes.clone();
    let last = corrupt.len() - 1;
    corrupt[last] ^= 1;
    let recovered = Volume::load_image(&corrupt).unwrap();
    assert_eq!(
        recovered.recovery,
        Recovery::RecoveredReadOnly { damaged_copies: 1 }
    );
    assert!(matches!(
        recovered.volume.open(&a, "/zeros", rw(), ShareMode::Shared),
        Err(Error::ReadOnly)
    ));
    assert_eq!(recovered.volume.encode(&a).unwrap(), v.encode(&a).unwrap());
    let mut header = bytes.clone();
    header[12] ^= 1;
    assert!(matches!(
        Volume::load_image(&header),
        Err(Error::InvalidImage)
    ));
    for cut in [0, 7, 44, bytes.len() - 1] {
        assert!(Volume::load_image(&bytes[..cut]).is_err());
    }
    let mut extra = bytes.clone();
    extra.push(0);
    assert!(Volume::load_image(&extra).is_err());
}

#[test]
fn vfs_default_acl_overrides_umask_and_roundtrips() {
    use crate::filesystem::{Acl, DefaultAcl, Umask};
    let v = volume();
    let root = actor(0);
    let guest = actor(10);
    v.mkdir(&root, "/shared", mode(0o2777)).unwrap();
    let default = DefaultAcl {
        owner: 7,
        other: 0,
        acl: Acl {
            users: std::collections::BTreeMap::from([(11, 7)]),
            group: Some(5),
            mask: Some(7),
            ..Acl::default()
        },
    };
    v.set_default_acl(&root, "/shared", Some(default.clone()))
        .unwrap();
    v.create_with_umask(&guest, "/shared/f", b"visible", Umask::new(0o777).unwrap())
        .unwrap();
    assert_eq!(v.stat(&root, "/shared/f", true).unwrap().mode.bits(), 0o660);
    assert_eq!(v.stat(&root, "/shared/f", true).unwrap().owner.gid, 0);
    assert_eq!(v.acl(&root, "/shared/f").unwrap().users.get(&11), Some(&7));
    assert_eq!(
        v.open(&actor(11), "/shared/f", FileMode::READ, ShareMode::Shared)
            .unwrap()
            .read(7)
            .unwrap(),
        b"visible"
    );
    v.mkdir_with_umask(&guest, "/shared/sub", Umask::new(0o777).unwrap())
        .unwrap();
    assert_eq!(
        v.stat(&root, "/shared/sub", true).unwrap().mode.bits(),
        0o2770
    );
    assert_eq!(v.default_acl(&root, "/shared/sub").unwrap(), Some(default));
    let encoded = v.encode(&root).unwrap();
    assert_eq!(
        Volume::decode(&encoded, false)
            .unwrap()
            .encode(&root)
            .unwrap(),
        encoded
    );
    v.set_default_acl(&root, "/shared", None).unwrap();
    v.create_with_umask(
        &guest,
        "/shared/control",
        b"hidden",
        Umask::new(0o777).unwrap(),
    )
    .unwrap();
    assert_eq!(
        v.stat(&root, "/shared/control", true).unwrap().mode.bits(),
        0
    );
    assert!(matches!(
        v.open(
            &actor(11),
            "/shared/control",
            FileMode::READ,
            ShareMode::Shared
        ),
        Err(Error::Denied)
    ));
}
#[test]
fn vfs_creation_fd_retains_capability_and_chgrp_clears_privilege() {
    use crate::filesystem::Umask;
    let v = volume();
    let root = actor(0);
    let guest = actor(10);
    v.mkdir(&root, "/shared", mode(0o777)).unwrap();
    let create = FileMode::new(
        FileAccess::ReadWrite,
        FileCreation::Exclusive,
        FileBehavior::Preserve,
    )
    .unwrap();
    let f = v
        .open_with_umask(
            &guest,
            "/shared/f",
            create,
            ShareMode::Shared,
            Umask::new(0o777).unwrap(),
        )
        .unwrap();
    f.write(b"capability").unwrap();
    assert_eq!(f.stat().unwrap().mode.bits(), 0);
    assert!(matches!(
        v.open(&guest, "/shared/f", FileMode::READ, ShareMode::Shared),
        Err(Error::Denied)
    ));
    v.chmod(&root, "/shared/f", mode(0o6700)).unwrap();
    v.set_access(&root, "/shared/f", None, Some(20), None)
        .unwrap();
    assert_eq!(f.stat().unwrap().mode.bits(), 0o700);
    let before = v.encode(&root).unwrap();
    assert_eq!(
        v.set_access(&guest, "/shared/f", Some(mode(0o777)), Some(20), None),
        Err(Error::Denied)
    );
    assert_eq!(v.encode(&root).unwrap(), before);
}

#[test]
fn vfs_directory_cursor_pins_removed_inode_and_busy_mount() {
    use crate::filesystem::{CreationOptions, DataRoot, FsError};
    let root = DataRoot::open(&super::fixture()).unwrap();
    let admin = crate::users::Account {
        uid: 0,
        name: "root".into(),
        home: "/root".into(),
        identity: Default::default(),
    };
    root.mount_memory_as(
        &admin,
        "/mnt/dir",
        Limits {
            bytes: 65536,
            inodes: 64,
        },
    )
    .unwrap();
    root.create_directory_as(&admin, "/mnt/dir/d", CreationOptions::default())
        .unwrap();
    let mut cursor = root.open_directory_as(&admin, "/mnt/dir/d").unwrap();
    root.remove_directory_as(&admin, "/mnt/dir/d").unwrap();
    root.create_directory_as(&admin, "/mnt/dir/d", CreationOptions::default())
        .unwrap();
    root.create_file_as(&admin, "/mnt/dir/d/new").unwrap();
    cursor.refresh().unwrap();
    assert!(cursor.read(10).unwrap().is_empty());
    assert!(matches!(
        root.unmount_as(&admin, "/mnt/dir"),
        Err(FsError::Vfs(Error::Busy))
    ));
    drop(cursor);
    root.unmount_as(&admin, "/mnt/dir").unwrap();
}

#[test]
fn vfs_pseudo_nodes_are_live_bounded_and_never_serialized_as_disk_files() {
    let proc = Volume::pseudo(PseudoKind::Proc).unwrap();
    let user = actor(37);
    let first = proc
        .open(&user, "/identity", FileMode::READ, ShareMode::Shared)
        .unwrap();
    let bytes = first.read(512).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("euid: 37"));
    assert!(first.read(512).unwrap().is_empty());
    assert_eq!(first.stat().unwrap().length, bytes.len() as u64);
    let uptime = proc
        .open(&user, "/uptime", FileMode::READ, ShareMode::Shared)
        .unwrap()
        .read(512)
        .unwrap();
    let later = proc
        .open(&user, "/uptime", FileMode::READ, ShareMode::Shared)
        .unwrap()
        .read(512)
        .unwrap();
    let ticks = |b: &[u8]| {
        String::from_utf8_lossy(b)
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse::<u128>()
            .unwrap()
    };
    assert!(ticks(&later) >= ticks(&uptime));
    assert!(matches!(
        proc.open(&actor(0), "/identity", rw(), ShareMode::Shared),
        Err(Error::ReadOnly)
    ));
    assert_eq!(
        proc.create(&actor(0), "/new", b"", mode(0o600)),
        Err(Error::ReadOnly)
    );
    assert!(matches!(
        proc.snapshot(&actor(0), false),
        Err(Error::Unsupported)
    ));
    let sys = Volume::pseudo(PseudoKind::Sys).unwrap();
    assert!(String::from_utf8_lossy(
        &sys.open(&user, "/limits", FileMode::READ, ShareMode::Shared)
            .unwrap()
            .read(1024)
            .unwrap()
    )
    .contains("max_file_io_bytes:"));
    let dev = Volume::pseudo(PseudoKind::Dev).unwrap();
    let null = dev.open(&user, "/null", rw(), ShareMode::Shared).unwrap();
    assert_eq!(null.write(b"discard").unwrap(), 7);
    assert!(null.read(4).unwrap().is_empty());
    let zero = dev.open(&user, "/zero", rw(), ShareMode::Shared).unwrap();
    assert_eq!(zero.read(8).unwrap(), [0; 8]);
    assert_eq!(zero.write(b"discard").unwrap(), 7);
    assert_eq!(zero.read(1024 * 1024 + 1), Err(Error::Quota));
    let full = dev.open(&user, "/full", rw(), ShareMode::Shared).unwrap();
    assert_eq!(full.write(b"x"), Err(Error::NoSpace));
    assert_eq!(full.position().unwrap(), 0);
    assert_eq!(full.write(b"").unwrap(), 0);
    assert_eq!(full.truncate(0), Err(Error::Unsupported));
    assert_eq!(
        dev.chmod(&actor(0), "/null", mode(0o777)),
        Err(Error::ReadOnly)
    );
    assert!(matches!(dev.image(&actor(0)), Err(Error::Unsupported)));
}

#[test]
fn vfs_metadata_names_are_bounded_and_quota_replace_is_atomic() {
    let v = Volume::memory(
        FileOwner { uid: 0, gid: 0 },
        Limits {
            bytes: 8,
            inodes: 8,
        },
    )
    .unwrap();
    let a = actor(0);
    v.create(&a, "/f", b"keep", mode(0o6700)).unwrap();
    v.link(&a, "/f", "/alias").unwrap();
    let f = v.open(&a, "/f", rw(), ShareMode::Shared).unwrap();
    let before = v.encode(&a).unwrap();
    assert_eq!(v.replace(&a, "/alias", b"too-large"), Err(Error::Quota));
    assert_eq!(v.encode(&a).unwrap(), before);
    assert_eq!(f.read_at(0, 8).unwrap(), b"keep");
    f.lock(LockMode::Shared).unwrap();
    assert_eq!(v.replace(&a, "/f", b"new"), Err(Error::LockConflict));
    f.unlock().unwrap();
    v.replace(&a, "/alias", b"new").unwrap();
    assert_eq!(f.read_at(0, 8).unwrap(), b"new");
    assert_eq!(f.stat().unwrap().mode.bits(), 0o700);
    let mut image: serde_json::Value = serde_json::from_slice(&v.encode(&a).unwrap()).unwrap();
    let entries = image["nodes"]["1"]["content"]["Directory"]
        .as_object_mut()
        .unwrap();
    entries.clear();
    for n in 0..4096 {
        entries.insert(std::format!("f{n}"), serde_json::json!(2));
    }
    image["nodes"]["2"]["links"] = serde_json::json!(4096);
    let loaded = Volume::decode(&serde_json::to_vec(&image).unwrap(), false).unwrap();
    let before = loaded.encode(&a).unwrap();
    assert_eq!(loaded.link(&a, "/f0", "/overflow"), Err(Error::Quota));
    assert_eq!(loaded.encode(&a).unwrap(), before);
    assert_eq!(loaded.stats().unwrap().directory_entries, 4096);
}

#[test]
fn vfs_commit_identity_distinguishes_same_inode_number_in_two_volumes() {
    use crate::filesystem::{CommitObject, DataRoot};
    let root = DataRoot::open(&super::fixture()).unwrap();
    let admin = crate::users::Account {
        uid: 0,
        name: "root".into(),
        home: "/root".into(),
        identity: Default::default(),
    };
    for p in ["/mnt/a", "/mnt/b"] {
        root.mount_memory_as(
            &admin,
            p,
            Limits {
                bytes: 65536,
                inodes: 64,
            },
        )
        .unwrap();
    }
    let a = root
        .create_owned_file(&admin, "/mnt/a/f", b"one", mode(0o600))
        .unwrap();
    let b = root
        .create_owned_file(&admin, "/mnt/b/f", b"two", mode(0o600))
        .unwrap();
    assert_ne!(a.object, b.object);
    let (
        CommitObject::Virtual {
            filesystem: av,
            inode: ai,
        },
        CommitObject::Virtual {
            filesystem: bv,
            inode: bi,
        },
    ) = (a.object, b.object)
    else {
        panic!("wrong identity domain")
    };
    assert_eq!(ai, bi);
    assert_ne!(av, bv);
    assert_eq!(
        root.replace_owned_file(&admin, "/mnt/a/f", b"new")
            .unwrap()
            .object,
        a.object
    );
    root.append_owned_file(&admin, "/mnt/a/f", b"!").unwrap();
    root.rename_owned_file(&admin, "/mnt/a/f", "/mnt/a/moved")
        .unwrap();
    assert_eq!(
        root.read_owned_file(&admin, "/mnt/a/moved", 16).unwrap(),
        b"new!"
    );
    assert_eq!(
        root.remove_owned_file(&admin, "/mnt/a/moved")
            .unwrap()
            .object,
        a.object
    );
}

#[test]
fn vfs_snapshot_shares_data_pages_and_only_touched_pages_are_copied() {
    let v = volume();
    let a = actor(0);
    let bytes = [7; 4 * 4096];
    v.create(&a, "/large", &bytes, mode(0o600)).unwrap();
    assert_eq!(v.stats().unwrap().allocated_data_bytes, 16384);
    assert_eq!(v.stats().unwrap().shared_data_pages, 0);
    // Negative control: serialization roundtrip preserves content, not shared pages.
    let independent = Volume::decode(&v.encode(&a).unwrap(), false).unwrap();
    assert_eq!(independent.stats().unwrap().shared_data_pages, 0);
    let snapshot = v.snapshot(&a, true).unwrap();
    assert_eq!(v.stats().unwrap().shared_data_pages, 4);
    assert_eq!(snapshot.stats().unwrap().shared_data_pages, 4);
    let file = v.open(&a, "/large", rw(), ShareMode::Shared).unwrap();
    file.write_at(4095, b"XY").unwrap();
    assert_eq!(v.stats().unwrap().cow_copied_bytes, 8192);
    assert_eq!(v.stats().unwrap().shared_data_pages, 2);
    assert_eq!(file.read_at(4094, 4).unwrap(), [7, b'X', b'Y', 7]);
    assert_eq!(
        snapshot
            .open(&a, "/large", FileMode::READ, ShareMode::Shared)
            .unwrap()
            .read_at(4094, 4)
            .unwrap(),
        [7; 4]
    );
    drop(snapshot);
    assert_eq!(v.stats().unwrap().shared_data_pages, 0);
    file.truncate(4097).unwrap();
    file.truncate(32768).unwrap();
    assert_eq!(v.stats().unwrap().allocated_data_bytes, 8192);
    assert_eq!(file.read_at(4096, 4).unwrap(), [b'Y', 0, 0, 0]);
    let (image, _) = v.image(&a).unwrap();
    let restored = Volume::load_image(&image).unwrap();
    assert_eq!(restored.volume.stats().unwrap().allocated_data_bytes, 8192);
    assert_eq!(restored.volume.encode(&a).unwrap(), v.encode(&a).unwrap());
}

#[test]
fn vfs_inner_run_name_does_not_acquire_host_runtime_lifecycle() {
    use crate::filesystem::{CreationOptions, DataRoot};
    let root = DataRoot::open(&super::fixture()).unwrap();
    let admin = crate::users::Account {
        uid: 0,
        name: "root".into(),
        home: "/root".into(),
        identity: Default::default(),
    };
    root.mount_memory_as(
        &admin,
        "/mnt/life",
        Limits {
            bytes: 65536,
            inodes: 64,
        },
    )
    .unwrap();
    root.create_directory_as(&admin, "/mnt/life/run", CreationOptions::default())
        .unwrap();
    root.create_file_with_options(
        &admin,
        "/mnt/life/run/file",
        b"persistent in volume",
        CreationOptions::default(),
    )
    .unwrap();
    assert_eq!(
        root.read_file_as(&admin, "/mnt/life/run/file", 64).unwrap(),
        b"persistent in volume"
    );
    root.unmount_as(&admin, "/mnt/life").unwrap();
}

#[test]
fn vfs_search_only_cwd_anchor_blocks_retarget_until_last_clone() {
    let v = volume();
    let root = actor(0);
    let guest = actor(9);
    v.mkdir(&root, "/parent", mode(0o755)).unwrap();
    v.mkdir(&root, "/parent/search", mode(0o111)).unwrap();
    v.symlink(&root, "parent/search", "/alias").unwrap();
    assert!(matches!(
        v.open_directory(&guest, "/alias"),
        Err(Error::Denied)
    ));
    let pin = v.anchor_directory(&guest, "/alias").unwrap();
    assert_eq!(pin.path(), "/parent/search");
    let copy = pin.clone();
    drop(pin);
    assert_eq!(v.unlink(&root, "/parent/search", true), Err(Error::Busy));
    assert_eq!(
        v.rename(&root, "/parent", "/moved", false),
        Err(Error::Busy)
    );
    v.unlink(&root, "/alias", false).unwrap();
    assert_eq!(copy.path(), "/parent/search");
    drop(copy);
    // Negative control: dropping the lease changes these exact operations.
    v.rename(&root, "/parent", "/moved", false).unwrap();
    v.unlink(&root, "/moved/search", true).unwrap();
}

#[test]
fn vfs_nested_mounts_pin_parents_and_check_every_ancestor_search_gate() {
    let registry = Registry::default();
    let root = actor(0);
    let parent = volume();
    parent.mkdir(&root, "/private", mode(0o700)).unwrap();
    registry.mount(&root, "/mnt/a", parent.clone()).unwrap();
    registry.mount(&root, "/mnt/a/private/b", volume()).unwrap();
    assert!(matches!(
        registry.route_as(&actor(9), "/mnt/a/private/b"),
        Err(Error::Denied)
    ));
    parent.chmod(&root, "/private", mode(0o711)).unwrap();
    assert!(matches!(
        registry.route_as(&actor(9), "/mnt/a/private/b"),
        Ok(Route::Mounted { .. })
    ));
    assert_eq!(parent.unlink(&root, "/private", true), Err(Error::Busy));
    assert_eq!(
        parent.rename(&root, "/private", "/moved", false),
        Err(Error::Busy)
    );
    assert_eq!(registry.unmount(&root, "/mnt/a"), Err(Error::Busy));
    parent.symlink(&root, "private", "/alias").unwrap();
    assert_eq!(
        registry.mount(&root, "/mnt/a/alias/c", volume()),
        Err(Error::InvalidPath)
    );
    registry.unmount(&root, "/mnt/a/private/b").unwrap();
    parent.rename(&root, "/private", "/moved", false).unwrap();
    drop(parent);
    registry.unmount(&root, "/mnt/a").unwrap();
}

#[test]
fn vfs_namespace_listing_refresh_walk_and_mount_pins() {
    use crate::filesystem::{
        CreationOptions, DataRoot, FsError, ObjectKind, WalkEntry, WalkOptions,
    };
    let root = DataRoot::open(&super::fixture()).unwrap();
    let admin = crate::users::Account {
        uid: 0,
        name: "root".into(),
        home: "/root".into(),
        identity: Default::default(),
    };
    let mut cursor = root.open_directory_as(&admin, "/mnt").unwrap();
    assert!(cursor.is_empty());
    let limits = Limits {
        bytes: 65536,
        inodes: 64,
    };
    root.mount_memory_as(&admin, "/mnt/outer", limits).unwrap();
    root.create_directory_as(&admin, "/mnt/outer/parent", CreationOptions::default())
        .unwrap();
    root.mount_memory_as(&admin, "/mnt/outer/parent/inner", limits)
        .unwrap();
    root.create_file_as(&admin, "/mnt/outer/parent/inner/file")
        .unwrap();
    let entries = root.list_as(&admin, "/mnt").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "outer");
    let walked = root
        .walk_as(&admin, "/mnt", WalkOptions::default())
        .unwrap();
    assert!(walked.iter().any(|e| matches!(e, WalkEntry::Accessible { path, .. } if path == "/mnt/outer/parent/inner/file")));
    cursor.refresh().unwrap();
    let page = cursor.read(16).unwrap();
    assert_eq!(page[0].kind, ObjectKind::Directory);
    let nested = root.open_directory_as(&admin, "/mnt/outer/parent").unwrap();
    assert!(matches!(
        root.unmount_as(&admin, "/mnt/outer/parent/inner"),
        Err(FsError::Vfs(Error::Busy))
    ));
    drop(nested);
    root.unmount_as(&admin, "/mnt/outer/parent/inner").unwrap();
    assert!(matches!(
        root.unmount_as(&admin, "/mnt/outer"),
        Err(FsError::Vfs(Error::Busy))
    ));
    drop(cursor);
    root.unmount_as(&admin, "/mnt/outer").unwrap();
    assert!(root.list_as(&admin, "/mnt").unwrap().is_empty());
    assert!(!root.root().join("mnt/outer").exists());
}

#[test]
fn sealed_checkpoint_authenticates_nonce_ciphertext_tag_and_key_before_loading() {
    let volume = volume();
    let root = actor(0);
    volume
        .create(&root, "/secret", b"confidential", mode(0o600))
        .unwrap();
    let key = [17; 32];
    let first = volume.sealed_image(&root, &key).unwrap();
    let second = volume.sealed_image(&root, &key).unwrap();
    assert_ne!(first.as_slice(), second.as_slice());
    let loaded = Volume::load_sealed_image(&first, &key).unwrap();
    let f = loaded
        .volume
        .open(&root, "/secret", FileMode::READ, ShareMode::Shared)
        .unwrap();
    assert_eq!(f.read(64).unwrap(), b"confidential");
    assert!(matches!(
        Volume::load_sealed_image(&first, &[18; 32]),
        Err(Error::Crypto)
    ));
    assert!(matches!(
        volume.sealed_image(&actor(1000), &key),
        Err(Error::Denied)
    ));
    for index in [0, 8, 19, 20, first.len() - 1] {
        let mut changed = first.to_vec();
        changed[index] ^= 1;
        assert!(matches!(
            Volume::load_sealed_image(&changed, &key),
            Err(Error::Crypto)
        ));
    }
    for length in [0, 7, 19, 35, first.len() - 1] {
        assert!(matches!(
            Volume::load_sealed_image(&first[..length], &key),
            Err(Error::Crypto)
        ));
    }
}

#[test]
fn overlay_frozen_merge_whiteouts_links_and_cow_do_not_modify_sources() {
    let lower = volume();
    let upper = volume();
    let root = actor(0);
    lower.mkdir(&root, "/dir", mode(0o700)).unwrap();
    upper.mkdir(&root, "/dir", mode(0o700)).unwrap();
    lower
        .create(&root, "/dir/lower", b"lower", mode(0o600))
        .unwrap();
    lower.create(&root, "/same", b"old", mode(0o600)).unwrap();
    lower
        .create(&root, "/hidden", b"hidden", mode(0o600))
        .unwrap();
    upper
        .create(&root, "/dir/upper", b"upper", mode(0o600))
        .unwrap();
    upper.create(&root, "/same", b"new", mode(0o600)).unwrap();
    upper.link(&root, "/same", "/hard").unwrap();
    let merged = Volume::overlay_snapshot(&root, &lower, &upper, &["/hidden".into()]).unwrap();
    let read = |v: &std::sync::Arc<Volume>, path: &str| {
        v.open(&root, path, FileMode::READ, ShareMode::Shared)
            .unwrap()
            .read(64)
            .unwrap()
    };
    assert_eq!(read(&merged, "/same"), b"new");
    assert_eq!(read(&merged, "/dir/lower"), b"lower");
    assert_eq!(read(&merged, "/dir/upper"), b"upper");
    assert!(matches!(
        merged.stat(&root, "/hidden", true),
        Err(Error::NotFound)
    ));
    assert_eq!(
        merged.stat(&root, "/hard", true).unwrap().inode,
        merged.stat(&root, "/same", true).unwrap().inode
    );
    assert!(merged.stats().unwrap().shared_data_pages > 0);
    let file = merged
        .open(&root, "/same", rw(), ShareMode::Shared)
        .unwrap();
    file.write_at(0, b"X").unwrap();
    assert_eq!(read(&merged, "/hard"), b"Xew");
    assert_eq!(read(&upper, "/same"), b"new");
    assert_eq!(read(&lower, "/same"), b"old");
    assert!(merged.stats().unwrap().cow_copied_bytes >= 4096);
    lower.create(&root, "/later", b"late", mode(0o600)).unwrap();
    assert!(matches!(
        merged.stat(&root, "/later", true),
        Err(Error::NotFound)
    ));
    assert_eq!(read(&lower, "/hidden"), b"hidden");
    let encoded = merged.encode(&root).unwrap();
    let restored = Volume::decode(&encoded, false).unwrap();
    assert!(matches!(
        restored.stat(&root, "/hidden", true),
        Err(Error::NotFound)
    ));
    assert_eq!(read(&restored, "/hard"), b"Xew");
    assert!(matches!(
        Volume::overlay_snapshot(&actor(5), &lower, &upper, &[]),
        Err(Error::Denied)
    ));
}

#[test]
fn overlay_rejects_unsafe_whiteouts_and_upper_quota_without_changing_sources() {
    let lower = volume();
    let root = actor(0);
    lower.mkdir(&root, "/dir", mode(0o700)).unwrap();
    lower
        .create(&root, "/dir/keep", b"keep", mode(0o600))
        .unwrap();
    lower.symlink(&root, "/dir", "/alias").unwrap();
    let upper = volume();
    for path in ["/", "/../dir", "/dir//keep", "relative"] {
        assert!(matches!(
            Volume::overlay_snapshot(&root, &lower, &upper, &[path.into()]),
            Err(Error::InvalidPath)
        ));
    }
    assert!(matches!(
        Volume::overlay_snapshot(&root, &lower, &upper, &["/alias/keep".into()]),
        Err(Error::NotDirectory)
    ));
    let small = Volume::memory(
        FileOwner { uid: 0, gid: 0 },
        Limits {
            bytes: 1,
            inodes: 2,
        },
    )
    .unwrap();
    assert!(matches!(
        Volume::overlay_snapshot(&root, &lower, &small, &[]),
        Err(Error::Quota)
    ));
    assert_eq!(lower.stat(&root, "/dir/keep", true).unwrap().length, 4);
    // Whiteout suppresses only lower: a same-name upper entry remains visible.
    upper.create(&root, "/dir", b"upper", mode(0o600)).unwrap();
    let merged = Volume::overlay_snapshot(&root, &lower, &upper, &["/dir".into()]).unwrap();
    assert_eq!(
        merged.stat(&root, "/dir", true).unwrap().kind,
        InodeKind::File
    );
}

#[test]
fn dedup_runtime_shares_equal_pages_without_changing_bytes_identity_or_quota() {
    let root = actor(0);
    let v = volume();
    let a = v.create(&root, "/a", &[7; 4096], mode(0o600)).unwrap();
    let b = v.create(&root, "/b", &[7; 4096], mode(0o600)).unwrap();
    v.create(&root, "/c", &[8; 4096], mode(0o600)).unwrap();
    let fd = v.open(&root, "/b", rw(), ShareMode::Shared).unwrap();
    let snapshot = v.snapshot(&root, true).unwrap();
    let revision = v.revision().unwrap();
    let before = v.stats().unwrap();
    let stats = v.deduplicate(&root).unwrap();
    assert_eq!(stats.page_references, 3);
    assert_eq!(stats.unique_pages_before, 3);
    assert_eq!(stats.unique_pages_after, 2);
    assert_eq!(stats.redirected_references, 1);
    assert_eq!(stats.referenced_bytes_reduced, 4096);
    assert_eq!(v.revision().unwrap(), revision);
    assert_eq!(v.stats().unwrap().bytes, before.bytes);
    assert_eq!(v.stat(&root, "/a", true).unwrap().inode, a);
    assert_eq!(v.stat(&root, "/b", true).unwrap().inode, b);
    assert_eq!(fd.read_at(0, 4096).unwrap(), [7; 4096]);
    let again = v.deduplicate(&root).unwrap();
    assert_eq!(again.redirected_references, 0);
    assert_eq!(again.referenced_bytes_reduced, 0);
    fd.write_at(0, b"X").unwrap();
    assert_eq!(
        v.open(&root, "/a", FileMode::READ, ShareMode::Shared)
            .unwrap()
            .read(1)
            .unwrap(),
        [7]
    );
    assert_eq!(
        snapshot
            .open(&root, "/b", FileMode::READ, ShareMode::Shared)
            .unwrap()
            .read(1)
            .unwrap(),
        [7]
    );
    assert_eq!(fd.read_at(0, 1).unwrap(), b"X");
    assert_eq!(v.deduplicate(&root).unwrap().unique_pages_before, 3);
    assert!(matches!(snapshot.deduplicate(&root), Err(Error::ReadOnly)));
    assert!(matches!(v.deduplicate(&actor(5)), Err(Error::Denied)));
}

#[test]
fn dedup_sparse_tail_hardlinks_and_open_orphans_keep_lifetimes() {
    let root = actor(0);
    let v = volume();
    v.create(&root, "/short", b"tail", mode(0o600)).unwrap();
    v.create(&root, "/orphan", b"tail", mode(0o600)).unwrap();
    v.link(&root, "/short", "/alias").unwrap();
    let orphan = v.open(&root, "/orphan", rw(), ShareMode::Shared).unwrap();
    v.unlink(&root, "/orphan", false).unwrap();
    v.create(&root, "/zero", &[0; 8192], mode(0o600)).unwrap();
    let stats = v.deduplicate(&root).unwrap();
    assert_eq!(stats.page_references, 2); // hardlink adds no page; sparse zero has none
    assert_eq!(stats.unique_pages_before, 2);
    assert_eq!(stats.unique_pages_after, 1); // open orphan participates until FD closes
    orphan.write_at(4, b"X").unwrap();
    assert_eq!(orphan.read_at(0, 16).unwrap(), b"tailX");
    assert_eq!(
        v.open(&root, "/alias", FileMode::READ, ShareMode::Shared)
            .unwrap()
            .read(16)
            .unwrap(),
        b"tail"
    );
    drop(orphan);
    let final_stats = v.deduplicate(&root).unwrap();
    assert_eq!(final_stats.page_references, 1);
    assert_eq!(final_stats.unique_pages_after, 1);
    assert_eq!(v.stat(&root, "/zero", true).unwrap().length, 8192);
    assert!(matches!(
        Volume::pseudo(PseudoKind::Proc).unwrap().deduplicate(&root),
        Err(Error::Unsupported)
    ));
}

#[test]
fn persistent_snapshot_all_codecs_keep_frozen_mode_and_explicit_clone_is_writable() {
    let root = actor(0);
    let source = volume();
    source
        .create(&root, "/file", b"before", mode(0o600))
        .unwrap();
    source.link(&root, "/file", "/alias").unwrap();
    let snapshot = source.snapshot(&root, true).unwrap();
    source
        .open(&root, "/file", rw(), ShareMode::Shared)
        .unwrap()
        .write_at(0, b"source")
        .unwrap();
    let encoded = snapshot.encode(&root).unwrap();
    assert!(encoded.starts_with(b"ELARASN1\n"));
    let image = snapshot.image(&root).unwrap().0;
    let records = snapshot.block_records(&root).unwrap();
    let key = [42; 32];
    let sealed = snapshot.sealed_image(&root, &key).unwrap();
    let restored = [
        Volume::decode(&encoded, false).unwrap(),
        Volume::load_image(&image).unwrap().volume,
        Volume::load_block_records(&records, false).unwrap(),
        Volume::load_sealed_image(&sealed, &key).unwrap().volume,
    ];
    for frozen in restored {
        assert_eq!(frozen.stats().unwrap().access, VolumeAccess::FrozenSnapshot);
        assert_eq!(frozen.encode(&root).unwrap(), encoded);
        assert_eq!(
            frozen.stat(&root, "/file", true).unwrap().inode,
            frozen.stat(&root, "/alias", true).unwrap().inode
        );
        assert_eq!(
            frozen
                .open(&root, "/file", FileMode::READ, ShareMode::Shared)
                .unwrap()
                .read(32)
                .unwrap(),
            b"before"
        );
        assert!(matches!(
            frozen.open(&root, "/file", rw(), ShareMode::Shared),
            Err(Error::ReadOnly)
        ));
        assert_eq!(
            frozen.create(&root, "/new", b"denied", mode(0o600)),
            Err(Error::ReadOnly)
        );
        assert_eq!(frozen.unlink(&root, "/alias", false), Err(Error::ReadOnly));
        assert!(matches!(frozen.deduplicate(&root), Err(Error::ReadOnly)));
        let clone = frozen.snapshot(&root, false).unwrap();
        assert_eq!(clone.stats().unwrap().access, VolumeAccess::ReadWrite);
        clone
            .open(&root, "/file", rw(), ShareMode::Shared)
            .unwrap()
            .write_at(0, b"CLONE!")
            .unwrap();
        assert_eq!(
            frozen
                .open(&root, "/alias", FileMode::READ, ShareMode::Shared)
                .unwrap()
                .read(32)
                .unwrap(),
            b"before"
        );
        assert_eq!(
            clone
                .open(&root, "/alias", FileMode::READ, ShareMode::Shared)
                .unwrap()
                .read(32)
                .unwrap(),
            b"CLONE!"
        );
        assert!(clone.encode(&root).unwrap().starts_with(b"{"));
    }
}

#[test]
fn persistent_snapshot_recovery_is_distinct_and_legacy_bytes_stay_compatible() {
    let root = actor(0);
    let source = volume();
    source.create(&root, "/f", b"payload", mode(0o600)).unwrap();
    let legacy = source.encode(&root).unwrap();
    assert!(legacy.starts_with(b"{"));
    let ordinary = Volume::decode(&legacy, false).unwrap();
    assert_eq!(ordinary.stats().unwrap().access, VolumeAccess::ReadWrite);
    let recovered = Volume::decode(&legacy, true).unwrap();
    assert_eq!(
        recovered.stats().unwrap().access,
        VolumeAccess::RecoveryReadOnly
    );
    assert_eq!(recovered.encode(&root).unwrap(), legacy);
    let snapshot = source.snapshot(&root, true).unwrap();
    let frozen_bytes = snapshot.encode(&root).unwrap();
    let mut image = snapshot.image(&root).unwrap().0;
    *image.last_mut().unwrap() ^= 1;
    let recovered = Volume::load_image(&image).unwrap();
    assert_eq!(
        recovered.recovery,
        Recovery::RecoveredReadOnly { damaged_copies: 1 }
    );
    assert_eq!(
        recovered.volume.stats().unwrap().access,
        VolumeAccess::FrozenRecoveredReadOnly
    );
    assert_eq!(recovered.volume.encode(&root).unwrap(), frozen_bytes);
    let blocks = snapshot.block_records(&root).unwrap();
    assert_eq!(
        Volume::load_block_records(&blocks, true)
            .unwrap()
            .stats()
            .unwrap()
            .access,
        VolumeAccess::FrozenRecoveredReadOnly
    );
    let mut future = frozen_bytes;
    future[7] = b'2';
    assert!(matches!(
        Volume::decode(&future, false),
        Err(Error::InvalidImage)
    ));
    assert!(matches!(
        Volume::decode(b"ELARASN1\n", false),
        Err(Error::InvalidImage)
    ));
    assert_eq!(
        Volume::pseudo(PseudoKind::Dev)
            .unwrap()
            .stats()
            .unwrap()
            .access,
        VolumeAccess::VirtualProvider
    );
}

#[test]
fn cross_mount_links_resolve_chain_but_never_host_and_keep_name_operations() {
    let registry = Registry::default();
    let root = actor(0);
    let a = volume();
    let b = volume();
    registry.mount(&root, "/mnt/a", a.clone()).unwrap();
    registry.mount(&root, "/mnt/b", b.clone()).unwrap();
    b.create(&root, "/file", b"target", mode(0o600)).unwrap();
    a.symlink_global(&root, "/mnt/b/file", "/link").unwrap();
    let Route::Mounted {
        volume: dest, path, ..
    } = registry
        .resolve_as(&root, "/mnt/a/link", true, |_| Ok(()))
        .unwrap()
    else {
        panic!("route")
    };
    assert_eq!(dest.id(), b.id());
    assert_eq!(path, "/file");
    assert_eq!(
        dest.open(&root, &path, FileMode::READ, ShareMode::Shared)
            .unwrap()
            .read(32)
            .unwrap(),
        b"target"
    );
    let Route::Mounted {
        volume: dest, path, ..
    } = registry
        .resolve_as(&root, "/mnt/a/link", false, |_| Ok(()))
        .unwrap()
    else {
        panic!("route")
    };
    assert_eq!(dest.id(), a.id());
    assert_eq!(path, "/link");
    assert_eq!(a.stat(&root, "/link", true), Err(Error::CrossDevice));
    registry
        .rename(&root, "/mnt/a/link", "/mnt/a/renamed", false)
        .unwrap();
    assert_eq!(a.readlink(&root, "/renamed").unwrap(), "/mnt/b/file");
    a.symlink_global(&root, "/root/private", "/host").unwrap();
    assert!(matches!(
        registry.resolve_as(&root, "/mnt/a/host", true, |_| Ok(())),
        Err(Error::CrossDevice)
    ));
    a.symlink_global(&root, "/mnt/b/cycle", "/cycle").unwrap();
    b.symlink_global(&root, "/mnt/a/cycle", "/cycle").unwrap();
    assert!(matches!(
        registry.resolve_as(&root, "/mnt/a/cycle", true, |_| Ok(())),
        Err(Error::LinkLoop)
    ));
    assert!(matches!(
        registry.resolve_as(&root, "/mnt/a/renamed", true, |p| {
            if p.starts_with("/mnt/b") {
                Err(Error::Denied)
            } else {
                Ok(())
            }
        }),
        Err(Error::Denied)
    ));
    let restored = Volume::decode(&a.encode(&root).unwrap(), false).unwrap();
    assert_eq!(restored.readlink(&root, "/renamed").unwrap(), "/mnt/b/file");
    assert_eq!(
        restored.stat(&root, "/renamed", true),
        Err(Error::CrossDevice)
    );
}

#[test]
fn cross_mount_links_check_intermediate_search_and_do_not_normalize_away_denial() {
    let registry = Registry::default();
    let root = actor(0);
    let guest = actor(8);
    let a = volume();
    let b = volume();
    registry.mount(&root, "/mnt/a", a.clone()).unwrap();
    registry.mount(&root, "/mnt/b", b.clone()).unwrap();
    b.mkdir(&root, "/private", mode(0o700)).unwrap();
    b.create(&root, "/public", b"ok", mode(0o644)).unwrap();
    b.symlink(&root, "private/../public", "/via").unwrap();
    a.symlink_global(&root, "/mnt/b/via", "/link").unwrap();
    assert!(matches!(
        registry.resolve_as(&guest, "/mnt/a/link", true, |_| Ok(())),
        Err(Error::Denied)
    ));
    b.chmod(&root, "/private", mode(0o711)).unwrap();
    let Route::Mounted { path, .. } = registry
        .resolve_as(&guest, "/mnt/a/link", true, |_| Ok(()))
        .unwrap()
    else {
        panic!("route")
    };
    assert_eq!(path, "/public");
}

#[test]
fn cross_mount_nested_aliases_keep_target_pinned_and_reject_cross_device_mutations() {
    let registry = Registry::default();
    let root = actor(0);
    let outer = volume();
    outer.mkdir(&root, "/parent", mode(0o755)).unwrap();
    registry.mount(&root, "/mnt/outer", outer.clone()).unwrap();
    let inner = volume();
    inner.create(&root, "/file", b"keep", mode(0o600)).unwrap();
    registry
        .mount(&root, "/mnt/outer/parent/inner", inner.clone())
        .unwrap();
    outer.symlink(&root, "parent/inner", "/alias").unwrap();
    let route = registry
        .resolve_as(&root, "/mnt/outer/alias/file", true, |_| Ok(()))
        .unwrap();
    let Route::Mounted {
        volume: target,
        path,
        ..
    } = &route
    else {
        panic!("route")
    };
    assert_eq!(target.id(), inner.id());
    assert_eq!(path, "/file");
    assert_eq!(
        registry.rename(&root, "/mnt/outer/alias/file", "/mnt/outer/moved", false),
        Err(Error::CrossDevice)
    );
    assert_eq!(
        registry.link(&root, "/mnt/outer/alias/file", "/mnt/outer/hard"),
        Err(Error::CrossDevice)
    );
    drop(inner);
    assert_eq!(
        registry.unmount(&root, "/mnt/outer/parent/inner"),
        Err(Error::Busy)
    );
    drop(route);
    registry.unmount(&root, "/mnt/outer/parent/inner").unwrap();
}

#[test]
fn cross_mount_exclusive_open_rejects_all_existing_link_names_without_following() {
    let v = volume();
    let root = actor(0);
    v.symlink(&root, "/missing", "/dangling").unwrap();
    v.symlink(&root, "/loop", "/loop").unwrap();
    v.symlink_global(&root, "/mnt/absent/file", "/global")
        .unwrap();
    let exclusive = FileMode::new(
        FileAccess::ReadWrite,
        FileCreation::Exclusive,
        FileBehavior::Preserve,
    )
    .unwrap();
    let before = v.encode(&root).unwrap();
    for name in ["/dangling", "/loop", "/global"] {
        assert!(matches!(
            v.open(&root, name, exclusive, ShareMode::Shared),
            Err(Error::Exists)
        ));
    }
    assert_eq!(v.encode(&root).unwrap(), before);
    assert_eq!(v.stat(&root, "/missing", false), Err(Error::NotFound));
}
