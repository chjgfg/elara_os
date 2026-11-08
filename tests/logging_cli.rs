//! Actual competing elara-host processes share the managed log protocol.
use elara_os::filesystem::{DataRoot, LogPolicy};
use elara_os::logging::Logger;
use elara_os::users;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{assert, assert_eq, env, format, fs, println};

#[test]
fn actual_processes_rotate_and_record_without_losing_each_others_events() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(format!("logging-cli-{}-{nonce}", std::process::id()));
    assert!(!fixture.exists());
    fs::create_dir_all(&fixture).unwrap();
    let data = fixture.join("data");
    elara_os::initialization::prepare(&data).unwrap();
    let root = DataRoot::open(&data).unwrap();
    users::ensure_passwd(&root).unwrap();
    let account = users::load(&root)
        .unwrap()
        .into_iter()
        .find(|user| user.is_admin())
        .unwrap();
    users::set_password(&root, &account, "root", "logging-cli-initial-password").unwrap();
    for worker in 0..2 {
        users::add_user(
            &root,
            &account,
            &format!("logwriter{worker}"),
            "logging-cli-user-password",
        )
        .unwrap();
    }
    let logger = Logger::open(&root).unwrap();
    let mut seed = b"x".repeat(LogPolicy::default().segment_bytes() - 16);
    seed.push(b'\n');
    root.append_log(LogPolicy::default(), &seed).unwrap();
    let configuration = fixture.join("fixture.env");
    fs::write(
        &configuration,
        format!("ELARA_DATA_ROOT={}\n", data.display()),
    )
    .unwrap();
    let mut children = Vec::new();
    for _ in 0..2 {
        children.push(
            Command::new(env!("CARGO_BIN_EXE_elara-host"))
                .arg(&configuration)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    for (worker, child) in children.iter_mut().enumerate() {
        let mut text = format!("su logwriter{worker} logging-cli-user-password\n");
        for command in ["whoami", "pwd", "ps", "users"] {
            text.push_str(command);
            text.push('\n');
        }
        text.push_str(&format!("echo visible-worker-{worker}\n"));
        text.push_str("exit\n");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    }
    for (worker, child) in children.into_iter().enumerate() {
        let result = child.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let text = String::from_utf8(result.stdout).unwrap();
        assert!(text.contains("Bye."), "{text}");
        assert!(text.contains(&format!("visible-worker-{worker}")), "{text}");
        assert!(text.contains(&format!("logwriter{worker}")), "{text}");
        assert!(!text.contains("log write failed"), "{text}");
    }
    let tail = logger.tail(&root, 100).unwrap();
    for worker in 0..2 {
        for command in ["whoami", "pwd", "ps", "users", "echo"] {
            let expected = format!(" logwriter{worker} command: {command}");
            assert_eq!(
                tail.iter().filter(|line| line.ends_with(&expected)).count(),
                1,
                "{expected}"
            );
        }
    }
    assert!(
        !tail
            .iter()
            .any(|line| line.contains("logging-cli-user-password")
                || line.contains("visible-worker-"))
    );
    let stats = logger.stats(&root).unwrap();
    assert!(stats.archives >= 1);
    assert!(stats.archives <= LogPolicy::default().archives());
    assert!(
        stats.retained_body_bytes
            <= LogPolicy::default().segment_bytes() * (LogPolicy::default().archives() + 1)
    );
    assert!(!fs::read_to_string(data.join("var/log/elara.log"))
        .unwrap()
        .contains("logging-cli-initial-password"));
    println!("actual-process log stats: {stats:?}; unique command events: 10");
}
