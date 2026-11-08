//! Real-process filesystem command acceptance, confined to a fresh test root.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{assert, assert_eq, env, format, fs, println};

fn process(configuration: &Path, text: &str) -> String {
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
        .write_all(text.as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}

#[test]
fn real_process_copies_moves_diagnoses_and_refuses_overwrite_and_escape() {
    let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture = project
        .join("target/test-data")
        .join(format!("filesystem-cli-{}-{nonce}", std::process::id()));
    assert!(!fixture.exists());
    fs::create_dir_all(&fixture).unwrap();
    let data = fixture.join("data");
    let config = fixture.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let output = process(&config,
        "\ncd /tmp\nsh -c 'printf abc > \"source file\"'\ncp \"source file\" copy\nmv copy moved\nstat moved\nfind moved\ndu moved\ncp \"source file\" moved\nmv \"source file\" moved\ncp moved /usr/refused\ncp moved C:/outside\nsh -c 'cp moved scriptcopy; mv scriptcopy scriptmoved; cat scriptmoved'\nexit\n");
    println!("=== ACTUAL FILESYSTEM COMMAND OUTPUT ===\n{output}");
    assert!(
        output.contains("path: /tmp/moved\nkind: File\nlength: 3\n"),
        "{output}"
    );
    assert!(output.contains("logical_file_bytes: 3\nfiles: 1\n"));
    assert!(output.contains("physical_usage: UnavailableOnHostBackend"));
    assert!(output.contains("The path already exists."));
    assert!(output.contains("System directory is read-only"));
    assert!(output.contains("Host path syntax and control characters are refused."));
    assert!(output.contains("Bye."));
    assert_eq!(fs::read(data.join("tmp/source file")).unwrap(), b"abc");
    assert_eq!(fs::read(data.join("tmp/moved")).unwrap(), b"abc");
    assert_eq!(fs::read(data.join("tmp/scriptmoved")).unwrap(), b"abc");
    assert!(!data.join("tmp/copy").exists());
    assert!(!data.join("usr/refused").exists());
}
