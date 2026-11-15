//! Same-object probes must use lock behaviour, not equal data or path strings.
use super::fixture;
use crate::filesystem::{DataRoot, ObjectBinding};
use std::{assert, assert_eq, fs, matches};

#[test]
fn repeated_path_opens_are_the_same_object_without_changing_bytes() {
    let path = fixture();
    fs::write(path.join("tmp/a"), b"unchanged").unwrap();
    let root = DataRoot::open(&path).unwrap();
    assert_eq!(
        root.compare_file_objects("/tmp/a", "/tmp/a").unwrap(),
        ObjectBinding::SameObject
    );
    assert_eq!(fs::read(path.join("tmp/a")).unwrap(), b"unchanged");
}

#[test]
fn equal_bytes_in_distinct_files_do_not_prove_identity() {
    let path = fixture();
    fs::write(path.join("tmp/a"), b"identical").unwrap();
    fs::write(path.join("tmp/b"), b"identical").unwrap();
    let root = DataRoot::open(&path).unwrap();
    assert_eq!(
        fs::read(path.join("tmp/a")).unwrap(),
        fs::read(path.join("tmp/b")).unwrap()
    );
    assert_eq!(
        root.compare_file_objects("/tmp/a", "/tmp/b").unwrap(),
        ObjectBinding::DifferentObject
    );
}

#[test]
fn native_internal_hardlink_has_the_same_lock_object() {
    let path = fixture();
    fs::write(path.join("tmp/a"), b"data").unwrap();
    fs::hard_link(path.join("tmp/a"), path.join("tmp/b")).unwrap();
    let root = DataRoot::open(&path).unwrap();
    assert_eq!(
        root.compare_file_objects("/tmp/a", "/tmp/b").unwrap(),
        ObjectBinding::SameObject
    );
}

#[test]
fn preexisting_lock_is_unconfirmed_not_a_false_identity_and_drop_releases_probe_locks() {
    let path = fixture();
    fs::write(path.join("tmp/a"), b"same").unwrap();
    fs::write(path.join("tmp/b"), b"same").unwrap();
    let root = DataRoot::open(&path).unwrap();
    let locked = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.join("tmp/b"))
        .unwrap();
    locked.lock().unwrap();
    assert_eq!(
        root.compare_file_objects("/tmp/a", "/tmp/b").unwrap(),
        ObjectBinding::Unconfirmed(std::io::ErrorKind::WouldBlock)
    );
    locked.unlock().unwrap();
    assert_eq!(
        root.compare_file_objects("/tmp/a", "/tmp/b").unwrap(),
        ObjectBinding::DifferentObject
    );
    locked.try_lock().unwrap();
    locked.unlock().unwrap();
}

#[test]
fn opened_identity_survives_rename_replacement_unlink_and_independent_domains() {
    use crate::filesystem::{FileAccess, FileBehavior, FileCreation, FileMode, VirtualMode};
    use crate::users::Account;
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let actor = Account {
        identity: Default::default(),
        name: "root".into(),
        uid: 0,
        home: "/root".into(),
    };
    let mode = FileMode::new(
        FileAccess::ReadWrite,
        FileCreation::Existing,
        FileBehavior::Preserve,
    )
    .unwrap();
    root.create_owned_file(&actor, "/root/a", b"same", VirtualMode::new(0o600).unwrap())
        .unwrap();
    let first = root.open_file_as(&actor, "/root/a", mode).unwrap();
    let independent = DataRoot::open(&path)
        .unwrap()
        .open_file_as(&actor, "/root/a", mode)
        .unwrap();
    assert_eq!(
        first.compare_identity(&first.clone()).unwrap(),
        ObjectBinding::SameObject
    );
    assert_eq!(
        first.compare_identity(&independent).unwrap(),
        ObjectBinding::SameObject
    );
    first.read(1).unwrap();
    root.rename_owned_file(&actor, "/root/a", "/root/moved")
        .unwrap();
    let renamed = root.open_file_as(&actor, "/root/moved", mode).unwrap();
    assert_eq!(
        first.compare_identity(&renamed).unwrap(),
        ObjectBinding::SameObject
    );
    root.replace_owned_file(&actor, "/root/moved", b"same")
        .unwrap();
    let replacement = root.open_file_as(&actor, "/root/moved", mode).unwrap();
    assert_eq!(
        first.compare_identity(&replacement).unwrap(),
        ObjectBinding::DifferentObject
    );
    assert_eq!(
        first.compare_identity(&independent).unwrap(),
        ObjectBinding::SameObject
    );
    root.remove_owned_file(&actor, "/root/moved").unwrap();
    assert_eq!(
        first.compare_identity(&replacement).unwrap(),
        ObjectBinding::DifferentObject
    );
    assert_eq!(first.read(8).unwrap(), b"ame");
    assert_eq!(independent.read(8).unwrap(), b"same");
    assert_eq!(replacement.read(8).unwrap(), b"same");
}

