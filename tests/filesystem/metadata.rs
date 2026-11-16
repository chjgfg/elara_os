use super::fixture;
use crate::filesystem::{DataRoot, DirectoryDurability, FsError, ObjectKind, Timestamp};
use std::{assert, assert_eq, assert_ne, fs, matches, panic};

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

#[test]
fn unified_inode_host_file_and_directory_have_metadata() {
    use crate::filesystem::VirtualMode;
    use crate::users::Account;
    let root = DataRoot::open(&fixture()).unwrap();
    let user = Account { identity: Default::default(), uid: 0, name: "root".into(), home: "/root".into() };
    root.create_owned_file(&user, "/root/data", b"host", VirtualMode::new(0o600).unwrap()).unwrap();
    root.create_directory_as(&user, "/root/dir", Default::default()).unwrap();
    let file = root.inode_as(&user, "/root/data", false).unwrap();
    let directory = root.inode_as(&user, "/root/dir", false).unwrap();
    assert_eq!(file.length, 4);
    assert_ne!(file.inode, directory.inode);
}

#[test]
fn unified_inode_domains_links_clocks_and_reopen_are_explicit() {
    use crate::filesystem::{InodeIdentity, InodeTimes, LinkCount, OwnershipState, VirtualMode, vfs};
    use crate::users::Account;
    let base = fixture();
    let root = DataRoot::open(&base).unwrap();
    let user = Account { identity: Default::default(), uid: 0, name: "root".into(), home: "/root".into() };
    root.create_owned_file(&user, "/root/data", b"host", VirtualMode::new(0o600).unwrap()).unwrap();
    root.create_directory_as(&user, "/root/dir", Default::default()).unwrap();
    let OwnershipState::Managed(file) = root.ownership_as(&user, "/root/data").unwrap() else { panic!("managed file expected") };
    let directory = root.directory_info_as(&user, "/root/dir").unwrap().unwrap();
    assert_eq!(file.id.value(), directory.id);
    assert_ne!(root.inode_as(&user, "/root/data", false).unwrap().inode, root.inode_as(&user, "/root/dir", false).unwrap().inode);
    root.hardlink_as(&user, "/root/data", "/root/alias").unwrap();
    let before = root.inode_as(&user, "/root/alias", false).unwrap();
    assert_eq!(before.links, LinkCount::Known(2));
    assert_eq!(before.inode, InodeIdentity::HostFile(file.id));
    assert!(matches!(before.times, InodeTimes::Host { modified: Timestamp::Available(_), .. }));
    root.symlink_as(&user, "alias", "/root/link").unwrap();
    assert_eq!(root.inode_as(&user, "/root/link", true).unwrap().inode, before.inode);
    assert_eq!(root.inode_as(&user, "/root/link", false).unwrap().kind, ObjectKind::Symlink);
    root.mount_memory_as(&user, "/mnt/meta", vfs::Limits { bytes: 4096, inodes: 16 }).unwrap();
    root.create_owned_file(&user, "/mnt/meta/data", b"memory", VirtualMode::new(0o600).unwrap()).unwrap();
    root.symlink_global_as(&user, "/root/link", "/mnt/meta/host").unwrap();
    assert_eq!(root.inode_as(&user, "/mnt/meta/host", true).unwrap().inode, before.inode);
    let memory = root.inode_as(&user, "/mnt/meta/data", false).unwrap();
    assert!(matches!(memory.inode, InodeIdentity::Virtual { .. }));
    assert!(matches!(memory.times, InodeTimes::Logical { .. }));
    root.remove_file_as(&user, "/root/data").unwrap();
    let reopened = DataRoot::open(&base).unwrap();
    let after = reopened.inode_as(&user, "/root/alias", false).unwrap();
    assert_eq!(after.inode, before.inode);
    assert_eq!(after.links, LinkCount::Known(1));
}

#[test]
fn unified_inode_unknown_and_substitution_never_gain_identity_or_ownership() {
    use crate::filesystem::{InodeAccess, InodeIdentity, LinkCount, VirtualMode};
    use crate::users::Account;
    let base = fixture();
    let root = DataRoot::open(&base).unwrap();
    let user = Account { identity: Default::default(), uid: 0, name: "root".into(), home: "/root".into() };
    fs::write(base.join("root/unknown"), b"unknown").unwrap();
    let unknown = root.inode_as(&user, "/root/unknown", false).unwrap();
    assert_eq!(unknown.inode, InodeIdentity::Unknown);
    assert_eq!(unknown.access, InodeAccess::Unknown);
    assert_eq!(unknown.links, LinkCount::Unknown);
    assert_eq!(root.inode_as(&user, "/root", false).unwrap().inode, InodeIdentity::ProtectedLayout);
    root.create_owned_file(&user, "/tmp/secret", b"secret", VirtualMode::new(0o600).unwrap()).unwrap();
    let guest = Account { identity: Default::default(), uid: 1000, name: "guest".into(), home: "/home/guest".into() };
    assert!(root.inode_as(&guest, "/tmp/secret", false).is_err());
    fs::rename(base.join("tmp/secret"), base.join("tmp/original")).unwrap();
    fs::write(base.join("tmp/secret"), b"secret").unwrap();
    assert!(root.inode_as(&user, "/tmp/secret", false).is_err());
    assert_eq!(fs::read(base.join("tmp/secret")).unwrap(), b"secret");
}
