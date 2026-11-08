use crate::support::{execute, fixture, run};
use elara_os::script::{runtime::Completion, Context};
use elara_os::virtual_path::VirtualPath;
use std::{assert, assert_eq, vec};

#[test]
fn function_locals_restore_on_return_exit_and_nested_calls() {
    let report = execute("X=global; f() { local X=outer Y=new; g; echo \"$X:$Y\"; return 4; }; g() { local X=inner; echo \"$X\"; }; f; echo $?; echo \"$X:${Y-unset}\"; sh -c 'f; echo \"$X\"'; echo \"$X\"");
    assert_eq!(
        report.stdout,
        b"inner\nouter:new\n4\nglobal:unset\ninner\nouter:new\nglobal\nglobal\n"
    );
    assert_eq!(report.status, 0);
}
#[test]
fn local_and_export_assignment_values_are_not_field_split() {
    let report = execute("X='a  b'; f() { local Y=$X; printf '[%s]\\n' \"$Y\"; }; f; export Z=$X; printf '[%s]\\n' \"$Z\"; unset Z; echo \"${Z-unset}\"");
    assert_eq!(report.stdout, b"[a  b]\n[a  b]\nunset\n");
    assert_eq!(report.status, 0);
    let report = execute("local X=value");
    assert!(String::from_utf8_lossy(&report.stderr).contains("local requires a function"));
}
#[test]
fn nounset_aborts_only_the_current_program_and_defaults_still_work() {
    let report = execute("set -u; echo \"${X:-safe}\"; echo \"$X\"; echo forbidden");
    assert_eq!(report.stdout, b"safe\n");
    assert_eq!(report.status, 2);
    assert_eq!(report.completion, Completion::Finished);
    assert!(String::from_utf8_lossy(&report.stderr).contains("unbound variable: X"));
    let report = execute("set -u; echo $((missing + 1)); echo forbidden");
    assert!(report.stdout.is_empty());
    assert_eq!(report.status, 2);
}
#[test]
fn errexit_does_not_trigger_in_conditions_or_unexecuted_and_or_tails() {
    let report = execute("set -e; false && echo forbidden; if false; then echo forbidden; else echo branch; fi; while false; do echo forbidden; done; false || echo recovered; f() { false; echo conditional-function; }; f || echo forbidden; echo alive; true && false; echo forbidden");
    assert_eq!(
        report.stdout,
        b"branch\nrecovered\nconditional-function\nalive\n"
    );
    assert_eq!(report.status, 1);
    assert_eq!(report.completion, Completion::ExitRequested);
}
#[test]
fn options_can_be_disabled_and_sh_does_not_change_parent_options() {
    let report = execute("sh -c 'set -eu; false; echo forbidden'; echo $?; false; echo alive; set -eu; set +eu; echo \"$missing\"; false; echo survived");
    assert_eq!(report.stdout, b"1\nalive\n\nsurvived\n");
    assert_eq!(report.status, 0);
    let report = execute("source -c 'set -e'; false; echo forbidden");
    assert!(report.stdout.is_empty());
    assert_eq!(report.status, 1);
}
#[test]
fn local_and_temporary_saved_bindings_cannot_escape_the_storage_quota_on_restore() {
    let (root, user) = fixture();
    let old = "x".repeat(40000);
    let part = "y".repeat(4000);
    let value = std::format!("$(printf '%s' {})", "\"$P\"".repeat(10));
    for body in [
        std::format!("f() {{ local X=; Y={value}; }}; f"),
        std::format!("f() {{ Y={value}; }}; X= f"),
    ] {
        let report = run(
            &root,
            &user,
            &mut Context::new(VirtualPath::root()),
            &[
                "-c".into(),
                std::format!("P='{part}'; X='{old}'; {body}; echo ${{#X}}:${{#Y}}"),
            ],
        )
        .unwrap();
        assert_eq!(String::from_utf8(report.stdout).unwrap(), "40000:0\n");
        assert!(String::from_utf8_lossy(&report.stderr).contains("variable storage exceeds 64 KiB"));
    }
}
#[test]
fn ifs_splits_only_unquoted_expansion_and_preserves_nonwhitespace_empty_fields() {
    let report = execute("IFS=', '; X=' ,a, ,b, '; printf '[%s]\\n' $X; printf '[%s]\\n' \"$X\"; IFS=; X='a b'; printf '[%s]\\n' $X; unset IFS; printf '[%s]\\n' $X");
    assert_eq!(
        report.stdout,
        b"[]\n[a]\n[]\n[b]\n[ ,a, ,b, ]\n[a b]\n[a]\n[b]\n"
    );
    assert_eq!(report.status, 0);
}
#[test]
fn ifs_applies_to_unquoted_positionals_and_function_local_scope_restores_it() {
    let (root, user) = fixture();
    let report = run(&root, &user, &mut Context::new(VirtualPath::root()), &["-c".into(), "IFS=,; printf '[%s]\\n' $@; printf '[%s]\\n' \"$@\"; f() { local IFS=:; X=a:b; printf '[%s]\\n' $X; }; f; X=a,b; printf '[%s]\\n' $X".into(), "caller".into(), "a,b".into(), "c,,d".into()]).unwrap();
    assert_eq!(
        report.stdout,
        b"[a]\n[b]\n[c]\n[]\n[d]\n[a,b]\n[c,,d]\n[a]\n[b]\n[a]\n[b]\n"
    );
}
#[test]
fn ifs_field_burst_hits_the_limit_before_commands_can_have_side_effects() {
    let (root, user) = fixture();
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &[
            "-c".into(),
            std::format!("IFS=,; X='{}'; touch /tmp/forbidden $X", ",".repeat(2000)),
        ],
    )
    .unwrap();
    assert!(String::from_utf8_lossy(&report.stderr)
        .contains("word expansion exceeds its argument budget"));
    assert!(!root.root().join("tmp/forbidden").exists());
}

