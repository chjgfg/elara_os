use super::fixture;
use crate::filesystem::{
    vfs::{Error, MountKind, PseudoKind},
    *,
};
use crate::users::Account;
use std::{assert, assert_eq, matches};
fn account(uid: u32) -> Account {
    Account {
        uid,
        name: if uid == 0 { "root" } else { "guest" }.into(),
        home: if uid == 0 { "/root" } else { "/home/guest" }.into(),
        identity: Default::default(),
    }
}
#[test]
fn system_providers_are_default_immutable_and_identity_specific() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let admin = account(0);
    let guest = account(17);
    assert_eq!(root.mounts_as(&admin).unwrap(), ["/dev", "/proc", "/sys"]);
    let info = root.mount_info_as(&admin).unwrap();
    assert_eq!(info[0].kind, MountKind::System(PseudoKind::Dev));
    assert_eq!(info[1].kind, MountKind::System(PseudoKind::Proc));
    assert_eq!(info[2].kind, MountKind::System(PseudoKind::Sys));
    let bytes = root.read_file_as(&guest, "/proc/identity", 1024).unwrap();
    assert!(String::from_utf8(bytes).unwrap().contains("euid: 17\n"));
    assert!(
        String::from_utf8(root.read_file_as(&admin, "/sys/limits", 1024).unwrap())
            .unwrap()
            .contains("max_file_io_bytes:")
    );
    for point in ["/proc", "/sys", "/dev"] {
        assert!(matches!(
            root.unmount_as(&admin, point),
            Err(FsError::Vfs(Error::ReadOnly))
        ));
        assert_eq!(
            std::fs::read_dir(path.join(&point[1..])).unwrap().count(),
            0
        );
    }
    assert!(matches!(
        root.create_file("/sys/new"),
        Err(FsError::ReadOnlySystemPath)
    ));
    assert!(matches!(
        root.create_file_as(&admin, "/dev/ordinary"),
        Err(FsError::ReadOnlySystemPath)
    ));
    assert!(matches!(
        root.remove_file_as(&admin, "/dev/null"),
        Err(FsError::ReadOnlySystemPath)
    ));
    assert!(matches!(
        root.chmod_as(&admin, "/dev/null", VirtualMode::new(0o777).unwrap()),
        Err(FsError::ReadOnlySystemPath)
    ));
    assert!(matches!(
        root.read_file("/proc/identity", 1024),
        Err(FsError::OwnershipContextRequired)
    ));
}
#[test]
fn system_providers_device_io_is_bounded_and_not_namespace_authority() {
    let root = DataRoot::open(&fixture()).unwrap();
    let guest = account(17);
    let mode = FileMode::new(
        FileAccess::ReadWrite,
        FileCreation::Existing,
        FileBehavior::Preserve,
    )
    .unwrap();
    // Negative control: the ordinary path permission gate stays closed.
    assert!(crate::permissions::check(
        &guest,
        &crate::virtual_path::VirtualPath::strict("/dev/null").unwrap(),
        crate::permissions::Access::Write
    )
    .is_err());
    std::fs::write(root.root().join("dev/null"), b"host sentinel").unwrap();
    let null = root.open_file_as(&guest, "/dev/null", mode).unwrap();
    assert_eq!(null.write(b"discarded").unwrap(), 9);
    assert!(null.read(16).unwrap().is_empty());
    assert_eq!(
        std::fs::read(root.root().join("dev/null")).unwrap(),
        b"host sentinel"
    );
    let zero = root
        .open_file_as(&guest, "/dev/zero", FileMode::READ)
        .unwrap();
    assert_eq!(zero.read(16).unwrap(), [0; 16]);
    assert!(zero
        .read(crate::config::ROOTFS_DEFAULTS.max_file_io_bytes + 1)
        .is_err());
    let full = root.open_file_as(&guest, "/dev/full", mode).unwrap();
    assert!(matches!(
        full.write(b"x"),
        Err(FsError::Vfs(Error::NoSpace))
    ));
    assert_eq!(full.write(b"").unwrap(), 0);
    assert!(root.create_file_as(&guest, "/dev/null").is_err());
    assert!(root
        .replace_owned_file(&guest, "/dev/null", b"not a device")
        .is_err());
    assert!(root.open_file_as(&guest, "/sys/limits", mode).is_err());
    assert!(root.open_file_as(&guest, "/dev/missing", mode).is_err());
    let create = FileMode::new(
        FileAccess::WriteOnly,
        FileCreation::Exclusive,
        FileBehavior::Preserve,
    )
    .unwrap();
    assert!(matches!(
        root.open_file_as(&guest, "/dev/null", create),
        Err(FsError::Vfs(Error::Exists))
    ));
    #[cfg(windows)]
    {
        std::fs::write(root.root().join("proc/identity"), b"host secret").unwrap();
        let generated = root.read_file_as(&guest, "/PROC/identity", 1024).unwrap();
        assert!(String::from_utf8(generated).unwrap().contains("euid: 17"));

        assert_eq!(
            root.open_file_as(&guest, "/DEV/zero", FileMode::READ)
                .unwrap()
                .read(4)
                .unwrap(),
            [0; 4]
        );
        assert!(matches!(
            root.read_file("/PROC/identity", 1024),
            Err(FsError::OwnershipContextRequired)
        ));
    }
}
#[test]
fn system_providers_refuse_unknown_backing_objects_without_adoption() {
    for name in ["dev", "proc", "sys"] {
        let path = fixture();
        std::fs::write(path.join(name).join("unknown"), b"keep").unwrap();
        assert!(matches!(
            DataRoot::open(&path),
            Err(FsError::SystemProviderConflict(_))
        ));
        assert_eq!(
            std::fs::read(path.join(name).join("unknown")).unwrap(),
            b"keep"
        );
    }
}

