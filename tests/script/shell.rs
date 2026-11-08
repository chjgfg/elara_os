use crate::support::{fixture, transcript, transcript_in};
use std::{assert, fs};

#[test]
fn switching_users_discards_private_script_variables_and_functions() {
    let text = transcript("useradd scoped secret\nsource -c 'X=private_script_value; rootfn() { echo private_script_value; }'\nsu scoped\nsh -c 'echo \"${X}\"; rootfn'\nexit\n");
    assert!(!text.contains("private_script_value"));
    assert!(text.contains("Unknown script command: rootfn"));
}
#[test]
fn scripts_use_the_same_task_user_and_log_modules_as_the_outer_shell() {
    let text =
        transcript("sleep 0 bg\nsh -c 'ps; wait 1; sleep 0 bg; users; log 2'\nps\nwait 2\nexit\n");
    assert!(text.contains("[1] exited 0"));
    assert!(text.contains("[2] started: sleep 0"));
    assert!(text.contains("[2] exited 0"));
    assert!(text.contains("uid 0"));
    assert!(text.contains("command: sh -c"));
}
#[test]
fn script_entry_runs_control_flow_and_keeps_the_shell_alive() {
    let text = transcript("sh -c 'for x in a b; do echo \"$x\"; done; false || echo recovered'\nsh -c 'exit 7; echo never'\necho still-alive\nexit\n");
    assert!(text.contains("a\nb\nrecovered\n"));
    assert!(text.contains("Script exited with status 7."));
    assert!(text.contains("still-alive\n"));
    assert!(!text.contains("never\n"));
}
#[test]
fn source_retains_context_but_sh_isolates_directory_changes() {
    let text = transcript("source -c 'X=shared; cd /tmp; f() { echo \"$X\"; }'\npwd\nsh -c 'f'\nsh -c 'cd /'\npwd\nexit\n");
    assert!(text.contains("/tmp\nshared\n/tmp\n"));
}
#[test]
fn inline_script_text_does_not_leak_into_logs() {
    let (root, _) = fixture();
    let text = transcript_in(&root, "sh -c 'echo script_literal_secret'\nexit\n");
    assert!(text.contains("script_literal_secret\n"));
    let log = fs::read_to_string(root.root().join("var/log/elara.log")).unwrap();
    assert!(!log.contains("script_literal_secret"));
    assert!(log.contains("script: sh"));
}
