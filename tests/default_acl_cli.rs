//! Literal default ACL commands in the actual elara-host process.
use elara_os::filesystem::{DataRoot, OwnershipState};
use elara_os::users::Account;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{assert, assert_eq, env, format, fs};
fn fixture() -> (PathBuf, PathBuf, PathBuf) {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-data").join(format!("default-acl-cli-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&parent).unwrap(); let parent = parent.canonicalize().unwrap();
    let data = parent.join("data"); let config = parent.join("root.env");
    elara_os::initialization::prepare(&data).unwrap();
    fs::write(&config, format!("ELARA_DATA_ROOT={}{}", data.display(), char::from(10))).unwrap();
    fs::write(parent.join("sentinel"), b"outside unchanged").unwrap(); fs::write(data.join("tmp/foreign"), b"unknown").unwrap();
    (parent, data, config)
}
fn run(config: &Path, lines: &[&str]) -> Output {
    let text = lines.join(&char::from(10).to_string());
    let mut child = Command::new(env!("CARGO_BIN_EXE_elara-host")).arg(config).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    // This fixture deliberately has no root password: skip its setup prompt
    // before sending the first filesystem command, rather than losing that command.
    let mut input = child.stdin.take().unwrap(); input.write_all(&[10]).unwrap(); input.write_all(text.as_bytes()).unwrap(); drop(input);
    child.wait_with_output().unwrap()
}
fn root_user() -> Account { Account { identity: Default::default(), name: "root".into(), uid: 0, home: "/root".into() } }
fn file_mode(root: &DataRoot, path: &str) -> u16 {
    let OwnershipState::Managed(info) = root.owned_file_info(path).unwrap() else { panic!("not managed") }; info.mode.bits()
}
#[test]
fn actual_default_commands_inherit_clip_clear_and_preserve_external_data() {
    let (parent, data, config) = fixture(); let before = fs::read(&config).unwrap();
    let output = run(&config, &["mkdir /root/team", "chmod 770 /root/team", "setfacl -d u::7,g::0,o::0,u:1001:7,m::4 /root/team", "umask 077", "printf inherited > /root/team/file", "getfacl /root/team/file", "mkdir /root/team/sub", "getfacl /root/team/sub", "setfacl -b /root/team/sub", "getfacl /root/team/sub", "setfacl -k /root/team", "touch /root/team/private", "getfacl /root/team/private", "exit", ""]);
    let text = String::from_utf8_lossy(&output.stdout); assert!(output.status.success(), "status {:?}, stdout: {text}, stderr: {}", output.status.code(), String::from_utf8_lossy(&output.stderr));
    assert!(text.contains("mode: 640"), "{text}"); assert!(text.contains("u:1001:7")); assert!(text.contains("m::4")); assert!(text.contains("default:u:1001:7")); assert!(text.contains("default:m::4")); assert!(text.contains("mode: 600"));
    let root = DataRoot::open(&data).unwrap(); let user = root_user();
    assert_eq!(file_mode(&root, "/root/team/file"), 0o640); assert_eq!(file_mode(&root, "/root/team/private"), 0o600);
    assert_eq!(root.default_acl_as(&user, "/root/team").unwrap(), None);
    let default = root.default_acl_as(&user, "/root/team/sub").unwrap().unwrap(); assert_eq!(default.acl.users.get(&1001), Some(&7)); assert_eq!(default.acl.mask, Some(4));
    assert_eq!(root.directory_info_as(&user, "/root/team/sub").unwrap().unwrap().mode.bits(), 0o700);
    assert_eq!(fs::read(data.join("root/team/file")).unwrap(), b"inherited"); assert_eq!(fs::read(data.join("tmp/foreign")).unwrap(), b"unknown");
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside unchanged"); assert_eq!(fs::read(&config).unwrap(), before);
}
#[test]
fn actual_default_parser_recalculates_mask_rejects_bad_bits_and_b_retains_group() {
    let (_, data, config) = fixture();
    let output = run(&config, &["mkdir /root/team", "setfacl -d u::7,g::0,o::0,u:1001:4 /root/team", "setfacl -d g:5000:2 /root/team", "getfacl /root/team", "setfacl -d u:1001:8 /root/team", "touch /root/team/file", "setfacl g::1,u:1001:7,m::4 /root/team/file", "setfacl -b /root/team/file", "getfacl /root/team/file", "exit", ""]);
    let text = String::from_utf8_lossy(&output.stdout); let errors = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "status {:?}, stdout: {text}, stderr: {errors}", output.status.code());
    assert!(text.contains("default:m::6"), "{text}"); assert!(text.contains("default:g:5000:2"));
    assert!(text.contains("ACL bits must be 0..7") || errors.contains("ACL bits must be 0..7"));
    let root = DataRoot::open(&data).unwrap(); let user = root_user(); let default = root.default_acl_as(&user, "/root/team").unwrap().unwrap();
    assert_eq!(default.acl.users.get(&1001), Some(&4)); assert_eq!(default.acl.groups.get(&5000), Some(&2)); assert_eq!(default.acl.mask, Some(6));
    assert_eq!(file_mode(&root, "/root/team/file"), 0o610); assert_eq!(root.acl_as(&user, "/root/team/file").unwrap(), elara_os::filesystem::Acl::default());
}
