//! Actual process-level acceptance; fixtures never touch the user's configured data root.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{assert, assert_eq, env, format, fs, println, vec};

fn run(configuration: &Path, input: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_elara-host"))
        .arg(configuration)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn actual_shell_runs_files_checks_syntax_and_retains_source_context() {
    let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture = project
        .join("target/test-data")
        .join(format!("script-cli-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&fixture).unwrap();
    let data = fixture.join("data");
    let config = fixture.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let startup = run(&config, "\nexit\n");
    assert!(startup.contains("Bye."));
    fs::copy(
        project.join("docs/examples/shell-demo.sh"),
        data.join("tmp/demo.sh"),
    )
    .unwrap();
    fs::write(data.join("tmp/check.sh"), b"touch /tmp/should-not-exist\n").unwrap();
    fs::write(
        data.join("tmp/bad.sh"),
        b"touch /tmp/also-not-exist; if true; then echo bad\n",
    )
    .unwrap();
    fs::write(data.join("tmp/context.sh"), b"X=retained; cd /tmp\n").unwrap();
    let text = run(&config, "\nsh /tmp/demo.sh world\nsh -n /tmp/check.sh\nsh /tmp/bad.sh\nsource /tmp/context.sh\npwd\nsh -c 'echo \"$X\"; cd /'\npwd\nsh -c 'nonexistent_command'\necho shell-still-alive\nexit\n");
    println!("=== ACTUAL SCRIPT CLI OUTPUT ===\n{text}");
    assert!(text.contains("星澜脚本\nhello Alice\nhello Bob\narg=world\n3\n"));
    assert!(text.contains("/tmp\nretained\n/tmp\n"));
    assert!(text.contains("Script exited with status 127."));
    assert!(text.contains("shell-still-alive\nBye.\n"));
    assert!(!data.join("tmp/should-not-exist").exists());
    assert!(!data.join("tmp/also-not-exist").exists());
    assert!(!text.contains("Unknown command: sh"));
    fs::write(
        data.join("tmp/child.sh"),
        b"printf '%s\\n' \"$1\"; sh -c 'printf nested'; source -c 'X=sourced; return 0'\n",
    )
    .unwrap();
    let nested = run(&config, "\nX=parent\nsh /tmp/child.sh argument | wc -c\necho \"$X\"\nsource /tmp/child.sh argument\necho \"$X\"\nexit 0\n");
    println!("=== ACTUAL NESTED SCRIPT OUTPUT ===\n{nested}");
    assert!(
        nested.contains("15\nparent\nargument\nnestedsourced\nBye.\n"),
        "{nested}"
    );
    let expanded = run(&config, "\nn=4\necho $((n + 2 * 3))\nX=$(printf captured)\necho \"${X}:${Y:-default}:${#X}\"\nprintf abc | printf '[%s]\\n' \"$(cat)\"\nexit 0\n");
    println!("=== ACTUAL EXPANSION OUTPUT ===\n{expanded}");
    assert!(
        expanded.contains("10\ncaptured:default:8\n[abc]\nBye.\n"),
        "{expanded}"
    );
    let core = run(&config, "\nmkdir /tmp/glob-demo\ntouch /tmp/glob-demo/a.txt /tmp/glob-demo/b.txt\ncd /tmp/glob-demo\necho *.txt\nf() { local X=local; echo \"$X\"; }\nX=global\nf\necho \"$X\"\nIFS=:\nX=a:b\nprintf '[%s]\\n' $X\nunset IFS\ncase abc in a*) echo case-ok ;; *) false ;; esac\ntrap 'echo trap:$?' EXIT\nexit 0\n");
    println!("=== ACTUAL SCRIPT CORE OUTPUT ===\n{core}");
    assert!(
        core.contains("a.txt b.txt\nlocal\nglobal\n[a]\n[b]\ncase-ok\ntrap:0\nBye.\n"),
        "{core}"
    );
    fs::write(data.join("tmp/stream-large"), vec![b'x'; 2 * 1024 * 1024]).unwrap();
    let flow = run(&config,"\ncat /tmp/stream-large | wc -c\nsh -c 'cd /tmp; echo background; exit 3' &\njobs\nwait %1\necho $?\nsleep 1 &\nkill -STOP %2\njobs\nbg %2\nfg %2\nsh -c 'sleep 10; touch /tmp/forbidden-cancel' &\nkill -TERM %3\nwait %3\necho $?\nexit 0\n");
    println!("=== ACTUAL STREAM/JOB OUTPUT ===\n{flow}");
    assert!(flow.contains("2097152\n"), "{flow}");
    assert!(flow.contains("background\n[1] exited 3\n"), "{flow}");
    assert!(flow.contains("[2] stopped\n"), "{flow}");
    assert!(flow.contains("[2] resume requested\n"), "{flow}");
    assert!(flow.contains("[3] cancelled 130\n"), "{flow}");
    assert!(flow.ends_with("130\nBye.\n"), "{flow}");
    assert!(!data.join("tmp/forbidden-cancel").exists());
    // The same executable accepts prompt syntax directly and returns exit N.
    let mut child = Command::new(env!("CARGO_BIN_EXE_elara-host"))
        .arg(&config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"\nX=direct\nprintf '%s' \"$X\" > /tmp/direct\ncat < /tmp/direct | wc -c\nfalse || echo recovered\nexit 7\necho forbidden_after_exit\n").unwrap();
    let output = child.wait_with_output().unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    println!("=== ACTUAL UNIFIED PROMPT OUTPUT ===\n{text}");
    assert_eq!(output.status.code(), Some(7));
    assert!(text.contains("6\nrecovered\nBye.\n"), "{text}");
    assert!(!text.contains("forbidden_after_exit"));
    assert_eq!(fs::read(data.join("tmp/direct")).unwrap(), b"direct");
}
