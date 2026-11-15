//! Real-process virtual sticky naming, preserving existing data and host authority.
use elara_os::filesystem::{CreationOptions, DataRoot, VirtualMode};
use elara_os::users::{self, Account};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{assert, assert_eq, env, format, fs};
fn run(config: &Path, script: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_elara-host")).arg(config)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(format!("\n{script}\nexit 0\n").as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    let out = String::from_utf8(output.stdout).unwrap(); let err = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "status {:?}, stdout {out}, stderr {err}", output.status.code());
    format!("{out}\n{err}")
}
#[test]
fn actual_process_sticky_denies_strangers_clear_bit_changes_only_names_and_parent_owner_can_remove() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-data")
        .join(format!("sticky-cli-{}-{nonce}", std::process::id()));
    assert!(!parent.exists()); fs::create_dir_all(&parent).unwrap(); let parent = parent.canonicalize().unwrap();
    let data = parent.join("data"); let config = parent.join("root.env");
    elara_os::initialization::prepare(&data).unwrap();
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let config_before = fs::read(&config).unwrap(); fs::write(parent.join("sentinel"), b"outside").unwrap();
    fs::write(data.join("tmp/unknown"), b"unknown").unwrap();
    let root = DataRoot::open(&data).unwrap(); users::ensure_passwd(&root).unwrap();
    let admin = Account { identity: Default::default(), name: "root".into(), uid: 0, home: "/root".into() };
    let alice = users::add_user(&root, &admin, "stickyalice", "sticky-password").unwrap();
    let bob = users::add_user(&root, &admin, "stickybob", "sticky-password").unwrap();
    users::add_user(&root, &admin, "stickystranger", "sticky-password").unwrap();
    root.create_directory_as(&alice, "/tmp/shared", CreationOptions::default()).unwrap();
    root.set_access_as(&alice, "/tmp/shared", Some(VirtualMode::new(0o1777).unwrap()), None, None).unwrap();
    let host_before = fs::metadata(data.join("tmp/shared")).unwrap().permissions();
    for name in ["kept", "parent-delete"] {
        root.create_owned_file(&bob, &format!("/tmp/shared/{name}"), b"private", VirtualMode::new(0).unwrap()).unwrap();
    }
    root.create_directory_as(&bob, "/tmp/shared/sub", CreationOptions::default()).unwrap();
    let deny = run(&config, "su stickystranger sticky-password\nsh -c 'if rm /tmp/shared/kept; then printf WRONG; else printf \"<sticky-denied>\"; fi'\nmv /tmp/shared/kept /tmp/shared/moved\nrmdir /tmp/shared/sub\ncat /tmp/shared/kept");
    assert!(deny.contains("<sticky-denied>"), "{deny}"); assert!(!deny.contains("WRONG"), "{deny}");
    assert!(deny.contains("Permission denied by the object's virtual owner/mode."), "{deny}");
    assert_eq!(fs::read(data.join("tmp/shared/kept")).unwrap(), b"private");
    assert!(data.join("tmp/shared/sub").exists()); assert!(!data.join("tmp/shared/moved").exists());
    let allow = run(&config, "chmod 777 /tmp/shared\nsu stickystranger sticky-password\nsh -c 'mv /tmp/shared/kept /tmp/shared/moved && mv /tmp/shared/sub /tmp/shared/sub-moved && rmdir /tmp/shared/sub-moved && printf \"<nonsticky-ok>\"'\ncat /tmp/shared/moved");
    assert!(allow.contains("<nonsticky-ok>"), "{allow}");
    assert!(allow.contains("Permission denied by the object's virtual owner/mode."), "{allow}");
    assert_eq!(fs::read(data.join("tmp/shared/moved")).unwrap(), b"private");
    assert!(!data.join("tmp/shared/sub").exists()); assert!(!data.join("tmp/shared/sub-moved").exists());
    let owner = run(&config, "chmod 1777 /tmp/shared\nsu stickyalice sticky-password\nsh -c 'rm /tmp/shared/parent-delete && printf \"<parent-owner-ok>\"'\ngetfacl /tmp/shared");
    assert!(owner.contains("<parent-owner-ok>"), "{owner}"); assert!(owner.contains("mode: 1777"), "{owner}");
    assert!(!data.join("tmp/shared/parent-delete").exists());
    assert_eq!(fs::metadata(data.join("tmp/shared")).unwrap().permissions(), host_before);
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside");
    assert_eq!(fs::read(data.join("tmp/unknown")).unwrap(), b"unknown");
    assert_eq!(fs::read(&config).unwrap(), config_before);
    let log = fs::read_to_string(data.join("var/log/elara.log")).unwrap();
    assert!(!log.contains("sticky-password")); assert!(!log.contains("private"));
}
