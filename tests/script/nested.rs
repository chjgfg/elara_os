use crate::support::{execute, fixture, run, transcript_in};
use elara_os::script::{
    runtime::{Completion, MAX_STEPS},
    Context,
};
use elara_os::users;
use elara_os::virtual_path::VirtualPath;
use std::{assert, assert_eq, assert_ne, fs};

#[test]
fn nested_sh_isolates_context_while_source_retains_it() {
    let report = execute("X=parent; sh -c 'X=child; cd /tmp; echo \"$X\"'; echo \"$X\"; pwd; source -c 'X=sourced; cd /tmp; f() { echo function; }'; echo \"$X\"; pwd; f");
    assert_eq!(
        report.stdout,
        b"child\nparent\n/\nsourced\n/tmp\nfunction\n"
    );
    assert_eq!(report.status, 0);
}
#[test]
fn nested_parameters_restore_and_source_without_args_inherits() {
    let (root, user) = fixture();
    fs::write(
        root.root().join("tmp/params.sh"),
        b"printf '%s:%s:%s\\n' \"$0\" \"$1\" \"$#\"; return 6; echo forbidden\n",
    )
    .unwrap();
    let report = run(&root, &user, &mut Context::new(VirtualPath::root()), &["-c".into(), "source /tmp/params.sh; echo $?; source /tmp/params.sh inner; echo $?; sh -c 'echo \"$0:$1\"' child argument; echo \"$0:$1:$#\"".into(), "caller".into(), "outer".into()]).unwrap();
    assert_eq!(
        report.stdout,
        b"caller:outer:1\n6\ncaller:inner:1\n6\nchild:argument\ncaller:outer:1\n"
    );
}
#[test]
fn exit_and_return_obey_the_actual_call_boundary() {
    let report = execute("sh -c 'exit 7; echo forbidden'; echo $?; sh -c 'return 8'; echo $?; source -c 'return 9'; echo $?; source -c 'exit 10'; echo forbidden");
    assert_eq!(report.stdout, b"7\n2\n9\n");
    assert_eq!(report.status, 10);
    assert_eq!(report.completion, Completion::ExitRequested);
}
#[test]
fn nested_calls_consume_inherited_input_once_without_consuming_redirected_parent_input() {
    let report = execute("f() { sh -c 'cat'; cat; }; printf abc | f; g() { source -c 'sh -c cat'; cat; }; printf xyz | g");
    assert_eq!(report.stdout, b"abcxyz");
    assert_eq!(report.status, 0);
}
#[test]
fn source_in_a_pipeline_is_isolated_but_uses_real_files() {
    let (root, user) = fixture();
    let report = run(&root, &user, &mut Context::new(VirtualPath::root()), &["-c".into(), "X=parent; source -c 'X=child; cd /tmp; printf data > nested; cat nested' | wc -c; echo \"$X\"; pwd".into()]).unwrap();
    assert_eq!(report.stdout, b"4\nparent\n/\n");
    assert_eq!(fs::read(root.root().join("tmp/nested")).unwrap(), b"data");
}
#[test]
fn combined_function_and_script_recursion_is_bounded() {
    let (root, user) = fixture();
    fs::write(
        root.root().join("tmp/recurse.sh"),
        b"f() { sh /tmp/recurse.sh; }; f\n",
    )
    .unwrap();
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["/tmp/recurse.sh".into()],
    )
    .unwrap();
    assert_ne!(report.status, 0);
    assert!(String::from_utf8_lossy(&report.stderr).contains("call depth exceeds 32"));
    assert!(report.steps < MAX_STEPS);
}
#[test]
fn nested_calls_share_step_budget_instead_of_resetting_it() {
    let report = execute("for x in a b c d; do sh -c 'for y in a b c d e f g h; do for z in a b c d e f g h; do for q in a b c d e f g h; do true; done; done; done'; done; echo forbidden_after_budget");
    assert_eq!(report.steps, MAX_STEPS);
    assert_eq!(report.status, 2);
    assert!(String::from_utf8_lossy(&report.stderr).contains("step budget exhausted"));
    assert!(!String::from_utf8_lossy(&report.stdout).contains("forbidden_after_budget"));
}
#[test]
fn nested_check_encoding_permissions_and_runtime_failure_preserve_their_contracts() {
    let (root, user) = fixture();
    fs::write(
        root.root().join("tmp/check.sh"),
        b"touch /tmp/not-created\n",
    )
    .unwrap();
    fs::write(
        root.root().join("tmp/bad.sh"),
        b"touch /tmp/not-created; if true; then\n",
    )
    .unwrap();
    fs::write(root.root().join("tmp/nonutf8.sh"), [255]).unwrap();
    let report = run(&root, &user, &mut Context::new(VirtualPath::root()), &["-c".into(), "sh -n /tmp/check.sh; sh /tmp/bad.sh; echo $?; sh /tmp/nonutf8.sh; echo $?; source -c 'X=retained; cd /tmp; missing'; echo \"$X\"; pwd".into()]).unwrap();
    assert_eq!(report.stdout, b"2\n2\nretained\n/tmp\n");
    assert!(!root.root().join("tmp/not-created").exists());
    fs::write(
        root.root().join("root/private.sh"),
        b"echo private_script\n",
    )
    .unwrap();
    let guest = users::add_user(&root, &user, "nestedguest", "secret1").unwrap();
    let report = run(
        &root,
        &guest,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), "sh /root/private.sh".into()],
    )
    .unwrap();
    assert_eq!(report.status, 2);
    assert!(report.stdout.is_empty());
    assert!(String::from_utf8_lossy(&report.stderr).contains("Permission denied"));
}
#[test]
fn nested_scripts_share_fd_capabilities_and_never_log_source_text() {
    let (root, _) = fixture();
    let text = transcript_in(&root, "fd open /tmp/shared create\nprintf payload | sh -c 'source -c \"fd write 0:1\"'\nfd seek 0:1 0 start\nfd read 0:1 7\nsh -c 'sh -c \"echo nested_literal_private\"'\nfd closeall\nexit 0\n");
    assert!(text.contains("payload"), "{text}");
    assert_eq!(
        fs::read(root.root().join("tmp/shared")).unwrap(),
        b"payload"
    );
    let log = fs::read_to_string(root.root().join("var/log/elara.log")).unwrap();
    assert!(!log.contains("nested_literal_private"));
    assert!(!log.contains("payload"));
    assert!(log.contains("script: sh"));
}
