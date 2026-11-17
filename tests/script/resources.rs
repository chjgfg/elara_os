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

#[test]
fn application_same_uid_cannot_reuse_parent_module_resources() {
    use elara_os::{filesystem::{CreationOptions, ResourceKind, ResourceLimits}, logging::Logger,
        script::{modules::{ModuleCommands, Request}, shared::{Resources, SharedModules}}};
    use std::sync::Arc;
    let (root, user) = fixture();
    let quota = ResourceLimits { bytes: 8192, objects: 16 };
    root.register_resource_as(&user, "/opt/a", ResourceKind::Application, quota).unwrap();
    root.register_resource_as(&user, "/opt/b", ResourceKind::Application, quota).unwrap();
    let a = root.application_account_as(&user, "/opt/a").unwrap();
    let b = root.application_account_as(&user, "/opt/b").unwrap();
    assert_eq!(a.uid, b.uid);
    let resources = Resources::new(user.uid, Arc::new(Logger::open(&root).unwrap()), 16).unwrap();
    let cwd = VirtualPath::root();
    let request = || Request { creation: CreationOptions::default(), name: "fd", args: &[], stdin: &[], cwd: &cwd };
    let mut parent = SharedModules::new(&root, &user, &resources);
    assert!(parent.execute_as(&a, request()).is_err());
    let mut mislabeled = SharedModules::new(&root, &a, &resources);
    assert!(mislabeled.execute(request()).is_err());
    assert!(mislabeled.execute_as(&b, request()).is_err());
}

#[test]
fn application_same_uid_context_switch_drops_variables_functions_and_traps() {
    use elara_os::filesystem::{ResourceKind, ResourceLimits};
    let (root, user) = fixture();
    let quota = ResourceLimits { bytes: 8192, objects: 16 };
    root.register_resource_as(&user, "/opt/a", ResourceKind::Application, quota).unwrap();
    root.register_resource_as(&user, "/opt/b", ResourceKind::Application, quota).unwrap();
    let a = root.application_account_as(&user, "/opt/a").unwrap();
    let b = root.application_account_as(&user, "/opt/b").unwrap();
    let mut context = Context::new(VirtualPath::strict("/opt/a").unwrap());
    let first = run(&root, &a, &mut context, &["-c".into(), "PRIVATE=hidden; f() { echo inherited; }".into()]).unwrap();
    assert_eq!(first.status, 0, "{first:?}");
    // Switching to a different application domain resets the context: the old
    // variable and function must not survive into the second domain.
    context.prepare_identity(&b);
    let second = run(&root, &b, &mut context, &["-c".into(), "echo ${PRIVATE-unset}; f".into()]).unwrap();
    let out = String::from_utf8_lossy(&second.stdout);
    assert!(out.contains("unset"), "variable leaked across domains: {out}");
    assert!(second.status != 0 || !String::from_utf8_lossy(&second.stderr).is_empty(),
        "function leaked across domains: {second:?}");
}
