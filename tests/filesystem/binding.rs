//! Same-object probes must use lock behaviour, not equal data or path strings.
use super::fixture;
use crate::filesystem::{DataRoot, ObjectBinding};
use std::{assert_eq, fs};

#[test]
fn repeated_path_opens_are_the_same_object_without_changing_bytes() {
    let path = fixture();
    fs::write(path.join("tmp/a"), b"unchanged").unwrap();
    let root = DataRoot::open(&path).unwrap();
    assert_eq!(root.compare_file_objects("/tmp/a", "/tmp/a").unwrap(), ObjectBinding::SameObject);
    assert_eq!(fs::read(path.join("tmp/a")).unwrap(), b"unchanged");
}

#[test]
fn equal_bytes_in_distinct_files_do_not_prove_identity() {
    let path = fixture();
    fs::write(path.join("tmp/a"), b"identical").unwrap();
    fs::write(path.join("tmp/b"), b"identical").unwrap();
    let root = DataRoot::open(&path).unwrap();
    assert_eq!(fs::read(path.join("tmp/a")).unwrap(), fs::read(path.join("tmp/b")).unwrap());
    assert_eq!(root.compare_file_objects("/tmp/a", "/tmp/b").unwrap(), ObjectBinding::DifferentObject);
}

#[test]
fn native_internal_hardlink_has_the_same_lock_object() {
    let path = fixture();
    fs::write(path.join("tmp/a"), b"data").unwrap();
    fs::hard_link(path.join("tmp/a"), path.join("tmp/b")).unwrap();
    let root = DataRoot::open(&path).unwrap();
    assert_eq!(root.compare_file_objects("/tmp/a", "/tmp/b").unwrap(), ObjectBinding::SameObject);
}

#[test]
fn preexisting_lock_is_unconfirmed_not_a_false_identity_and_drop_releases_probe_locks() {
    let path = fixture();
    fs::write(path.join("tmp/a"), b"same").unwrap();
    fs::write(path.join("tmp/b"), b"same").unwrap();
    let root = DataRoot::open(&path).unwrap();
    let locked = fs::OpenOptions::new().read(true).write(true).open(path.join("tmp/b")).unwrap();
    locked.lock().unwrap();
    assert_eq!(root.compare_file_objects("/tmp/a", "/tmp/b").unwrap(), ObjectBinding::Unconfirmed(std::io::ErrorKind::WouldBlock));
    locked.unlock().unwrap();
    assert_eq!(root.compare_file_objects("/tmp/a", "/tmp/b").unwrap(), ObjectBinding::DifferentObject);
    locked.try_lock().unwrap();
    locked.unlock().unwrap();
}
