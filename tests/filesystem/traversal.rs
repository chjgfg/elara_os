use super::fixture;
use crate::filesystem::{DataRoot, FsError, ObjectKind, WalkEntry, WalkOptions};
use std::{assert, assert_eq, fs, matches};

#[test]
fn bounded_walk_reports_a_sorted_tree_without_creating_private_storage() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    fs::create_dir(root.join("home/tree")).unwrap();
    fs::write(root.join("home/tree/b"), b"bbb").unwrap();
    fs::write(root.join("home/a"), b"a").unwrap();
    let entries = data.walk("/home", WalkOptions::default()).unwrap();
    let paths: Vec<_> = entries
        .iter()
        .map(|entry| match entry {
            WalkEntry::Accessible { path, .. } => path.as_str(),
            _ => std::panic!("unexpected blocked name"),
        })
        .collect();
    assert_eq!(paths, ["/home", "/home/a", "/home/tree", "/home/tree/b"]);
    assert!(!root.join("tmp/.elara-transactions").exists());
}
#[test]
fn traversal_limits_never_return_a_partial_tree_as_complete() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    fs::create_dir(root.join("home/tree")).unwrap();
    fs::write(root.join("home/tree/value"), b"keep").unwrap();
    assert!(matches!(
        data.walk("/home", WalkOptions::new(1, 64).unwrap()),
        Err(FsError::WalkLimitExceeded)
    ));
    assert!(matches!(
        data.walk("/home", WalkOptions::new(100, 1).unwrap()),
        Err(FsError::WalkLimitExceeded)
    ));
    assert!(matches!(
        WalkOptions::new(0, 1),
        Err(FsError::InvalidWalkLimit)
    ));
    assert!(matches!(
        WalkOptions::new(1, 65),
        Err(FsError::InvalidWalkLimit)
    ));
    assert!(matches!(
        data.walk("C:/Windows", WalkOptions::default()),
        Err(FsError::InvalidPath)
    ));
    assert_eq!(fs::read(root.join("home/tree/value")).unwrap(), b"keep");
    assert_eq!(
        data.walk("/home/tree/value", WalkOptions::new(1, 0).unwrap())
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn walk_reports_native_links_but_never_traverses_the_external_tree() {
    let root = fixture();
    let outside = fixture();
    let data = DataRoot::open(&root).unwrap();
    fs::write(outside.join("home/sentinel"), b"outside").unwrap();
    #[cfg(windows)]
    assert!(std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(root.join("home/link"))
        .arg(outside.join("home"))
        .status()
        .unwrap()
        .success());
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.join("home"), root.join("home/link")).unwrap();
    let entries = data.walk("/home", WalkOptions::default()).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(
        matches!(&entries[1], WalkEntry::Accessible { path, stat, .. }
        if path == "/home/link" && stat.kind == ObjectKind::BlockedLink)
    );
    assert_eq!(fs::read(outside.join("home/sentinel")).unwrap(), b"outside");
}
#[cfg(unix)]
#[test]
fn host_only_names_are_preserved_literally_not_lossily_reinterpreted() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::vec;
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    let name = OsString::from_vec(vec![0xff, b'x']);
    fs::write(root.join("home").join(&name), b"host-only").unwrap();
    let entries = data.walk("/home", WalkOptions::default()).unwrap();
    assert!(
        matches!(&entries[1], WalkEntry::BlockedName { parent, name: literal, .. }
        if parent == "/home" && literal == &name)
    );
}
