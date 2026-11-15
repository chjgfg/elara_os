use super::fixture;
use crate::filesystem::{
    vfs::{
        self,
        block_store::{Device, Error, Records, Recovery, Store, BLOCK},
    },
    *,
};
use crate::users::Account;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::{assert, assert_eq, matches, vec};
fn file(path: &std::path::Path) -> std::fs::File {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap()
}
fn fresh(path: &std::path::Path) -> std::fs::File {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap()
}
fn records(a: u8) -> Records {
    Records::from([
        ((2, 1, 0), vec![a; BLOCK]),
        ((2, 1, 1), vec![2; BLOCK]),
        ((2, 2, 0), vec![3; BLOCK]),
    ])
}
fn admin() -> Account {
    Account {
        identity: Default::default(),
        name: "root".into(),
        uid: 0,
        home: "/root".into(),
    }
}
#[test]
fn block_store_single_page_commit_cache_budget_and_two_generation_reuse() {
    let path = fixture().join("home/image");
    let mut store = Store::format(fresh(&path), 24, 1).unwrap();
    store.commit(&records(1)).unwrap();
    let before = store.stats();
    store.commit(&records(4)).unwrap();
    let after = store.stats();
    assert_eq!(after.bytes_written - before.bytes_written, 3 * BLOCK as u64); // one data + manifest + root
    assert!(after.cached_pages <= 1);
    assert_eq!(after.dirty_pages, 0);
    for i in 5..30 {
        store.commit(&records(i)).unwrap();
    }
    assert!(store.stats().allocated_slots <= 9);
    drop(store);
    let mut store = Store::open(file(&path), 8).unwrap();
    assert_eq!(store.recovery(), Recovery::Clean);
    assert_eq!(store.records().unwrap(), records(29));
    store.read((2, 1, 0)).unwrap();
    let hits = store.stats().hits;
    store.read((2, 1, 0)).unwrap();
    assert_eq!(store.stats().hits, hits + 1);
}
#[test]
fn block_store_capacity_refusal_preserves_committed_bytes_and_unknown_format() {
    let path = fixture().join("home/image");
    let mut store = Store::format(fresh(&path), 8, 1).unwrap();
    store.commit(&records(1)).unwrap();
    let before = std::fs::read(&path).unwrap();
    let huge: Records = (0..8).map(|i| ((2, 1, i), vec![9; BLOCK])).collect();
    assert!(matches!(store.commit(&huge), Err(Error::NoSpace)));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(store.records().unwrap(), records(1));
    drop(store);
    assert!(matches!(
        Store::format(file(&path), 8, 1),
        Err(Error::Bounds)
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
struct FaultFile {
    file: std::fs::File,
    calls: usize,
    fail: usize,
}
impl Read for FaultFile {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.file.read(bytes)
    }
}
impl Seek for FaultFile {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.file.seek(from)
    }
}
impl Write for FaultFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let fail = self.calls == self.fail;
        self.calls += 1;
        if fail {
            self.file.write_all(&bytes[..bytes.len() / 2])?;
            return Err(io::Error::other("injected torn write"));
        }
        self.file.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
impl Device for FaultFile {
    fn sync(&mut self) -> io::Result<()> {
        let fail = self.calls == self.fail;
        self.calls += 1;
        if fail {
            Err(io::Error::other("injected uncertain sync"))
        } else {
            self.file.sync_all()
        }
    }
    fn resize(&mut self, n: u64) -> io::Result<()> {
        self.file.set_len(n)
    }
}
#[test]
fn block_store_each_write_or_sync_fault_recovers_only_old_or_complete_new_root() {
    let base = fixture().join("home/base");
    let mut store = Store::format(fresh(&base), 24, 1).unwrap();
    store.commit(&records(1)).unwrap();
    drop(store);
    let before = std::fs::read(&base).unwrap();
    for fault in 0..7 {
        let path = fixture().join("home/fault");
        std::fs::write(&path, &before).unwrap();
        let device = FaultFile {
            file: file(&path),
            calls: 0,
            fail: fault,
        };
        let mut store = Store::open(device, 1).unwrap();
        if store.commit(&records(9)).is_err() {
            assert!(matches!(store.commit(&records(7)), Err(Error::ReadOnly)));
        }
        drop(store);
        let mut recovered = Store::open(file(&path), 1).unwrap();
        let actual = recovered.records().unwrap();
        assert!(
            actual == records(1) || actual == records(9),
            "fault={fault}"
        );
    }
}
#[test]
fn block_store_corrupt_latest_root_is_readonly_and_repair_keeps_verified_survivor() {
    let path = fixture().join("home/image");
    let mut store = Store::format(fresh(&path), 24, 1).unwrap();
    store.commit(&records(1)).unwrap();
    store.commit(&records(9)).unwrap();
    drop(store);
    let mut image = file(&path);
    image.seek(SeekFrom::Start(0)).unwrap();
    image.write_all(b"!").unwrap();
    image.sync_all().unwrap();
    let mut store = Store::open(file(&path), 1).unwrap();
    assert_eq!(
        store.recovery(),
        Recovery::RecoveredReadOnly {
            selected_generation: 2
        }
    );
    assert_eq!(store.records().unwrap(), records(1));
    assert!(matches!(store.commit(&records(9)), Err(Error::ReadOnly)));
    store.repair().unwrap();
    drop(store);
    let mut store = Store::open(file(&path), 1).unwrap();
    assert_eq!(store.recovery(), Recovery::Clean);
    assert_eq!(store.records().unwrap(), records(1));
    drop(store);
    image.seek(SeekFrom::Start(0)).unwrap();
    image.write_all(b"!").unwrap();
    image.seek(SeekFrom::Start(BLOCK as u64)).unwrap();
    image.write_all(b"!").unwrap();
    assert!(matches!(Store::open(file(&path), 1), Err(Error::Invalid)));
}
#[test]
fn block_store_filesystem_roundtrip_incremental_save_and_unknown_boundary() {
    let base = fixture();
    let root = DataRoot::open(&base).unwrap();
    let actor = admin();
    root.mount_memory_as(
        &actor,
        "/mnt/a",
        vfs::Limits {
            bytes: 65536,
            inodes: 64,
        },
    )
    .unwrap();
    root.create_owned_file(
        &actor,
        "/mnt/a/file",
        b"before",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    root.hardlink_as(&actor, "/mnt/a/file", "/mnt/a/link")
        .unwrap();
    root.create_block_image_as(&actor, "/mnt/a", "/root/image", 64, 1)
        .unwrap();
    root.replace_owned_file(&actor, "/mnt/a/file", b"after")
        .unwrap();
    root.save_block_image_as(&actor, "/mnt/a", "/root/image", 1)
        .unwrap();
    root.unmount_as(&actor, "/mnt/a").unwrap();
    drop(root);
    let root = DataRoot::open(&base).unwrap();
    assert_eq!(
        root.load_block_image_as(&actor, "/root/image", "/mnt/b", 1)
            .unwrap(),
        Recovery::Clean
    );
    assert_eq!(
        root.read_file_as(&actor, "/mnt/b/file", 64).unwrap(),
        b"after"
    );
    assert_eq!(
        root.read_file_as(&actor, "/mnt/b/link", 64).unwrap(),
        b"after"
    );
    std::fs::write(base.join("root/unknown"), b"keep").unwrap();
    assert!(root
        .create_block_image_as(&actor, "/mnt/b", "/root/unknown", 64, 1)
        .is_err());
    assert!(matches!(
        root.load_block_image_as(&actor, "/root/unknown", "/mnt/no", 1),
        Err(FsError::LegacyOwnershipUnknown)
    ));
    assert_eq!(std::fs::read(base.join("root/unknown")).unwrap(), b"keep");
}

#[test]
fn block_store_fragmented_free_slots_are_reused_without_contiguous_allocation() {
    let path = fixture().join("home/fragmented");
    let mut store = Store::format(fresh(&path), 16, 1).unwrap();
    let mut data: Records = (0..8)
        .map(|i| ((2, 1, i), vec![i as u8 + 1; BLOCK]))
        .collect();
    store.commit(&data).unwrap();
    data.retain(|key, _| key.2 % 2 == 0);
    store.commit(&data).unwrap();
    store.commit(&data).unwrap(); // retire the last root referencing deleted odd pages
    let stats = store.stats();
    assert!(stats.free_slots > stats.largest_free_run);
    assert!(stats.free_slots >= 6);
    assert!(stats.largest_free_run < 6);
    for i in 10..15 {
        data.insert((2, 2, i), vec![9; BLOCK]);
    }
    store.commit(&data).unwrap(); // five data pages + manifest fit only when holes are reused
    drop(store);
    assert_eq!(
        Store::open(file(&path), 1).unwrap().records().unwrap(),
        data
    );
}

#[test]
fn block_store_data_corruption_falls_back_and_shared_corruption_refuses_both_roots() {
    let path = fixture().join("home/corrupt-data");
    let mut store = Store::format(fresh(&path), 24, 1).unwrap();
    store.commit(&records(1)).unwrap();
    store.commit(&records(9)).unwrap();
    drop(store);
    let mut image = std::fs::read(&path).unwrap();
    let changed = image
        .chunks_exact(BLOCK)
        .position(|p| p.iter().all(|b| *b == 9))
        .unwrap();
    image[changed * BLOCK] = 99;
    std::fs::write(&path, &image).unwrap();
    let mut recovered = Store::open(file(&path), 1).unwrap();
    assert_eq!(
        recovered.recovery(),
        Recovery::RecoveredReadOnly {
            selected_generation: 2
        }
    );
    assert_eq!(recovered.records().unwrap(), records(1));
    drop(recovered);
    let shared = image
        .chunks_exact(BLOCK)
        .position(|p| p.iter().all(|b| *b == 2))
        .unwrap();
    image[shared * BLOCK] = 99;
    std::fs::write(&path, &image).unwrap();
    assert!(matches!(Store::open(file(&path), 1), Err(Error::Invalid)));
}

#[test]
fn block_store_inode_pages_preserve_sparse_lengths_and_exclude_open_orphans() {
    let actor = Credentials {
        uid: 0,
        gid: 0,
        groups: std::collections::BTreeSet::from([0]),
    };
    let volume = vfs::Volume::memory(
        FileOwner { uid: 0, gid: 0 },
        vfs::Limits {
            bytes: 131072,
            inodes: 64,
        },
    )
    .unwrap();
    volume
        .create(&actor, "/sparse", b"", VirtualMode::new(0o600).unwrap())
        .unwrap();
    let mode = FileMode::new(
        FileAccess::ReadWrite,
        FileCreation::Existing,
        FileBehavior::Preserve,
    )
    .unwrap();
    let sparse = volume
        .open(&actor, "/sparse", mode, vfs::ShareMode::Shared)
        .unwrap();
    sparse.write_at(65535, b"X").unwrap();
    volume
        .create(
            &actor,
            "/orphan",
            b"private",
            VirtualMode::new(0o600).unwrap(),
        )
        .unwrap();
    let orphan = volume
        .open(&actor, "/orphan", mode, vfs::ShareMode::Shared)
        .unwrap();
    volume.unlink(&actor, "/orphan", false).unwrap();
    let records = volume.block_records(&actor).unwrap();
    assert_eq!(records.keys().filter(|key| key.0 == 2).count(), 1);
    let loaded = vfs::Volume::load_block_records(&records, false).unwrap();
    assert_eq!(loaded.stats().unwrap().inodes, 2);
    assert_eq!(loaded.stat(&actor, "/sparse", true).unwrap().length, 65536);
    assert!(matches!(
        loaded.stat(&actor, "/orphan", true),
        Err(vfs::Error::NotFound)
    ));
    let file = loaded
        .open(&actor, "/sparse", FileMode::READ, vfs::ShareMode::Shared)
        .unwrap();
    assert_eq!(file.read_at(65533, 3).unwrap(), [0, 0, b'X']);
    assert_eq!(orphan.read_at(0, 16).unwrap(), b"private");
    let mut invalid = records.clone();
    invalid.insert((2, u64::MAX, 0), vec![1]);
    assert!(matches!(
        vfs::Volume::load_block_records(&invalid, false),
        Err(vfs::Error::InvalidImage)
    ));
}

#[test]
fn block_store_initial_capacity_is_checked_before_creating_owned_image() {
    let base = fixture();
    let root = DataRoot::open(&base).unwrap();
    let actor = admin();
    root.mount_memory_as(
        &actor,
        "/mnt/a",
        vfs::Limits {
            bytes: 65536,
            inodes: 64,
        },
    )
    .unwrap();
    root.create_owned_file(
        &actor,
        "/mnt/a/file",
        &[7; 32768],
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        root.create_block_image_as(&actor, "/mnt/a", "/root/too-small", 8, 1),
        Err(FsError::Vfs(vfs::Error::NoSpace))
    ));
    assert!(!base.join("root/too-small").exists());
    assert!(matches!(
        root.owned_file_info("/root/too-small").unwrap(),
        OwnershipState::LegacyUnknown
    ));
    root.create_block_image_as(&actor, "/mnt/a", "/root/fits", 32, 1)
        .unwrap();
}

#[test]
fn persistent_snapshot_block_repair_preserves_frozen_mode_after_recovery() {
    let base = fixture();
    let root = DataRoot::open(&base).unwrap();
    let actor = admin();
    root.mount_memory_as(
        &actor,
        "/mnt/source",
        vfs::Limits {
            bytes: 65536,
            inodes: 64,
        },
    )
    .unwrap();
    root.create_owned_file(
        &actor,
        "/mnt/source/file",
        b"frozen",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    root.snapshot_mount_as(&actor, "/mnt/source", "/mnt/snapshot", true)
        .unwrap();
    root.create_block_image_as(&actor, "/mnt/snapshot", "/root/image", 64, 1)
        .unwrap();
    // Generation 2 contains the snapshot; generation 3 republishes it. Damage
    // slot 0 (generation 3) so the surviving root still has a complete tree.
    root.save_block_image_as(&actor, "/mnt/snapshot", "/root/image", 1)
        .unwrap();
    {
        let mut image = file(&base.join("root/image"));
        image.seek(SeekFrom::Start(0)).unwrap();
        image.write_all(b"!").unwrap();
        image.sync_all().unwrap();
    }
    assert_eq!(
        root.load_block_image_as(&actor, "/root/image", "/mnt/recovered", 1)
            .unwrap(),
        Recovery::RecoveredReadOnly {
            selected_generation: 2
        }
    );
    assert_eq!(
        root.volume_stats_as(&actor, "/mnt/recovered")
            .unwrap()
            .access,
        vfs::VolumeAccess::FrozenRecoveredReadOnly
    );
    root.unmount_as(&actor, "/mnt/recovered").unwrap();
    root.repair_block_image_as(&actor, "/root/image", 1)
        .unwrap();
    assert_eq!(
        root.load_block_image_as(&actor, "/root/image", "/mnt/repaired", 1)
            .unwrap(),
        Recovery::Clean
    );
    assert_eq!(
        root.volume_stats_as(&actor, "/mnt/repaired")
            .unwrap()
            .access,
        vfs::VolumeAccess::FrozenSnapshot
    );
    assert!(matches!(
        root.replace_owned_file(&actor, "/mnt/repaired/file", b"denied"),
        Err(FsError::Vfs(vfs::Error::ReadOnly))
    ));
    assert_eq!(
        root.read_file_as(&actor, "/mnt/repaired/file", 32).unwrap(),
        b"frozen"
    );
}
