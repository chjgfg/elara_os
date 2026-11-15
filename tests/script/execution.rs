use crate::support::{fixture, run, transcript_in};
use elara_os::filesystem::{DataRoot, VirtualMode};
use elara_os::script::{runtime::{Completion, MAX_STEPS}, Context};
use elara_os::users::Account;
use elara_os::virtual_path::VirtualPath;
use std::{assert, assert_eq, fs};
fn image(root: &DataRoot, user: &Account, path: &str, text: &[u8], bits: u16) {
    root.create_owned_file(user, path, text, VirtualMode::new(bits).unwrap()).unwrap();
}
#[test]
fn execution_replaces_boundary_preserves_arguments_and_discards_old_trap() {
    let (root, user) = fixture();
    image(&root, &user, "/root/program", b"printf '%s:%s' \"$0\" \"$1\"; trap 'echo image-trap' EXIT; exit 7", 0o700);
    let report = run(&root, &user, &mut Context::new(VirtualPath::root()), &["-c".into(), "trap 'touch /root/old-trap' EXIT; exec /root/program argument; touch /root/after-exec".into()]).unwrap();
    assert_eq!(report.status, 7, "{report:?}");
    assert_eq!(report.completion, Completion::ExitRequested);
    assert_eq!(report.stdout, b"/root/program:argumentimage-trap\n");
    assert!(!root.root().join("root/old-trap").exists());
    assert!(!root.root().join("root/after-exec").exists());
}
#[test]
fn execution_denial_does_not_replace_caller_and_sh_remains_read_based() {
    let (root, user) = fixture();
    image(&root, &user, "/root/readable", b"touch /root/body", 0o600);
    let report = run(&root, &user, &mut Context::new(VirtualPath::root()), &["-c".into(), "trap 'touch /root/kept-trap' EXIT; exec /root/readable || echo denied; echo alive".into()]).unwrap();
    assert_eq!(report.stdout, b"denied\nalive\n");
    assert!(!root.root().join("root/body").exists());
    assert!(root.root().join("root/kept-trap").is_file());
    let report = run(&root, &user, &mut Context::new(VirtualPath::root()), &["/root/readable".into()]).unwrap();
    assert_eq!(report.status, 0); assert!(root.root().join("root/body").is_file());
}
#[test]
fn execution_preflights_encoding_syntax_and_unknown_files() {
    let (root, user) = fixture();
    image(&root, &user, "/root/bad", b"touch /root/forbidden; if true; then", 0o700);
    image(&root, &user, "/root/binary", &[255], 0o700);
    fs::write(root.root().join("root/legacy"), b"touch /root/forbidden").unwrap();
    for path in ["/root/bad", "/root/binary", "/root/legacy", "C:/Windows/program.exe"] {
        let report = run(&root, &user, &mut Context::new(VirtualPath::root()), &["-c".into(), std::format!("exec {path}")]).unwrap();
        assert_eq!(report.status, 2, "{report:?}");
        assert!(!root.root().join("root/forbidden").exists());
    }
}
#[test]
fn execution_nested_boundary_and_shared_budget_cannot_be_reset() {
    let (root, user) = fixture();
    image(&root, &user, "/root/end", b"echo child; exit 9", 0o700);
    let report = run(&root, &user, &mut Context::new(VirtualPath::root()), &["-c".into(), "sh -c 'exec /root/end; touch /root/child-tail'; echo parent:$?".into()]).unwrap();
    assert_eq!(report.stdout, b"child\nparent:9\n");
    assert!(!root.root().join("root/child-tail").exists());
    image(&root, &user, "/root/loop", b"while true; do :; done", 0o700);
    let report = run(&root, &user, &mut Context::new(VirtualPath::root()), &["-c".into(), "true; exec /root/loop; touch /root/budget-tail".into()]).unwrap();
    assert_eq!(report.steps, MAX_STEPS); assert_eq!(report.status, 2);
    assert!(!root.root().join("root/budget-tail").exists());
}
#[test]
fn execution_preserves_fd_capability_and_task_cleanup() {
    let (root, user) = fixture();
    image(&root, &user, "/root/fd-image", b"fd write 0:1 image; touch /tmp/image-task; touch /root/image-persistent", 0o700);
    let text = transcript_in(&root, "fd open /root/output create\nsh -c 'exec /root/fd-image; touch /root/forbidden' &\nwait %1\nfd readat 0:1 0 5\nexit 0\n");
    assert!(text.contains("[1] exited 0"), "{text}");
    assert_eq!(fs::read(root.root().join("root/output")).unwrap(), b"image");
    assert!(root.root().join("root/image-persistent").is_file());
    assert!(!root.root().join("tmp/image-task").exists());
    assert!(!root.root().join("root/forbidden").exists());
}

