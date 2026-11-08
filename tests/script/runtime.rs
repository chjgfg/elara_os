use crate::support::{execute, fixture, run};
use elara_os::script::{modules, run_with_modules, runtime, Context};
use elara_os::virtual_path::VirtualPath;
use std::{assert, assert_eq, assert_ne, format, fs};

#[test]
fn string_length_predicates_cover_both_empty_and_nonempty_values() {
    let report =
        execute("test -n ''; echo $?; test -z x; echo $?; test -n x; echo $?; test -z ''; echo $?");
    assert_eq!(report.stdout, b"1\n1\n0\n0\n");
}

#[test]
fn invalid_module_input_consumption_is_rejected() {
    struct Bad;
    impl modules::ModuleCommands for Bad {
        fn execute(&mut self, request: modules::Request<'_>) -> Result<modules::Dispatch, String> {
            Ok(modules::Dispatch::Handled(modules::CommandOutput {
                status: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
                input_use: modules::InputUse::Consumed(request.stdin.len() + 1),
            }))
        }
    }
    let (root, user) = fixture();
    let mut ctx = Context::new(VirtualPath::root());
    let report = run_with_modules(
        &root,
        &user,
        &mut ctx,
        &["-c".into(), "printf x | bad".into()],
        &mut Bad,
    )
    .unwrap();
    assert_ne!(report.status, 0);
    assert!(String::from_utf8(report.stderr)
        .unwrap()
        .contains("invalid stdin consumption"));
}
#[test]
fn entry_arguments_and_script_file_size_are_bounded() {
    let (root, user) = fixture();
    let mut ctx = Context::new(VirtualPath::root());
    let args: Vec<String> = (0..1025).map(|_| String::new()).collect();
    assert!(run(&root, &user, &mut ctx, &args).is_err());
    fs::write(root.root().join("tmp/large.sh"), "x".repeat(65537)).unwrap();
    assert!(run(&root, &user, &mut ctx, &["/tmp/large.sh".into()]).is_err());
}

#[test]
fn functions_do_not_register_ineffective_or_fake_system_overrides() {
    let report = execute("ps() { echo fake; }; ps");
    assert_eq!(report.status, 2);
    assert!(report.stdout.is_empty());
}

