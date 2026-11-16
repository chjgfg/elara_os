//! Real binary, explicit fresh configuration, never the user's .env/data root.
use elara_os::filesystem::{DataRoot, VirtualMode};
use elara_os::users::{self, Account};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{assert, assert_eq, env, format, fs};

fn run(config: &Path, script: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_elara-host"))
        .arg(config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("\n{script}\n").as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}
#[test]
fn virtual_execution_real_process_preserves_host_boundary_and_replaces_only_virtual_session() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(format!("execution-cli-{}-{nonce}", std::process::id()));
    assert!(!parent.exists());
    fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data");
    let config = parent.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let before_config = fs::read(&config).unwrap();
    fs::write(parent.join("sentinel"), b"outside").unwrap();
    elara_os::initialization::prepare(&data).unwrap();
    let root = DataRoot::open(&data).unwrap();
    users::ensure_passwd(&root).unwrap();
    let user = Account {
        identity: Default::default(),
        name: "root".into(),
        uid: 0,
        home: "/root".into(),
    };
    root.create_owned_file(
        &user,
        "/root/image",
        b"cat; printf '%s' \"$1\"; touch /tmp/transient; exit 7",
        VirtualMode::new(0o700).unwrap(),
    )
    .unwrap();
    root.create_owned_file(
        &user,
        "/root/input",
        b"payload",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    let host_mode = fs::metadata(data.join("root/image")).unwrap().permissions();
    let out = run(&config, "trap 'touch /root/old-trap' EXIT\nexec /root/image private_exec_argument < /root/input > /root/output\ntouch /root/tail\nexit 0");
    assert_eq!(
        out.status.code(),
        Some(7),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read(data.join("root/output")).unwrap(),
        b"payloadprivate_exec_argument"
    );
    for path in ["root/old-trap", "root/tail", "tmp/transient"] {
        assert!(!data.join(path).exists());
    }
    let denied = run(
        &config,
        "exec C:/Windows/not-an-elara-image\necho survived\nexit 0",
    );
    assert!(denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stdout).contains("survived"));
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside");
    assert_eq!(fs::read(&config).unwrap(), before_config);
    assert_eq!(
        fs::metadata(data.join("root/image")).unwrap().permissions(),
        host_mode
    );
    assert!(!fs::read_to_string(data.join("var/log/elara.log"))
        .unwrap()
        .contains("private_exec_argument"));
}

#[test]
fn virtual_execution_cli_applies_setuid_without_touching_host_accounts() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(format!("privilege-cli-{}-{nonce}", std::process::id()));
    assert!(!parent.exists());
    fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data");
    let config = parent.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let before_config = fs::read(&config).unwrap();
    fs::write(parent.join("sentinel"), b"outside").unwrap();
    elara_os::initialization::prepare(&data).unwrap();
    let root = DataRoot::open(&data).unwrap();
    users::ensure_passwd(&root).unwrap();
    let admin = Account {
        identity: Default::default(),
        name: "root".into(),
        uid: 0,
        home: "/root".into(),
    };
    let guest = users::add_user(&root, &admin, "cliguest", "cli-password").unwrap();
    root.create_owned_file(
        &admin,
        "/tmp/suid",
        b"id; whoami; touch /root/cli-elevated; exit 7",
        VirtualMode::new(0o4755).unwrap(),
    )
    .unwrap();
    let host_mode = fs::metadata(data.join("tmp/suid")).unwrap().permissions();
    let passwd = fs::read(data.join("etc/passwd")).unwrap();
    let output = run(&config, "su cliguest cli-password\nexec /tmp/suid\n");
    let text = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(7),
        "stdout={text} stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains(&format!("ruid={} euid=0 suid=0", guest.uid)),
        "{text}"
    );
    assert!(text.contains("root\n"), "{text}");
    assert!(data.join("root/cli-elevated").is_file());
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside");
    assert_eq!(fs::read(&config).unwrap(), before_config);
    assert!(!text.contains("cli-password"));
    assert_eq!(
        fs::metadata(data.join("tmp/suid")).unwrap().permissions(),
        host_mode
    );
    assert_eq!(fs::read(data.join("etc/passwd")).unwrap(), passwd);
}

#[test]
fn vfs_real_process_checkpoint_restart_and_pseudo_devices_preserve_host_boundary() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(format!("vfs-cli-{}-{nonce}", std::process::id()));
    assert!(!parent.exists());
    fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data");
    let config = parent.join("fixture.env");
    fs::write(
        &config,
        format!(
            "ELARA_DATA_ROOT={}
",
            data.display()
        ),
    )
    .unwrap();
    let configuration = fs::read(&config).unwrap();
    fs::write(parent.join("sentinel"), b"outside").unwrap();
    let first = run(
        &config,
        "vfs mount /mnt/work 65536 64
mkdir /mnt/work/dir
printf checkpoint > /mnt/work/dir/file
vfs link /mnt/work/dir/file /mnt/work/alias
vfs symlink dir/file /mnt/work/link
cat /mnt/work/link
vfs pseudo proc /mnt/proc
cat /mnt/proc/identity
vfs pseudo dev /mnt/dev
printf discarded > /mnt/dev/null
fd open /mnt/work/alias rw
fd lock 0:1 exclusive
fd unlock 0:1
fd closeall
vfs save /mnt/work /root/volume.img
vfs unmount /mnt/dev
vfs unmount /mnt/proc
vfs unmount /mnt/work
exit 0",
    );
    let stdout = String::from_utf8_lossy(&first.stdout);
    let stderr = String::from_utf8_lossy(&first.stderr);
    assert!(first.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.contains("checkpoint"), "{stdout}");
    assert!(stdout.contains("euid: 0"), "{stdout}");
    assert!(stdout.contains("ImageStats"), "{stdout}");
    assert!(fs::metadata(data.join("root/volume.img"))
        .unwrap()
        .is_file());
    let second = run(
        &config,
        "vfs load /root/volume.img /mnt/restored
cat /mnt/restored/alias
printf changed > /mnt/restored/alias
cat /mnt/restored/dir/file
find /mnt/restored
vfs unmount /mnt/restored
exit 0",
    );
    let stdout = String::from_utf8_lossy(&second.stdout);
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(second.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.contains("Clean"), "{stdout}");
    assert!(stdout.contains("checkpoint"), "{stdout}");
    assert!(stdout.contains("changed"), "{stdout}");
    for name in ["work", "proc", "dev", "restored"] {
        assert!(!data.join("mnt").join(name).exists());
    }
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside");
    assert_eq!(fs::read(&config).unwrap(), configuration);
}

