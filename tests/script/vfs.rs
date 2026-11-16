use crate::support::{fixture, run};
use elara_os::{script::Context, virtual_path::VirtualPath};
use std::{assert, assert_eq};
#[test]
fn vfs_commands_route_regular_io_links_fd_and_snapshots() {
    let (root, user) = fixture();
    let text="vfs mount /mnt/work 65536 64; mkdir /mnt/work/dir; printf payload > /mnt/work/dir/file; vfs link /mnt/work/dir/file /mnt/work/hard; vfs symlink dir/file /mnt/work/soft; cat /mnt/work/soft; fd open /mnt/work/hard read; rm /mnt/work/dir/file; rm /mnt/work/hard; fd read 0:1 7; fd close 0:1; printf before > /mnt/work/snap; vfs snapshot /mnt/work /mnt/snapshot; printf after > /mnt/work/snap; cat /mnt/snapshot/snap; vfs clone /mnt/snapshot /mnt/clone; printf clone > /mnt/clone/snap; cat /mnt/clone/snap; vfs unmount /mnt/clone; vfs unmount /mnt/snapshot; vfs unmount /mnt/work";
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), text.into()],
    )
    .unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    assert!(report.stderr.is_empty(), "{report:?}");
    let out = String::from_utf8_lossy(&report.stdout);
    assert!(out.contains("payload0:1\npayload"), "{out}");
    assert!(out.contains("beforeclone"), "{out}");
    assert_eq!(root.mounts_as(&user).unwrap(), ["/dev", "/proc", "/sys"]);
    assert!(!root.root().join("mnt/work").exists());
}
#[test]
fn vfs_rejects_cross_device_move_and_busy_unmount_without_losing_data() {
    let (root, user) = fixture();
    let text="vfs mount /mnt/a 65536 64; vfs mount /mnt/b 65536 64; printf keep > /mnt/a/f; fd open /mnt/a/f read; vfs unmount /mnt/a; mv /mnt/a/f /mnt/b/f; cat /mnt/a/f; fd close 0:1; vfs unmount /mnt/a; vfs unmount /mnt/b";
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), text.into()],
    )
    .unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    let errors = String::from_utf8_lossy(&report.stderr);
    assert!(errors.contains("Busy"), "{errors}");
    assert!(errors.contains("CrossDevice"), "{errors}");
    assert!(String::from_utf8_lossy(&report.stdout).contains("keep"));
}

#[test]
fn vfs_image_save_reload_preserves_links_and_data_without_host_mounts() {
    let (root, user) = fixture();
    let text="vfs mount /mnt/work 65536 64; printf durable > /mnt/work/file; vfs link /mnt/work/file /mnt/work/link; vfs save /mnt/work /root/volume.img; vfs unmount /mnt/work; vfs load /root/volume.img /mnt/restored; cat /mnt/restored/link; printf changed > /mnt/restored/link; cat /mnt/restored/file; vfs unmount /mnt/restored";
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), text.into()],
    )
    .unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    assert!(report.stderr.is_empty(), "{report:?}");
    assert!(
        String::from_utf8_lossy(&report.stdout).contains("durablechanged"),
        "{report:?}"
    );
    assert!(root.root().join("root/volume.img").is_file());
    assert!(!root.root().join("mnt/work").exists());
}

#[test]
fn vfs_acl_commands_inherit_strip_and_persist() {
    let (root, user) = fixture();
    let text="vfs mount /mnt/acl 65536 64; mkdir /mnt/acl/shared; setfacl -d u::7,g::5,o::0,u:42:7,m::7 /mnt/acl/shared; umask 0777; printf payload > /mnt/acl/shared/file; getfacl /mnt/acl/shared/file; mkdir /mnt/acl/shared/sub; getfacl /mnt/acl/shared/sub; chmod 6700 /mnt/acl/shared/file; chgrp 99 /mnt/acl/shared/file; setfacl -b /mnt/acl/shared/file; getfacl /mnt/acl/shared/file; vfs save /mnt/acl /root/acl.img; vfs unmount /mnt/acl; vfs load /root/acl.img /mnt/acl; getfacl /mnt/acl/shared/sub; vfs unmount /mnt/acl";
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), text.into()],
    )
    .unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    assert!(report.stderr.is_empty(), "{report:?}");
    let out = String::from_utf8_lossy(&report.stdout);
    assert!(out.contains("mode: 660"), "{out}");
    assert!(out.contains("u:42:7"), "{out}");
    assert!(out.contains("gid: 99"), "{out}");
    assert!(out.contains("mode: 750"), "{out}");
    assert_eq!(out.matches("default:u:42:7").count(), 2, "{out}");
}

