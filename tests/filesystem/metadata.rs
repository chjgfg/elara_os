use super::fixture;
use crate::filesystem::{DataRoot, DirectoryDurability, FsError, ObjectKind, Timestamp};
use std::{assert, assert_eq, fs, matches};

#[test]
fn stat_reports_host_file_and_directory_metadata_without_inventing_uid() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    fs::write(root.join("home/value"), b"metadata").unwrap();
    let stat = data.stat("/home/value").unwrap();
    assert_eq!(stat.kind, ObjectKind::File);
    assert_eq!(stat.length, 8);
    let host = fs::metadata(root.join("home/value")).unwrap();
    match host.modified() {
        Ok(time) => assert_eq!(stat.modified, Timestamp::Available(time)),
        Err(error) => assert_eq!(stat.modified, Timestamp::Unavailable(error.kind())),
    }
    assert_eq!(stat.host_readonly, host.permissions().readonly());
    assert_eq!(data.stat("/home").unwrap().kind, ObjectKind::Directory);
    assert_eq!(data.stat("/").unwrap().kind, ObjectKind::Directory);
    assert!(matches!(data.stat("/home/missing"), Err(FsError::NotFound)));
    assert!(matches!(data.stat("C:/Windows"), Err(FsError::InvalidPath)));
}
#[test]
fn stat_classifies_a_final_blocked_link_without_following_it() {
    let root = fixture();
    let outside = fixture();
    let data = DataRoot::open(&root).unwrap();
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
    assert_eq!(
        data.stat("/home/link").unwrap().kind,
        ObjectKind::BlockedLink
    );
    assert!(matches!(
        data.stat("/home/link/child"),
        Err(FsError::Link(_))
    ));
}
#[test]
fn directory_sync_distinguishes_host_support_from_confirmed_durability() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    let host = data
        .dir
        .open_dir("home")
        .unwrap()
        .into_std_file()
        .sync_all();
    match host {
        Ok(()) => assert_eq!(
            data.sync_directory("/home").unwrap(),
            DirectoryDurability::Synced
        ),
        Err(error) => assert_eq!(
            data.sync_directory("/home").unwrap(),
            DirectoryDurability::Unconfirmed(error.kind())
        ),
    }
    fs::write(root.join("home/value"), b"value").unwrap();
    assert!(matches!(
        data.sync_directory("/home/value"),
        Err(FsError::NotDirectory)
    ));
}