#[test]
fn vfs_namespace_real_process_cd_find_and_nested_unmount() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(format!("vfs-namespace-cli-{}-{nonce}", std::process::id()));
    assert!(!parent.exists());
    fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data");
    let config = parent.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let config_before = fs::read(&config).unwrap();
    fs::write(parent.join("sentinel"), b"untouched").unwrap();
    let output = run(&config, "vfs mount /mnt/a 65536 64\nmkdir /mnt/a/d\nvfs mount /mnt/a/d/b 65536 64\nprintf mounted > /mnt/a/d/b/f\ncd /mnt/a/d/b\nvfs unmount /mnt/a/d/b\ncat f\ncd /\nfind /mnt\necho /mnt/*/d/*/f\nvfs unmount /mnt/a/d/b\nvfs unmount /mnt/a\nvfs mounts\nexit 0");
    let out = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{out}\n{err}");
    // Interactive shell reports command errors on its transcript stream.
    assert_eq!(out.matches("Busy").count(), 1, "{out}\n{err}");
    assert_eq!(out.matches("/mnt/a/d/b/f").count(), 2, "{out}");
    assert!(out.contains("mounted"), "{out}");
    assert!(err.is_empty(), "{err}");
    assert!(!data.join("mnt/a").exists());
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"untouched");
    assert_eq!(fs::read(&config).unwrap(), config_before);
}

#[test]
fn system_providers_real_process_defaults_and_guest_devices_are_virtual() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(format!(
            "system-providers-cli-{}-{nonce}",
            std::process::id()
        ));
    assert!(!parent.exists());
    fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data");
    let config = parent.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let config_before = fs::read(&config).unwrap();
    fs::write(parent.join("sentinel"), b"keep").unwrap();
    elara_os::initialization::prepare(&data).unwrap();
    let root = DataRoot::open(&data).unwrap();
    users::ensure_passwd(&root).unwrap();
    let admin = Account {
        identity: Default::default(),
        uid: 0,
        name: "root".into(),
        home: "/root".into(),
    };
    let guest = users::add_user(&root, &admin, "deviceguest", "test-password").unwrap();
    let output = run(&config, "vfs mounts\ncat /sys/version\nvfs unmount /dev\nsu deviceguest test-password\ncat /proc/identity\nprintf discarded > /dev/null\nfd open /dev/zero read\nfd read 0:1 4\nfd closeall\nprintf rejected > /dev/full\ntouch /dev/not-a-device\necho SYSTEM_DONE\nexit 0");
    let out = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{out}\n{err}");
    assert!(out.contains("/dev System(Dev)"), "{out}");
    assert!(out.contains("/proc System(Proc)"), "{out}");
    assert!(out.contains("/sys System(Sys)"), "{out}");
    assert!(out.contains(&format!("euid: {}\n", guest.uid)), "{out}");
    assert!(out.contains("ReadOnly"), "{out}");
    assert!(out.contains("NoSpace"), "{out}");
    assert!(out.contains("SYSTEM_DONE"), "{out}");
    assert!(output.stdout.windows(4).any(|b| b == [0; 4]));
    assert!(err.is_empty(), "{err}");
    for point in ["dev", "proc", "sys"] {
        assert_eq!(fs::read_dir(data.join(point)).unwrap().count(), 0);
    }
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"keep");
    assert_eq!(fs::read(&config).unwrap(), config_before);
}

