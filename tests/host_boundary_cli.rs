//! Actual startup checks; all configurations and sentinels belong to target/test-data.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{assert, assert_eq, env, format, fs};

fn fixture() -> PathBuf {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-data")
        .join(format!("host-boundary-cli-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&path).unwrap(); path.canonicalize().unwrap()
}
fn run(config: &Path, text: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_elara-host")).arg(config)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    // Root-password setup consumes one line on every password-less startup.
    let mut input = child.stdin.take().unwrap(); input.write_all(&[10]).unwrap(); input.write_all(text).unwrap(); drop(input);
    child.wait_with_output().unwrap()
}
#[test]
fn actual_startup_refuses_ambiguous_root_before_any_root_creation() {
    let path = fixture(); let config = path.join("bad.env");
    fs::write(path.join("sentinel"), b"untouched").unwrap();
    fs::write(&config, format!("ELARA_DATA_ROOT={}/./data{}", path.display(), char::from(10))).unwrap();
    let output = run(&config, b"");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Startup refused:"));
    assert!(!path.join("data").exists());
    assert_eq!(fs::read(path.join("sentinel")).unwrap(), b"untouched");
    assert_eq!(fs::read_dir(&path).unwrap().count(), 2);
}
#[test]
fn actual_commands_and_task_recovery_preserve_siblings_and_unknown_tmp_data() {
    let path = fixture(); let data = path.join("data"); let config = path.join("root.env");
    elara_os::initialization::prepare(&data).unwrap();
    fs::write(path.join("sentinel"), b"outside data root").unwrap();
    fs::write(data.join("tmp/foreign"), b"unregistered").unwrap();
    fs::write(&config, format!("ELARA_DATA_ROOT={}{}", data.display(), char::from(10))).unwrap();
    let before_config = fs::read(&config).unwrap();
    let script = ["touch /root/persistent", "touch /tmp/leased", "exit", ""].join(&char::from(10).to_string());
    let output = run(&config, script.as_bytes());
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("Bye."));
    assert!(data.join("root/persistent").is_file()); assert!(!data.join("tmp/leased").exists());
    assert_eq!(fs::read(data.join("tmp/foreign")).unwrap(), b"unregistered");
    assert_eq!(fs::read(path.join("sentinel")).unwrap(), b"outside data root");
    assert_eq!(fs::read(config).unwrap(), before_config);
}

#[test]
fn runtime_state_real_process_exit_and_crash_recovery_preserve_unknown_data() {
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc;
    use std::time::Duration;
    let path=fixture();let data=path.join("data");let config=path.join("runtime.env");
    elara_os::initialization::prepare(&data).unwrap();
    fs::write(data.join("run/unknown"),b"unregistered state").unwrap();
    fs::write(path.join("sentinel"),b"outside").unwrap();
    fs::write(&config,format!("ELARA_DATA_ROOT={}{}",data.display(),char::from(10))).unwrap();
    let configuration=fs::read(&config).unwrap();
    let graceful=["mkdir /run/session","printf state > /run/session/file","cat /run/session/file","owned stat /run/session/file","exit", ""].join(&char::from(10).to_string());
    let result=run(&config,graceful.as_bytes());let out=String::from_utf8_lossy(&result.stdout);
    assert!(result.status.success(),"{result:?}");assert!(out.contains("RuntimeState"),"{out}");assert!(out.contains("TaskExit"),"{out}");
    assert!(!data.join("run/session").exists());
    let mut child=Command::new(env!("CARGO_BIN_EXE_elara-host")).arg(&config).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let output=child.stdout.take().unwrap();let (tx,rx)=mpsc::channel();
    let reader=std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            let Ok(line)=line else {break;};
            if line.contains("RUNTIME_CRASH_READY") {let _=tx.send(());}
        }
    });
    let commands=["", "mkdir /run/crashed", "printf durable > /run/crashed/file", "echo RUNTIME_CRASH_READY", ""].join(&char::from(10).to_string());
    let sent=child.stdin.as_mut().unwrap().write_all(commands.as_bytes());
    let ready=if sent.is_ok() {rx.recv_timeout(Duration::from_secs(30)).is_ok()} else {false};
    // Kill only the test-owned child; no host account, service, or unrelated process.
    let killed=child.kill();let waited=child.wait();reader.join().unwrap();
    assert!(ready,"child did not finish registered writes: {sent:?}");assert!(killed.is_ok(),"{killed:?}");assert!(waited.is_ok(),"{waited:?}");
    assert_eq!(fs::read(data.join("run/crashed/file")).unwrap(),b"durable");
    let restarted=run(&config,b"exit
");assert!(restarted.status.success(),"{restarted:?}");
    assert!(!data.join("run/crashed").exists());
    assert_eq!(fs::read(data.join("run/unknown")).unwrap(),b"unregistered state");
    assert_eq!(fs::read(path.join("sentinel")).unwrap(),b"outside");assert_eq!(fs::read(&config).unwrap(),configuration);
}
