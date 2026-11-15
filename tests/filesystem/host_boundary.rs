//! Host impact stays in a selected data root; no raw-device probes in tests.
use super::fixture;
use crate::filesystem::{
    host_boundary::validate_root_location, Acl, CreationOptions, DataRoot, FsError, Umask,
    VirtualMode,
};
use crate::users::Account;
use std::path::Path;
use std::{assert, assert_eq, format, fs, matches};

#[test]
fn local_root_gate_rejects_traversal_and_ambiguous_components_without_io() {
    let root = fixture();
    assert!(validate_root_location(&root).is_ok());
    for suffix in [
        "/./data",
        "/../data",
        "/NUL",
        "/name:stream",
        "/trailing.",
        "/trailing ",
        "/wild*",
        "/bad?",
    ] {
        let target = std::path::PathBuf::from(format!("{}{suffix}", root.display()));
        assert!(
            matches!(validate_root_location(&target), Err(FsError::InvalidRoot)),
            "{target:?}"
        );
    }
    let control =
        std::path::PathBuf::from(format!("{}/line{}feed", root.display(), char::from(10)));
    assert!(validate_root_location(&control).is_err());
    assert!(validate_root_location(Path::new("relative/data")).is_err());
}

#[test]
#[cfg(windows)]
fn devices_network_roots_volume_roots_and_system_trees_are_not_data_roots() {
    // Pure syntax policy only: never open a drive/device to test rejection.
    let separator = char::from(92).to_string();
    for value in [
        "C:/",
        "C:",
        "//./PhysicalDrive0",
        "//./C:",
        "//?/GLOBALROOT/Device/HarddiskVolume1/data",
        "//server/share/data",
        "//?/UNC/server/share/data",
        "C:/Windows/ElaraData",
        "D:/WINDOWS/data",
        "C:/Program Files/data",
        "C:/Program Files (x86)/data",
    ] {
        let value = value.replace('/', &separator);
        assert!(
            matches!(
                validate_root_location(Path::new(&value)),
                Err(FsError::InvalidRoot)
            ),
            "{value}"
        );
    }
}

#[test]
#[cfg(unix)]
fn unix_system_and_device_trees_are_not_data_roots() {
    for value in [
        "/",
        "/dev/sda",
        "/proc/elara",
        "/sys/elara",
        "/etc/elara",
        "/usr/elara",
    ] {
        assert!(validate_root_location(Path::new(value)).is_err(), "{value}");
    }
}

#[test]
fn virtual_mode_and_acl_do_not_change_host_file_permissions() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let admin = Account {
        identity: Default::default(),
        name: "root".into(),
        uid: 0,
        home: "/root".into(),
    };
    root.create_owned_file(
        &admin,
        "/root/file",
        b"host bytes",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    let before = fs::metadata(path.join("root/file")).unwrap().permissions();
    let mut acl = Acl::default();
    acl.users.insert(1000, 4);
    root.set_access_as(
        &admin,
        "/root/file",
        Some(VirtualMode::new(0).unwrap()),
        None,
        Some(acl),
    )
    .unwrap();
    assert_eq!(
        fs::metadata(path.join("root/file")).unwrap().permissions(),
        before
    );
    assert_eq!(fs::read(path.join("root/file")).unwrap(), b"host bytes");
    // File sticky remains unsupported; virtual bits never change host permissions.
    assert!(VirtualMode::new(0o10000).is_err());
    assert!(matches!(
        root.set_access_as(
            &admin,
            "/root/file",
            Some(VirtualMode::new(0o1600).unwrap()),
            None,
            None
        ),
        Err(FsError::DirectoryModeOnFile)
    ));
    assert_eq!(
        fs::metadata(path.join("root/file")).unwrap().permissions(),
        before
    );
    assert_eq!(fs::read(path.join("root/file")).unwrap(), b"host bytes");
    root.create_directory_as(&admin, "/root/team", CreationOptions::default())
        .unwrap();
    let directory_before = fs::metadata(path.join("root/team")).unwrap().permissions();
    root.set_access_as(
        &admin,
        "/root/team",
        Some(VirtualMode::new(0o2700).unwrap()),
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        root.directory_info_as(&admin, "/root/team")
            .unwrap()
            .unwrap()
            .mode
            .bits(),
        0o2700
    );
    assert_eq!(
        fs::metadata(path.join("root/team")).unwrap().permissions(),
        directory_before
    );
}

#[test]
fn recovery_only_unlinks_registered_task_names_not_sibling_or_unknown_files() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let sibling = crate::test_support::fresh_path("host-boundary-sibling");
    fs::create_dir(&sibling).unwrap();
    fs::write(sibling.join("sentinel"), b"outside").unwrap();
    fs::write(path.join("tmp/foreign"), b"unknown").unwrap();
    let admin = Account {
        identity: Default::default(),
        name: "root".into(),
        uid: 0,
        home: "/root".into(),
    };
    let scope = root.begin_task_as(&admin).unwrap();
    root.create_file_with_options(
        &admin,
        "/tmp/task",
        b"owned",
        CreationOptions {
            umask: Umask::default(),
            scope: Some(&scope),
        },
    )
    .unwrap();
    let report = scope.finish().unwrap();
    assert_eq!((report.files, report.deferred), (1, 0));
    assert!(!path.join("tmp/task").exists());
    assert_eq!(fs::read(path.join("tmp/foreign")).unwrap(), b"unknown");
    assert_eq!(fs::read(sibling.join("sentinel")).unwrap(), b"outside");
}
