//! Actual fd command regression with tokens read from a live elara-host process.
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{assert, assert_eq, assert_ne, env, format, fs, println};

const MARKER: &str = "__fd_cli_end__";
struct Console {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    trace: String,
}
impl Console {
    fn start(config: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_elara-host"))
            .arg(config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut console = Self {
            child,
            input: Some(input),
            output,
            trace: String::new(),
        };
        console.input.as_mut().unwrap().write_all(b"\n").unwrap();
        console.input.as_mut().unwrap().flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(console.output.read_line(&mut line).unwrap() != 0);
            console.trace.push_str(&line);
            if line.contains("Type 'help' for commands.") {
                break;
            }
        }
        console
    }
    fn command(&mut self, text: &str) -> String {
        let input = self.input.as_mut().unwrap();
        input
            .write_all(format!("{text}\necho {MARKER}\n").as_bytes())
            .unwrap();
        input.flush().unwrap();
        let mut output = String::new();
        loop {
            let mut line = String::new();
            assert!(
                self.output.read_line(&mut line).unwrap() != 0,
                "process ended before marker"
            );
            self.trace.push_str(&line);
            if let Some(at) = line.find(MARKER) {
                output.push_str(&line[..at]);
                break;
            }
            output.push_str(&line);
        }
        output
    }
    fn finish(&mut self) {
        let mut input = self.input.take().unwrap();
        input.write_all(b"exit\n").unwrap();
        drop(input);
        let mut tail = String::new();
        self.output.read_to_string(&mut tail).unwrap();
        self.trace.push_str(&tail);
        assert!(self.child.wait().unwrap().success());
    }
}
impl Drop for Console {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
fn handle(output: String) -> String {
    let text = output.trim();
    let (slot, generation) = text.split_once(':').expect("SLOT:GEN");
    assert!(slot.bytes().all(|b| b.is_ascii_digit()));
    assert!(generation.bytes().all(|b| b.is_ascii_digit()));
    assert!(!slot.is_empty() && !generation.is_empty());
    text.into()
}

#[test]
fn real_fd_commands_share_handles_and_user_switch_closes_root_capabilities() {
    let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture = project
        .join("target/test-data")
        .join(format!("fd-cli-{}-{nonce}", std::process::id()));
    assert!(!fixture.exists());
    fs::create_dir_all(&fixture).unwrap();
    let data = fixture.join("data");
    let config = fixture.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let mut console = Console::start(&config);
    let private = handle(console.command("fd open /root/private create"));
    assert_eq!(
        console.command(&format!("fd write {private} root_private")),
        "12\n"
    );
    let shared = handle(console.command("fd open /tmp/shared create"));
    assert_eq!(
        console.command(&format!("sh -c 'printf abc | fd write {shared}'")),
        "3\n"
    );
    let duplicate = handle(console.command(&format!("fd dup {shared}")));
    assert_eq!(console.command(&format!("fd seek {shared} 0 start")), "0\n");
    assert_eq!(console.command(&format!("fd read {shared} 1")), "a");
    assert_eq!(console.command(&format!("fd read {duplicate} 2")), "bc");
    assert!(console
        .command(&format!("source -c 'fd stat {shared}'"))
        .contains("position: 3\n"));
    console.command(&format!("fd close {shared}"));
    let replacement = handle(console.command("fd open /tmp/shared read"));
    assert_eq!(shared.split(':').next(), replacement.split(':').next());
    assert_ne!(shared, replacement);
    assert!(console
        .command(&format!("fd read {shared} 1"))
        .contains("stale"));
    console.command("useradd fdcli explicit_password");
    assert!(console
        .command("su fdcli")
        .contains("Closed 3 file handles on identity change."));
    assert!(console
        .command(&format!("fd read {private} 1"))
        .contains("stale"));
    let guest = handle(console.command("fd open /tmp/guest create"));
    assert_eq!(private.split(':').next(), guest.split(':').next());
    assert_ne!(private, guest);
    assert!(console
        .command(&format!("fd read {private} 1"))
        .contains("stale"));
    assert!(console
        .command("fd open /root/private read")
        .contains("Permission denied"));
    assert_eq!(console.command("fd closeall"), "1\n");
    console.finish();
    assert_eq!(
        fs::read(data.join("root/private")).unwrap(),
        b"root_private"
    );
    let log = fs::read_to_string(data.join("var/log/elara.log")).unwrap();
    assert!(!log.contains("root_private"));
    assert!(!log.contains("explicit_password"));
    assert!(log.contains("fd owner uid 0 ->"));
    println!("=== ACTUAL FD CLI TRACE ===\n{}", console.trace);
}

#[test]
fn real_fd_ownership_modes_generations_and_legacy_guards_survive_multiple_sessions() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-data")
        .join(format!("fd-owned-cli-{}-{nonce}", std::process::id()));
    assert!(!fixture.exists());
    fs::create_dir_all(&fixture).unwrap();
    let data = fixture.join("data");
    let config = fixture.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let mut admin = Console::start(&config);
    admin.command("useradd fdalice fd-owner-password");
    admin.command("useradd fdbob fd-owner-password");
    let mut alice = Console::start(&config);
    let mut bob = Console::start(&config);
    alice.command("su fdalice fd-owner-password");
    bob.command("su fdbob fd-owner-password");
    let original = handle(alice.command("fd open /tmp/fd-owned create"));
    assert_eq!(alice.command(&format!("fd write {original} abcdef")), "6\n");
    let before = alice.command(&format!("fd stat {original}"));
    assert!(before.contains("uid: 1000\ngid: 1000\nmode: 600\n"), "{before}");
    assert!(bob.command("fd open /tmp/fd-owned read").contains("Permission denied by the object's"));
    assert_eq!(alice.command("cat /tmp/fd-owned"), "abcdef");
    assert_eq!(alice.command("wc -c /tmp/fd-owned"), "6
");
    assert_eq!(alice.command("cat /tmp/fd-owned | wc -c"), "6
");
    assert_eq!(alice.command("wc -c < /tmp/fd-owned"), "6
");
    for command in ["cat /tmp/fd-owned", "wc -c /tmp/fd-owned", "cat /tmp/fd-owned | wc -c", "wc -c < /tmp/fd-owned", "stat /tmp/fd-owned", "find /tmp/fd-owned", "du /tmp/fd-owned"] {
        assert!(bob.command(command).contains("Permission denied by the object's"), "{command}");
    }
    alice.command("cp /tmp/fd-owned /tmp/fd-copy");
    assert_eq!(alice.command("cat /tmp/fd-copy"), "abcdef");
    assert!(bob.command("cat /tmp/fd-copy").contains("Permission denied by the object's"));
    alice.command("mv /tmp/fd-copy /tmp/fd-copy-moved");
    assert_eq!(alice.command("cat /tmp/fd-copy-moved"), "abcdef");
    alice.command("owned rm /tmp/fd-copy-moved");
    alice.command("mkdir /tmp/fd-tree");
    alice.command("cp /tmp/fd-owned /tmp/fd-tree/value");
    assert!(!alice.command("mv /tmp/fd-tree /tmp/fd-tree-moved").contains("Error:"));
    assert_eq!(alice.command("cat /tmp/fd-tree-moved/value"), "abcdef");
    assert!(!data.join("tmp/fd-tree").exists());
    admin.command("owned chmod 666 /tmp/fd-owned");
    let grant = handle(bob.command("fd open /tmp/fd-owned rw"));
    admin.command("owned chmod 600 /tmp/fd-owned");
    assert!(bob.command("fd open /tmp/fd-owned read").contains("Permission denied by the object's"));
    assert_eq!(bob.command(&format!("fd writeat {grant} 0 XY")), "2\n");
    assert_eq!(alice.command(&format!("fd readat {original} 0 8")), "XYcdef");
    alice.command("owned mv /tmp/fd-owned /tmp/fd-moved");
    let after_move = alice.command(&format!("fd stat {original}"));
    assert!(after_move.contains("path: /tmp/fd-owned\n"));
    let field = |text: &str, key: &str| text.lines().find_map(|line| line.strip_prefix(key)).unwrap().to_owned();
    assert_eq!(field(&before, "object: "), field(&after_move, "object: "));
    alice.command("owned write /tmp/fd-moved new_bytes");
    let fresh = handle(alice.command("fd open /tmp/fd-moved read"));
    let after_replace = alice.command(&format!("fd stat {fresh}"));
    assert_eq!(field(&before, "object: "), field(&after_replace, "object: "));
    assert_eq!(field(&after_replace, "generation: "), "2");
    assert_eq!(alice.command(&format!("fd writeat {original} 0 OLD")), "3\n");
    assert_eq!(alice.command(&format!("fd readat {original} 0 16")), "OLDdef");
    assert_eq!(alice.command(&format!("fd readat {fresh} 0 16")), "new_bytes");
    assert_eq!(fs::read(data.join("tmp/fd-moved")).unwrap(), b"new_bytes");
    fs::write(data.join("tmp/fd-legacy"), b"legacy_bytes").unwrap();
    assert!(alice.command("fd open /tmp/fd-legacy truncate").contains("automatic adoption was refused"));
    let legacy = handle(alice.command("fd open /tmp/fd-legacy read"));
    assert!(alice.command(&format!("fd stat {legacy}")).contains("ownership: LegacyUnknown"));
    assert_eq!(alice.command(&format!("fd read {legacy} 16")), "legacy_bytes");
    fs::hard_link(data.join("tmp/fd-moved"), data.join("tmp/fd-alias")).unwrap();
    assert!(bob.command("fd open /tmp/fd-alias read").contains("cannot be confirmed"));
    alice.command("owned rm /tmp/fd-moved");
    assert!(!data.join("tmp/fd-moved").exists());
    assert_eq!(alice.command(&format!("fd readat {fresh} 0 16")), "new_bytes");
    alice.finish(); bob.finish(); admin.finish();
    let root = elara_os::filesystem::DataRoot::open(&data).unwrap();
    assert_eq!(root.owned_file_info("/tmp/fd-legacy").unwrap(), elara_os::filesystem::OwnershipState::LegacyUnknown);
    let log = fs::read_to_string(data.join("var/log/elara.log")).unwrap();
    assert!(!log.contains("fd-owner-password"));
    assert!(!log.contains("new_bytes"));
    println!("=== OWNED FD MULTI-SESSION CLI ===\n{}\n{}\n{}", admin.trace, alice.trace, bob.trace);
}

fn access_fixture() -> (PathBuf, PathBuf) {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-data").join(format!("access-cli-{}-{nonce}", std::process::id()));
    assert!(!fixture.exists()); fs::create_dir_all(&fixture).unwrap();
    let data = fixture.join("data"); let config = fixture.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap(); (data, config)
}
#[test]
fn real_directory_umask_group_acl_and_scope_boundaries() {
    let (data, config) = access_fixture(); let mut admin = Console::start(&config);
    assert!(!admin.command("useradd aclalice access-password").contains("Error:"));
    assert!(!admin.command("useradd aclbob access-password").contains("Error:"));
    assert!(!admin.command("groupadd 5000 team").contains("Error:"));
    assert!(!admin.command("groupmem add 5000 1000; groupmem add 5000 1001; groupmem primary 5000 1000").contains("Error:"));
    let mut alice = Console::start(&config); let mut bob = Console::start(&config);
    alice.command("su aclalice access-password"); bob.command("su aclbob access-password");
    assert_eq!(alice.command("umask"), "0077\n");
    assert!(!alice.command("umask 0007; mkdir /tmp/team; touch /tmp/team/f; printf abc > /tmp/team/f").contains("Error:"));
    assert!(alice.command("getfacl /tmp/team").contains("mode: 770\n"));
    assert!(alice.command("getfacl /tmp/team/f").contains("gid: 5000\nmode: 660\n"));
    assert_eq!(bob.command("cat /tmp/team/f"), "abc");
    assert!(!bob.command("printf Z >> /tmp/team/f").contains("Error:"));
    let grant = handle(bob.command("fd open /tmp/team/f read"));
    assert!(!alice.command("setfacl u:1001:0 /tmp/team/f").contains("Error:"));
    assert!(bob.command("cat /tmp/team/f").contains("Permission denied"));
    assert_eq!(bob.command(&format!("fd read {grant} 8")), "abcZ");
    assert!(!alice.command("setfacl u:1001:6,m::4 /tmp/team/f").contains("Error:"));
    assert!(bob.command("printf bad >> /tmp/team/f").contains("Permission denied"));
    assert_eq!(bob.command("cat /tmp/team/f"), "abcZ");
    assert!(!alice.command("chmod 700 /tmp/team").contains("Error:"));
    for command in ["cat /tmp/team/f", "ls /tmp/team", "cd /tmp/team", "find /tmp/team", "du /tmp/team", "echo /tmp/team/*"] {
        assert!(bob.command(command).contains("Permission denied"), "{command}");
    }
    assert!(!alice.command("mv /tmp/team /tmp/moved-team").contains("Error:"));
    assert_eq!(alice.command("cat /tmp/moved-team/f"), "abcZ");
    assert!(!alice.command("umask 0022; sh -c 'umask 0000; touch /home/aclalice/child'; touch /home/aclalice/parent").contains("Error:"));
    assert_eq!(alice.command("umask"), "0022\n");
    assert!(alice.command("getfacl /home/aclalice/child").contains("mode: 666\n"));
    assert!(alice.command("getfacl /home/aclalice/parent").contains("mode: 644\n"));
    assert!(!alice.command("source -c 'umask 0002'; touch /home/aclalice/source").contains("Error:"));
    assert_eq!(alice.command("umask"), "0002\n");
    assert!(alice.command("getfacl /home/aclalice/source").contains("mode: 664\n"));
    assert!(!alice.command("touch /tmp/identity-temp").contains("Error:"));
    assert!(!alice.command("su aclbob access-password").contains("Error:"));
    assert_eq!(alice.command("umask"), "0077\n"); assert!(!data.join("tmp/identity-temp").exists());
    assert!(!data.join("tmp/moved-team").exists());
    alice.finish(); bob.finish(); admin.finish();
    assert_eq!(fs::read(data.join("home/aclalice/child")).unwrap(), b"");
    println!("=== ACCESS CLI ===\n{}\n{}\n{}", admin.trace, alice.trace, bob.trace);
}
#[test]
fn real_process_death_recovery_keeps_live_tasks_and_unknown_tmp_contents() {
    let (data, config) = access_fixture(); let mut live = Console::start(&config);
    let created = live.command("mkdir /tmp/lifetime; printf ephemeral > /tmp/lifetime/f; printf persistent > /root/persistent");
    assert!(!created.contains("Error:"), "{created}");
    fs::write(data.join("tmp/unknown"), b"not ours").unwrap();
    let mut observer = Console::start(&config);
    assert_eq!(observer.command("cat /tmp/lifetime/f"), "ephemeral");
    observer.finish(); assert!(data.join("tmp/lifetime/f").exists());
    // Abrupt host termination: no EXIT trap, Context drop, or scope Drop runs.
    live.child.kill().unwrap(); live.child.wait().unwrap(); live.input.take();
    assert!(data.join("tmp/lifetime/f").exists());
    let mut restart = Console::start(&config);
    assert_eq!(restart.command("cat /root/persistent"), "persistent");
    assert!(!data.join("tmp/lifetime").exists()); assert_eq!(fs::read(data.join("tmp/unknown")).unwrap(), b"not ours");
    let root = elara_os::filesystem::DataRoot::open(&data).unwrap(); assert_eq!(root.recover_tasks().unwrap().deferred, 0);
    restart.finish();
    let bytes = fs::read_to_string(data.join("var/.elara-objects/access.catalog")).unwrap();
    let body = bytes.strip_prefix("ELARA_ACCESS_CATALOG_V1\n").unwrap();
    let value: serde_json::Value = serde_json::from_str(body).unwrap(); assert!(value["tasks"].as_object().unwrap().is_empty());
    println!("=== PROCESS DEATH RECOVERY ===\n{}\n{}", live.trace, restart.trace);
}
#[test]
fn real_job_completion_cancellation_and_fd_cleanup_leave_parent_grants_live() {
    use std::time::{Duration, Instant};
    let (data, config) = access_fixture(); let mut console = Console::start(&config);
    let parent = handle(console.command("fd open /root/parent create"));
    console.command(&format!("fd write {parent} parent"));
    console.command("sh -c 'touch /tmp/job-end; fd open /tmp/job-fd create; fd dup 0:1; touch /root/job-persistent' &");
    assert!(console.command("wait %1").contains("exited 0"));
    assert!(!data.join("tmp/job-end").exists()); assert!(!data.join("tmp/job-fd").exists()); assert!(data.join("root/job-persistent").exists());
    assert!(console.command("fd stat 1:1").contains("stale")); assert!(console.command("fd stat 2:1").contains("stale"));
    assert_eq!(console.command(&format!("fd readat {parent} 0 8")), "parent");
    console.command("sh -c 'touch /tmp/job-ready; sleep 30; touch /root/after-cancel' &");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !data.join("tmp/job-ready").exists() && Instant::now() < deadline { std::thread::sleep(Duration::from_millis(10)); }
    assert!(data.join("tmp/job-ready").exists());
    // A Host name is published before its ownership catalog commits. Wait for
    // readiness through the same namespace gate as a real reader, not an
    // ambient exists() probe that may still observe an in-flight creation.
    let owner_store = data.join("var/.elara-objects");
    println!("READY_BEFORE_NAMESPACE_SYNC owner_pending={} access_pending={}",
        owner_store.join("pending").exists(), owner_store.join("access.pending").exists());
    assert_eq!(console.command("cat /tmp/job-ready"), "");
    assert!(!owner_store.join("pending").exists());
    assert!(!owner_store.join("access.pending").exists());
    let before = Instant::now();
    assert!(console.command("kill -TERM %2; wait %2").contains("cancelled 130"));
    let elapsed = before.elapsed();
    println!("CANCEL_AND_CLEANUP_ELAPSED={elapsed:?}");
    assert!(elapsed < Duration::from_secs(2), "cancel and cleanup took {elapsed:?}");
    assert!(!data.join("tmp/job-ready").exists()); assert!(!data.join("root/after-cancel").exists());
    assert_eq!(console.command(&format!("fd readat {parent} 0 8")), "parent");
    console.finish(); println!("=== JOB LIFECYCLE CLI ===\n{}", console.trace);
}
