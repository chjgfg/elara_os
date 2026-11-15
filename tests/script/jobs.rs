use crate::support::{fixture, transcript_in};
use elara_os::logging::Logger;
use elara_os::script::{
    jobs::{JobId, JobTable, Spawn, Waited},
    modules::{Dispatch, ModuleCommands, Request},
    parser,
    runtime::Budget,
    shared::{Resources, SharedModules},
    Context,
};
use elara_os::virtual_path::VirtualPath;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{assert, assert_eq, panic};

#[test]
fn background_scripts_are_real_isolated_workers_with_shared_modules_and_exit_status() {
    let (root, _) = fixture();
    let text = transcript_in(&root,"X=parent\nsh -c 'X=child; cd /tmp; touch /root/job-file; echo \"$X\"; pwd; exit 7' &\necho \"$X\"\npwd\njobs\nwait %1\necho $?\nexit 0\n");
    assert!(text.contains("started job: sh -c"), "{text}");
    assert!(text.contains("parent\n/\n"), "{text}");
    assert!(text.contains("child\n/tmp\n[1] exited 7\n"), "{text}");
    assert!(text.ends_with("7\nBye.\n"), "{text}");
    assert!(root.root().join("root/job-file").is_file());
}
#[test]
fn stopped_is_acknowledged_bg_resumes_and_fg_reaps() {
    let (root, _) = fixture();
    let text = transcript_in(
        &root,
        "sleep 1 &\nkill -STOP %1\njobs\nbg %1\nfg %1\njobs\nexit 0\n",
    );
    assert!(text.contains("[1] stopped\n"), "{text}");
    assert!(text.contains("Stopped uid 0 sleep"), "{text}");
    assert!(text.contains("[1] resume requested\n"), "{text}");
    assert!(text.contains("[1] exited 0\n"), "{text}");
}
#[test]
fn cancellation_prevents_later_effects_and_wait_reports_observed_cancellation() {
    let (root, _) = fixture();
    let before = Instant::now();
    let text = transcript_in(
        &root,
        "sh -c 'sleep 10; touch /root/after-cancel' &\nkill -TERM %1\nwait %1\necho $?\nexit 0\n",
    );
    assert!(before.elapsed() < Duration::from_secs(2));
    assert!(text.contains("cancellation requested"), "{text}");
    assert!(text.contains("[1] cancelled 130\n"), "{text}");
    assert!(text.ends_with("130\nBye.\n"), "{text}");
    assert!(!text.contains("Script exited with status 2."), "{text}");
    assert!(
        text.matches("Error: execution cancelled").count() <= 1,
        "{text}"
    );
    assert!(!root.root().join("root/after-cancel").exists());
}
#[test]
fn identity_change_joins_old_jobs_closes_capabilities_and_discards_private_output() {
    let (root, _) = fixture();
    let text = transcript_in(&root,"useradd jobguest secret1\nX=private_job_value\nfd open /tmp/old create\nsh -c 'sleep 10; echo \"$X\"; touch /root/after-change' &\nsu jobguest\nwait %1\nfd read 0:1 1\necho \"${X-unset}\"\nexit 0\n");
    assert!(
        text.contains("Joined 1 script jobs before identity change."),
        "{text}"
    );
    assert!(
        text.contains("Closed 1 file handles on identity change."),
        "{text}"
    );
    assert!(text.contains("no scoped job %1"), "{text}");
    assert!(
        text.contains("closed, stale or belongs to another table"),
        "{text}"
    );
    assert!(text.contains("unset\n"), "{text}");
    assert!(!text.contains("private_job_value"));
    assert!(!root.root().join("root/after-change").exists());
}
#[test]
fn exit_cancels_and_joins_unfinished_jobs_instead_of_leaving_detached_work() {
    let (root, _) = fixture();
    let before = Instant::now();
    let text = transcript_in(&root, "sh -c 'sleep 10; touch /root/late' &\nexit 0\n");
    assert!(text.ends_with("Bye.\n"));
    assert!(before.elapsed() < Duration::from_secs(2));
    assert!(!root.root().join("root/late").exists());
}
#[test]
fn jobs_cannot_wait_on_themselves_and_authentication_never_uses_worker_stdin() {
    let (root, _) = fixture();
    let text = transcript_in(&root, "wait %1 &\nwait %1\nsu root &\necho alive\nexit 0\n");
    assert!(text.contains("a job cannot wait on itself"), "{text}");
    assert!(
        text.contains("session controls require a standalone command"),
        "{text}"
    );
    assert!(text.contains("alive\nBye.\n"));
}
#[test]
fn total_job_records_are_bounded_and_reaping_does_not_reuse_old_ids() {
    let (root, user) = fixture();
    std::thread::scope(|scope| {
        let jobs = JobTable::new(scope, user.uid, 1);
        let resources =
            Resources::new(user.uid, Arc::new(Logger::open(&root).unwrap()), 16).unwrap();
        let modules = SharedModules::new(&root, &user, &resources);
        let program = parser::parse("true").unwrap();
        let first = jobs
            .spawn(
                &root,
                &user,
                Spawn {
                    context: Context::new(VirtualPath::root()),
                    program: program.clone(),
                    label: "true".into(),
                    budget: Arc::new(Budget::new()),
                },
                &modules,
            )
            .unwrap();
        assert!(jobs
            .spawn(
                &root,
                &user,
                Spawn {
                    context: Context::new(VirtualPath::root()),
                    program: program.clone(),
                    label: "true".into(),
                    budget: Arc::new(Budget::new())
                },
                &modules
            )
            .is_err());
        jobs.wait(user.uid, first, None, None).unwrap();
        let second = jobs
            .spawn(
                &root,
                &user,
                Spawn {
                    context: Context::new(VirtualPath::root()),
                    program,
                    label: "true".into(),
                    budget: Arc::new(Budget::new()),
                },
                &modules,
            )
            .unwrap();
        assert!(second.number() > first.number());
        assert!(jobs.wait(user.uid, first, None, None).is_err());
        jobs.wait(user.uid, second, None, None).unwrap();
        assert!(jobs.snapshot(user.uid).unwrap().is_empty());
        jobs.shutdown().unwrap();
    });
}
#[test]
fn asynchronous_descendants_cannot_reset_the_shared_invocation_budget() {
    let (root, user) = fixture();
    std::thread::scope(|scope| {
        let jobs = JobTable::new(scope, user.uid, 2);
        let resources =
            Resources::new(user.uid, Arc::new(Logger::open(&root).unwrap()), 16).unwrap();
        let modules = SharedModules::new(&root, &user, &resources);
        let budget = Arc::new(Budget::new());
        let first = jobs
            .spawn(
                &root,
                &user,
                Spawn {
                    context: Context::new(VirtualPath::root()),
                    program: parser::parse("while true; do true; done").unwrap(),
                    label: "loop".into(),
                    budget: budget.clone(),
                },
                &modules,
            )
            .unwrap();
        let Waited::Finished { report, .. } = jobs.wait(user.uid, first, None, None).unwrap()
        else {
            panic!("expected bounded completion");
        };
        assert_eq!(report.steps, 4096);
        let second = jobs
            .spawn(
                &root,
                &user,
                Spawn {
                    context: Context::new(VirtualPath::root()),
                    program: parser::parse("touch /root/forbidden").unwrap(),
                    label: "touch".into(),
                    budget,
                },
                &modules,
            )
            .unwrap();
        let Waited::Finished { report, .. } = jobs.wait(user.uid, second, None, None).unwrap()
        else {
            panic!("expected budget refusal");
        };
        assert_eq!(report.steps, 0);
        assert_eq!(report.status, 2);
        assert!(!root.root().join("root/forbidden").exists());
        jobs.shutdown().unwrap();
    });
}
#[test]
fn nested_background_jobs_share_real_descriptors_and_inherit_empty_stdin() {
    let (root, _) = fixture();
    let text = transcript_in(&root,"fd open /root/job-shared create\nsh -c 'printf child | fd write 0:1; sh -c \"wc -c; touch /root/grandchild\" &; wait %2' &\nwait %1\nfd seek 0:1 0 start\nfd read 0:1 5\necho END\nexit 0\n");
    assert!(text.contains("0\n[2] exited 0\n"), "{text}");
    assert!(text.contains("childEND\n"), "{text}");
    assert!(root.root().join("root/grandchild").is_file());
    assert_eq!(
        std::fs::read(root.root().join("root/job-shared")).unwrap(),
        b"child"
    );
}
#[test]
fn cancellation_crosses_nested_sh_source_and_conditionals_without_running_traps() {
    let (root, user) = fixture();
    std::thread::scope(|scope| {
        let jobs = JobTable::new(scope, user.uid, 1);
        let resources =
            Resources::new(user.uid, Arc::new(Logger::open(&root).unwrap()), 16).unwrap();
        let modules = SharedModules::new(&root, &user, &resources);
        let id=jobs.spawn(&root,&user,Spawn { context:Context::new(VirtualPath::root()),program:parser::parse("trap 'touch /root/cancel-trap' EXIT; source -c 'sh -c \"touch /tmp/cancel-ready; sleep 10 || touch /root/cancel-or\"'; touch /root/cancel-tail").unwrap(),label:"nested".into(),budget:Arc::new(Budget::new()) },&modules).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !root.root().join("tmp/cancel-ready").exists() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let entered = root.root().join("tmp/cancel-ready").exists();
        if !entered {
            let state = jobs.snapshot(user.uid).unwrap();
            jobs.cancel(user.uid, id).unwrap();
            let state: Vec<_> = state
                .iter()
                .map(|job| (job.id.number(), job.state))
                .collect();
            let Waited::Finished { report, cancelled } =
                jobs.wait(user.uid, id, None, None).unwrap()
            else {
                panic!("nested job panicked before readiness");
            };
            panic!("nested interpreter must really reach its module effect; state {state:?}; cancelled {cancelled}; report {report:?}");
        }
        assert!(
            entered,
            "nested interpreter must really reach its module effect"
        );
        // Actual ACK proves every worker is stopped, not just pause-requested.
        assert_eq!(
            jobs.pause(user.uid, id).unwrap(),
            elara_os::script::jobs::PauseOutcome::Stopped
        );
        jobs.cancel(user.uid, id).unwrap();
        let Waited::Finished { report, cancelled } = jobs.wait(user.uid, id, None, None).unwrap()
        else {
            panic!("job panicked");
        };
        assert!(cancelled);
        assert_eq!(report.status, 130);
        assert!(!String::from_utf8_lossy(&report.stderr).contains("status 2"));
        for path in ["root/cancel-trap", "root/cancel-or", "root/cancel-tail"] {
            assert!(!root.root().join(path).exists());
        }
        jobs.shutdown().unwrap();
    });
}
/// An absent TaskExit name cannot prove that a command was never executed.
/// Exercise the real interpreter and reaper, not a synthetic host-file fixture.
#[test]
fn side_effect_witnesses_survive_program_and_job_cleanup() {
    for background in [false, true] {
        let (root, user) = fixture();
        let program = "trap 'touch /tmp/cancel-trap; touch /root/cancel-trap' EXIT; source -c 'sh -c \"touch /tmp/cancel-ready; false || touch /root/cancel-or\"'; touch /tmp/cancel-tail; touch /root/cancel-tail; mkdir /tmp/witness-dir; mkdir /root/witness-dir";
        if background {
            let text = transcript_in(
                &root,
                &std::format!(
                    "sh -c '{0}' &\nwait %1\nexit 0\n",
                    program.replace('\'', "'\\''")
                ),
            );
            assert!(text.contains("[1] exited 0"), "{text}");
        } else {
            let report = crate::support::run(
                &root,
                &user,
                &mut Context::new(VirtualPath::root()),
                &["-c".into(), program.into()],
            )
            .unwrap();
            assert_eq!(report.status, 0, "{report:?}");
        }
        for path in [
            "tmp/cancel-trap",
            "tmp/cancel-ready",
            "tmp/cancel-tail",
            "tmp/witness-dir",
        ] {
            assert!(
                !root.root().join(path).exists(),
                "temporary witness must be reclaimed: {path}"
            );
        }
        for path in ["/root/cancel-trap", "/root/cancel-or", "/root/cancel-tail"] {
            assert!(
                root.root().join(&path[1..]).is_file(),
                "executed side effect must remain visible: {path}"
            );
            let elara_os::filesystem::OwnershipState::Managed(info) =
                root.ownership_as(&user, path).unwrap()
            else {
                panic!("persistent witness must be managed");
            };
            assert_eq!(
                info.delete_condition,
                elara_os::filesystem::DeleteCondition::OwnerRequest
            );
        }
        let directory = root
            .directory_info_as(&user, "/root/witness-dir")
            .unwrap()
            .unwrap();
        assert_eq!(
            directory.delete_condition,
            elara_os::filesystem::DeleteCondition::OwnerRequest
        );
    }
}

