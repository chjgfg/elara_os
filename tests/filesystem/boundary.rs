//! Capability-boundary regression: the actual open stays inside the granted
//! root even if a directory changes after a successful path inspection.
use super::fixture;
use crate::filesystem::DataRoot;
use cap_std::fs::OpenOptions;
use std::{assert, assert_eq, format, fs};

#[test]
fn replacing_a_checked_parent_cannot_redirect_capability_io_outside_the_root() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    let outside = root.parent().unwrap().join(format!(
        "outside-{}",
        root.file_name().unwrap().to_string_lossy()
    ));
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.txt"), b"outside-sentinel").unwrap();
    fs::create_dir(root.join("home/race")).unwrap();
    fs::write(root.join("home/race/secret.txt"), b"inside").unwrap();
    let checked = data.resolve("/home/race/secret.txt").unwrap();
    fs::rename(root.join("home/race"), root.join("home/race.saved")).unwrap();
    #[cfg(windows)]
    {
        let status = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(root.join("home/race"))
            .arg(&outside)
            .status()
            .unwrap();
        assert!(status.success());
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, root.join("home/race")).unwrap();
    // Counterexample: ambient path IO would read the external sentinel.
    assert_eq!(fs::read(root.join(&checked)).unwrap(), b"outside-sentinel");
    assert!(data.dir.open(&checked).is_err());
    assert!(data
        .dir
        .open_with(&checked, OpenOptions::new().write(true).truncate(true))
        .is_err());
    assert!(data.read_file("/home/race/secret.txt", 64).is_err());
    assert_eq!(
        fs::read(outside.join("secret.txt")).unwrap(),
        b"outside-sentinel"
    );
}
