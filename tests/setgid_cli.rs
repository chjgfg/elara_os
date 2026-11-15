//! Actual commands prove directory setgid affects metadata, not host identity.
use elara_os::filesystem::{DataRoot, OwnershipState};
use elara_os::users::Account;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{assert, assert_eq, env, format, fs};
#[test]
fn actual_setgid_commands_inherit_group_directory_bit_and_clear_only_future_births() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let parent = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-data").join(format!("setgid-cli-{}-{nonce}",std::process::id()));
    fs::create_dir_all(&parent).unwrap(); let parent = parent.canonicalize().unwrap();
    let data = parent.join("data"); let config = parent.join("root.env");
    elara_os::initialization::prepare(&data).unwrap();
    fs::write(&config,format!("ELARA_DATA_ROOT={}{}",data.display(),char::from(10))).unwrap();
    let before = fs::read(&config).unwrap(); let host_before = fs::metadata(&data).unwrap().permissions();
    fs::write(parent.join("sentinel"),b"outside").unwrap(); fs::write(data.join("tmp/foreign"),b"unknown").unwrap();
    let lines = ["", "groupadd 5000 team", "mkdir /root/team", "chgrp 5000 /root/team", "chmod 2770 /root/team", "setfacl -d u::7,g::7,o::0 /root/team", "umask 777", "printf inherited > /root/team/f", "mkdir /root/team/sub", "setfacl -b /root/team/sub", "getfacl /root/team/sub", "chmod 1600 /root/team/f", "chmod 770 /root/team", "printf primary > /root/team/new", "getfacl /root/team/new", "exit", ""];
    let mut child = Command::new(env!("CARGO_BIN_EXE_elara-host")).arg(&config).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(lines.join(&char::from(10).to_string()).as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap(); let text = String::from_utf8_lossy(&output.stdout); let errors = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(),"status {:?}, stdout {text}, stderr {errors}",output.status.code());
    assert!(text.contains("mode: 2770"),"{text}"); assert!(text.contains("gid: 5000"),"{text}");
    assert!(text.contains("Sticky permission requires a managed directory") || errors.contains("Sticky permission requires a managed directory"),"stdout {text}, stderr {errors}");
    let root = DataRoot::open(&data).unwrap(); let admin = Account { identity: Default::default(), name:"root".into(),uid:0,home:"/root".into() };
    let OwnershipState::Managed(old) = root.owned_file_info("/root/team/f").unwrap() else { panic!("not managed") };
    let OwnershipState::Managed(new) = root.owned_file_info("/root/team/new").unwrap() else { panic!("not managed") };
    assert_eq!(old.owner.gid,5000); assert_eq!(old.mode.bits(),0o660); assert_eq!(new.owner.gid,0); assert_eq!(new.mode.bits(),0o660);
    let sub = root.directory_info_as(&admin,"/root/team/sub").unwrap().unwrap(); assert_eq!(sub.owner.gid,5000); assert_eq!(sub.mode.bits(),0o2770);
    assert_eq!(fs::read(data.join("root/team/f")).unwrap(),b"inherited"); assert_eq!(fs::read(data.join("root/team/new")).unwrap(),b"primary");
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(),b"outside"); assert_eq!(fs::read(data.join("tmp/foreign")).unwrap(),b"unknown");
    assert_eq!(fs::read(&config).unwrap(),before); assert_eq!(fs::metadata(&data).unwrap().permissions(),host_before);
}