#[test]
fn resources_real_process_restart_quota_and_cleanup_preserve_host_boundary() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(format!("resources-cli-{}-{nonce}", std::process::id()));
    assert!(!parent.exists());
    fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data");
    let config = parent.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let configuration = fs::read(&config).unwrap();
    fs::write(parent.join("sentinel"), b"outside").unwrap();
    let first=run(&config,"app register /opt/demo 4 8\nprintf data > /opt/demo/file\nprintf X >> /opt/demo/file\napp stat /opt/demo\ncache register /var/cache/demo 32 8\nprintf cached > /var/cache/demo/item\nexit 0");
    let out = String::from_utf8_lossy(&first.stdout);
    assert!(first.status.success(), "{out}");
    assert!(out.contains("quota exceeded"), "{out}");
    assert!(
        out.contains("ResourceUsage { bytes: 4, objects: 2 }"),
        "{out}"
    );
    assert_eq!(fs::read(data.join("opt/demo/file")).unwrap(), b"data");
    fs::write(data.join("opt/demo/unknown"), b"keep").unwrap();
    let second=run(&config,"cat /opt/demo/file\ncache clear /var/cache/demo\ncache stat /var/cache/demo\napp uninstall /opt/demo\nexit 0");
    let out = String::from_utf8_lossy(&second.stdout);
    assert!(second.status.success(), "{out}");
    assert!(out.contains("Cleanup deferred"), "{out}");
    assert!(
        out.contains("ResourceUsage { bytes: 0, objects: 1 }"),
        "{out}"
    );
    assert_eq!(fs::read(data.join("opt/demo/unknown")).unwrap(), b"keep");
    assert!(!data.join("opt/demo/file").exists());
    fs::rename(data.join("opt/demo/unknown"), data.join("home/preserved")).unwrap();
    let third = run(
        &config,
        "app list\ncache unregister /var/cache/demo\ncache list\nexit 0",
    );
    assert!(
        third.status.success(),
        "{}",
        String::from_utf8_lossy(&third.stdout)
    );
    assert!(!data.join("opt/demo").exists());
    assert!(!data.join("var/cache/demo").exists());
    assert_eq!(fs::read(data.join("home/preserved")).unwrap(), b"keep");
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside");
    assert_eq!(fs::read(&config).unwrap(), configuration);
    for result in [&first, &second, &third] {
        assert!(
            result.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn resources_application_locations_survive_process_restart_and_uninstall_together() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(format!("app-locations-cli-{}-{nonce}", std::process::id()));
    assert!(!parent.exists());
    fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data");
    let config = parent.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let before = fs::read(&config).unwrap();
    fs::write(parent.join("sentinel"), b"outside").unwrap();
    let first = run(&config, "app register /opt/demo 32 8\napp cache /opt/demo /var/cache/demo 32 8\napp data /opt/demo /root/demo 32 8\nprintf cache > /var/cache/demo/item\nprintf data > /root/demo/state\nexit 0");
    assert!(first.status.success(), "{first:?}");
    assert!(first.stderr.is_empty(), "{first:?}");
    assert_eq!(fs::read(data.join("root/demo/state")).unwrap(), b"data");
    assert_eq!(
        fs::read(data.join("var/cache/demo/item")).unwrap(),
        b"cache"
    );
    let second = run(
        &config,
        "app locations /opt/demo\napp uninstall /opt/demo\nexit 0",
    );
    assert!(second.status.success(), "{second:?}");
    assert!(second.stderr.is_empty(), "{second:?}");
    let out = String::from_utf8_lossy(&second.stdout);
    assert!(out.contains("ApplicationData"), "{out}");
    assert!(
        out.contains("ResourceRecovery { resources: 3, files: 2, directories: 3, deferred: 0 }"),
        "{out}"
    );
    for path in ["opt/demo", "var/cache/demo", "root/demo"] {
        assert!(!data.join(path).exists());
    }
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside");
    assert_eq!(fs::read(&config).unwrap(), before);
}

#[test]
fn block_store_real_process_commit_and_reload_preserve_host_boundary() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(format!("block-store-cli-{}-{nonce}", std::process::id()));
    assert!(!parent.exists());
    fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data");
    let config = parent.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let before = fs::read(&config).unwrap();
    fs::write(parent.join("sentinel"), b"outside").unwrap();
    let first = run(&config, "vfs mount /mnt/a 65536 64\nprintf durable > /mnt/a/file\nvfs disk-create /mnt/a /root/disk 64 1\nexit 0");
    assert!(first.status.success(), "{first:?}");
    assert!(first.stderr.is_empty(), "{first:?}");
    assert_eq!(
        fs::metadata(data.join("root/disk")).unwrap().len(),
        64 * 4096
    );
    let second = run(
        &config,
        "vfs disk-load /root/disk /mnt/b 1\ncat /mnt/b/file\nvfs disk-stat /root/disk 1\nexit 0",
    );
    assert!(second.status.success(), "{second:?}");
    assert!(second.stderr.is_empty(), "{second:?}");
    let out = String::from_utf8_lossy(&second.stdout);
    assert!(out.contains("durable"), "{out}");
    assert!(out.contains("Clean"), "{out}");
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside");
    assert_eq!(fs::read(&config).unwrap(), before);
}

#[test]
fn sealed_checkpoint_real_process_roundtrip_never_prints_key() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(format!("sealed-cli-{}-{nonce}", std::process::id()));
    assert!(!parent.exists());
    fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data");
    let config = parent.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let before = fs::read(&config).unwrap();
    fs::write(parent.join("sentinel"), b"outside").unwrap();
    let first = run(&config, "vfs mount /mnt/a 65536 64\nprintf confidential > /mnt/a/file\nvfs keygen /root/key\nvfs seal /mnt/a /root/sealed /root/key\nexit 0");
    assert!(first.status.success(), "{first:?}");
    assert!(first.stderr.is_empty(), "{first:?}");
    let key = fs::read(data.join("root/key")).unwrap();
    assert_eq!(key.len(), 32);
    let second = run(
        &config,
        "vfs unseal /root/sealed /mnt/b /root/key\ncat /mnt/b/file\nexit 0",
    );
    assert!(second.status.success(), "{second:?}");
    assert!(second.stderr.is_empty(), "{second:?}");
    assert!(String::from_utf8_lossy(&second.stdout).contains("confidential"));
    for bytes in [&first.stdout, &first.stderr, &second.stdout, &second.stderr] {
        assert!(!bytes.windows(32).any(|w| w == key));
    }
    assert!(!fs::read(data.join("var/log/elara.log"))
        .unwrap()
        .windows(32)
        .any(|w| w == key));
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside");
    assert_eq!(fs::read(&config).unwrap(), before);
}

