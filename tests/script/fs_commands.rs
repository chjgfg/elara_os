//! Command adapters use the same filesystem and authorization in shell/scripts.
use crate::support::{fixture, run, transcript_in};
use elara_os::filesystem::{FsError, WalkOptions};
use elara_os::script::Context;
use elara_os::users;
use elara_os::virtual_path::VirtualPath;
use std::{assert, assert_eq, assert_ne, fs, matches};

#[test]
fn shell_and_scripts_move_copy_and_inspect_relative_quoted_paths() {
    let (root, _) = fixture();
    let text = transcript_in(&root,
        "cd /tmp\nsh -c 'printf abc > \"source file\"'\ncp \"source file\" copy\nmv copy moved\nstat moved\nfind .\ndu moved\nsh -c 'cp moved scriptcopy; mv scriptcopy scriptmoved; cat scriptmoved'\nexit\n");
    assert!(
        text.contains("path: /tmp/moved\nkind: File\nlength: 3\n"),
        "{text}"
    );
    assert!(text.contains("/tmp/moved\n"));
    assert!(text.contains("logical_file_bytes: 3\nfiles: 1\ndirectories: 0\n"));
    assert!(text.contains("physical_usage: UnavailableOnHostBackend"));
    assert_eq!(
        fs::read(root.root().join("tmp/source file")).unwrap(),
        b"abc"
    );
    assert_eq!(fs::read(root.root().join("tmp/moved")).unwrap(), b"abc");
    assert_eq!(
        fs::read(root.root().join("tmp/scriptmoved")).unwrap(),
        b"abc"
    );
    assert!(!root.root().join("tmp/copy").exists());
    assert!(!root.root().join("tmp/scriptcopy").exists());
}

#[test]
fn command_copy_is_independent_and_existing_targets_are_not_overwritten() {
    let (root, user) = fixture();
    fs::write(root.root().join("tmp/source"), b"original").unwrap();
    fs::write(root.root().join("tmp/occupied"), b"keep").unwrap();
    let mut context = Context::new(VirtualPath::root());
    let report = run(&root, &user, &mut context, &["-c".into(),
        "cp /tmp/source /tmp/copy; printf changed > /tmp/source; cp /tmp/source /tmp/occupied || echo copy-refused; mv /tmp/source /tmp/occupied || echo move-refused".into()]).unwrap();
    assert_eq!(report.status, 0);
    assert_eq!(fs::read(root.root().join("tmp/copy")).unwrap(), b"original");
    assert_eq!(fs::read(root.root().join("tmp/occupied")).unwrap(), b"keep");
    assert!(String::from_utf8_lossy(&report.stdout).contains("copy-refused\nmove-refused\n"));
    assert!(root.root().join("tmp/source").exists());
}

#[test]
fn commands_keep_system_readonly_and_user_authorization_gates() {
    let (root, user) = fixture();
    fs::write(root.root().join("tmp/source"), b"source").unwrap();
    fs::write(root.root().join("root/secret"), b"secret").unwrap();
    users::add_user(&root, &user, "cmdguest", "password").unwrap();
    let text = transcript_in(&root,
        "cp /tmp/source /usr/new\nmv /tmp/source /usr/new\nsu cmdguest\ncp /root/secret /tmp/leak\nstat /root/secret\nfind /root\ndu /root\nexit\n");
    assert!(text.contains("System directory is read-only"));
    assert!(text.contains("Permission denied"));
    assert!(!root.root().join("usr/new").exists());
    assert!(!root.root().join("tmp/leak").exists());
    assert_eq!(
        fs::read(root.root().join("root/secret")).unwrap(),
        b"secret"
    );
}

#[test]
fn checked_walk_authorizes_before_stat_or_directory_enumeration() {
    let (root, _) = fixture();
    let missing = root.walk_checked("/tmp/not-created", WalkOptions::default(), |path| {
        assert_eq!(path, "/tmp/not-created");
        Err(FsError::AccessDenied("guard rejected before stat".into()))
    });
    assert!(matches!(missing, Err(FsError::AccessDenied(_))));
    // Removing the check reaches the actual lookup and fails for another reason.
    assert!(matches!(
        root.walk("/tmp/not-created", WalkOptions::default()),
        Err(FsError::NotFound)
    ));
    fs::create_dir(root.root().join("tmp/tree")).unwrap();
    fs::write(root.root().join("tmp/tree/blocked"), b"private").unwrap();
    let result = root.usage_checked("/tmp/tree", WalkOptions::default(), |path| {
        if path.ends_with("/blocked") {
            Err(FsError::AccessDenied("blocked child".into()))
        } else {
            Ok(())
        }
    });
    assert!(matches!(result, Err(FsError::AccessDenied(_))));
    assert_eq!(
        root.usage("/tmp/tree", WalkOptions::default())
            .unwrap()
            .logical_file_bytes,
        7
    );
}

#[test]
fn failed_recursive_diagnostics_do_not_publish_a_partial_tree_or_usage() {
    let (root, user) = fixture();
    users::add_user(&root, &user, "treeguest", "password").unwrap();
    fs::write(root.root().join("root/hidden"), b"never-list-this").unwrap();
    let guest = users::load(&root)
        .unwrap()
        .into_iter()
        .find(|u| u.name == "treeguest")
        .unwrap();
    let mut context = Context::new(VirtualPath::root());
    for command in ["find /", "du /"] {
        let report = run(&root, &guest, &mut context, &["-c".into(), command.into()]).unwrap();
        assert_eq!(report.status, 1);
        assert!(report.stdout.is_empty());
        assert!(String::from_utf8_lossy(&report.stderr).contains("Permission denied"));
    }
}

#[test]
fn diagnostic_commands_do_not_create_remove_or_replace_data() {
    let (root, user) = fixture();
    fs::write(root.root().join("tmp/keep"), b"keep").unwrap();
    let mut context = Context::new(VirtualPath::root());
    let report = run(
        &root,
        &user,
        &mut context,
        &[
            "-c".into(),
            "stat /tmp/keep; find /tmp/keep; du /tmp/keep".into(),
        ],
    )
    .unwrap();
    assert_eq!(report.status, 0);
    assert_eq!(fs::read(root.root().join("tmp/keep")).unwrap(), b"keep");
    assert_eq!(root.list("/tmp").unwrap().len(), 1);
}

#[test]
fn new_system_commands_cannot_be_shadowed_by_functions() {
    let (root, user) = fixture();
    let mut context = Context::new(VirtualPath::root());
    for name in ["mv", "cp", "stat", "find", "du"] {
        let text = std::format!("{name}() {{ echo fake; }}; {name}");
        let report = run(&root, &user, &mut context, &["-c".into(), text]).unwrap();
        assert_ne!(report.status, 0);
        assert!(String::from_utf8_lossy(&report.stderr).contains("cannot shadow"));
    }
}

#[test]
fn new_command_arity_errors_do_not_change_data() {
    let (root, user) = fixture();
    fs::write(root.root().join("tmp/source"), b"unchanged").unwrap();
    let mut context = Context::new(VirtualPath::root());
    for command in [
        "cp /tmp/source",
        "mv /tmp/source",
        "stat",
        "find /tmp /root",
        "du /tmp /root",
    ] {
        let report = run(&root, &user, &mut context, &["-c".into(), command.into()]).unwrap();
        assert_eq!(report.status, 1);
        assert!(String::from_utf8_lossy(&report.stderr).contains("Usage:"));
    }
    assert_eq!(
        fs::read(root.root().join("tmp/source")).unwrap(),
        b"unchanged"
    );
}