#[test]
fn vfs_find_du_exec_and_fd_locks_use_the_mounted_backend() {
    let (root, user) = fixture();
    let text="vfs mount /mnt/api 65536 64; mkdir /mnt/api/d; printf data > /mnt/api/d/f; find /mnt/api/d; du /mnt/api/d; printf 'printf executed' > /mnt/api/run; chmod 0100 /mnt/api/run; sh -c 'exec /mnt/api/run'; fd open /mnt/api/d/f rw; fd open /mnt/api/d/f rw; fd lock 0:1 exclusive; fd read 1:1 4; fd unlock 0:1; fd read 1:1 4; fd closeall; fd open /mnt/api/d/f read exclusive; fd open /mnt/api/d/f read; fd closeall; vfs unmount /mnt/api";
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), text.into()],
    )
    .unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    let out = String::from_utf8_lossy(&report.stdout);
    let err = String::from_utf8_lossy(&report.stderr);
    assert!(out.contains("/mnt/api/d/f"), "{out}");
    assert!(out.contains("logical_file_bytes: 4"), "{out}");
    assert!(out.contains("executed"), "{out}");
    assert!(out.contains("data"), "{out}");
    assert_eq!(err.matches("LockConflict").count(), 1, "{err}");
    assert_eq!(err.matches("Busy").count(), 1, "{err}");
    assert_eq!(err.lines().count(), 2, "{err}");
    assert_eq!(root.mounts_as(&user).unwrap(), ["/dev", "/proc", "/sys"]);
}

#[test]
fn vfs_pseudo_commands_read_generated_files_and_device_failure_is_explicit() {
    let (root, user) = fixture();
    let text="vfs pseudo proc /mnt/proc; vfs pseudo sys /mnt/sys; vfs pseudo dev /mnt/dev; cat /mnt/proc/identity; cat /mnt/sys/limits; printf discard > /mnt/dev/null; fd open /mnt/dev/zero read; fd read 0:1 4; fd closeall; printf rejected > /mnt/dev/full; printf rejected > /mnt/sys/limits; ls /mnt/dev; vfs unmount /mnt/proc; vfs unmount /mnt/sys; vfs unmount /mnt/dev";
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), text.into()],
    )
    .unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    let out = String::from_utf8_lossy(&report.stdout);
    let err = String::from_utf8_lossy(&report.stderr);
    assert!(out.contains("euid: 0"), "{out}");
    assert!(out.contains("max_file_io_bytes:"), "{out}");
    assert!(report.stdout.windows(4).any(|b| b == [0; 4]));
    assert!(out.contains("[virtual device]"), "{out}");
    assert!(err.contains("NoSpace"), "{err}");
    assert!(err.contains("ReadOnly"), "{err}");
    assert!(!root.root().join("mnt/dev").exists());
    assert_eq!(root.mounts_as(&user).unwrap(), ["/dev", "/proc", "/sys"]);
}

#[test]
fn vfs_namespace_shell_cd_nested_mount_find_glob_and_release() {
    let (root, user) = fixture();
    let text = "vfs mount /mnt/outer 65536 64; mkdir /mnt/outer/parent; vfs mount /mnt/outer/parent/inner 65536 64; printf payload > /mnt/outer/parent/inner/file; ls /mnt; echo /mnt/*/parent/*/file; find /mnt; cd /mnt/outer/parent/inner; vfs unmount /mnt/outer/parent/inner; cat file; cd /; vfs unmount /mnt/outer; vfs unmount /mnt/outer/parent/inner; vfs unmount /mnt/outer";
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), text.into()],
    )
    .unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    let out = String::from_utf8_lossy(&report.stdout);
    let err = String::from_utf8_lossy(&report.stderr);
    assert!(out.contains("outer/"), "{out}");
    assert_eq!(
        out.matches("/mnt/outer/parent/inner/file").count(),
        2,
        "{out}"
    );
    assert!(out.contains("payload"), "{out}");
    assert_eq!(err.matches("Busy").count(), 2, "{err}");
    assert_eq!(err.lines().count(), 2, "{err}");
    assert_eq!(root.mounts_as(&user).unwrap(), ["/dev", "/proc", "/sys"]);
}