#[test]
fn full_job_table_refuses_before_module_fork_and_foreground_commands_still_run() {
    struct CountFork(std::sync::atomic::AtomicUsize);
    impl ModuleCommands for CountFork {
        fn execute(&mut self, _: Request<'_>) -> Result<Dispatch, String> {
            Ok(Dispatch::Unsupported)
        }
        fn fork<'a>(
            &self,
            _: &'a elara_os::filesystem::DataRoot,
            _: &elara_os::users::Account,
            _: elara_os::script::control::Worker,
        ) -> Result<elara_os::script::modules::ModuleFork<'a>, String> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(elara_os::script::modules::ModuleFork::Unsupported)
        }
    }
    let (root, user) = fixture();
    std::thread::scope(|scope| {
        let jobs = JobTable::new(scope, user.uid, 0);
        let modules = CountFork(std::sync::atomic::AtomicUsize::new(0));
        assert!(jobs
            .spawn(
                &root,
                &user,
                Spawn {
                    context: Context::new(VirtualPath::root()),
                    program: parser::parse("true").unwrap(),
                    label: "true".into(),
                    budget: Arc::new(Budget::new())
                },
                &modules
            )
            .unwrap_err()
            .contains("job table full"));
        assert_eq!(modules.0.load(std::sync::atomic::Ordering::SeqCst), 0);
        jobs.shutdown().unwrap();
    });
    let source = std::format!("{}\necho foreground-alive\nexit 0\n", "true &\n".repeat(17));
    let text = transcript_in(&root, &source);
    assert!(text.contains("job table full"), "{text}");
    assert!(text.contains("foreground-alive\nBye.\n"), "{text}");
}

