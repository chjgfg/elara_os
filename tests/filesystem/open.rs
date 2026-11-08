use super::fixture;
use crate::filesystem::{DataRoot, FileAccess, FileBehavior, FileCreation, FileMode, FsError};
use std::{assert, assert_eq, fs, matches};

fn existing_write() -> FileMode {
    FileMode::new(
        FileAccess::ReadWrite,
        FileCreation::Existing,
        FileBehavior::Preserve,
    )
    .unwrap()
}

#[test]
fn opening_modes_create_exclusively_and_preserve_existing_contents() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    assert!(matches!(
        data.open_file("/home/new", FileMode::READ),
        Err(FsError::NotFound)
    ));
    let file = data.open_file("/home/new", FileMode::CREATE).unwrap();
    assert_eq!(file.write(b"abc").unwrap(), 3);
    assert!(matches!(
        data.open_file("/home/new", FileMode::CREATE),
        Err(FsError::AlreadyExists)
    ));
    let mode = FileMode::new(
        FileAccess::ReadWrite,
        FileCreation::IfMissing,
        FileBehavior::Preserve,
    )
    .unwrap();
    assert_eq!(
        data.open_file("/home/new", mode).unwrap().read(16).unwrap(),
        b"abc"
    );
    assert_eq!(
        data.open_file("/home/missing", mode)
            .unwrap()
            .info()
            .unwrap()
            .length,
        0
    );
    assert_eq!(fs::read(root.join("home/new")).unwrap(), b"abc");
}

#[test]
fn truncate_open_and_writeonly_are_not_implicit_readwrite() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    fs::write(root.join("home/value"), b"previous").unwrap();
    let mode = FileMode::new(
        FileAccess::WriteOnly,
        FileCreation::Existing,
        FileBehavior::Truncate,
    )
    .unwrap();
    let file = data.open_file("/home/value", mode).unwrap();
    assert_eq!(file.info().unwrap().length, 0);
    assert!(matches!(file.read(1), Err(FsError::HandleReadDenied)));
    assert_eq!(file.write(b"new").unwrap(), 3);
    assert_eq!(data.read_file("/home/value", 16).unwrap(), b"new");
}

#[test]
fn every_open_write_mode_passes_the_system_policy_gate() {
    let root = fixture();
    let mut data = DataRoot::open(&root).unwrap();
    fs::write(root.join("usr/value"), b"keep").unwrap();
    for behavior in [
        FileBehavior::Preserve,
        FileBehavior::Truncate,
        FileBehavior::Append,
    ] {
        let mode = FileMode::new(FileAccess::ReadWrite, FileCreation::Existing, behavior).unwrap();
        assert!(matches!(
            data.open_file("/usr/value", mode),
            Err(FsError::ReadOnlySystemPath)
        ));
    }
    assert_eq!(
        data.open_file("/usr/value", FileMode::READ)
            .unwrap()
            .read(16)
            .unwrap(),
        b"keep"
    );
    assert!(matches!(
        data.open_file("/usr/new", FileMode::CREATE),
        Err(FsError::ReadOnlySystemPath)
    ));
    // Remove the actual production gate: the same open must become possible.
    data.bypass_write_policy = true;
    assert!(data.open_file("/usr/value", existing_write()).is_ok());
    assert!(data.open_file("/usr/new", FileMode::CREATE).is_ok());
    assert_eq!(fs::read(root.join("usr/value")).unwrap(), b"keep");
}

#[test]
fn open_refuses_unclassified_paths_directories_missing_parents_and_host_syntax() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    fs::write(root.join("unclassified"), b"keep").unwrap();
    assert!(matches!(
        data.open_file("/unclassified", existing_write()),
        Err(FsError::UnclassifiedWritePath)
    ));
    assert!(matches!(
        data.open_file("/missing", existing_write()),
        Err(FsError::NotFound)
    ));
    assert!(matches!(
        data.open_file("/", FileMode::READ),
        Err(FsError::IsDirectory)
    ));
    assert!(matches!(
        data.open_file("/home", existing_write()),
        Err(FsError::IsDirectory)
    ));
    assert!(matches!(
        data.open_file("/home/missing/child", FileMode::CREATE),
        Err(FsError::NotDirectory)
    ));
    for path in [
        "C:/Windows/value",
        "/home/../etc/passwd",
        "/tmp/.elara-transactions/payload",
        "/NUL",
        "/home/value:stream",
    ] {
        assert!(
            matches!(
                data.open_file(path, FileMode::CREATE),
                Err(FsError::InvalidPath)
            ),
            "{path}"
        );
    }
    assert_eq!(fs::read(root.join("unclassified")).unwrap(), b"keep");
}

#[cfg(windows)]
#[test]
fn windows_case_aliases_cannot_open_system_files_for_writing() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    fs::write(root.join("usr/value"), b"keep").unwrap();
    assert!(matches!(
        data.open_file("/USR/VALUE", existing_write()),
        Err(FsError::ReadOnlySystemPath)
    ));
}

#[test]
fn an_unowned_transaction_store_never_causes_unlocked_mutation_fallback() {
    let root = fixture();
    let data = DataRoot::open(&root).unwrap();
    fs::write(root.join("home/keep"), b"keep").unwrap();
    let store = root
        .join("tmp")
        .join(crate::config::ROOTFS_DEFAULTS.transaction_directory);
    fs::create_dir(&store).unwrap();
    fs::write(store.join("unknown"), b"not-ours").unwrap();
    assert!(matches!(
        data.open_file("/home/keep", existing_write()),
        Err(FsError::UnownedTransactionStore)
    ));
    assert!(matches!(
        data.create_file("/home/new"),
        Err(FsError::UnownedTransactionStore)
    ));
    assert!(matches!(
        data.remove_file("/home/keep"),
        Err(FsError::UnownedTransactionStore)
    ));
    assert!(matches!(
        data.rename("/home/keep", "/home/moved"),
        Err(FsError::UnownedTransactionStore)
    ));
    assert_eq!(
        data.open_file("/home/keep", FileMode::READ)
            .unwrap()
            .read(8)
            .unwrap(),
        b"keep"
    );
    assert!(!root.join("home/new").exists());
    assert!(!root.join("home/moved").exists());
    assert_eq!(fs::read(store.join("unknown")).unwrap(), b"not-ours");
}

#[test]
fn native_links_cannot_grant_open_file_capabilities() {
    let root = fixture();
    let outside = fixture();
    let data = DataRoot::open(&root).unwrap();
    fs::write(outside.join("home/keep"), b"sentinel").unwrap();
    #[cfg(windows)]
    assert!(std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(root.join("home/link"))
        .arg(outside.join("home"))
        .status()
        .unwrap()
        .success());
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.join("home"), root.join("home/link")).unwrap();
    assert!(matches!(
        data.open_file("/home/link/keep", FileMode::READ),
        Err(FsError::Link(_))
    ));
    assert!(matches!(
        data.open_file("/home/link/new", FileMode::CREATE),
        Err(FsError::Link(_))
    ));
    assert_eq!(fs::read(outside.join("home/keep")).unwrap(), b"sentinel");
    assert!(!outside.join("home/new").exists());
}
