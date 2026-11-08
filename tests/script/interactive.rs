use crate::support::{fixture, transcript, transcript_in};
use elara_os::script::interactive::{self, Line};
use std::{assert, assert_eq, fs, matches, vec};

#[test]
fn prompt_uses_persistent_script_variables_conditions_and_pipelines() {
    let text = transcript("X='hello world'\necho \"$X\"\nfalse\necho $?\ntrue && echo yes\nfalse || echo recovered\nprintf '%s\\n' a b | wc -l\nexit\n");
    assert!(
        text.contains("hello world\n1\nyes\nrecovered\n2\n"),
        "{text}"
    );
}
#[test]
fn prompt_direct_redirection_and_relative_paths_use_real_modules() {
    let (root, _) = fixture();
    let text = transcript_in(
        &root,
        "cd /tmp\nprintf hello > a\ncat < a | wc -c\ncp a b\nmv b c\nstat c\nexit\n",
    );
    assert!(text.contains("5\n"));
    assert!(text.contains("path: /tmp/c\n"));
    assert_eq!(fs::read(root.root().join("tmp/c")).unwrap(), b"hello");
}
#[test]
fn prompt_compound_control_flow_and_functions_work_without_sh_wrapper() {
    let text = transcript("f() { echo \"$1\"; }\nfor x in a b; do f \"$x\"; done\nif false; then echo __after_exit_should_not_run__; else echo done; fi\nexit\n");
    assert!(text.contains("a\nb\ndone\n"), "{text}");
    assert!(!text.contains("__after_exit_should_not_run__\n"));
}
#[test]
fn session_controls_share_the_parser_but_are_not_script_stdin_consumers() {
    assert!(
        matches!(interactive::prepare("su 'alice'").unwrap(), Line::Session { name, .. } if name == "su")
    );
    assert!(matches!(
        interactive::prepare("echo '$X'").unwrap(),
        Line::Program(_)
    ));
    assert!(interactive::prepare("touch /tmp/a; if true; then").is_err());
}
#[test]
fn exit_requests_stop_the_prompt_and_preserve_process_status() {
    let (root, _) = fixture();
    let mut input = std::io::Cursor::new(b"\nfalse\nexit\necho __after_exit_should_not_run__\n");
    let mut output = Vec::new();
    let status = elara_os::shell::run_status(&mut input, &mut output, false, &root).unwrap();
    assert_eq!(status, 1);
    assert!(!String::from_utf8(output)
        .unwrap()
        .contains("__after_exit_should_not_run__"));
    let mut input = std::io::Cursor::new(b"\nexit 7\necho __after_exit_should_not_run__\n");
    assert_eq!(
        elara_os::shell::run_status(&mut input, &mut Vec::new(), false, &root).unwrap(),
        7
    );
}
#[test]
fn pipeline_exit_is_isolated_and_session_statuses_are_not_stale() {
    let text = transcript("exit 9 | cat\necho alive\nfalse\nhelp\necho $?\ninfo extra\necho $?\nuseradd alice secret1\nsu alice\npasswd root wrong\necho $?\nexit 0\n");
    assert!(text.contains("alive\n"), "{text}");
    assert!(
        text.contains("0\n'info' takes no arguments.\n2\n"),
        "{text}"
    );
    assert!(text.contains("Only root can manage users.\n1\n"), "{text}");
}
#[test]
fn compound_session_controls_are_rejected_before_any_side_effect() {
    let (root, _) = fixture();
    let text = transcript_in(
        &root,
        "touch /tmp/forbidden; su root\nclear > /tmp/clear-output\necho alive\nexit 0\n",
    );
    assert!(
        text.contains("session controls require a standalone command"),
        "{text}"
    );
    assert!(!root.root().join("tmp/forbidden").exists());
    assert!(!root.root().join("tmp/clear-output").exists());
    assert!(text.contains("alive\nBye.\n"));
}
#[test]
fn prompt_line_limits_drain_the_entire_rejected_command_and_recover() {
    let mut bytes = b"touch /tmp/forbidden; ".to_vec();
    bytes.extend_from_slice(&vec![b'x'; elara_os::script::lexer::MAX_BYTES]);
    bytes.extend_from_slice(b"\necho alive\n");
    let mut input = std::io::Cursor::new(bytes);
    assert!(matches!(
        interactive::read_prompt(&mut input).unwrap(),
        interactive::PromptInput::Rejected(_)
    ));
    assert!(
        matches!(interactive::read_prompt(&mut input).unwrap(), interactive::PromptInput::Program(text) if text == "echo alive")
    );
    let mut input = std::io::Cursor::new(b"\xff\necho safe\r\n");
    assert!(matches!(
        interactive::read_prompt(&mut input).unwrap(),
        interactive::PromptInput::Rejected(_)
    ));
    assert!(
        matches!(interactive::read_prompt(&mut input).unwrap(), interactive::PromptInput::Program(text) if text == "echo safe")
    );
}
#[test]
fn oversized_and_invalid_utf8_prompt_input_never_executes_a_partial_line() {
    let (root, _) = fixture();
    let mut bytes = b"\ntouch /tmp/forbidden; ".to_vec();
    bytes.extend_from_slice(&vec![b'x'; elara_os::script::lexer::MAX_BYTES]);
    bytes.extend_from_slice(b"\n\xff\necho alive\nexit 0\n");
    let mut input = std::io::Cursor::new(bytes);
    let mut output = Vec::new();
    assert_eq!(
        elara_os::shell::run_status(&mut input, &mut output, false, &root).unwrap(),
        0
    );
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("prompt line exceeds 64 KiB"));
    assert!(text.contains("prompt input must be UTF-8"));
    assert!(text.contains("alive\nBye.\n"));
    assert!(!root.root().join("tmp/forbidden").exists());
}

#[test]
fn assignments_and_source_text_do_not_leak_to_logs() {
    let (root, _) = fixture();
    transcript_in(&root, "X=private_interactive_value\necho \"$X\"\nexit\n");
    assert!(!fs::read_to_string(root.root().join("var/log/elara.log"))
        .unwrap()
        .contains("private_interactive_value"));
}