#[test]
fn persistent_snapshot_real_process_reload_rejects_writes_and_clone_is_independent() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(format!(
            "persistent-snapshot-cli-{}-{nonce}",
            std::process::id()
        ));
    assert!(!parent.exists());
    fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data");
    let config = parent.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let before = fs::read(&config).unwrap();
    fs::write(parent.join("sentinel"), b"outside").unwrap();
    let first = run(&config, "vfs mount /mnt/source 65536 64\nprintf BEFORE > /mnt/source/file\nvfs snapshot /mnt/source /mnt/snapshot\nprintf AFTER > /mnt/source/file\nvfs save /mnt/snapshot /root/snapshot.img\nvfs disk-create /mnt/snapshot /root/snapshot.disk 64 1\nvfs keygen /root/key\nvfs seal /mnt/snapshot /root/snapshot.sealed /root/key\nexit 0");
    let out = String::from_utf8_lossy(&first.stdout);
    assert!(
        first.status.success() && first.stderr.is_empty(),
        "{first:?}"
    );
    assert!(!out.contains("Error:"), "{out}");
    let second = run(&config, "vfs load /root/snapshot.img /mnt/image\nvfs disk-load /root/snapshot.disk /mnt/disk 1\nvfs unseal /root/snapshot.sealed /mnt/sealed /root/key\nprintf DENIED > /mnt/image/file\nprintf DENIED > /mnt/disk/file\nprintf DENIED > /mnt/sealed/file\ncat /mnt/image/file\ncat /mnt/disk/file\ncat /mnt/sealed/file\nvfs stats /mnt/disk\nvfs clone /mnt/disk /mnt/clone\nprintf CLONED > /mnt/clone/file\ncat /mnt/disk/file\ncat /mnt/clone/file\nexit 0");
    let out = String::from_utf8_lossy(&second.stdout);
    assert!(
        second.status.success() && second.stderr.is_empty(),
        "{second:?}"
    );
    assert_eq!(
        out.matches("Error: virtual filesystem: ReadOnly").count(),
        3,
        "{out}"
    );
    assert_eq!(out.matches("Error:").count(), 3, "{out}");
    assert!(out.contains("BEFOREBEFOREBEFORE"), "{out}");
    assert!(out.contains("access: FrozenSnapshot"), "{out}");
    assert!(out.contains("BEFORECLONED"), "{out}");
    assert_eq!(fs::read(&config).unwrap(), before);
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside");
    assert!(!data.join("mnt/disk").exists());
}

#[test]
fn cross_mount_real_process_saved_links_follow_internal_targets_after_restart() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(format!("cross-mount-cli-{}-{nonce}", std::process::id()));
    assert!(!parent.exists());
    fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data");
    let config = parent.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let before = fs::read(&config).unwrap();
    fs::write(parent.join("sentinel"), b"outside").unwrap();
    let first = run(&config, "vfs mount /mnt/a 65536 64\nvfs mount /mnt/b 65536 64\nprintf initial > /mnt/b/file\nvfs symlink-global /mnt/b/file /mnt/a/link\ncat /mnt/a/link\nvfs save /mnt/a /root/a.img\nvfs disk-create /mnt/b /root/b.disk 64 1\nexit 0");
    assert!(
        first.status.success() && first.stderr.is_empty(),
        "{first:?}"
    );
    assert!(
        !String::from_utf8_lossy(&first.stdout).contains("Error:"),
        "{first:?}"
    );
    let second = run(&config, "vfs load /root/a.img /mnt/restored\ncat /mnt/restored/link\nvfs disk-load /root/b.disk /mnt/b 1\ncat /mnt/restored/link\nprintf updated > /mnt/restored/link\ncat /mnt/b/file\nrm /mnt/restored/link\ncat /mnt/b/file\nexit 0");
    let out = String::from_utf8_lossy(&second.stdout);
    assert!(
        second.status.success() && second.stderr.is_empty(),
        "{second:?}"
    );
    assert_eq!(out.matches("Error:").count(), 1, "{out}");
    // Global links may now reach Host. Before /mnt/b is mounted its parent
    // is absent, rather than forbidden merely for crossing backend boundaries.
    let absent = fs::metadata(data.join("mnt/b")).unwrap_err();
    assert_eq!(absent.kind(), std::io::ErrorKind::NotFound);
    let expected = format!("Error: {}", elara_os::filesystem::FsError::from(absent));
    assert!(out.contains(&expected), "{out}");
    assert!(!data.join("mnt/b").exists());
    assert!(out.contains("initialupdatedupdated"), "{out}");
    assert_eq!(fs::read(&config).unwrap(), before);
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside");
}

