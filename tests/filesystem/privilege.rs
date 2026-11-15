use super::fixture;
use crate::filesystem::{
    DataRoot, FileAccess, FileBehavior, FileCreation, FileMode, OwnershipState, VirtualMode,
};
use crate::users::Account;
use std::{assert, assert_eq, format};
fn mode(bits: u16) -> VirtualMode {
    VirtualMode::new(bits).unwrap()
}
fn bits(root: &DataRoot, path: &str) -> u16 {
    let OwnershipState::Managed(info) = root.owned_file_info(path).unwrap() else {
        panic!("managed")
    };
    info.mode.bits()
}
#[test]
fn privilege_all_byte_mutations_clear_special_bits_including_preexisting_fds() {
    let root = DataRoot::open(&fixture()).unwrap();
    let admin = Account {
        identity: Default::default(),
        uid: 0,
        name: "root".into(),
        home: "/root".into(),
    };
    for op in 0..6 {
        let path = format!("/root/file{op}");
        root.create_owned_file(&admin, &path, b"old", mode(0o700))
            .unwrap();
        let fd = root
            .open_file_as(
                &admin,
                &path,
                FileMode::new(
                    FileAccess::ReadWrite,
                    FileCreation::Existing,
                    FileBehavior::Preserve,
                )
                .unwrap(),
            )
            .unwrap();
        root.set_access_as(&admin, &path, Some(mode(0o6700)), None, None)
            .unwrap();
        assert_eq!(bits(&root, &path), 0o6700);
        match op {
            0 => {
                fd.write(b"new").unwrap();
            }
            1 => {
                fd.write_at(0, b"new").unwrap();
            }
            2 => {
                fd.truncate(0).unwrap();
            }
            3 => {
                root.append_owned_file(&admin, &path, b"new").unwrap();
            }
            4 => {
                root.replace_owned_file(&admin, &path, b"new").unwrap();
            }
            _ => {
                root.open_file_as(
                    &admin,
                    &path,
                    FileMode::new(
                        FileAccess::WriteOnly,
                        FileCreation::Existing,
                        FileBehavior::Truncate,
                    )
                    .unwrap(),
                )
                .unwrap();
            }
        }
        assert_eq!(bits(&root, &path), 0o700, "operation {op}");
        let expected: &[u8] = match op {
            0 | 1 | 4 => b"new",
            3 => b"oldnew",
            _ => b"",
        };
        assert_eq!(
            root.read_file_as(&admin, &path, 64).unwrap(),
            expected,
            "operation {op}"
        );
    }
}
#[test]
fn privilege_catalog_v3_rejects_old_headers_and_detached_fd_cannot_clear_new_generation() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let admin = Account {
        identity: Default::default(),
        uid: 0,
        name: "root".into(),
        home: "/root".into(),
    };
    root.create_owned_file(&admin, "/root/file", b"old", mode(0o4700))
        .unwrap();
    let bytes = std::fs::read(path.join("var/.elara-objects/catalog")).unwrap();
    assert!(bytes.starts_with(b"ELARA_OWNERSHIP_CATALOG_V3\n"));
    assert_eq!(
        crate::filesystem::ownership_model::Catalog::decode(&bytes)
            .unwrap()
            .encode()
            .unwrap(),
        bytes
    );
    let fake = String::from_utf8(bytes)
        .unwrap()
        .replace("CATALOG_V3", "CATALOG_V2");
    assert!(crate::filesystem::ownership_model::Catalog::decode(fake.as_bytes()).is_err());
    let fd = root
        .open_file_as(
            &admin,
            "/root/file",
            FileMode::new(
                FileAccess::ReadWrite,
                FileCreation::Existing,
                FileBehavior::Preserve,
            )
            .unwrap(),
        )
        .unwrap();
    root.replace_owned_file(&admin, "/root/file", b"replacement")
        .unwrap();
    root.set_access_as(&admin, "/root/file", Some(mode(0o4700)), None, None)
        .unwrap();
    fd.write(b"old-write").unwrap();
    assert_eq!(bits(&root, "/root/file"), 0o4700);
    assert_eq!(
        root.read_file_as(&admin, "/root/file", 64).unwrap(),
        b"replacement"
    );
}