#[test]
fn exit_traps_run_once_keep_status_and_use_real_modules() {
    let report = execute("trap 'echo cleanup:$?' EXIT; echo body; exit 7");
    assert_eq!(report.stdout, b"body\ncleanup:7\n");
    assert_eq!(report.status, 7);
    let report =
        execute("trap 'echo parent' EXIT; sh -c 'trap \"echo child\" EXIT; true'; echo done");
    assert_eq!(report.stdout, b"child\ndone\nparent\n");
    let report = execute("trap 'echo forbidden' EXIT; trap - EXIT; true");
    assert!(report.stdout.is_empty());
    let report = execute("trap 'exit 9' EXIT; true");
    assert_eq!(report.status, 9);
    assert!(
        String::from_utf8_lossy(&execute("trap 'echo fake' INT").stderr)
            .contains("only the EXIT trap")
    );
}
#[test]
fn prompt_keeps_exit_trap_until_explicit_exit_or_eof() {
    let (root, _) = fixture();
    let text = crate::support::transcript_in(
        &root,
        "trap 'echo cleanup:$?' EXIT\necho body\nexit 7\necho forbidden\n",
    );
    assert!(text.contains("body\ncleanup:7\nBye.\n"), "{text}");
    assert!(!text.contains("forbidden"));
    let text = crate::support::transcript_in(&root, "trap 'touch /tmp/cleaned' EXIT\n");
    assert!(text.ends_with("Bye.\n"));
    assert!(root.root().join("tmp/cleaned").exists());
}

#[test]
fn aggregate_expansions_are_bounded_before_commands_and_loop_bodies() {
    let (root, user) = fixture();
    let values = "a ".repeat(600);
    for body in [
        "touch /tmp/forbidden $X $X",
        "for item in $X $X; do touch /tmp/forbidden; break; done",
    ] {
        let report = run(
            &root,
            &user,
            &mut Context::new(VirtualPath::root()),
            &["-c".into(), std::format!("X='{values}'; {body}")],
        )
        .unwrap();
        assert!(String::from_utf8_lossy(&report.stderr)
            .contains("command expansion exceeds 1024 arguments"));
        assert!(!root.root().join("tmp/forbidden").exists());
    }
    let part = "x".repeat(3300);
    let expression = "\"$P\"".repeat(10);
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &[
            "-c".into(),
            std::format!("P='{part}'; X=$(printf '%s' {expression}); echo $X $X"),
        ],
    )
    .unwrap();
    assert!(report.stdout.is_empty());
    assert!(String::from_utf8_lossy(&report.stderr).contains("command arguments exceed 64 KiB"));
}
#[test]
fn direct_runtime_api_checks_arguments_before_cloning_or_executing() {
    struct NoModule;
    impl elara_os::script::modules::ModuleCommands for NoModule {
        fn execute(
            &mut self,
            _: elara_os::script::modules::Request<'_>,
        ) -> Result<elara_os::script::modules::Dispatch, String> {
            Ok(elara_os::script::modules::Dispatch::Unsupported)
        }
    }
    let (root, user) = fixture();
    let ast = elara_os::script::parser::parse("touch /tmp/forbidden").unwrap();
    for parameters in [vec![String::new(); 1025], vec!["x".repeat(65537)]] {
        let report = elara_os::script::runtime::execute(
            &ast,
            &root,
            &user,
            &mut Context::new(VirtualPath::root()),
            &parameters,
            &mut NoModule,
        );
        assert_eq!(report.status, 2);
        assert_eq!(report.steps, 0);
        assert!(!root.root().join("tmp/forbidden").exists());
    }
}

#[test]
fn declarations_and_options_cannot_be_shadowed_or_silently_ignored() {
    for name in ["local", "unset", "set"] {
        let report = execute(&std::format!("{name}() {{ echo fake; }}"));
        assert!(String::from_utf8_lossy(&report.stderr).contains("cannot shadow"));
    }
    let report = execute("set -ev; false; echo still-running");
    assert_eq!(report.stdout, b"still-running\n");
    assert!(String::from_utf8_lossy(&report.stderr).contains("only e/u"));
}