#[test]
fn opened_identity_conflicting_probe_is_unconfirmed_and_never_releases_external_lock() {
    use crate::filesystem::{FileAccess, FileBehavior, FileCreation, FileMode};
    let path = fixture();
    fs::write(path.join("tmp/a"), b"same").unwrap();
    fs::write(path.join("tmp/b"), b"same").unwrap();
    let root = DataRoot::open(&path).unwrap();
    let mode = FileMode::new(
        FileAccess::ReadWrite,
        FileCreation::Existing,
        FileBehavior::Preserve,
    )
    .unwrap();
    let a = root.open_file("/tmp/a", mode).unwrap();
    let b = root.open_file("/tmp/b", mode).unwrap();
    let external = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.join("tmp/b"))
        .unwrap();
    external.lock().unwrap();
    assert_eq!(
        a.compare_identity(&b).unwrap(),
        ObjectBinding::Unconfirmed(std::io::ErrorKind::WouldBlock)
    );
    let check = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.join("tmp/b"))
        .unwrap();
    assert!(matches!(
        check.try_lock(),
        Err(fs::TryLockError::WouldBlock)
    ));
    external.unlock().unwrap();
    assert_eq!(
        a.compare_identity(&b).unwrap(),
        ObjectBinding::DifferentObject
    );
    check.try_lock().unwrap();
    check.unlock().unwrap();
    std::thread::scope(|scope| {
        let left = scope.spawn(|| {
            for _ in 0..16 {
                assert_eq!(
                    a.compare_identity(&b).unwrap(),
                    ObjectBinding::DifferentObject
                );
            }
        });
        let right = scope.spawn(|| {
            for _ in 0..16 {
                assert_eq!(
                    b.compare_identity(&a).unwrap(),
                    ObjectBinding::DifferentObject
                );
            }
        });
        left.join().unwrap();
        right.join().unwrap();
    });
}

#[test]
fn opened_identity_virtual_aliases_and_backend_domains_do_not_collide() {
    use crate::filesystem::{vfs::Limits, FileMode, VirtualMode};
    use crate::users::Account;
    let root = DataRoot::open(&fixture()).unwrap();
    let actor = Account {
        identity: Default::default(),
        name: "root".into(),
        uid: 0,
        home: "/root".into(),
    };
    root.create_owned_file(&actor, "/root/a", b"same", VirtualMode::new(0o600).unwrap())
        .unwrap();
    let host = root
        .open_file_as(&actor, "/root/a", FileMode::READ)
        .unwrap();
    for mount in ["/mnt/a", "/mnt/b"] {
        root.mount_memory_as(
            &actor,
            mount,
            Limits {
                bytes: 4096,
                inodes: 16,
            },
        )
        .unwrap();
        root.create_file_as(&actor, &std::format!("{mount}/f"))
            .unwrap();
    }
    root.hardlink_as(&actor, "/mnt/a/f", "/mnt/a/alias")
        .unwrap();
    let a = root
        .open_file_as(&actor, "/mnt/a/f", FileMode::READ)
        .unwrap();
    let alias = root
        .open_file_as(&actor, "/mnt/a/alias", FileMode::READ)
        .unwrap();
    let b = root
        .open_file_as(&actor, "/mnt/b/f", FileMode::READ)
        .unwrap();
    assert_eq!(
        a.compare_identity(&alias).unwrap(),
        ObjectBinding::SameObject
    );
    assert_eq!(
        a.compare_identity(&b).unwrap(),
        ObjectBinding::DifferentObject
    );
    assert_eq!(
        host.compare_identity(&a).unwrap(),
        ObjectBinding::DifferentObject
    );
    root.remove_file_as(&actor, "/mnt/a/f").unwrap();
    root.remove_file_as(&actor, "/mnt/a/alias").unwrap();
    assert_eq!(
        a.compare_identity(&alias).unwrap(),
        ObjectBinding::SameObject
    );
}