#[test]
fn vfs_namespace_context_clone_keeps_cwd_and_failed_cd_keeps_old_pin() {
    let (root, user) = fixture();
    let mut context = Context::new(VirtualPath::root());
    let report = run(
        &root,
        &user,
        &mut context,
        &[
            "-c".into(),
            "vfs mount /mnt/work 65536 64; mkdir /mnt/work/d; cd /mnt/work/d".into(),
        ],
    )
    .unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    assert_eq!(context.cwd.to_string(), "/mnt/work/d");
    let mut clone = context.clone();
    let report = run(
        &root,
        &user,
        &mut context,
        &["-c".into(), "cd /missing; vfs unmount /mnt/work".into()],
    )
    .unwrap();
    assert_eq!(context.cwd.to_string(), "/mnt/work/d");
    assert!(String::from_utf8_lossy(&report.stderr).contains("Busy"));
    let report = run(
        &root,
        &user,
        &mut context,
        &["-c".into(), "cd /; vfs unmount /mnt/work".into()],
    )
    .unwrap();
    assert_eq!(report.status, 1, "{report:?}");
    assert!(String::from_utf8_lossy(&report.stderr).contains("Busy"));
    let report = run(
        &root,
        &user,
        &mut clone,
        &["-c".into(), "cd /; vfs unmount /mnt/work".into()],
    )
    .unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    assert!(report.stderr.is_empty(), "{report:?}");
}

#[test]
fn system_providers_shell_guest_device_io_without_system_write_authority() {
    let (root, admin) = fixture();
    let guest = elara_os::users::add_user(&root, &admin, "deviceguest", "test-password").unwrap();
    let source = "cat /proc/identity; cat /sys/version; printf discard > /dev/null; fd open /dev/zero read; fd read 0:1 4; fd closeall; printf rejected > /dev/full; touch /dev/not-a-device; printf rejected > /sys/version; echo SYSTEM_DONE";
    let report = run(
        &root,
        &guest,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), source.into()],
    )
    .unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    let out = String::from_utf8_lossy(&report.stdout);
    let err = String::from_utf8_lossy(&report.stderr);
    assert!(
        out.contains(&std::format!("euid: {}\n", guest.uid)),
        "{out}"
    );
    assert!(out.contains("ElaraOS"), "{out}");
    assert!(report.stdout.windows(4).any(|b| b == [0; 4]));
    assert!(out.contains("SYSTEM_DONE"), "{out}");
    assert_eq!(err.lines().count(), 3, "{err}");
    assert!(err.contains("NoSpace"), "{err}");
    assert!(!root.root().join("dev/null").exists());
    assert!(!root.root().join("dev/not-a-device").exists());
}

#[test]
fn block_store_commands_create_save_load_and_report_real_image_slots() {
    let (root, user) = fixture();
    let source = "vfs mount /mnt/a 65536 64; printf before > /mnt/a/file; vfs disk-create /mnt/a /root/disk 64 1; printf after > /mnt/a/file; vfs disk-save /mnt/a /root/disk 1; vfs unmount /mnt/a; vfs disk-load /root/disk /mnt/b 1; cat /mnt/b/file; vfs disk-stat /root/disk 1; vfs unmount /mnt/b";
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), source.into()],
    )
    .unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    assert!(report.stderr.is_empty(), "{report:?}");
    let out = String::from_utf8_lossy(&report.stdout);
    assert!(out.contains("after"), "{out}");
    assert!(out.contains("allocated_slots:"), "{out}");
    assert!(out.contains("dirty_pages: 0"), "{out}");
    assert_eq!(
        std::fs::metadata(root.root().join("root/disk"))
            .unwrap()
            .len(),
        64 * 4096
    );
}

#[test]
fn sealed_checkpoint_commands_reject_wrong_key_without_mount_or_plaintext_output() {
    let (root, user) = fixture();
    let source = "vfs mount /mnt/a 65536 64; printf private > /mnt/a/file; vfs keygen /root/key; vfs keygen /root/wrong; vfs seal /mnt/a /root/sealed /root/key; vfs unmount /mnt/a; vfs unseal /root/sealed /mnt/denied /root/wrong; vfs unseal /root/sealed /mnt/b /root/key; cat /mnt/b/file";
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), source.into()],
    )
    .unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    let err = String::from_utf8_lossy(&report.stderr);
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.contains("Crypto"), "{err}");
    assert!(String::from_utf8_lossy(&report.stdout).contains("private"));
    assert!(!root
        .mounts_as(&user)
        .unwrap()
        .iter()
        .any(|p| p == "/mnt/denied"));
    let key = std::fs::read(root.root().join("root/key")).unwrap();
    assert_eq!(key.len(), 32);
    assert!(!report.stdout.windows(32).any(|window| window == key));
    assert!(!report.stderr.windows(32).any(|window| window == key));
}

