use elara_os::script::parser::*;
use std::{assert, assert_eq, format};

#[test]
fn control_flow_and_functions() {
    assert_eq!(
        parse(
            "f() { echo \"$1\"; }; for x in a b; do if true; then f \"$x\"; else false; fi; done\n"
        )
        .unwrap()
        .len(),
        2
    );
    assert!(parse("while false; do echo x; done\r\n").is_ok());
    assert!(parse("echo if; echo 'then'\n").is_ok());
}
#[test]
fn invalid_script_is_rejected_as_a_whole() {
    for source in [
        "echo ok; if true; echo bad",
        "echo ${x",
        "echo ${x@unknown}",
        "echo x |& y",
        "cat |",
        "echo x &&",
        "echo x 2>f",
        "echo x >a >b",
        "(echo x)",
        "echo prefix\"$@\"",
    ] {
        assert!(parse(source).is_err(), "{source}");
    }
}
#[test]
fn nested_syntax_has_a_hard_limit() {
    let source = format!("{}true;{}", "if true; then ".repeat(40), "fi;".repeat(40));
    assert!(parse(&source).is_err());
}
#[test]
fn malformed_for_and_function_definitions_fail() {
    for source in [
        "for 1x in a; do true; done",
        "for x in a do true; done",
        "for x; do true",
        "f() echo x",
        "f() { true;",
    ] {
        assert!(parse(source).is_err(), "{source}");
    }
}