#[test]
fn module_fork_refusal_does_not_publish_a_fake_job() {
    struct NoFork;
    impl ModuleCommands for NoFork {
        fn execute(&mut self, _: Request<'_>) -> Result<Dispatch, String> {
            Ok(Dispatch::Unsupported)
        }
    }
    let (root, user) = fixture();
    std::thread::scope(|scope| {
        let jobs = JobTable::new(scope, user.uid, 1);
        assert!(jobs
            .spawn(
                &root,
                &user,
                Spawn {
                    context: Context::new(VirtualPath::root()),
                    program: parser::parse("true").unwrap(),
                    label: "true".into(),
                    budget: Arc::new(Budget::new())
                },
                &NoFork
            )
            .is_err());
        assert!(jobs.snapshot(user.uid).unwrap().is_empty());
        assert!(jobs.snapshot(user.uid + 1).is_err());
        assert!(jobs.complete_owner_switch(user.uid + 1).is_err());
        assert!(JobId::parse("1").is_err());
        assert!(JobId::parse("%0").is_err());
        jobs.shutdown().unwrap();
    });
}

#[test]
fn proc_jobs_commands_show_acknowledged_stop_and_reap_without_exposing_labels() {
    let (root, user) = fixture();
    let text = transcript_in(&root,
        "sh -c 'sleep 30; echo private-job-body' &\nkill -STOP %1\ncat /proc/jobs/*/status\nkill -TERM %1\nwait %1\nprintf after-reap\n\nls /proc/jobs\nexit 0\n");
    assert!(text.contains("state: Stopped\n"), "{text}");
    assert!(text.contains("job_id: 1\n"), "{text}");
    assert!(text.contains("kind: scoped-job\n"), "{text}");
    assert!(text.contains("[1] cancelled 130\n"), "{text}");
    assert!(root.list_as(&user, "/proc/jobs").unwrap().is_empty());
    assert!(!root.root().join("proc/jobs").exists());
}