#[test]
fn proc_tasks_real_process_shows_virtual_lease_and_leaves_no_host_proc_files() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(format!("proc-tasks-cli-{}-{nonce}", std::process::id()));
    assert!(!parent.exists());
    fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data");
    let config = parent.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let before = fs::read(&config).unwrap();
    fs::write(parent.join("sentinel"), b"outside").unwrap();
    let result = run(&config, "sh -c 'ls /proc/tasks; printf before-create; touch /tmp/proc-lease; cat /proc/tasks/*/status'\nexit 0");
    let out = String::from_utf8_lossy(&result.stdout);
    assert!(
        result.status.success() && result.stderr.is_empty(),
        "{result:?}"
    );
    assert!(!out.contains("Error:"), "{out}");
    assert!(out.contains("lease_state: LeaseHeld"), "{out}");
    assert!(out.contains("kind: resource-lease"), "{out}");
    assert!(out.contains("before-createtask_scope_id:"), "{out}");
    assert!(!data.join("tmp/proc-lease").exists());
    assert!(!data.join("proc/tasks").exists());
    let root = DataRoot::open(&data).unwrap();
    let admin = Account {
        identity: Default::default(),
        name: "root".into(),
        uid: 0,
        home: "/root".into(),
    };
    assert!(root.list_as(&admin, "/proc/tasks").unwrap().is_empty());
    assert_eq!(fs::read(&config).unwrap(), before);
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside");
}

#[test]
fn proc_jobs_real_process_reports_stopped_worker_and_reaps_view() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(format!("proc-jobs-cli-{}-{nonce}", std::process::id()));
    assert!(!parent.exists());
    fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data");
    let config = parent.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let before = fs::read(&config).unwrap();
    fs::write(parent.join("sentinel"), b"outside").unwrap();
    let result = run(&config, "sleep 30 &\nkill -STOP %1\ncat /proc/jobs/*/status\nkill -TERM %1\nwait %1\necho REAPED\nls /proc/jobs\nexit 0");
    let text = String::from_utf8_lossy(&result.stdout);
    assert!(
        result.status.success() && result.stderr.is_empty(),
        "{result:?}"
    );
    assert!(
        text.contains("state: Stopped\n") && text.contains("kind: scoped-job\n"),
        "{text}"
    );
    assert!(text.contains("[1] cancelled 130\n"), "{text}");
    assert!(text.ends_with("REAPED\nBye.\n"), "{text}");
    assert!(!data.join("proc/jobs").exists());
    assert_eq!(fs::read(config).unwrap(), before);
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside");
}

#[test]
fn host_locks_real_process_blocks_second_process_and_recovers_after_holder_death() {
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc;
    use std::time::Duration;
    struct Holder(std::process::Child);
    impl Drop for Holder {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = self.0.kill();
            }
            let _ = self.0.wait();
        }
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(format!("host-locks-cli-{}-{nonce}", std::process::id()));
    assert!(!parent.exists());
    fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data");
    let config = parent.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let original = fs::read(&config).unwrap();
    fs::write(parent.join("sentinel"), b"outside").unwrap();
    elara_os::initialization::prepare(&data).unwrap();
    let root = DataRoot::open(&data).unwrap();
    users::ensure_passwd(&root).unwrap();
    let user = Account {
        identity: Default::default(),
        name: "root".into(),
        uid: 0,
        home: "/root".into(),
    };
    root.create_owned_file(
        &user,
        "/root/a",
        b"original",
        VirtualMode::new(0o600).unwrap(),
    )
    .unwrap();
    let mut holder = Holder(
        Command::new(env!("CARGO_BIN_EXE_elara-host"))
            .arg(&config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    holder.0.stdin.take().unwrap().write_all(b"\nfd open /root/a rw\nfd lock 0:1 exclusive\necho HOST_LOCK_READY\nsleep 60\nexit 0\n").unwrap();
    let stdout = holder.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut all = String::new();
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap();
            all.push_str(&line);
            all.push('\n');
            if line == "HOST_LOCK_READY" {
                let _ = tx.send(());
            }
        }
        all
    });
    rx.recv_timeout(Duration::from_secs(30))
        .expect("holder readiness");
    let denied = run(
        &config,
        "cat /root/a\nowned append /root/a denied\nfd open /root/a read\nfd read 0:1 8\nexit 0",
    );
    assert!(denied.status.success());
    let out = String::from_utf8(denied.stdout).unwrap();
    assert_eq!(out.matches("LockConflict").count(), 3, "{out}");
    assert_eq!(fs::read(data.join("root/a")).unwrap(), b"original");
    // Kill only the child created above, inside its unique fixture.
    holder.0.kill().unwrap();
    holder.0.wait().unwrap();
    let holder_output = reader.join().unwrap();
    assert!(!holder_output.contains("Error:"), "{holder_output}");
    let admitted=run(&config,"cat /root/a\necho READ_END\nfd open /root/a rw exclusive\nfd lock 0:1 exclusive\nfd writeat 0:1 0 changed!\nfd closeall\nexit 0");
    assert!(admitted.status.success());
    let out = String::from_utf8(admitted.stdout).unwrap();
    assert!(out.contains("originalREAD_END"), "{out}");
    assert!(!out.contains("Error:"), "{out}");
    assert_eq!(fs::read(data.join("root/a")).unwrap(), b"changed!");
    assert_eq!(fs::read(&config).unwrap(), original);
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside");
    assert!(!fs::read_dir(data.join("var/.elara-objects"))
        .unwrap()
        .any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".lease")));
}

#[test]
fn host_hardlinks_real_process_restart_shares_names_and_keeps_unlinked_fd() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-data")
        .join(format!("host-hardlinks-cli-{}-{nonce}", std::process::id()));
    assert!(!parent.exists()); fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data"); let config = parent.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}