#[test]
fn proc_tasks_live_lease_permissions_exit_and_open_snapshot_are_explicit() {
    let path = fixture();
    let root = DataRoot::open(&path).unwrap();
    let admin = account(0);
    let owner = account(17);
    let stranger = account(18);
    let lease = root.begin_task_as(&owner).unwrap();
    let task_path = std::format!("/proc/tasks/{}/status", lease.id().value());
    let bytes = root.read_file_as(&owner, &task_path, 1024).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(text.contains("lease_state: LeaseHeld\n"), "{text}");
    assert!(text.contains("kind: resource-lease\n"), "{text}");
    assert!(!text.contains("Running"));
    assert_eq!(root.read_file_as(&admin, &task_path, 1024).unwrap(), bytes);
    assert!(root.read_file_as(&stranger, &task_path, 1024).is_err());
    assert!(root
        .list_as(&admin, "/proc/tasks")
        .unwrap()
        .iter()
        .any(|entry| entry.name == std::ffi::OsString::from(lease.id().value().to_string())));
    let fd = root
        .open_file_as(&owner, &task_path, FileMode::READ)
        .unwrap();
    let prefix = fd.read(7).unwrap();
    let completed = lease.finish().unwrap();
    assert_eq!(completed.deferred, 0);
    assert!(matches!(
        root.read_file_as(&owner, &task_path, 1024),
        Err(FsError::Vfs(Error::NotFound))
    ));
    let mut old = prefix;
    old.extend(fd.read(1024).unwrap());
    assert_eq!(old, bytes);
    assert!(root.list_as(&admin, "/proc/tasks").unwrap().is_empty());
    assert_eq!(std::fs::read_dir(path.join("proc")).unwrap().count(), 0);
    assert!(matches!(
        root.create_file_as(&admin, "/proc/tasks/fake"),
        Err(FsError::ReadOnlySystemPath)
    ));
}

