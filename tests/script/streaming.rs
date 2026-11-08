use crate::support::{execute, fixture, run};
use elara_os::script::{
    runtime::{ExecutionMode, MAX_OUTPUT},
    streaming::{
        self, BufferedReason, Builtin, Completion, QueueSnapshot, Stage, StageOutcome, Strategy,
        WorkClass,
    },
    Context,
};
use elara_os::virtual_path::VirtualPath;
use std::{assert, assert_eq, fs, matches, panic, vec};

#[test]
fn real_large_file_pipeline_streams_past_the_old_materialization_limit() {
    let (root, user) = fixture();
    let bytes = vec![b'x'; 2 * 1024 * 1024];
    fs::write(root.root().join("tmp/large"), &bytes).unwrap();
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), "cat /tmp/large | wc -c".into()],
    )
    .unwrap();
    assert_eq!(report.stdout, b"2097152\n");
    assert_eq!(report.status, 0);
    assert!(matches!(
        report.mode,
        ExecutionMode::InProcessStreaming { .. }
    ));
    let first = &report.pipelines[0];
    assert_eq!(
        first.stage_statuses,
        vec![StageOutcome::Exited(0), StageOutcome::Exited(0)]
    );
    assert!(
        matches!(first.queues[0],QueueSnapshot::Known(stats) if stats.enqueued == bytes.len() as u64 && stats.read == bytes.len() as u64 && stats.peak_queued_bytes <= 32768)
    );
}
#[test]
fn printf_streaming_does_not_materialize_the_entire_intermediate_output() {
    let source = std::format!(
        "printf '{}%s' {} | wc -c",
        "x".repeat(1024),
        "a ".repeat(128)
    );
    let report = execute(&source);
    assert_eq!(report.stdout, b"131200\n");
    assert_eq!(report.status, 0);
    assert!(matches!(
        report.mode,
        ExecutionMode::InProcessStreaming { .. }
    ));
}
#[test]
fn a_consumer_that_exits_early_closes_upstream_without_deadlock_or_false_success() {
    let source = std::format!(
        "printf '{}%s' {} | true; echo alive",
        "x".repeat(1024),
        "a ".repeat(128)
    );
    let report = execute(&source);
    assert_eq!(report.stdout, b"alive\n");
    assert_eq!(report.status, 0);
    assert_eq!(
        report.pipelines[0].stage_statuses,
        vec![StageOutcome::Exited(141), StageOutcome::Exited(0)]
    );
}
#[test]
fn module_progress_and_atomic_redirection_use_typed_buffered_fallbacks() {
    let (root, user) = fixture();
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &[
            "-c".into(),
            "fd open /tmp/shared create; printf hi | fd write 0:1; printf copy | cat > /tmp/output"
                .into(),
        ],
    )
    .unwrap();
    assert_eq!(fs::read(root.root().join("tmp/shared")).unwrap(), b"hi");
    assert_eq!(fs::read(root.root().join("tmp/output")).unwrap(), b"copy");
    assert!(matches!(
        report.pipelines[0].strategy,
        Strategy::Buffered {
            reason: BufferedReason::InterpreterCommand,
            byte_limit: MAX_OUTPUT
        }
    ));
    assert!(matches!(
        report.pipelines[1].strategy,
        Strategy::Buffered {
            reason: BufferedReason::AtomicOutputRedirect,
            byte_limit: MAX_OUTPUT
        }
    ));
}
#[test]
fn collector_failure_still_returns_real_input_consumption_and_partial_stage_statuses() {
    let (root, user) = fixture();
    let stage = || Stage {
        builtin: Builtin::Cat,
        args: Vec::new(),
        cwd: VirtualPath::root(),
        input_file: None,
    };
    let result = streaming::execute(
        vec![stage(), stage()],
        &root,
        &user,
        &vec![b'x'; 10000],
        None,
        64,
        streaming::reserve(2, WorkClass::Foreground).unwrap(),
    )
    .unwrap();
    let Completion::Failed { error, partial } = result else {
        panic!("collector must fail rather than silently truncate");
    };
    assert!(error.to_string().contains("output limit exceeded"));
    assert!(partial.consumed > 0 && partial.consumed <= 10000);
    assert_eq!(partial.statuses.len(), 2);
}
#[test]
fn failed_streams_do_not_replay_already_consumed_parent_stdin() {
    let (root, user) = fixture();
    fs::write(root.root().join("tmp/input"), vec![b'x'; 60000]).unwrap();
    let source = std::format!("H='{}'; f() {{ printf '%s' {}; cat | cat; printf '\\nremaining='; wc -c; }}; cat /tmp/input | f", "h".repeat(4000), "\"$H\"".repeat(15));
    let report = run(
        &root,
        &user,
        &mut Context::new(VirtualPath::root()),
        &["-c".into(), source],
    )
    .unwrap();
    let stream = report
        .pipelines
        .iter()
        .find(|trace| matches!(trace.strategy, Strategy::Streaming { .. }))
        .unwrap();
    assert!(stream.inherited_input_consumed > 0 && stream.inherited_input_consumed <= 60000);
    let remaining = 60000 - stream.inherited_input_consumed;
    assert!(report
        .stdout
        .ends_with(std::format!("remaining={remaining}\n").as_bytes()));
    assert!(String::from_utf8_lossy(&report.stderr).contains("output limit exceeded"));
    assert!(matches!(report.mode, ExecutionMode::InProcessMixed { .. }));
}
#[test]
fn worker_quota_fallback_is_observable_and_release_reenables_streaming() {
    // The worker pool is process-global. Exhaust it in a separate test process
    // rather than racing unrelated streaming tests in cargo test's default mode.
    const ISOLATED: &str = "ELARA_ISOLATED_WORKER_QUOTA_TEST";
    if std::env::var_os(ISOLATED).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "stream_tests::worker_quota_fallback_is_observable_and_release_reenables_streaming",
                "--nocapture",
            ])
            .env(ISOLATED, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let held = streaming::reserve(streaming::MAX_WORKERS, WorkClass::Foreground).unwrap();
    let report = execute("printf hi | wc -c");
    assert_eq!(report.stdout, b"2\n");
    assert!(matches!(
        report.pipelines[0].strategy,
        Strategy::Buffered {
            reason: BufferedReason::WorkerQuota,
            ..
        }
    ));
    drop(held);
    let report = execute("printf hi | wc -c");
    assert!(matches!(
        report.pipelines[0].strategy,
        Strategy::Streaming { .. }
    ));
}