#[test]
fn proc_legacy_timers_publish_terminal_state_without_ps_or_wait_refresh() {
    let (root, user) = fixture();
    let text = transcript_in(&root,
        "sleep 0 bg\nsleep 0\ncat /proc/jobs/*/status\nexit 0\n");
    assert!(text.contains("kind: legacy-task\n"), "{text}");
    assert!(text.contains("task_id: 1\n"), "{text}");
    assert!(text.contains("state: TaskExited(0)\n"), "{text}");
    assert!(!text.contains("Error:"), "{text}");
    assert!(root.list_as(&user, "/proc/jobs").unwrap().is_empty());
    assert!(!root.root().join("proc/jobs").exists());
}

#[test]
fn proc_legacy_identity_switch_retires_old_tasks_without_reusing_ids() {
    let (root, _) = fixture();
    let text = transcript_in(&root,
        "useradd timeruser secret1
sleep 30 bg
su timeruser
printf BEFORE
ps
wait 1
sleep 0
printf AFTER
ps
exit 0
");
    let before = text.split("BEFORE").nth(1).unwrap().split("AFTER").next().unwrap();
    assert!(!before.contains("sleep 30"), "{text}");
    assert!(before.contains("No task with id 1"), "{text}");
    assert!(text.contains("[2] exited 0"), "{text}");
}

#[test]
fn proc_executions_tracks_real_foreground_invocation_and_not_background_or_private_text() {
    let (root, user) = fixture();
    let report = crate::support::run(&root, &user, &mut Context::new(VirtualPath::root()),
        &["-c".into(), "sh -c 'cat /proc/executions/*/status' private-argument".into()]).unwrap();
    let text = String::from_utf8(report.stdout).unwrap();
    assert_eq!(report.status, 0, "{}", String::from_utf8_lossy(&report.stderr));
    assert!(text.contains("state: Running
kind: foreground-execution
"), "{text}");
    assert_eq!(text.matches("execution_id:").count(), 1, "{text}");
    assert!(!text.contains("private-argument"), "{text}");
    assert!(root.list_as(&user, "/proc/executions").unwrap().is_empty());
    assert!(!root.root().join("proc/executions").exists());
}

#[test]
fn proc_executions_exec_streams_errors_and_scoped_workers_keep_their_boundaries() {
    let (root, user) = fixture();
    root.create_owned_file(&user, "/root/execution-image", b"cat /proc/executions/*/status; exit 7",
        elara_os::filesystem::VirtualMode::new(0o700).unwrap()).unwrap();
    let text = transcript_in(&root, "sh -c 'exec /root/execution-image'\necho AFTER_EXEC\nfalse\nunknown-command\ncat /proc/executions/*/status | wc -l\nsh -c 'ls /proc/executions; echo BG_DONE' &\nkill -STOP %1\nkill -CONT %1\nwait %1\nexit 0\n");
    assert!(text.contains("kind: foreground-execution\n"), "{text}");
    assert!(text.contains("Script exited with status 7."), "{text}");
    assert!(text.contains("AFTER_EXEC"), "{text}");
    assert!(text.contains("Unknown script command: unknown-command"), "{text}");
    assert!(text.contains("4\n"), "{text}");
    assert!(text.contains("BG_DONE"), "{text}");
    assert!(root.list_as(&user, "/proc/executions").unwrap().is_empty());
    assert!(root.list_as(&user, "/proc/jobs").unwrap().is_empty());
}