#[test]
fn proc_tasks_retired_directory_cursor_keeps_children_without_reusing_ids() {
    let root = DataRoot::open(&fixture()).unwrap();
    let admin = account(0);
    let task = root.begin_task_as(&admin).unwrap();
    let old_id = task.id().value();
    let path = std::format!("/proc/tasks/{old_id}");
    let mut cursor = root.open_directory_as(&admin, &path).unwrap();
    task.finish().unwrap();
    root.list_as(&admin, "/proc/tasks").unwrap();
    assert_eq!(cursor.read(8).unwrap()[0].name, "status");
    let next = root.begin_task_as(&admin).unwrap();
    assert!(next.id().value() > old_id);
    let rows = root.list_as(&admin, "/proc/tasks").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].name,
        std::ffi::OsString::from(next.id().value().to_string())
    );
    assert!(matches!(
        root.read_file_as(&admin, &std::format!("{path}/status"), 1024),
        Err(FsError::Vfs(Error::NotFound))
    ));
}

#[test]
fn proc_tasks_probe_distinguishes_missing_invalid_recoverable_and_locked_without_writes() {
    let base = fixture();
    let root = DataRoot::open(&base).unwrap();
    let admin = account(0);
    let task = root.begin_task_as(&admin).unwrap();
    let id = task.id();
    assert_eq!(root.task_lease_state(id, 0), TaskLeaseState::LeaseHeld);
    task.finish().unwrap();
    assert_eq!(root.task_lease_state(id, 0), TaskLeaseState::MissingLease);
    let path = base.join(crate::filesystem::scope::lock_path(id));
    std::fs::write(&path, b"unknown-marker").unwrap();
    assert_eq!(root.task_lease_state(id, 0), TaskLeaseState::Unconfirmed);
    assert_eq!(std::fs::read(&path).unwrap(), b"unknown-marker");
    let marker = crate::filesystem::scope::lock_magic(id, 0);
    std::fs::write(&path, &marker).unwrap();
    assert_eq!(root.task_lease_state(id, 0), TaskLeaseState::Recoverable);
    assert_eq!(root.task_lease_state(id, 99), TaskLeaseState::Unconfirmed);
    assert_eq!(std::fs::read(&path).unwrap(), marker);
    assert!(root.list_as(&admin, "/proc/tasks").unwrap().is_empty()); // never adopts orphan marker
    assert!(path.exists()); // observation is not cleanup
}

#[test]
fn proc_jobs_states_permissions_retired_fds_and_domains_are_explicit() {
    use std::sync::{Arc, Mutex};
    struct Observed(Mutex<RuntimeJobState>);
    impl crate::filesystem::JobObservation for Observed {
        fn state(&self) -> Result<RuntimeJobState, FsError> {
            Ok(*self.0.lock().unwrap())
        }
    }
    let base = fixture();
    let root = DataRoot::open(&base).unwrap();
    let admin = account(0);
    let owner = account(17);
    let other = account(18);
    let source = Arc::new(Observed(Mutex::new(RuntimeJobState::Running)));
    root.observe_job(owner.uid, 1, source.clone()).unwrap();
    let path = "/proc/jobs/1/status";
    let first = root.read_file_as(&owner, path, 1024).unwrap();
    assert!(String::from_utf8_lossy(&first).contains("state: Running\n"));
    assert!(matches!(
        root.read_file_as(&other, path, 1024),
        Err(FsError::Vfs(Error::Denied))
    ));
    let fd = root.open_file_as(&owner, path, FileMode::READ).unwrap();
    let duplicate = fd.clone();
    let mut assembled = fd.read(11).unwrap();
    let mut cursor = root.open_directory_as(&owner, "/proc/jobs/1").unwrap();
    for state in [
        RuntimeJobState::PauseRequested,
        RuntimeJobState::Stopped,
        RuntimeJobState::Resuming,
        RuntimeJobState::Cancelling,
        RuntimeJobState::Cancelled,
        RuntimeJobState::Exited(7),
        RuntimeJobState::Panicked,
    ] {
        *source.0.lock().unwrap() = state;
        let text = String::from_utf8(root.read_file_as(&admin, path, 1024).unwrap()).unwrap();
        assert!(text.contains(&std::format!("state: {state:?}\n")), "{text}");
        assert!(text.contains("job_id: 1\n") && text.contains("kind: scoped-job\n"));
        assert_eq!(fd.info().unwrap().length, first.len() as u64);
        assert_eq!(fd.read_at(0, 1024).unwrap(), first);
        assert!(fd.read_at(u64::MAX, 8).unwrap().is_empty());
        assert_eq!(fd.info().unwrap().position, FilePosition::Known(11));
    }
    assert_eq!(
        root.clone_file_domain()
            .unwrap()
            .list_as(&admin, "/proc/jobs")
            .unwrap()
            .len(),
        1
    );
    assert!(DataRoot::open(&base)
        .unwrap()
        .list_as(&admin, "/proc/jobs")
        .unwrap()
        .is_empty());
    let lease = root.begin_task_as(&owner).unwrap();
    assert!(String::from_utf8_lossy(
        &root
            .read_file_as(
                &owner,
                &std::format!("/proc/tasks/{}/status", lease.id().value()),
                1024
            )
            .unwrap()
    )
    .contains("resource-lease"));
    drop(source);
    assert!(root.list_as(&admin, "/proc/jobs").unwrap().is_empty());
    assembled.extend(duplicate.read(1024).unwrap());
    assert_eq!(assembled, first);
    assert!(fd.read(1).unwrap().is_empty());
    assert_eq!(
        fd.seek(std::io::SeekFrom::End(-1)).unwrap(),
        first.len() as u64 - 1
    );
    assert_eq!(
        duplicate.read(2).unwrap(),
        b"
"
    );
    assert_eq!(cursor.read(8).unwrap()[0].name, "status");
    assert!(matches!(
        root.read_file_as(&owner, path, 1024),
        Err(FsError::Vfs(Error::NotFound))
    ));
    let next = Arc::new(Observed(Mutex::new(RuntimeJobState::Running)));
    root.observe_job(owner.uid, 1, next.clone()).unwrap();
    assert_eq!(root.list_as(&admin, "/proc/jobs").unwrap()[0].name, "2");
    assert_eq!(std::fs::read_dir(base.join("proc")).unwrap().count(), 0);
}