#[test]
fn overlay_commands_merge_hide_and_save_flattened_view() {
    let (root, user) = fixture();
    let source = "vfs mount /mnt/lo 65536 64; vfs mount /mnt/up 65536 64; printf lower > /mnt/lo/a; printf hidden > /mnt/lo/hide; printf upper > /mnt/up/b; vfs overlay /mnt/lo /mnt/up /mnt/view /hide; cat /mnt/view/a; cat /mnt/view/b; vfs disk-create /mnt/view /root/overlay 64 1; vfs unmount /mnt/view; vfs disk-load /root/overlay /mnt/reloaded 1; cat /mnt/reloaded/a";
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), source.into()],
    )
    .unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    assert!(report.stderr.is_empty(), "{report:?}");
    let out = String::from_utf8_lossy(&report.stdout);
    assert!(out.contains("lowerupper"), "{out}");
    assert!(std::matches!(
        root.inode_as(&user, "/mnt/reloaded/hide", false),
        Err(elara_os::filesystem::FsError::Vfs(
            elara_os::filesystem::vfs::Error::NotFound
        ))
    ));
    assert_eq!(
        root.read_file_as(&user, "/mnt/lo/hide", 32).unwrap(),
        b"hidden"
    );
}

#[test]
fn dedup_command_reports_referenced_bytes_and_preserves_file_isolation() {
    let (root, user) = fixture();
    let source = "vfs mount /mnt/a 65536 64; printf identical > /mnt/a/one; printf identical > /mnt/a/two; vfs dedup /mnt/a; fd open /mnt/a/two rw; fd writeat 0:1 0 X; fd closeall; cat /mnt/a/one; cat /mnt/a/two";
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), source.into()],
    )
    .unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    assert!(report.stderr.is_empty(), "{report:?}");
    let out = String::from_utf8_lossy(&report.stdout);
    assert!(
        out.contains("unique_pages_before: 2, unique_pages_after: 1"),
        "{out}"
    );
    assert!(out.contains("referenced_bytes_reduced: 4096"), "{out}");
    assert!(out.contains("identicalXdentical"), "{out}");
}

#[test]
fn sealed_checkpoint_acl_mask_controls_key_admission() {
    let (root, user) = fixture();
    let source = "vfs mount /mnt/a 65536 64; printf private > /mnt/a/file; vfs keygen /root/key; setfacl u:42:4 /root/key; vfs seal /mnt/a /root/denied /root/key; chmod 0600 /root/key; vfs seal /mnt/a /root/sealed /root/key; vfs unseal /root/sealed /mnt/b /root/key; cat /mnt/b/file";
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), source.into()],
    )
    .unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    let err = String::from_utf8_lossy(&report.stderr);
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(
        err.contains(&elara_os::filesystem::FsError::OwnershipDenied.to_string()),
        "{err}"
    );
    assert!(!root.root().join("root/denied").exists());
    assert!(String::from_utf8_lossy(&report.stdout).contains("private"));
}

#[test]
fn cross_mount_commands_read_write_fd_cwd_names_and_host_refusal() {
    let (root, user) = fixture();
    let source = "vfs mount /mnt/a 65536 64; vfs mount /mnt/b 65536 64; mkdir /mnt/b/d; printf target > /mnt/b/d/file; vfs symlink-global /mnt/b/d /mnt/a/dir; cat /mnt/a/dir/file; printf changed > /mnt/a/dir/file; fd open /mnt/a/dir/file read; fd read 0:1 32; fd closeall; cd /mnt/a/dir; pwd; cd /; vfs symlink-global /mnt/b/d/file /mnt/a/link; vfs readlink /mnt/a/link; mv /mnt/a/link /mnt/a/moved; rm /mnt/a/moved; cat /mnt/b/d/file; vfs symlink-global /root/private /mnt/a/host; cat /mnt/a/host; echo DONE";
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), source.into()],
    )
    .unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    let out = String::from_utf8_lossy(&report.stdout);
    let err = String::from_utf8_lossy(&report.stderr);
    assert_eq!(
        err.as_ref(),
        "Warning: committed; source directory durability unconfirmed: Unsupported
Warning: committed; destination directory durability unconfirmed: Unsupported
Error: virtual filesystem: CrossDevice
"
    );
    assert!(out.contains("target"), "{out}");
    assert!(out.contains("/mnt/b/d\n"), "{out}");
    assert!(out.contains("/mnt/b/d/file"), "{out}");
    assert!(out.contains("changedDONE"), "{out}");
    assert_eq!(
        root.read_file_as(&user, "/mnt/b/d/file", 32).unwrap(),
        b"changed"
    );
}

