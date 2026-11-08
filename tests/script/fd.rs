use crate::support::{fixture, transcript, transcript_in};
use elara_os::filesystem::DataRoot;
use elara_os::filesystem::FsError;
use elara_os::script::fd::{write_result, FdSession};
use elara_os::script::modules::InputUse;
use elara_os::users::Account;
use elara_os::virtual_path::VirtualPath;
use std::{assert, assert_eq, assert_ne, fs, io, matches, vec};

#[test]
fn documented_command_sequence_keeps_handles_live_until_last_use() {
    let text = transcript("fd open /tmp/example create\nfd write 0:1 \"hello world\"\nfd seek 0:1 0 start\nfd read 0:1 11\nfd stat 0:1\nfd dup 0:1\nsh -c 'fd seek 0:1 0 start; fd read 0:1 11 | wc -c'\nfd closeall\nexit\n");
    assert!(!text.contains("Error:"));
    assert!(text.contains("1:1\n0\n11\n2\nBye."));
}

#[test]
fn owner_reset_closes_capabilities_instead_of_only_changing_the_uid_label() {
    let (root, admin) = fixture();
    let guest = elara_os::users::add_user(&root, &admin, "fdguard", "password").unwrap();
    fs::write(root.root().join("root/private"), b"private").unwrap();
    let mut session = FdSession::new(admin.uid).unwrap();
    let old = token(call(
        &mut session,
        &root,
        &admin,
        &["open", "/root/private", "read"],
    ));
    let _ = session.switch_owner(guest.uid);
    fails(&mut session, &root, &guest, &["read", &old, "7"]);
}

fn call(
    session: &mut FdSession,
    root: &DataRoot,
    user: &Account,
    args: &[&str],
) -> elara_os::script::modules::CommandOutput {
    let args: Vec<String> = args.iter().map(|s| (*s).into()).collect();
    session
        .execute(root, user, &VirtualPath::root(), &args, b"")
        .unwrap()
}
fn fails(session: &mut FdSession, root: &DataRoot, user: &Account, args: &[&str]) {
    let args: Vec<String> = args.iter().map(|s| (*s).into()).collect();
    assert!(
        session
            .execute(root, user, &VirtualPath::root(), &args, b"")
            .is_err(),
        "{args:?}"
    );
}
fn token(output: elara_os::script::modules::CommandOutput) -> String {
    String::from_utf8(output.stdout).unwrap().trim().into()
}