#[test]
fn privilege_rename_unlink_denied_and_empty_writes_follow_object_identity() {
    let root = DataRoot::open(&fixture()).unwrap();
    let admin = Account {
        identity: Default::default(),
        uid: 0,
        name: "root".into(),
        home: "/root".into(),
    };
    root.create_owned_file(&admin, "/root/file", b"old", mode(0o4700))
        .unwrap();
    let ro = root
        .open_file_as(&admin, "/root/file", FileMode::READ)
        .unwrap();
    assert!(ro.write(b"denied").is_err());
    assert_eq!(bits(&root, "/root/file"), 0o4700);
    let fd = root
        .open_file_as(
            &admin,
            "/root/file",
            FileMode::new(
                FileAccess::ReadWrite,
                FileCreation::Existing,
                FileBehavior::Preserve,
            )
            .unwrap(),
        )
        .unwrap();
    fd.write(b"").unwrap();
    fd.read_at(0, 3).unwrap();
    fd.sync_data().unwrap();
    assert_eq!(bits(&root, "/root/file"), 0o4700);
    root.rename_file_as(&admin, "/root/file", "/root/moved")
        .unwrap();
    fd.write_at(0, b"new").unwrap();
    assert_eq!(bits(&root, "/root/moved"), 0o700);
    assert_eq!(
        root.read_file_as(&admin, "/root/moved", 64).unwrap(),
        b"new"
    );
    root.remove_file_as(&admin, "/root/moved").unwrap();
    root.create_owned_file(&admin, "/root/moved", b"unrelated", mode(0o4700))
        .unwrap();
    fd.write_at(0, b"old-fd").unwrap();
    assert_eq!(bits(&root, "/root/moved"), 0o4700);
    assert_eq!(
        root.read_file_as(&admin, "/root/moved", 64).unwrap(),
        b"unrelated"
    );
}

#[test]
fn privilege_clear_failure_blocks_bytes_and_recovers_before_retry() {
    use crate::filesystem::access::AccessFault;
    for fault in [
        AccessFault::AfterNamespace,
        AccessFault::AfterFiles,
        AccessFault::AfterCatalog,
    ] {
        let path = fixture();
        let mut root = DataRoot::open(&path).unwrap();
        let admin = Account {
            identity: Default::default(),
            uid: 0,
            name: "root".into(),
            home: "/root".into(),
        };
        root.create_owned_file(&admin, "/root/file", b"original", mode(0o6700))
            .unwrap();
        root.access_fault = fault;
        assert!(root
            .append_owned_file(&admin, "/root/file", b"mutation")
            .is_err());
        // Host observation intentionally does not trigger protocol recovery.
        assert_eq!(std::fs::read(path.join("root/file")).unwrap(), b"original");
        root.access_fault = AccessFault::None;
        root.recover_transactions().unwrap();
        assert_eq!(bits(&root, "/root/file"), 0o700);
        root.append_owned_file(&admin, "/root/file", b"mutation")
            .unwrap();
        assert_eq!(
            root.read_file_as(&admin, "/root/file", 64).unwrap(),
            b"originalmutation"
        );
    }
}

#[test]
fn privilege_nonmember_cannot_install_inherited_group_execution_bit() {
    let root = DataRoot::open(&fixture()).unwrap();
    let admin = Account {
        identity: Default::default(),
        uid: 0,
        name: "root".into(),
        home: "/root".into(),
    };
    let guest = Account {
        identity: Default::default(),
        uid: 1000,
        name: "guest".into(),
        home: "/home/guest".into(),
    };
    root.group_add_as(&admin, 7000, "team").unwrap();
    root.create_directory_as(&admin, "/tmp/team", Default::default())
        .unwrap();
    root.set_access_as(&admin, "/tmp/team", Some(mode(0o2777)), Some(7000), None)
        .unwrap();
    root.create_owned_file(&guest, "/tmp/team/image", b"true", mode(0o6755))
        .unwrap();
    assert_eq!(bits(&root, "/tmp/team/image"), 0o4755);
    root.chmod_as(&guest, "/tmp/team/image", mode(0o6755))
        .unwrap();
    assert_eq!(bits(&root, "/tmp/team/image"), 0o4755);
    root.group_member_as(&admin, 7000, guest.uid, true).unwrap();
    root.chmod_as(&guest, "/tmp/team/image", mode(0o6755))
        .unwrap();
    assert_eq!(bits(&root, "/tmp/team/image"), 0o6755);
    root.set_access_as(&admin, "/tmp/team/image", None, Some(0), None)
        .unwrap();
    assert_eq!(bits(&root, "/tmp/team/image"), 0o755);
    assert!(root
        .set_access_as(&admin, "/tmp/team", Some(mode(0o4777)), None, None)
        .is_err());
    assert_eq!(
        root.directory_info_as(&admin, "/tmp/team")
            .unwrap()
            .unwrap()
            .mode
            .bits(),
        0o2777
    );
}