", data.display())).unwrap();
    let before_config = fs::read(&config).unwrap();
    fs::write(parent.join("sentinel"), b"outside").unwrap();
    elara_os::initialization::prepare(&data).unwrap();
    let first = run(&config, "owned create /root/a old
vfs link /root/a /root/b
fd open /root/a read
owned write /root/b new
printf 'OLD='
fd read 0:1 3
printf 'NEW='
cat /root/a
exit 0");
    let stdout = String::from_utf8_lossy(&first.stdout);
    assert!(first.status.success(), "{stdout}");
    assert!(!stdout.contains("Error:"), "{stdout}");
    assert!(stdout.contains("OLD=old"), "{stdout}");
    assert!(stdout.contains("NEW=new"), "{stdout}");
    assert_eq!(fs::read(data.join("root/a")).unwrap(), b"new");
    assert_eq!(fs::read(data.join("root/b")).unwrap(), b"new");
    let second = run(&config, "fd open /root/b read
owned rm /root/a
owned mv /root/b /root/c
owned rm /root/c
printf 'UNLINKED='
fd read 0:1 3
fd closeall
exit 0");
    let stdout = String::from_utf8_lossy(&second.stdout);
    assert!(second.status.success(), "{stdout}");
    assert!(!stdout.contains("Error:"), "{stdout}");
    assert!(stdout.contains("UNLINKED=new"), "{stdout}");
    for name in ["root/a", "root/b", "root/c"] { assert!(!data.join(name).exists()); }
    assert_eq!(fs::read(&config).unwrap(), before_config);
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside");
}

#[test]
fn host_symlinks_real_process_restart_directory_exec_and_unlink() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-data")
        .join(format!("host-symlinks-cli-{}-{nonce}", std::process::id()));
    assert!(!parent.exists()); fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data"); let config = parent.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let before_config = fs::read(&config).unwrap();
    fs::write(parent.join("sentinel"), b"outside").unwrap();
    elara_os::initialization::prepare(&data).unwrap();
    let first = run(&config, "mkdir /root/d
vfs symlink d /root/dirlink
owned create /root/dirlink/data old
vfs symlink dirlink/data /root/link
fd open /root/link read
owned write /root/link new
printf 'OLD='
fd read 0:1 3
printf 'NEW='
cat /root/d/data
fd closeall
cd /root/dirlink
pwd
exit 0");
    let stdout = String::from_utf8_lossy(&first.stdout);
    assert!(first.status.success(), "{stdout}");
    assert!(!stdout.contains("Error:"), "{stdout}");
    assert!(stdout.contains("OLD=old"), "{stdout}");
    assert!(stdout.contains("NEW=new"), "{stdout}");
    assert!(stdout.contains("/root/d"), "{stdout}");
    assert!(fs::symlink_metadata(data.join("root/link")).unwrap().is_file());
    let second = run(&config, "printf 'TARGET='
vfs readlink /root/link
printf 'RESTART='
cat /root/link
mv /root/link /root/moved
rm /root/moved
printf 'SURVIVES='
cat /root/d/data
owned create /root/d/image 'echo LINK_EXEC_OK'
chmod 700 /root/d/image
vfs symlink dirlink/image /root/exec-link
exec /root/exec-link");
    let stdout = String::from_utf8_lossy(&second.stdout);
    assert!(second.status.success(), "{stdout}");
    assert!(!stdout.contains("Error:"), "{stdout}");
    for expected in ["TARGET=dirlink/data", "RESTART=new", "SURVIVES=new", "LINK_EXEC_OK"] {
        assert!(stdout.contains(expected), "{expected}: {stdout}");
    }
    assert!(!data.join("root/link").exists()); assert!(!data.join("root/moved").exists());
    assert_eq!(fs::read(data.join("root/d/data")).unwrap(), b"new");
    assert_eq!(fs::read(&config).unwrap(), before_config);
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside");
}

#[test]
fn host_rename_real_process_replaces_file_and_directory_with_restart() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-data")
        .join(format!("host-rename-cli-{}-{nonce}", std::process::id()));
    assert!(!parent.exists()); fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data"); let config = parent.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let before = fs::read(&config).unwrap(); fs::write(parent.join("sentinel"), b"outside").unwrap();
    let out = run(&config, "owned create /root/source new
owned create /root/dest old
vfs link /root/dest /root/victim-alias
fd open /root/dest read
vfs rename /root/source /root/dest
printf 'NEW='
cat /root/dest
printf 'OLD_FD='
fd read 0:1 3
printf 'OLD_ALIAS='
cat /root/victim-alias
fd closeall
mkdir /root/tree
mkdir /root/empty
owned create /root/tree/data payload
vfs symlink data /root/tree/link
vfs rename /root/tree /root/empty
printf 'TREE='
cat /root/empty/link
exit 0");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}"); assert!(!text.contains("Error:"), "{text}");
    for value in ["NEW=new", "OLD_FD=old", "OLD_ALIAS=old", "TREE=payload"] {
        assert!(text.contains(value), "{value}: {text}");
    }
    let out = run(&config, "printf 'RESTART_FILE='
cat /root/dest
printf 'RESTART_TREE='
cat /root/empty/link
exit 0");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}"); assert!(!text.contains("Error:"), "{text}");
    assert!(text.contains("RESTART_FILE=new"), "{text}");
    assert!(text.contains("RESTART_TREE=payload"), "{text}");
    assert!(!data.join("root/source").exists()); assert!(!data.join("root/tree").exists());
    assert_eq!(fs::read(&config).unwrap(), before);
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside");
}

#[test]
fn proc_legacy_real_process_timers_identity_retirement_and_restart() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-data")
        .join(format!("legacy-proc-cli-{}-{nonce}", std::process::id()));
    assert!(!parent.exists());
    fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data"); let config = parent.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}
", data.display())).unwrap();
    let before = fs::read(&config).unwrap();
    fs::write(parent.join("sentinel"), b"outside").unwrap();
    let out = run(&config, "sleep 30 bg
cat /proc/jobs/1/status
fd open /proc/jobs/1/status read
sleep 0
cat /proc/jobs/2/status
useradd timercli secret1
su timercli
printf RETIRED
ls /proc/jobs
ps
wait 1
sleep 0
cat /proc/jobs/3/status
exit 0");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("task_id: 1
owner_uid: 0
state: Running
kind: legacy-task"), "{text}");
    assert!(text.contains("task_id: 2
owner_uid: 0
state: TaskExited(0)"), "{text}");
    let after = text.split("RETIRED").nth(1).unwrap();
    assert!(after.starts_with("ID    STATE      COMMAND
"), "{text}");
    assert!(after.contains("No task with id 1"), "{text}");
    assert!(after.contains("task_id: 3
owner_uid: 1000
state: TaskExited(0)"), "{text}");
    assert!(!after.contains("sleep 30"), "{text}");
    let out = run(&config, "printf EMPTY
ls /proc/jobs
exit 0");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success() && text.contains("EMPTYBye."), "{text}");
    assert!(!data.join("proc/jobs").exists());
    assert_eq!(fs::read(&config).unwrap(), before);
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside");
}

#[test]
fn proc_executions_real_process_snapshot_and_restart_are_virtual_only() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-data")
        .join(format!("execution-proc-cli-{}-{nonce}", std::process::id()));
    assert!(!parent.exists()); fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data"); let config = parent.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let before = fs::read(&config).unwrap(); fs::write(parent.join("sentinel"), b"outside").unwrap();
    let out = run(&config, "fd open /proc/executions/1/status read\nprintf CURRENT; cat /proc/executions/*/status\nprintf OLD; fd read 0:1 1024\nfd closeall\nexit 0");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success() && !text.contains("Error:"), "{text}");
    assert!(text.contains("CURRENTexecution_id: 2\n"), "{text}");
    assert!(text.contains("OLDexecution_id: 1\n"), "{text}");
    assert_eq!(text.matches("kind: foreground-execution").count(), 2);
    let out = run(&config, "cat /proc/executions/*/status\nexit 0");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success() && !text.contains("Error:"), "{text}");
    assert!(text.contains("execution_id: 1\n"), "{text}");
    assert_eq!(text.matches("kind: foreground-execution").count(), 1);
    assert!(!data.join("proc/executions").exists());
    assert_eq!(fs::read(&config).unwrap(), before);
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside");
}

#[test]
fn host_cwd_real_process_blocks_retarget_and_recovers_after_holder_death() {
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc;
    use std::time::Duration;
    struct Holder(std::process::Child);
    impl Drop for Holder {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = self.0.kill();
            }
            let _ = self.0.wait();
        }
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(format!("host-cwd-cli-{}-{nonce}", std::process::id()));
    assert!(!parent.exists());
    fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data");
    let config = parent.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let original = fs::read(&config).unwrap();
    fs::write(parent.join("sentinel"), b"outside").unwrap();
    elara_os::initialization::prepare(&data).unwrap();
    let root = DataRoot::open(&data).unwrap();
    users::ensure_passwd(&root).unwrap();
    let user = Account {
        identity: Default::default(),
        name: "root".into(),
        uid: 0,
        home: "/root".into(),
    };
    root.create_directory_as(&user, "/root/cwd-parent", Default::default()).unwrap();
    root.create_directory_as(&user, "/root/cwd-parent/leaf", Default::default()).unwrap();
    let mut holder = Holder(
        Command::new(env!("CARGO_BIN_EXE_elara-host"))
            .arg(&config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    holder.0.stdin.take().unwrap().write_all(
        b"
cd /root/cwd-parent/leaf
echo HOST_CWD_READY
sleep 60
exit 0
"
    ).unwrap();
    let stdout = holder.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut all = String::new();
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap();
            all.push_str(&line);
            all.push('\n');
            if line == "HOST_CWD_READY" {
                let _ = tx.send(());
            }
        }
        all
    });
    rx.recv_timeout(Duration::from_secs(30))
        .expect("holder readiness");
    let denied = run(&config,
        "rmdir /root/cwd-parent/leaf
mv /root/cwd-parent /root/cwd-moved
exit 0");
    assert!(denied.status.success());
    let out = String::from_utf8(denied.stdout).unwrap();
    assert_eq!(out.matches("Busy").count(), 2, "{out}");
    assert!(data.join("root/cwd-parent/leaf").is_dir());
    assert!(!data.join("root/cwd-moved").exists());
    // Kill only the child created above, inside its unique fixture.
    holder.0.kill().unwrap();
    holder.0.wait().unwrap();
    let holder_output = reader.join().unwrap();
    assert!(!holder_output.contains("Error:"), "{holder_output}");
    let admitted = run(&config,
        "rmdir /root/cwd-parent/leaf
mkdir /root/cwd-parent/leaf
rmdir /root/cwd-parent/leaf
rmdir /root/cwd-parent
echo CWD_RELEASED
exit 0");
    assert!(admitted.status.success());
    let out = String::from_utf8(admitted.stdout).unwrap();
    assert!(out.contains("CWD_RELEASED"), "{out}");
    assert!(!out.contains("Error:"), "{out}");
    assert!(!data.join("root/cwd-parent").exists());
    assert_eq!(fs::read(&config).unwrap(), original);
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside");
    assert!(!fs::read_dir(data.join("var/.elara-objects"))
        .unwrap()
        .any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".lease")));
}

#[test]
fn cross_backend_real_process_links_metadata_and_image_restart() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-data")
        .join(format!("cross-backend-{}-{nonce}", std::process::id()));
    assert!(!parent.exists());
    fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data");
    let config = parent.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let before = fs::read(&config).unwrap();
    fs::write(parent.join("sentinel"), b"outside").unwrap();
    elara_os::initialization::prepare(&data).unwrap();
    let root = DataRoot::open(&data).unwrap();
    users::ensure_passwd(&root).unwrap();
    drop(root);
    let first = run(&config, "vfs mount /mnt/cross 65536 64\nmkdir /root/host\nprintf host > /root/host/data\nprintf volume > /mnt/cross/data\nvfs symlink /mnt/cross/data /root/to-volume\nvfs symlink-global /root/host /mnt/cross/to-host\ncat /root/to-volume\ncat /mnt/cross/to-host/data\nvfs inode /root/host/data\nvfs inode /mnt/cross/data\ncd /mnt/cross/to-host\npwd\ncd /\nvfs save /mnt/cross /root/cross.img\necho CROSS_FIRST_OK\nexit 0");
    let output = String::from_utf8_lossy(&first.stdout);
    assert!(first.status.success(), "{output}");
    assert!(!output.contains("Error:"), "{output}");
    assert!(first.stderr.is_empty(), "{}", String::from_utf8_lossy(&first.stderr));
    for expected in ["volume", "host", "HostFile", "Virtual", "Logical", "/root/host", "CROSS_FIRST_OK"] {
        assert!(output.contains(expected), "missing {expected}: {output}");
    }
    let second = run(&config, "vfs load /root/cross.img /mnt/cross\ncat /root/to-volume\ncat /mnt/cross/to-host/data\nprintf changed > /mnt/cross/to-host/data\ncat /root/host/data\necho CROSS_RESTART_OK\nexit 0");
    let output = String::from_utf8_lossy(&second.stdout);
    assert!(second.status.success(), "{output}");
    assert!(!output.contains("Error:"), "{output}");
    assert!(second.stderr.is_empty(), "{}", String::from_utf8_lossy(&second.stderr));
    for expected in ["volume", "host", "changed", "CROSS_RESTART_OK"] { assert!(output.contains(expected), "{output}"); }
    assert_eq!(fs::read(data.join("root/host/data")).unwrap(), b"changed");
    assert_eq!(fs::read(&config).unwrap(), before);
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside");
    assert!(fs::symlink_metadata(data.join("root/to-volume")).unwrap().is_file());
}

#[test]
fn joint_catalog_real_process_upgrade_and_restart_use_single_authority() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-data")
        .join(format!("joint-catalog-{}-{nonce}", std::process::id()));
    assert!(!parent.exists());
    fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let data = parent.join("data");
    let config = parent.join("fixture.env");
    fs::write(&config, format!("ELARA_DATA_ROOT={}\n", data.display())).unwrap();
    let before = fs::read(&config).unwrap();
    fs::write(parent.join("sentinel"), b"outside").unwrap();
    let first = run(&config, "mkdir /root/joint\nprintf before > /root/joint/data\nvfs catalog-upgrade\nprintf after > /root/joint/data\necho UPGRADED\nexit 0");
    let output = String::from_utf8_lossy(&first.stdout);
    assert!(first.status.success() && first.stderr.is_empty(), "{first:?}");
    assert!(!output.contains("Error:"), "{output}");
    let store = data.join("var/.elara-objects");
    assert_eq!(fs::read(store.join("store.owner")).unwrap(), b"ELARA_OWNERSHIP_STORE_V2\n");
    assert!(store.join("namespace.catalog").is_file());
    assert!(!store.join("catalog").exists());
    assert!(!store.join("access.catalog").exists());
    let second = run(&config, "cat /root/joint/data\necho READ_BEFORE_MOVE\nvfs catalog-upgrade\nmkdir /root/joint/sub\nmv /root/joint/data /root/joint/sub/moved\ncat /root/joint/sub/moved\necho REOPENED\nexit 0");
    let output = String::from_utf8_lossy(&second.stdout);
    assert!(second.status.success() && second.stderr.is_empty(), "{second:?}");
    assert!(!output.contains("Error:"), "{output}");
    assert!(output.contains("afterREAD_BEFORE_MOVE"), "{output}");
    assert!(output.contains("afterREOPENED"), "{output}");
    assert_eq!(fs::read(data.join("root/joint/sub/moved")).unwrap(), b"after");
    assert_eq!(fs::read(&config).unwrap(), before);
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"outside");
    assert!(!store.join("catalog").exists());
    assert!(!store.join("access.catalog").exists());
}