#[test]
fn proc_jobs_registry_is_bounded_and_releases_dead_sources() {
    use std::sync::Arc;
    struct Observed;
    impl crate::filesystem::JobObservation for Observed {
        fn state(&self) -> Result<RuntimeJobState, FsError> {
            Ok(RuntimeJobState::Running)
        }
    }
    let root = DataRoot::open(&fixture()).unwrap();
    let source = Arc::new(Observed);
    for id in 1..=256 {
        root.observe_job(0, id, source.clone()).unwrap();
    }
    assert!(matches!(
        root.observe_job(0, 257, source.clone()),
        Err(FsError::Vfs(Error::Quota))
    ));
    assert_eq!(root.list_as(&account(0), "/proc/jobs").unwrap().len(), 256);
    drop(source);
    assert!(root.list_as(&account(0), "/proc/jobs").unwrap().is_empty());
    let next = Arc::new(Observed);
    root.observe_job(0, 1, next.clone()).unwrap();
    assert_eq!(
        root.list_as(&account(0), "/proc/jobs").unwrap()[0].name,
        "257"
    );
}

#[test]
fn proc_legacy_foreground_live_state_exit_codes_permissions_and_fd_snapshot() {
    use crate::tasks::{ExitStatus, TaskTable};
    let base = fixture(); let root = DataRoot::open(&base).unwrap();
    let owner = account(17); let other = account(18);
    let mut table = TaskTable::new(2);
    let task = table.begin_observed(&root, owner.uid, "private-command-text").unwrap();
    let path = "/proc/jobs/1/status";
    let first = root.read_file_as(&owner, path, 1024).unwrap();
    let text = String::from_utf8_lossy(&first);
    assert!(text.contains("state: Running\n"), "{text}");
    assert!(text.contains("kind: legacy-task\n") && text.contains("task_id: 1\n"));
    assert!(!text.contains("private-command-text") && !text.contains("job_id:"));
    assert!(matches!(root.read_file_as(&other, path, 1024), Err(FsError::Vfs(Error::Denied))));
    let fd = root.open_file_as(&owner, path, FileMode::READ).unwrap();
    task.finish(ExitStatus::Failure(i32::MIN));
    // No ps/snapshot/wait refresh: publication reads the actual terminal state.
    let text = String::from_utf8(root.read_file_as(&owner, path, 1024).unwrap()).unwrap();
    assert!(text.contains("state: TaskExited(-2147483648)\n"), "{text}");
    assert_eq!(fd.read_at(0, 1024).unwrap(), first);
    assert_eq!(table.wait(1).unwrap(), ExitStatus::Failure(i32::MIN));
    let task = table.begin_observed(&root, owner.uid, "aborted").unwrap();
    drop(task);
    let text = String::from_utf8(root.read_file_as(&owner, "/proc/jobs/2/status", 1024).unwrap()).unwrap();
    assert!(text.contains("state: TaskExited(1)\n"), "{text}");
    drop(table);
    assert!(root.list_as(&owner, "/proc/jobs").unwrap().is_empty());
    assert_eq!(fd.read_at(0, 1024).unwrap(), first);
    assert!(!base.join("proc/jobs").exists());
}
#[test]
fn proc_legacy_bounded_history_and_registry_refusal_precede_task_start() {
    use crate::tasks::{ExitStatus, TaskError, TaskTable};
    use std::sync::Arc;
    let root = DataRoot::open(&fixture()).unwrap(); let owner = account(0);
    let mut table = TaskTable::new(1);
    let task = table.begin_observed(&root, 0, "first").unwrap();
    assert!(matches!(table.begin_observed(&root, 0, "overflow"), Err(TaskError::TableFull)));
    assert_eq!(root.list_as(&owner, "/proc/jobs").unwrap().len(), 1);
    task.finish(ExitStatus::Success);
    for _ in 0..8 {
        table.begin_observed(&root, 0, "bounded").unwrap().finish(ExitStatus::Success);
    }
    assert!(root.list_as(&owner, "/proc/jobs").unwrap().len() <= 2);
    drop(table);
    struct Source;
    impl crate::filesystem::JobObservation for Source {
        fn state(&self) -> Result<RuntimeJobState, FsError> { Ok(RuntimeJobState::Running) }
    }
    let source = Arc::new(Source);
    for id in 0..256 { root.observe_job(0, id, source.clone()).unwrap(); }
    let mut table = TaskTable::new(1);
    assert!(matches!(table.spawn_sleep_observed(&root, 0, std::time::Duration::ZERO), Err(TaskError::TableFull)));
    assert!(table.snapshot().is_empty());
    drop(source);
    let task = table.begin_observed(&root, 0, "after-release").unwrap();
    assert_eq!(task.id, 1);
    task.finish(ExitStatus::Success);
}

