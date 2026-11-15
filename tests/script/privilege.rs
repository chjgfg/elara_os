use crate::support::{fixture, run};
use elara_os::filesystem::{VirtualMode, OwnershipState};
use elara_os::script::Context;
use elara_os::virtual_path::VirtualPath;
use elara_os::users;
use std::{assert, assert_eq, format};
#[test]
fn privilege_exec_setuid_changes_virtual_actor_and_restores_nested_caller() {
    let (root, admin) = fixture();
    let guest = users::add_user(&root, &admin, "privguest", "test-password").unwrap();
    root.create_owned_file(&admin, "/tmp/elevated", b"id; whoami; touch /root/elevated; touch /tmp/ephemeral; exit 7", VirtualMode::new(0o4755).unwrap()).unwrap();
    let script = "sh -c 'exec /tmp/elevated'; echo result:$?; whoami; touch /root/denied";
    let report = run(&root, &guest, &mut Context::new(VirtualPath::root()), &["-c".into(), script.into()]).unwrap();
    let out = String::from_utf8_lossy(&report.stdout);
    assert!(out.contains(&format!("ruid={} euid=0 suid=0", guest.uid)), "{report:?}");
    assert!(out.contains("root\nresult:7\nprivguest\n"), "{report:?}");
    assert!(root.root().join("root/elevated").is_file());
    assert!(!root.root().join("root/denied").exists());
    assert!(!root.root().join("tmp/ephemeral").exists());
    root.remove_file_as(&admin, "/root/elevated").unwrap();
    // Removing only setuid leaves the same x-authorized image unable to write /root.
    root.set_access_as(&admin, "/tmp/elevated", Some(VirtualMode::new(0o755).unwrap()), None, None).unwrap();
    let report = run(&root, &guest, &mut Context::new(VirtualPath::root()), &["-c".into(), "exec /tmp/elevated".into()]).unwrap();
    assert!(String::from_utf8_lossy(&report.stdout).contains(&format!("ruid={} euid={} suid={}", guest.uid, guest.uid, guest.uid)));
    assert!(String::from_utf8_lossy(&report.stderr).contains("Permission denied"));
    assert!(!root.root().join("root/elevated").exists());
}
#[test]
fn privilege_setgid_drives_acl_creation_and_keeps_real_user_and_groups() {
    let (root, admin) = fixture();
    let guest = users::add_user(&root, &admin, "groupguest", "test-password").unwrap();
    root.group_add_as(&admin, 7000, "executable").unwrap();
    root.group_add_as(&admin, 7100, "supplementary").unwrap();
    root.group_member_as(&admin, 7100, guest.uid, true).unwrap();
    root.create_owned_file(&admin, "/tmp/group-secret", b"group-data", VirtualMode::new(0o640).unwrap()).unwrap();
    root.set_access_as(&admin, "/tmp/group-secret", None, Some(7000), None).unwrap();
    root.create_owned_file(&admin, "/tmp/group-image", b"id; cat /tmp/group-secret; touch /home/groupguest/created", VirtualMode::new(0o755).unwrap()).unwrap();
    root.set_access_as(&admin, "/tmp/group-image", Some(VirtualMode::new(0o2755).unwrap()), Some(7000), None).unwrap();
    assert!(root.read_file_as(&guest, "/tmp/group-secret", 64).is_err());
    let report = run(&root, &guest, &mut Context::new(VirtualPath::root()), &["-c".into(), "exec /tmp/group-image".into()]).unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    let out = String::from_utf8_lossy(&report.stdout);
    assert!(out.contains("egid=7000 sgid=7000"), "{out}"); assert!(out.contains("group-data"));
    assert!(out.contains("groups={7000, 7100}"), "{out}");
    assert!(out.contains(&format!("ruid={} euid={} suid={} rgid={}", guest.uid, guest.uid, guest.uid, guest.uid)), "{out}");
    let OwnershipState::Managed(info) = root.owned_file_info("/home/groupguest/created").unwrap() else { panic!("missing object") };
    assert_eq!(info.owner.uid, guest.uid); assert_eq!(info.owner.gid, 7000);
    root.set_access_as(&admin, "/tmp/group-image", Some(VirtualMode::new(0o755).unwrap()), None, None).unwrap();
    let report = run(&root, &guest, &mut Context::new(VirtualPath::root()), &["-c".into(), "exec /tmp/group-image".into()]).unwrap();
    assert!(!String::from_utf8_lossy(&report.stdout).contains("group-data"));
}