#[test]
fn execution_redirected_input_and_output_use_the_existing_command_boundary() {
    let (root, user) = fixture();
    image(&root, &user, "/root/copy-input", b"cat; printf suffix", 0o700);
    image(&root, &user, "/root/input", b"payload", 0o600);
    let report = run(&root, &user, &mut Context::new(VirtualPath::root()),
        &["-c".into(), "printf prefix; exec /root/copy-input < /root/input > /root/output".into()]).unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    assert_eq!(report.stdout, b"prefix");
    assert_eq!(fs::read(root.root().join("root/output")).unwrap(), b"payloadsuffix");
}

#[test]
fn execution_only_permission_runs_without_disclosing_source_or_changing_actor() {
    let (root, admin) = fixture();
    let guest = elara_os::users::add_user(&root, &admin, "execguest", "test-password").unwrap();
    image(&root, &admin, "/tmp/execute-only", b"whoami; printf '%s' \"$1\"", 0o111);
    let before = fs::metadata(root.root().join("tmp/execute-only")).unwrap().permissions();
    assert!(std::matches!(root.read_file_as(&guest, "/tmp/execute-only", 64), Err(elara_os::filesystem::FsError::OwnershipDenied)));
    let snapshot = root.load_executable_as(&guest, "/tmp/execute-only", 64).unwrap();
    assert!(!std::format!("{snapshot:?}").contains("whoami"));
    let report = run(&root, &guest, &mut Context::new(VirtualPath::root()),
        &["-c".into(), "exec /tmp/execute-only argument".into()]).unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    assert_eq!(report.stdout, b"execguest\nargument");
    assert_eq!(fs::metadata(root.root().join("tmp/execute-only")).unwrap().permissions(), before);
}

#[test]
fn execution_in_job_observes_shared_pause_cancel_and_no_later_effects() {
    use elara_os::script::{jobs::{JobTable, Spawn, Waited, PauseOutcome}, modules::{ModuleCommands, Request, Dispatch}, shared::{Resources, SharedModules}, runtime::Budget, parser};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    let (root, user) = fixture();
    let resources = Resources::new(user.uid, Arc::new(elara_os::logging::Logger::open(&root).unwrap()), 16).unwrap();
    let cwd = VirtualPath::root();
    let mut modules = SharedModules::new(&root, &user, &resources);
    let Dispatch::Handled(opened) = modules.execute(Request {
        name: "fd", args: &["open".into(), "/root/ready".into(), "create".into()],
        stdin: &[], cwd: &cwd, creation: Default::default(),
    }).unwrap() else { std::panic!("real descriptor open required"); };
    assert_eq!(opened.status, 0);
    let token = String::from_utf8(opened.stdout).unwrap();
    let source = std::format!("trap 'touch /root/image-trap' EXIT; fd write {} ready; sleep 30 || touch /root/image-or; touch /root/image-tail", token.trim());
    image(&root, &user, "/root/cancel-image", source.as_bytes(), 0o700);
    std::thread::scope(|scope| {
        let jobs = JobTable::new(scope, user.uid, 1);
        let id = jobs.spawn(&root, &user, Spawn {
            context: Context::new(VirtualPath::root()),
            program: parser::parse("exec /root/cancel-image; touch /root/caller-tail").unwrap(),
            label: "exec".into(), budget: Arc::new(Budget::new()),
        }, &modules).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let ready = root.root().join("root/ready");
        while fs::read(&ready).unwrap() != b"ready" && Instant::now() < deadline { std::thread::yield_now(); }
        let entered = fs::read(&ready).unwrap() == b"ready";
        if !entered { jobs.cancel(user.uid, id).unwrap(); }
        assert!(entered, "image must execute through the real inherited FD before cancellation");
        assert_eq!(jobs.pause(user.uid, id).unwrap(), PauseOutcome::Stopped);
        jobs.cancel(user.uid, id).unwrap();
        let Waited::Finished { report, cancelled } = jobs.wait(user.uid, id, None, None).unwrap() else { std::panic!("job panicked"); };
        assert!(cancelled); assert_eq!(report.status, 130, "{report:?}");
        for path in ["root/image-trap", "root/image-or", "root/image-tail", "root/caller-tail"] {
            assert!(!root.root().join(path).exists(), "{path}");
        }
        assert_eq!(fs::read(ready).unwrap(), b"ready");
        jobs.shutdown().unwrap();
    });
}
