use crate::support::{execute, fixture, run};
use elara_os::script::{pattern, Context};
use elara_os::users;
use elara_os::virtual_path::VirtualPath;
use std::{assert, assert_eq, fs};

#[test]
fn wildcard_matching_is_bounded_unicode_aware_and_quote_aware() {
    assert!(pattern::matches("a*[0-9]?", "abc7澜").unwrap());
    assert!(!pattern::matches("a[!b]", "ab").unwrap());
    assert!(pattern::matches("a[!b]", "ac").unwrap());
    assert!(pattern::matches("a\\*", "a*").unwrap());
    assert!(!pattern::has_wildcards(&pattern::escape("*?[]")).unwrap());
    assert!(pattern::matches("[z-a]", "q").is_err());
    assert!(pattern::matches(&"*".repeat(4097), "x").is_err());
}
#[test]
fn glob_uses_the_virtual_directory_and_preserves_quoted_literals_and_unmatched_patterns() {
    let (root, user) = fixture();
    fs::write(root.root().join("tmp/a.txt"), b"a").unwrap();
    fs::write(root.root().join("tmp/b.txt"), b"b").unwrap();
    fs::write(root.root().join("tmp/.hidden.txt"), b"h").unwrap();
    let report = run(&root, &user, &mut Context::new(VirtualPath::root()), &["-c".into(), "cd /tmp; printf '[%s]\\n' *.txt; echo '*.txt' \\*.txt missing-*.xyz; cat [ab].txt; echo .*.txt".into()]).unwrap();
    assert_eq!(
        report.stdout,
        b"[a.txt]\n[b.txt]\n*.txt *.txt missing-*.xyz\nab.hidden.txt\n"
    );
    assert_eq!(report.status, 0);
}
#[test]
fn mixed_quote_segments_and_expanded_globs_keep_their_provenance() {
    let (root, user) = fixture();
    fs::write(root.root().join("tmp/a.txt"), b"a").unwrap();
    fs::write(root.root().join("tmp/ab.txt"), b"b").unwrap();
    let report = run(&root, &user, &mut Context::new(VirtualPath::root()), &["-c".into(), "cd /tmp; P='a*.txt'; echo $P; echo \"$P\"; echo 'a'*.txt; echo 'a*'.txt; X=*.txt; echo \"$X\"".into()]).unwrap();
    assert_eq!(
        report.stdout,
        b"a.txt ab.txt\na*.txt\na.txt ab.txt\na*.txt\n*.txt\n"
    );
}
#[test]
fn directory_globs_and_literal_suffixes_only_return_real_matches() {
    let (root, user) = fixture();
    fs::create_dir(root.root().join("tmp/a")).unwrap();
    fs::create_dir(root.root().join("tmp/b")).unwrap();
    fs::write(root.root().join("tmp/a/child"), b"a").unwrap();
    fs::write(root.root().join("tmp/b/other"), b"b").unwrap();
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &[
            "-c".into(),
            "echo /tmp/*/child; echo /tmp/none/*; echo /tmp/*/".into(),
        ],
    )
    .unwrap();
    assert_eq!(
        report.stdout,
        b"/tmp/a/child\n/tmp/none/*\n/tmp/a/ /tmp/b/\n"
    );
}
#[test]
fn glob_does_not_enumerate_forbidden_directories_or_modify_data_on_expansion_failure() {
    let (root, user) = fixture();
    fs::write(root.root().join("root/private"), b"secret").unwrap();
    let guest = users::add_user(&root, &user, "globguest", "secret1").unwrap();
    let report = run(
        &root,
        &guest,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), "echo /root/*; touch /tmp/a /root/*".into()],
    )
    .unwrap();
    assert!(report.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&report.stderr).contains("Permission denied: cannot read /root")
    );
    assert!(!root.root().join("tmp/a").exists());
}
#[test]
fn glob_has_an_explicit_path_limit_before_command_side_effects() {
    let (root, user) = fixture();
    fs::create_dir(root.root().join("tmp/many")).unwrap();
    for index in 0..1025 {
        fs::write(root.root().join(std::format!("tmp/many/f{index:04}")), []).unwrap();
    }
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), "touch /tmp/forbidden /tmp/many/*".into()],
    )
    .unwrap();
    assert!(String::from_utf8_lossy(&report.stderr).contains("glob expansion exceeds 1024 paths"));
    assert!(!root.root().join("tmp/forbidden").exists());
}
#[test]
fn tilde_uses_home_without_field_splitting_and_cannot_change_identity() {
    let (root, user) = fixture();
    fs::create_dir(root.root().join("home/a b")).unwrap();
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &[
            "-c".into(),
            "HOME='/home/a b'; cd ~; pwd; echo '~/literal' \\~; USER=fake; whoami".into(),
        ],
    )
    .unwrap();
    assert_eq!(report.stdout, b"/home/a b\n~/literal ~\nroot\n");
    let report = execute("echo ~");
    assert_eq!(report.stdout, b"/root\n");
    assert!(elara_os::script::parser::parse("touch /tmp/forbidden; echo ~someone").is_err());
}