#[test]
fn privilege_sh_source_and_nonexecutable_images_never_elevate() {
    let (root, admin) = fixture();
    let guest = users::add_user(&root, &admin, "readguest", "test-password").unwrap();
    root.create_owned_file(&admin, "/tmp/image", b"id; touch /root/forbidden", VirtualMode::new(0o4755).unwrap()).unwrap();
    for script in ["sh /tmp/image", "source /tmp/image"] {
        let report = run(&root, &guest, &mut Context::new(VirtualPath::root()), &["-c".into(), script.into()]).unwrap();
        assert!(String::from_utf8_lossy(&report.stdout).contains(&format!("ruid={} euid={} suid={}", guest.uid, guest.uid, guest.uid)), "{report:?}");
        assert!(!root.root().join("root/forbidden").exists());
    }
    root.chmod_as(&admin, "/tmp/image", VirtualMode::new(0o4644).unwrap()).unwrap();
    let report = run(&root, &guest, &mut Context::new(VirtualPath::root()), &["-c".into(), "exec /tmp/image; echo survived".into()]).unwrap();
    assert_eq!(report.stdout, b"survived\n");
    assert!(!root.root().join("root/forbidden").exists());
}

#[test]
fn privilege_jobs_pipes_modules_and_fds_share_session_not_effective_uid() {
    use elara_os::script::{jobs::{JobTable, Spawn, Waited}, modules::{Dispatch, ModuleCommands, Request}, parser, runtime::Budget, shared::{Resources, SharedModules}};
    use std::sync::Arc;
    let (root, admin) = fixture();
    let guest = users::add_user(&root, &admin, "jobguest", "test-password").unwrap();
    root.create_owned_file(&admin, "/root/secret", b"secret", VirtualMode::new(0o600).unwrap()).unwrap();
    let resources = Resources::new(guest.uid, Arc::new(elara_os::logging::Logger::open(&root).unwrap()), 16).unwrap();
    let mut modules = SharedModules::new(&root, &guest, &resources);
    let cwd = VirtualPath::root();
    let opened = match modules.execute(Request {
        name: "fd", args: &["open".into(), "/home/jobguest/fd".into(), "create".into()],
        stdin: &[], cwd: &cwd, creation: Default::default(),
    }).unwrap() {
        Dispatch::Handled(output) => output,
        Dispatch::Unsupported => panic!("fd module unavailable"),
    };
    let token = String::from_utf8(opened.stdout).unwrap();
    let body = format!("id; cp /root/secret /root/copied; fd write {} inherited; child() {{ whoami; printf child-function; touch /root/job; touch /tmp/job-temp; }}; child & wait %2; cat /root/secret | wc -c; trap 'touch /root/trap' EXIT", token.trim());
    root.create_owned_file(&admin, "/tmp/image", body.as_bytes(), VirtualMode::new(0o4755).unwrap()).unwrap();
    std::thread::scope(|scope| {
        let jobs = JobTable::new(scope, guest.session_uid(), 2);
        let id = jobs.spawn(&root, &guest, Spawn {
            context: Context::new(VirtualPath::root()),
            program: parser::parse("exec /tmp/image; touch /root/denied").unwrap(),
            label: "privilege".into(), budget: Arc::new(Budget::new()),
        }, &modules).unwrap();
        let Waited::Finished { report, cancelled } = jobs.wait(guest.session_uid(), id, None, None).unwrap() else { panic!("job panicked") };
        assert!(!cancelled, "{report:?}");
        assert_eq!(report.status, 0, "{report:?}");
        let out = String::from_utf8_lossy(&report.stdout);
        assert!(out.contains("euid=0 suid=0"), "{report:?}");
        assert!(out.contains("[2] exited 0"), "{report:?}");
        assert!(out.contains("child-function"), "{report:?}");
        assert!(out.contains("6\n"), "{report:?}");
        assert_eq!(std::fs::read(root.root().join("root/copied")).unwrap(), b"secret");
        assert_eq!(std::fs::read(root.root().join("home/jobguest/fd")).unwrap(), b"inherited");
        for path in ["root/job", "root/trap"] { assert!(root.root().join(path).exists(), "{report:?}"); }
        assert!(!root.root().join("root/denied").exists(), "{report:?}");
        assert!(!root.root().join("tmp/job-temp").exists(), "{report:?}");
        jobs.shutdown().unwrap();
    });
}

#[test]
fn privilege_exec_does_not_import_caller_functions_into_elevated_image() {
    let (root, admin) = fixture();
    let guest = users::add_user(&root, &admin, "funcguest", "test-password").unwrap();
    root.create_owned_file(&admin, "/tmp/function-image", b"helper; echo image", VirtualMode::new(0o4755).unwrap()).unwrap();
    let report = run(&root, &guest, &mut Context::new(VirtualPath::root()), &["-c".into(), "helper() { touch /root/injected; }; launch() { local value=caller; exec /tmp/function-image; }; launch".into()]).unwrap();
    assert_eq!(report.stdout, b"image\n");
    assert!(!root.root().join("root/injected").exists(), "caller function must not acquire image authority: {report:?}");
    assert!(String::from_utf8_lossy(&report.stderr).contains("Unknown script command: helper"));
}
