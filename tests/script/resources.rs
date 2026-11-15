use crate::support::{fixture, run};
use elara_os::{filesystem::ResourceState, script::Context, virtual_path::VirtualPath};
use std::{assert, assert_eq};
#[test]
fn resources_commands_register_quota_fd_clear_and_uninstall() {
    let (root, user) = fixture();
    let source="app register /opt/demo 4 8; printf data > /opt/demo/file; fd open /opt/demo/file rw; fd writeat 0:1 4 X; fd closeall; app stat /opt/demo; app quota /opt/demo 8 8; printf X >> /opt/demo/file; cat /opt/demo/file; cache register /var/cache/demo 16 8; printf cached > /var/cache/demo/item; cache clear /var/cache/demo; cache stat /var/cache/demo; app uninstall /opt/demo; cache unregister /var/cache/demo; app list; cache list";
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), source.into()],
    )
    .unwrap();
    let out = String::from_utf8_lossy(&report.stdout);
    let err = String::from_utf8_lossy(&report.stderr);
    assert_eq!(report.status, 0, "{report:?}");
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.contains("quota exceeded"), "{err}");
    assert!(out.contains("dataX"), "{out}");
    assert!(
        out.contains("ResourceUsage { bytes: 4, objects: 2 }"),
        "{out}"
    );
    assert!(
        out.contains("ResourceUsage { bytes: 0, objects: 1 }"),
        "{out}"
    );
    assert!(root.resources_as(&user).unwrap().is_empty());
    assert!(!root.root().join("opt/demo").exists());
    assert!(!root.root().join("var/cache/demo").exists());
}
#[test]
fn resources_commands_report_deferred_cleanup_as_failure() {
    let (root, user) = fixture();
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), "app register /opt/demo 32 8".into()],
    )
    .unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    std::fs::write(root.root().join("opt/demo/unknown"), b"keep").unwrap();
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), "app uninstall /opt/demo".into()],
    )
    .unwrap();
    assert_eq!(report.status, 1, "{report:?}");
    assert!(String::from_utf8_lossy(&report.stderr).contains("Cleanup deferred"));
    assert_eq!(
        root.resource_info_as(&user, "/opt/demo").unwrap().0.state,
        ResourceState::Removing
    );
    assert_eq!(
        std::fs::read(root.root().join("opt/demo/unknown")).unwrap(),
        b"keep"
    );
}
#[test]
fn resources_guest_cannot_register_or_reclaim_administrative_data() {
    let (root, admin) = fixture();
    let guest = elara_os::users::add_user(&root, &admin, "resourceguest", "test-password").unwrap();
    let report = run(
        &root,
        &guest,
        &mut Context::new(VirtualPath::root()),
        &[
            "-c".into(),
            "app register /opt/no 32 8; cache register /var/cache/no 32 8; app list".into(),
        ],
    )
    .unwrap();
    assert_eq!(report.status, 1, "{report:?}");
    assert_eq!(String::from_utf8_lossy(&report.stderr).lines().count(), 3);
    assert!(root.resources_as(&admin).unwrap().is_empty());
}

#[test]
fn resources_commands_associate_locations_and_uninstall_the_group() {
    let (root, user) = fixture();
    let source = "app register /opt/group 32 8; app cache /opt/group /var/cache/group 8 8; app data /opt/group /root/group 8 8; printf user > /root/group/state; printf cached > /var/cache/group/item; app locations /opt/group; app uninstall /opt/group; app list; cache list";
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), source.into()],
    )
    .unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    assert!(report.stderr.is_empty(), "{report:?}");
    let out = String::from_utf8_lossy(&report.stdout);
    assert!(out.contains("ApplicationData"), "{out}");
    assert!(
        out.contains("ResourceRecovery { resources: 3, files: 2, directories: 3, deferred: 0 }"),
        "{out}"
    );
    assert!(root.resources_as(&user).unwrap().is_empty());
    assert!(!root.root().join("root/group").exists());
}