#[test]
fn assignment_values_and_temporary_prefixes_preserve_whitespace() {
    let report = execute("X=' a  b '; Y=$X; X=temp true; printf '%s\\n' \"$X\" \"$Y\" $X");
    assert_eq!(report.stdout, b" a  b \n a  b \na\nb\n");
}
#[test]
fn failed_redirect_prevents_command_effects() {
    let (root, user) = fixture();
    let mut ctx = Context::new(VirtualPath::root());
    let report = run(
        &root,
        &user,
        &mut ctx,
        &["-c".into(), "mkdir /tmp/no-effect > /missing/out".into()],
    )
    .unwrap();
    assert_ne!(report.status, 0);
    assert!(!root.root().join("tmp/no-effect").exists());
}
#[test]
fn pipeline_state_is_isolated_and_last_stage_controls_status() {
    let report = execute("cd /tmp | cat; pwd; false | true; echo $?");
    assert_eq!(report.stdout, b"/\n0\n");
}
#[test]
fn output_limit_preserves_already_completed_output() {
    let source = format!("X='{}'; echo \"$X\"; echo \"$X\"", "a".repeat(32768));
    let report = execute(&source);
    assert_eq!(report.status, 2);
    assert_eq!(report.stdout.len(), 32769);
}
#[test]
fn persistent_context_variable_and_function_counts_are_bounded() {
    let (root, user) = fixture();
    let mut ctx = Context::new(VirtualPath::root());
    let source: String = (0..300).map(|i| format!("V{i}=x;")).collect();
    assert_ne!(
        run(&root, &user, &mut ctx, &["-c".into(), source])
            .unwrap()
            .status,
        0
    );
    let source: String = (0..70).map(|i| format!("f{i}() {{ true; }};")).collect();
    assert_eq!(
        run(&root, &user, &mut ctx, &["-c".into(), source])
            .unwrap()
            .status,
        2
    );
}
#[test]
fn elif_continue_and_return_have_real_control_flow() {
    let report = execute("f() { return 7; echo never; }; f; echo $?; for x in a b; do if false; then echo never; elif test \"$x\" = a; then continue; else echo B; fi; done");
    assert_eq!(report.stdout, b"7\nB\n");
}
#[test]
fn invalid_late_expansion_is_preflighted_before_writes() {
    let (root, user) = fixture();
    let mut ctx = Context::new(VirtualPath::root());
    for source in [
        "touch /tmp/no-effect; echo ${x@unknown}",
        "touch /tmp/no-effect; echo prefix\"$@\"",
    ] {
        assert!(run(&root, &user, &mut ctx, &["-c".into(), source.into()]).is_err());
        assert!(!root.root().join("tmp/no-effect").exists());
    }
}
#[test]
fn another_module_receives_real_arguments_virtual_directory_and_input() {
    struct Probe;
    impl modules::ModuleCommands for Probe {
        fn execute(&mut self, request: modules::Request<'_>) -> Result<modules::Dispatch, String> {
            assert_eq!(request.name, "probe");
            assert_eq!(request.args, &[String::from("arg")]);
            assert_eq!(request.cwd.to_string(), "/tmp");
            assert_eq!(request.stdin, b"input");
            Ok(modules::Dispatch::Handled(modules::CommandOutput {
                status: 0,
                stdout: b"module-ok\n".to_vec(),
                stderr: Vec::new(),
                input_use: modules::InputUse::Consumed(request.stdin.len()),
            }))
        }
    }
    let (root, user) = fixture();
    let mut ctx = Context::new(VirtualPath::strict("/tmp").unwrap());
    let report = run_with_modules(
        &root,
        &user,
        &mut ctx,
        &["-c".into(), "printf input | probe arg".into()],
        &mut Probe,
    )
    .unwrap();
    assert_eq!(report.status, 0);
    assert_eq!(report.stdout, b"module-ok\n");
}
#[test]
fn functions_consume_pipeline_input_once_and_nested_calls_share_it() {
    let report = execute("inner() { cat; }; outer() { echo header; inner; cat; }; printf 'hello\\n' | outer; echo end");
    assert_eq!(report.stdout, b"header\nhello\nend\n");
    assert_eq!(
        execute("count_input() { wc -c; cat; }; printf hello | count_input").stdout,
        b"5\n"
    );
    assert_eq!(
        execute("IFS=x; X=axb; printf '[%s]\\n' $X").stdout,
        b"[a]\n[b]\n"
    );
}
#[test]
fn assignments_quotes_and_exit_status() {
    let report = execute("X='星澜 系统'; echo \"$X\"; echo '$X'; echo \\$X; false; echo $?; true && echo yes; false || echo recovered\n");
    assert_eq!(report.status, 0);
    assert_eq!(
        String::from_utf8(report.stdout).unwrap(),
        "星澜 系统\n$X\n$X\n1\nyes\nrecovered\n"
    );
}
#[test]
fn loops_conditions_and_functions() {
    let report = execute("greet() { printf '%s\\n' \"$1\"; return 7; }; for x in a b; do if test \"$x\" = a; then greet \"$x\"; else echo B; fi; done; while true; do echo once; break; done; until true; do echo never; done\n");
    assert_eq!(report.status, 0);
    assert_eq!(report.stdout, b"a\nB\nonce\n");
}
#[test]
fn positional_parameters_and_quoted_at() {
    let (root, user) = fixture();
    let mut ctx = Context::new(VirtualPath::root());
    let report = run(
        &root,
        &user,
        &mut ctx,
        &[
            "-c".into(),
            "printf '%s\\n' \"$0\" \"$#\" \"$@\"".into(),
            "name".into(),
            "a b".into(),
            "c".into(),
        ],
    )
    .unwrap();
    assert_eq!(report.stdout, b"name\n2\na b\nc\n");
}
#[test]
fn redirection_pipeline_and_crlf_script_file() {
    let (root, user) = fixture();
    let mut ctx = Context::new(VirtualPath::root());
    fs::write(root.root().join("tmp/demo.sh"), b"#!/bin/sh\r\nprintf '%s\\n' a b > /tmp/out.txt\r\necho c >> /tmp/out.txt\r\ncat < /tmp/out.txt | wc -l\r\n").unwrap();
    let report = run(&root, &user, &mut ctx, &["/tmp/demo.sh".into()]).unwrap();
    assert_eq!(report.status, 0);
    assert_eq!(report.stdout, b"3\n");
    assert_eq!(
        fs::read(root.root().join("tmp/out.txt")).unwrap(),
        b"a\nb\nc\n"
    );
    assert_eq!(
        report.mode,
        runtime::ExecutionMode::InProcessStreaming {
            queue_bytes: 32768,
            max_workers: 16,
        }
    );
    assert!(matches!(
        report.pipelines[0].strategy,
        elara_os::script::streaming::Strategy::Streaming { .. }
    ));
}
#[test]
fn syntax_error_and_check_mode_have_no_effects() {
    let (root, user) = fixture();
    let mut ctx = Context::new(VirtualPath::root());
    assert!(run(
        &root,
        &user,
        &mut ctx,
        &[
            "-c".into(),
            "touch /tmp/must-not-exist; if true; then echo oops".into()
        ]
    )
    .is_err());
    assert!(!root.root().join("tmp/must-not-exist").exists());
    fs::write(
        root.root().join("tmp/check.sh"),
        b"touch /tmp/must-not-exist\n",
    )
    .unwrap();
    let report = run(
        &root,
        &user,
        &mut ctx,
        &["-n".into(), "/tmp/check.sh".into()],
    )
    .unwrap();
    assert_eq!(report.steps, 0);
    assert!(!root.root().join("tmp/must-not-exist").exists());
}
#[test]
fn budgets_and_unknown_commands_fail_observably() {
    let report = execute("while true; do :; done");
    assert_eq!(report.status, 2);
    assert!(String::from_utf8(report.stderr).unwrap().contains("budget"));
    assert_eq!(execute("unknown_command").status, 127);
    assert_ne!(execute("f() { f; }; f").status, 0);
}
#[test]
fn host_paths_and_invalid_utf8_are_refused() {
    let (root, user) = fixture();
    let mut ctx = Context::new(VirtualPath::root());
    assert!(run(&root, &user, &mut ctx, &["C:/Windows/test.sh".into()]).is_err());
    fs::write(root.root().join("tmp/invalid.sh"), [255, 254]).unwrap();
    assert!(run(&root, &user, &mut ctx, &["/tmp/invalid.sh".into()]).is_err());
}
#[test]
fn guest_permissions_are_not_bypassed_by_scripts() {
    let (root, admin) = fixture();
    let guest = elara_os::users::add_user(&root, &admin, "scriptguest", "testpassword").unwrap();
    let mut ctx = Context::new(VirtualPath::root());
    let report = run(
        &root,
        &guest,
        &mut ctx,
        &[
            "-c".into(),
            "cat /etc/shadow; touch /root/unauthorized".into(),
        ],
    )
    .unwrap();
    assert_ne!(report.status, 0);
    assert!(!root.root().join("root/unauthorized").exists());
    assert!(report.stdout.is_empty());
}
#[test]
fn printf_formats_and_repeats_are_real_and_unsupported_formats_fail() {
    assert_eq!(execute("printf '%d%%\\t%s\\n' 7 ok").stdout, b"7%\tok\n");
    assert_eq!(execute("printf '%s\\n' a b").stdout, b"a\nb\n");
    assert_ne!(execute("printf '%q' x").status, 0);
}
#[test]
fn for_without_in_uses_positionals_and_until_runs_its_condition() {
    let (root, user) = fixture();
    let mut ctx = Context::new(VirtualPath::root());
    let report = run(&root, &user, &mut ctx, &["-c".into(), "for x; do echo \"$x\"; done; FLAG=go; until test \"$FLAG\" = stop; do FLAG=stop; echo once; done".into(), "name".into(), "a b".into(), "c".into()]).unwrap();
    assert_eq!(report.stdout, b"a b\nc\nonce\n");
}
#[test]
fn file_commands_and_word_counts_use_the_real_backend() {
    let report = execute("mkdir /tmp/d; touch /tmp/d/a; ls /tmp/d; rm /tmp/d/a; rmdir /tmp/d; printf '星澜 系统\\n' > /tmp/count; cat /tmp/count; wc -c /tmp/count; wc -w /tmp/count; wc -l /tmp/count");
    assert_eq!(report.status, 0);
    assert_eq!(
        String::from_utf8(report.stdout).unwrap(),
        "a\n星澜 系统\n14\n2\n1\n"
    );
}
#[test]
fn home_export_and_condition_tests_affect_execution() {
    let report = execute("export X=hello; HOME=/tmp; cd; pwd; if [ \"$X\" = hello ]; then echo yes; fi; test -z ''; echo $?; test -n x; echo $?; test ! x; echo $?");
    assert_eq!(report.stdout, b"/tmp\nyes\n0\n0\n1\n");
}
#[test]
fn script_exit_prevents_later_file_operations() {
    let (root, user) = fixture();
    let mut ctx = Context::new(VirtualPath::root());
    let report = run(
        &root,
        &user,
        &mut ctx,
        &["-c".into(), "exit 9; touch /tmp/never".into()],
    )
    .unwrap();
    assert_eq!(report.status, 9);
    assert!(!root.root().join("tmp/never").exists());
}