#[test]
fn cross_mount_aliases_preserve_system_policy_dangling_creation_and_exclusive_names() {
    let (root, user) = fixture();
    let source = "vfs mount /mnt/a 65536 64; vfs mount /mnt/b 65536 64; vfs symlink-global /sys/version /mnt/a/system; printf forbidden > /mnt/a/system; vfs symlink-global /mnt/b/new /mnt/a/dangling; printf created > /mnt/a/dangling; cat /mnt/b/new; vfs save /mnt/a /root/links.img; vfs unmount /mnt/a; vfs load /root/links.img /mnt/restored; cat /mnt/restored/dangling; echo DONE";
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), source.into()],
    )
    .unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    let err = String::from_utf8_lossy(&report.stderr);
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.contains("Denied"), "{err}");
    let out = String::from_utf8_lossy(&report.stdout);
    assert!(out.contains("createdDONE"), "{out}");
    use elara_os::filesystem::{vfs, FileAccess, FileBehavior, FileCreation, FileMode, FsError};
    let exclusive = FileMode::new(
        FileAccess::ReadWrite,
        FileCreation::Exclusive,
        FileBehavior::Preserve,
    )
    .unwrap();
    assert!(std::matches!(
        root.open_file_as(&user, "/mnt/restored/dangling", exclusive),
        Err(FsError::Vfs(vfs::Error::Exists))
    ));
    assert!(root
        .read_file_as(&user, "/sys/version", 1024)
        .unwrap()
        .starts_with(b"ElaraOS"));
    assert_eq!(
        root.read_file_as(&user, "/mnt/b/new", 32).unwrap(),
        b"created"
    );
}

#[test]
fn proc_tasks_commands_read_current_scope_without_host_processes() {
    let (root, user) = fixture();
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &[
            "-c".into(),
            "touch /tmp/proc-lease; ls /proc/tasks; cat /proc/tasks/*/status".into(),
        ],
    )
    .unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    assert!(report.stderr.is_empty(), "{report:?}");
    let out = String::from_utf8_lossy(&report.stdout);
    assert!(out.contains("lease_state: LeaseHeld"), "{out}");
    assert!(out.contains("kind: resource-lease"), "{out}");
    assert!(root.list_as(&user, "/proc/tasks").unwrap().is_empty());
    assert!(!root.root().join("proc/tasks").exists());
}

#[test]
fn proc_tasks_read_only_script_does_not_create_a_resource_lease() {
    let (root, user) = fixture();
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), "ls /proc/tasks".into()],
    )
    .unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    assert!(report.stdout.is_empty(), "{report:?}");
    assert!(report.stderr.is_empty(), "{report:?}");
    assert!(root.list_as(&user, "/proc/tasks").unwrap().is_empty());
}

#[test]
fn host_cwd_context_clone_and_failed_cd_keep_identity_until_last_release() {
    let (root, user) = fixture();
    let mut context = Context::new(VirtualPath::root());
    let report = run(&root, &user, &mut context,
        &["-c".into(), "mkdir /root/cwd-parent; mkdir /root/cwd-parent/leaf; cd /root/cwd-parent/leaf".into()]).unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    let mut copy = context.clone();
    let report = run(&root, &user, &mut context,
        &["-c".into(), "cd /missing; rmdir /root/cwd-parent/leaf".into()]).unwrap();
    assert_eq!(context.cwd.to_string(), "/root/cwd-parent/leaf");
    assert!(String::from_utf8_lossy(&report.stderr).contains("Busy"), "{report:?}");
    let report = run(&root, &user, &mut context,
        &["-c".into(), "cd /; rmdir /root/cwd-parent/leaf".into()]).unwrap();
    assert_eq!(report.status, 1, "{report:?}");
    assert!(String::from_utf8_lossy(&report.stderr).contains("Busy"), "{report:?}");
    let report = run(&root, &user, &mut copy,
        &["-c".into(), "cd /; rmdir /root/cwd-parent/leaf; mkdir /root/cwd-parent/leaf; rmdir /root/cwd-parent/leaf; rmdir /root/cwd-parent".into()]).unwrap();
    assert_eq!(report.status, 0, "{report:?}");
    assert!(report.stderr.is_empty(), "{report:?}");
}