#[test]
fn proc_legacy_background_finishes_without_consuming_completion_receiver() {
    use crate::tasks::{ExitStatus, TaskTable};
    use std::time::{Duration, Instant};
    let root = DataRoot::open(&fixture()).unwrap();
    let owner = account(0);
    let mut table = TaskTable::new(1);
    let id = table.spawn_sleep_observed(&root, 0, Duration::ZERO).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let text = String::from_utf8(root.read_file_as(&owner, "/proc/jobs/1/status", 1024).unwrap()).unwrap();
        assert!(text.contains("task_id: 1
"), "{text}");
        if text.contains("state: TaskExited(0)
") { break; }
        assert!(Instant::now() < deadline, "{text}");
        std::thread::yield_now();
    }
    assert_eq!(table.wait(id).unwrap(), ExitStatus::Success);
    table.retire_session();
    assert!(root.list_as(&owner, "/proc/jobs").unwrap().is_empty());
}

#[test]
fn proc_executions_permissions_snapshot_and_all_three_inode_lanes_are_disjoint() {
    use std::sync::Arc;
    struct Job;
    impl crate::filesystem::JobObservation for Job {
        fn state(&self) -> Result<RuntimeJobState, FsError> { Ok(RuntimeJobState::Running) }
    }
    let base = fixture(); let root = DataRoot::open(&base).unwrap();
    let owner = account(17); let other = account(18); let admin = account(0);
    let lease = root.begin_task_as(&owner).unwrap();
    let job = Arc::new(Job);
    root.observe_job(owner.uid, 7, job.clone()).unwrap();
    let execution = root.begin_execution(owner.uid).unwrap();
    let path = "/proc/executions/1/status";
    let first = root.read_file_as(&owner, path, 1024).unwrap();
    assert!(String::from_utf8_lossy(&first).contains("kind: foreground-execution\n"));
    assert_eq!(root.read_file_as(&admin, path, 1024).unwrap(), first);
    assert!(matches!(root.read_file_as(&other, path, 1024), Err(FsError::Vfs(Error::Denied))));
    let fd = root.open_file_as(&owner, path, FileMode::READ).unwrap();
    let mut cursor = root.open_directory_as(&owner, "/proc/executions/1").unwrap();
    // Visit all lanes in alternating order; collisions would overwrite an old FD/node.
    let lease_path = std::format!("/proc/tasks/{}/status", lease.id().value());
    for _ in 0..3 {
        assert!(String::from_utf8_lossy(&root.read_file_as(&owner, &lease_path, 1024).unwrap()).contains("resource-lease"));
        assert!(String::from_utf8_lossy(&root.read_file_as(&owner, "/proc/jobs/1/status", 1024).unwrap()).contains("job_id: 7\n"));
        assert_eq!(root.read_file_as(&owner, path, 1024).unwrap(), first);
    }
    let ids: std::collections::BTreeSet<_> = [lease_path.as_str(), "/proc/jobs/1/status", path].iter()
        .map(|path| root.inode_as(&owner, path, false).unwrap().inode).collect();
    assert_eq!(ids.len(), 3);
    drop(execution);
    assert!(root.list_as(&owner, "/proc/executions").unwrap().is_empty());
    assert_eq!(fd.read_at(0, 1024).unwrap(), first);
    assert_eq!(cursor.read(8).unwrap()[0].name, "status");
    assert!(matches!(root.read_file_as(&owner, path, 1024), Err(FsError::Vfs(Error::NotFound))));
    let _next = root.begin_execution(owner.uid).unwrap();
    assert_eq!(root.list_as(&owner, "/proc/executions").unwrap()[0].name, "2");
    assert!(!base.join("proc/executions").exists());
}

#[test]
fn proc_executions_reserved_capacity_precedes_side_effects_and_unwind_releases() {
    use std::sync::Arc;
    struct Job;
    impl crate::filesystem::JobObservation for Job {
        fn state(&self) -> Result<RuntimeJobState, FsError> { Ok(RuntimeJobState::Running) }
    }
    let root = DataRoot::open(&fixture()).unwrap(); let owner = account(0);
    let job = Arc::new(Job);
    for id in 0..256 { root.observe_job(0, id, job.clone()).unwrap(); }
    let mut active = Vec::new();
    for _ in 0..32 { active.push(root.begin_execution(0).unwrap()); }
    assert!(matches!(root.begin_execution(0), Err(FsError::Vfs(Error::Quota))));
    struct Modules;
    impl crate::script::modules::ModuleCommands for Modules {
        fn execute(&mut self, _: crate::script::modules::Request<'_>) -> Result<crate::script::modules::Dispatch, String> {
            std::panic!("must refuse before module effects");
        }
    }
    let ast = crate::script::parser::parse("touch /root/forbidden; module-effect").unwrap();
    let mut context = crate::script::Context::new(crate::virtual_path::VirtualPath::root());
    let report = crate::script::runtime::execute(&ast, &root, &owner, &mut context, &[], &mut Modules);
    assert_eq!(report.status, 2); assert_eq!(report.steps, 0);
    assert!(!root.root().join("root/forbidden").exists());
    active.clear();
    let ast = crate::script::parser::parse("module-effect").unwrap();
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::script::runtime::execute(&ast, &root, &owner, &mut context, &[], &mut Modules)
    }));
    assert!(panicked.is_err());
    assert!(root.list_as(&owner, "/proc/executions").unwrap().is_empty());
    assert_eq!(root.list_as(&owner, "/proc/jobs").unwrap().len(), 256);
    let ast = crate::script::parser::parse("true").unwrap();
    assert_eq!(crate::script::runtime::execute(&ast, &root, &owner, &mut context, &[], &mut Modules).status, 0);
}
