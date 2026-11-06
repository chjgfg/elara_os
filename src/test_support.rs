//! Test fixtures belong to the host project's target directory, not the OS runtime root.
use crate::config::ROOTFS_DEFAULTS;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, format, fs};

static NEXT: AtomicU64 = AtomicU64::new(0);

pub fn fresh_path(prefix: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-data");
    fs::create_dir_all(&base).unwrap();
    base.canonicalize().unwrap().join(format!(
        "{prefix}-{}-{nonce}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

pub fn marked_root(prefix: &str) -> PathBuf {
    let directory = fresh_path(prefix);
    fs::create_dir_all(&directory).unwrap();
    for name in ROOTFS_DEFAULTS.directories {
        fs::create_dir_all(directory.join(name)).unwrap();
    }
    fs::write(
        directory.join(ROOTFS_DEFAULTS.marker_filename),
        ROOTFS_DEFAULTS.marker_contents,
    )
    .unwrap();
    directory
}
