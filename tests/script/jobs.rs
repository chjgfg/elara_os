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
    let text = transcript_in(&root,"X=parent\nsh -c 'X=child; cd /tmp; touch job-file; echo \"$X\"; pwd; exit 7' &\necho \"$X\"\npwd\njobs\nwait %1\necho $?\nexit 0\n");
    assert!(text.contains("started job: sh -c"), "{text}");
    assert!(text.contains("parent\n/\n"), "{text}");
    assert!(text.contains("child\n/tmp\n[1] exited 7\n"), "{text}");
    assert!(text.ends_with("7\nBye.\n"), "{text}");
    assert!(root.root().join("tmp/job-file").is_file());
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
        "sh -c 'sleep 10; touch /tmp/after-cancel' &\nkill -TERM %1\nwait %1\necho $?\nexit 0\n",
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
    assert!(!root.root().join("tmp/after-cancel").exists());
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
    let text = transcript_in(&root, "sh -c 'sleep 10; touch /tmp/late' &\nexit 0\n");
    assert!(text.ends_with("Bye.\n"));
    assert!(before.elapsed() < Duration::from_secs(2));
    assert!(!root.root().join("tmp/late").exists());
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
                    program: parser::parse("touch /tmp/forbidden").unwrap(),
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
        assert!(!root.root().join("tmp/forbidden").exists());
        jobs.shutdown().unwrap();
    });
}
#[test]
fn nested_background_jobs_share_real_descriptors_and_inherit_empty_stdin() {
    let (root, _) = fixture();
    let text = transcript_in(&root,"fd open /tmp/job-shared create\nsh -c 'printf child | fd write 0:1; sh -c \"wc -c; touch /tmp/grandchild\" &; wait %2' &\nwait %1\nfd seek 0:1 0 start\nfd read 0:1 5\necho END\nexit 0\n");
    assert!(text.contains("0\n[2] exited 0\n"), "{text}");
    assert!(text.contains("childEND\n"), "{text}");
    assert!(root.root().join("tmp/grandchild").is_file());
    assert_eq!(
        std::fs::read(root.root().join("tmp/job-shared")).unwrap(),
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
        let id=jobs.spawn(&root,&user,Spawn { context:Context::new(VirtualPath::root()),program:parser::parse("trap 'touch /tmp/cancel-trap' EXIT; source -c 'sh -c \"touch /tmp/cancel-ready; sleep 10 || touch /tmp/cancel-or\"'; touch /tmp/cancel-tail").unwrap(),label:"nested".into(),budget:Arc::new(Budget::new()) },&modules).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !root.root().join("tmp/cancel-ready").exists() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let entered = root.root().join("tmp/cancel-ready").exists();
        if !entered {
            jobs.cancel(user.uid, id).unwrap();
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
        for path in ["tmp/cancel-trap", "tmp/cancel-or", "tmp/cancel-tail"] {
            assert!(!root.root().join(path).exists());
        }
        jobs.shutdown().unwrap();
    });
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
