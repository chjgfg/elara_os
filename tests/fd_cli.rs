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