#[test]
fn open_stream_io_seek_stat_truncate_and_sync_delegate_to_real_handles() {
    let (root, user) = fixture();
    let mut session = FdSession::new(user.uid).unwrap();
    let h = token(call(
        &mut session,
        &root,
        &user,
        &["open", "/tmp/a", "create"],
    ));
    assert_eq!(
        call(&mut session, &root, &user, &["write", &h, "abcdef"]).stdout,
        b"6\n"
    );
    assert_eq!(
        call(&mut session, &root, &user, &["seek", &h, "0", "start"]).stdout,
        b"0\n"
    );
    assert_eq!(
        call(&mut session, &root, &user, &["read", &h, "3"]).stdout,
        b"abc"
    );
    assert_eq!(
        call(&mut session, &root, &user, &["seek", &h, "-1", "current"]).stdout,
        b"2\n"
    );
    assert_eq!(
        call(&mut session, &root, &user, &["read", &h, "2"]).stdout,
        b"cd"
    );
    call(&mut session, &root, &user, &["truncate", &h, "4"]);
    call(&mut session, &root, &user, &["sync", &h, "data"]);
    call(&mut session, &root, &user, &["sync", &h, "all"]);
    let info = String::from_utf8(call(&mut session, &root, &user, &["stat", &h]).stdout).unwrap();
    assert!(info.contains("position: 4\n"));
    assert!(info.contains("length: 4\n"));
    assert_eq!(fs::read(root.root().join("tmp/a")).unwrap(), b"abcd");
    call(&mut session, &root, &user, &["close", &h]);
    assert!(session.is_empty());
}
#[test]
fn duplicate_shares_position_and_generation_reuse_rejects_stale_tokens() {
    let (root, user) = fixture();
    fs::write(root.root().join("tmp/a"), b"abcdef").unwrap();
    let mut session = FdSession::new(user.uid).unwrap();
    let first = token(call(&mut session, &root, &user, &["open", "/tmp/a", "rw"]));
    let duplicate = token(call(&mut session, &root, &user, &["dup", &first]));
    assert_eq!(
        call(&mut session, &root, &user, &["read", &first, "2"]).stdout,
        b"ab"
    );
    assert_eq!(
        call(&mut session, &root, &user, &["read", &duplicate, "2"]).stdout,
        b"cd"
    );
    call(&mut session, &root, &user, &["close", &first]);
    let replacement = token(call(
        &mut session,
        &root,
        &user,
        &["open", "/tmp/a", "read"],
    ));
    assert_eq!(first.split(':').next(), replacement.split(':').next());
    assert_ne!(first, replacement);
    fails(&mut session, &root, &user, &["read", &first, "1"]);
    assert_eq!(
        call(&mut session, &root, &user, &["closeall"]).stdout,
        b"2\n"
    );
    fails(&mut session, &root, &user, &["read", &duplicate, "1"]);
}
#[test]
fn positioned_io_preserves_position_and_append_rejects_writeat() {
    let (root, user) = fixture();
    fs::write(root.root().join("tmp/a"), b"abcdef").unwrap();
    let mut session = FdSession::new(user.uid).unwrap();
    let h = token(call(&mut session, &root, &user, &["open", "/tmp/a", "rw"]));
    assert_eq!(
        call(&mut session, &root, &user, &["readat", &h, "2", "2"]).stdout,
        b"cd"
    );
    assert_eq!(
        call(&mut session, &root, &user, &["writeat", &h, "1", "X"]).stdout,
        b"1\n"
    );
    let info = String::from_utf8(call(&mut session, &root, &user, &["stat", &h]).stdout).unwrap();
    assert!(info.contains("position: 0\n"));
    let append = token(call(
        &mut session,
        &root,
        &user,
        &["open", "/tmp/a", "append"],
    ));
    fails(
        &mut session,
        &root,
        &user,
        &["writeat", &append, "0", "bad"],
    );
    call(&mut session, &root, &user, &["write", &append, "Z"]);
    assert_eq!(fs::read(root.root().join("tmp/a")).unwrap(), b"aXcdefZ");
}
#[test]
fn readonly_writeonly_and_open_modes_have_no_silent_permission_fallback() {
    let (root, user) = fixture();
    fs::write(root.root().join("tmp/a"), b"old").unwrap();
    let mut session = FdSession::new(user.uid).unwrap();
    let read = token(call(
        &mut session,
        &root,
        &user,
        &["open", "/tmp/a", "read"],
    ));
    for args in [
        vec!["write", read.as_str(), "bad"],
        vec!["truncate", read.as_str(), "0"],
        vec!["sync", read.as_str(), "all"],
    ] {
        fails(&mut session, &root, &user, &args);
    }
    let write = token(call(
        &mut session,
        &root,
        &user,
        &["open", "/tmp/a", "write"],
    ));
    fails(&mut session, &root, &user, &["read", &write, "1"]);
    fails(&mut session, &root, &user, &["open", "/tmp/a", "create"]);
    assert_eq!(fs::read(root.root().join("tmp/a")).unwrap(), b"old");
    let truncated = token(call(
        &mut session,
        &root,
        &user,
        &["open", "/tmp/a", "truncate"],
    ));
    assert_eq!(fs::read(root.root().join("tmp/a")).unwrap(), b"");
    call(&mut session, &root, &user, &["write", &truncated, "new"]);
}
#[test]
fn full_table_does_not_truncate_target_and_read_limits_do_not_consume_data() {
    let (root, user) = fixture();
    fs::write(root.root().join("tmp/a"), b"old").unwrap();
    let mut session = FdSession::with_limit(user.uid, 1).unwrap();
    let h = token(call(&mut session, &root, &user, &["open", "/tmp/a", "rw"]));
    fails(&mut session, &root, &user, &["open", "/tmp/a", "truncate"]);
    fails(&mut session, &root, &user, &["read", &h, "65537"]);
    fails(&mut session, &root, &user, &["readat", &h, "0", "65537"]);
    assert_eq!(
        call(&mut session, &root, &user, &["read", &h, "3"]).stdout,
        b"old"
    );
}
#[test]
fn malformed_tokens_offsets_and_requests_are_rejected() {
    let (root, user) = fixture();
    let mut session = FdSession::new(user.uid).unwrap();
    for handle in [
        "0",
        "+0:1",
        "0:1:2",
        "0:18446744073709551616",
        "0:0",
        "999999:1",
        "０:1",
    ] {
        fails(&mut session, &root, &user, &["read", handle, "1"]);
    }
    let h = token(call(
        &mut session,
        &root,
        &user,
        &["open", "/tmp/a", "create"],
    ));
    for args in [
        vec!["seek", h.as_str(), "-1", "start"],
        vec!["seek", h.as_str(), "-1", "current"],
        vec!["read", h.as_str(), "-1"],
        vec!["sync", h.as_str(), "other"],
    ] {
        fails(&mut session, &root, &user, &args);
    }
    fails(&mut session, &root, &user, &["open", "/tmp/a", "other"]);
}
#[test]
fn open_authorization_and_owner_transition_prevent_root_capability_inheritance() {
    let (root, admin) = fixture();
    let guest = elara_os::users::add_user(&root, &admin, "fdguest", "password").unwrap();
    let mut session = FdSession::new(admin.uid).unwrap();
    fs::write(root.root().join("root/private"), b"private").unwrap();
    let old = token(call(
        &mut session,
        &root,
        &admin,
        &["open", "/root/private", "read"],
    ));
    fails(&mut session, &root, &guest, &["read", &old, "1"]);
    assert_eq!(session.switch_owner(guest.uid), 1);
    assert_eq!(session.owner_uid(), guest.uid);
    fails(
        &mut session,
        &root,
        &guest,
        &["open", "/root/private", "read"],
    );
    fails(&mut session, &root, &guest, &["open", "/etc/passwd", "rw"]);
    let fresh = token(call(
        &mut session,
        &root,
        &guest,
        &["open", "/tmp/guest", "create"],
    ));
    assert_eq!(old.split(':').next(), fresh.split(':').next());
    assert_ne!(old, fresh);
    fails(&mut session, &root, &guest, &["read", &old, "1"]);
}
#[test]
fn partial_write_reporting_preserves_progress_and_consumption_without_retry() {
    let result = write_result(
        Err(FsError::WritePositionUnconfirmed {
            bytes: 3,
            error: io::Error::other("injected position error"),
        }),
        true,
    )
    .unwrap();
    assert_eq!(result.status, 1);
    assert_eq!(result.stdout, b"3\n");
    assert!(matches!(result.input_use, InputUse::Consumed(3)));
    assert!(String::from_utf8(result.stderr)
        .unwrap()
        .contains("no retry"));
    let short = write_result(Ok(2), true).unwrap();
    assert_eq!(short.stdout, b"2\n");
    assert!(matches!(short.input_use, InputUse::Consumed(2)));
}
#[test]
fn shell_scripts_share_the_table_and_fd_write_does_not_leak_into_logs() {
    let text = transcript("fd open /tmp/shared create\nsh -c 'printf hello | fd write 0:1'\nfd seek 0:1 0 start\nsource -c 'fd read 0:1 5'\nfd closeall\nexit\n");
    assert!(text.contains("0:1\n5\n0\nhello1\n"));
}
#[test]
fn fd_write_text_is_not_copied_into_system_logs() {
    let (root, _) = fixture();
    let text = transcript_in(
        &root,
        "fd open /tmp/secret create\nfd write 0:1 fd_literal_secret\nexit\n",
    );
    assert!(!text.contains("Unknown command: fd"));
    let log = fs::read_to_string(root.root().join("var/log/elara.log")).unwrap();
    assert!(!log.contains("fd_literal_secret"));
    assert!(log.contains("command: fd write"));
}
#[test]
fn oversized_stream_writes_are_rejected_before_any_bytes_are_written() {
    let (root, user) = fixture();
    let mut session = FdSession::new(user.uid).unwrap();
    let handle = token(call(
        &mut session,
        &root,
        &user,
        &["open", "/tmp/limit", "create"],
    ));
    let large = "x".repeat(1048577);
    fails(&mut session, &root, &user, &["write", &handle, &large]);
    assert_eq!(fs::read(root.root().join("tmp/limit")).unwrap(), b"");
}
#[test]
fn close_and_drop_release_handles_but_do_not_delete_user_data() {
    let (root, user) = fixture();
    {
        let mut session = FdSession::new(user.uid).unwrap();
        let h = token(call(
            &mut session,
            &root,
            &user,
            &["open", "/tmp/keep", "create"],
        ));
        call(&mut session, &root, &user, &["write", &h, "keep"]);
    }
    assert_eq!(fs::read(root.root().join("tmp/keep")).unwrap(), b"keep");
}
