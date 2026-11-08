//! Real-process managed-file ownership; no configured user root is touched.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{assert, assert_eq, env, format, fs, println};
fn run(config: &Path, input: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_elara-host"))
        .arg(config).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "status: {}\nstdout:\n{}\nstderr:\n{}", output.status,
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap()
}
#[test]
fn actual_process_records_owners_denies_other_accounts_and_keeps_identity_after_restart() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-data")
        .join(format!("ownership-cli-{}-{nonce}", std::process::id()));
    assert!(!fixture.exists()); fs::create_dir_all(&fixture).unwrap();
    let data = fixture.join("data"); let config = fixture.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let output = run(&config, "\nuseradd ownalice own-cli-password\nuseradd ownbob own-cli-password\nsu ownalice own-cli-password\nowned create /tmp/private owner_private_text\nowned stat /tmp/private\nowned append /tmp/private _tail\nowned write /tmp/private replacement_private_text\nowned mv /tmp/private /tmp/moved\nowned stat /tmp/moved\nsu ownbob own-cli-password\nowned read /tmp/moved\nowned write /tmp/moved attack\nowned rm /tmp/moved\nowned chmod 666 /tmp/moved\nexit 0\n");
    assert!(output.contains("uid: 1000\ngid: 1000\nmode: 600\n"), "{output}");
    assert!(output.contains("Permission denied by the object's virtual owner/mode."), "{output}");
    assert!(output.contains("path: /tmp/moved"));
    assert!(!output.contains("replacement_private_text\n"));
    assert!(!data.join("tmp/private").exists());
    assert_eq!(fs::read(data.join("tmp/moved")).unwrap(), b"replacement_private_text");
    let restart = run(&config, "\nsu ownalice own-cli-password\nowned stat /tmp/moved\nowned read /tmp/moved\nowned chmod 644 /tmp/moved\nsu ownbob own-cli-password\nowned read /tmp/moved\nowned append /tmp/moved attack\nexit 0\n");
    assert!(restart.contains("uid: 1000\ngid: 1000\nmode: 600\n"), "{restart}");
    assert!(restart.contains("replacement_private_text"));
    assert!(restart.contains("Permission denied by the object's virtual owner/mode."));
    assert_eq!(fs::read(data.join("tmp/moved")).unwrap(), b"replacement_private_text");
    let log = fs::read_to_string(data.join("var/log/elara.log")).unwrap();
    assert!(!log.contains("owner_private_text"));
    assert!(!log.contains("replacement_private_text"));
    assert!(!log.contains("own-cli-password"));
    assert!(!data.join("var/.elara-objects/pending").exists());
    println!("=== ACTUAL OWNERSHIP CLI ===\n{output}\n=== RESTART ===\n{restart}");
}
