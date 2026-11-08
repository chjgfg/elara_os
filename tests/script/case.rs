use crate::support::{execute, fixture, run, transcript_in};
use elara_os::script::{parser, Context};
use elara_os::virtual_path::VirtualPath;
use std::{assert, assert_eq};

#[test]
fn case_selects_first_matching_alternative_and_preserves_quote_metadata() {
    let report = execute("X=ab7; case $X in 'ab*') echo forbidden ;; ab[0-9]|foo) echo selected ;; *) echo forbidden ;; esac; case '*' in '*') echo literal ;; *) echo forbidden ;; esac; case unmatched in foo) echo forbidden ;; esac; echo $?");
    assert_eq!(report.stdout, b"selected\nliteral\n0\n");
    assert_eq!(report.status, 0);
}
#[test]
fn case_word_and_patterns_are_not_field_split_or_filesystem_globbed() {
    let report = execute("X='a b'; P='a*'; case \"$X\" in $P) echo matched ;; *) echo forbidden ;; esac; case 'a b' in \"$P\") echo forbidden ;; 'a b') echo quoted ;; esac");
    assert_eq!(report.stdout, b"matched\nquoted\n");
}
#[test]
fn nested_case_control_flow_and_command_substitution_use_the_same_grammar() {
    let report = execute("for x in a b; do case $x in a) case nested in nested) echo inner ;; esac ;; b) continue ;; esac; done; echo \"$(case x in x) printf ')value' ;; *) false ;; esac)\"; echo \"$(printf '%s' \"$(printf ')nested')\")\"");
    assert_eq!(report.stdout, b"inner\n)value\n)nested\n");
    assert_eq!(report.status, 0);
}
#[test]
fn invalid_case_is_preflighted_and_session_controls_in_arms_cannot_read_passwords() {
    let (root, user) = fixture();
    for body in [
        "case x in x echo bad ;; esac",
        "case x in x) echo x ;;",
        "echo x ;; echo y",
        "case x x) true ;; esac",
    ] {
        assert!(
            run(
                &root,
                &user,
                &mut Context::new(VirtualPath::root()),
                &["-c".into(), std::format!("touch /tmp/forbidden; {body}")]
            )
            .is_err(),
            "{body}"
        );
    }
    let text = transcript_in(
        &root,
        "touch /tmp/forbidden; case x in x) su root ;; esac\necho alive\nexit 0\n",
    );
    assert!(text.contains("session controls require a standalone command"));
    assert!(text.contains("alive\n"));
    assert!(!root.root().join("tmp/forbidden").exists());
}
#[test]
fn case_depth_and_comment_parentheses_in_substitution_are_bounded_and_correct() {
    let report = execute("echo \"$(echo value # ignored )\n)\"");
    assert_eq!(report.stdout, b"value\n");
    let source = std::format!(
        "{}true; {}",
        "case x in x) ".repeat(40),
        ";; esac; ".repeat(40)
    );
    assert!(parser::parse(&source).is_err());
}
