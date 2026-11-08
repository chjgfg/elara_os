use crate::support::{execute, fixture, run};
use elara_os::script::{arithmetic, Context};
use elara_os::virtual_path::VirtualPath;
use std::{assert, assert_eq, assert_ne, fs};

#[test]
fn arithmetic_precedence_unary_variables_comparisons_and_short_circuit_are_real() {
    let report = execute("n=4; echo $((n + 2 * 3)); echo $((-n + 5)); echo $((n >= 4 && 8 % 3 == 2)); echo $((0 && 1 / 0)); echo $((1 || 1 / 0)); n=$((n + 1)); echo \"$n\"");
    assert_eq!(report.stdout, b"10\n1\n1\n0\n1\n5\n");
    assert_eq!(report.status, 0);
    assert!(arithmetic::evaluate("9223372036854775807 + 1", &|_| Ok(0)).is_err());
    assert!(arithmetic::evaluate("1 / 0", &|_| Ok(0)).is_err());
    assert!(arithmetic::validate("1 / 0").is_ok());
    assert!(arithmetic::validate("1 +").is_err());
}
#[test]
fn command_substitution_is_real_nested_quoted_and_strips_only_trailing_newlines() {
    let report = execute("X=parent; Y=\"$(X=child; cd /tmp; printf 'a\\nb\\n\\n')\"; printf '[%s]\\n' \"$Y\"; echo \"$X\"; pwd; echo \"$(echo \"$(printf nested)\")\"; echo '$(literal)' '\\$((1+1))'");
    assert_eq!(
        report.stdout,
        b"[a\nb]\nparent\n/\nnested\n$(literal) \\$((1+1))\n"
    );
    assert_eq!(report.status, 0);
}
#[test]
fn substitutions_do_not_reparse_values_as_commands_and_assignment_keeps_status() {
    let report = execute("X=$(printf '; echo injected'); echo \"$X\"; X=$(false); echo $?; echo \"$(exit 8)\"; echo alive");
    assert_eq!(report.stdout, b"; echo injected\n1\n\nalive\n");
    assert_eq!(report.status, 0);
}
#[test]
fn substitutions_obey_stage_stdin_and_consume_it_only_once() {
    let report = execute(
        "printf abc | printf '[%s]' \"$(cat)\"; f() { echo \"$(cat)\"; cat; }; printf xyz | f",
    );
    assert_eq!(report.stdout, b"[abc]xyz\n");
    assert_eq!(report.status, 0);
}
#[test]
fn parameter_defaults_assignments_alternates_and_lengths_preserve_semantics() {
    let report = execute("E=; echo \"${X-fallback}:${E-fallback}:${E:-fallback}\"; echo \"${X:=a  b}\"; echo \"${X:+yes}:${Y:+no}:${#X}\"; echo \"${X}\"; echo \"${Y:-$(printf nested)}\"");
    assert_eq!(
        report.stdout,
        b"fallback::fallback\na  b\nyes::4\na  b\nnested\n"
    );
    assert_eq!(report.status, 0);
}
#[test]
fn unused_defaults_do_not_run_side_effects_and_required_parameters_fail() {
    let (root, user) = fixture();
    let report = run(&root, &user, &mut Context::new(VirtualPath::root()), &["-c".into(), "X=set; echo \"${X:-$(touch /tmp/forbidden)}\"; echo \"${Y:+$(touch /tmp/forbidden)}\"; echo \"${Y:?required parameter}\"".into()]).unwrap();
    assert_eq!(report.stdout, b"set\n\n");
    assert_ne!(report.status, 0);
    assert!(String::from_utf8_lossy(&report.stderr).contains("required parameter"));
    assert!(!root.root().join("tmp/forbidden").exists());
}
#[test]
fn parameter_trim_uses_quote_aware_shortest_and_longest_patterns() {
    let report = execute("X='alpha/foo/bar.txt'; echo \"${X#*/}\" \"${X##*/}\" \"${X%/*}\" \"${X%%/*}\"; P='*/'; echo \"${X#$P}\" \"${X#\"$P\"}\"; Y='a*b'; echo \"${Y#'a*'}\"");
    assert_eq!(
        report.stdout,
        b"foo/bar.txt bar.txt alpha/foo alpha\nfoo/bar.txt alpha/foo/bar.txt\nb\n"
    );
    assert_eq!(report.status, 0);
}
#[test]
fn literal_parameter_replacement_supports_first_all_and_deletion_without_mutating_variable() {
    let report = execute("X=abab; echo \"${X/ab/Z}\" \"${X//ab/Z}\" \"${X/ab}\" \"$X\"; P='a/b'; echo \"${P/'/'/_}\"; Y='a*b'; echo \"${Y/'*'/Z}\"");
    assert_eq!(report.stdout, b"Zab ZZ ab abab\na_b\naZb\n");
    assert_eq!(report.status, 0);
}
#[test]
fn replacement_rejects_unsupported_wildcards_and_overflow_before_redirect_side_effects() {
    let (root, user) = fixture();
    assert!(run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), "touch /tmp/forbidden; echo ${X/a*/Z}".into()]
    )
    .is_err());
    assert!(!root.root().join("tmp/forbidden").exists());
    let part = "r".repeat(3300);
    let words = "\"$P\"".repeat(10);
    let report = run(&root, &user, &mut Context::new(VirtualPath::root()), &["-c".into(), std::format!("P='{part}'; R=$(printf '%s' {words}); X=xx; echo \"${{X//x/$R}}\" > /tmp/forbidden")]).unwrap();
    assert!(String::from_utf8_lossy(&report.stderr).contains("64 KiB output limit exceeded"));
    assert!(!root.root().join("tmp/forbidden").exists());
}

#[test]
fn invalid_expansions_are_preflighted_before_any_file_side_effect() {
    let (root, user) = fixture();
    for suffix in [
        "echo $((1 +))",
        "echo $(if true; then echo x)",
        "echo ${X@unknown}",
        "echo ${1:=bad}",
    ] {
        assert!(
            run(
                &root,
                &user,
                &mut Context::new(VirtualPath::root()),
                &["-c".into(), std::format!("touch /tmp/forbidden; {suffix}")]
            )
            .is_err(),
            "{suffix}"
        );
    }
    assert!(!root.root().join("tmp/forbidden").exists());
}
#[test]
fn substitution_still_obeys_output_encoding_and_file_authorization() {
    let (root, user) = fixture();
    fs::write(root.root().join("tmp/binary"), [255]).unwrap();
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), "X=$(cat /tmp/binary); echo $?".into()],
    )
    .unwrap();
    assert_eq!(report.stdout, b"1\n");
    assert!(String::from_utf8_lossy(&report.stderr).contains("must be UTF-8"));
    let mut text = "$(printf x)".to_owned();
    for _ in 0..33 {
        text = std::format!("$(echo \"{text}\")");
    }
    assert!(elara_os::script::parser::parse(&std::format!("echo \"{text}\"")).is_err());
}
