//! Internal filesystem control. Never invokes mount(2), host tools or devices.
use super::modules::{CommandOutput, InputUse};
use crate::{
    filesystem::{vfs::Limits, DataRoot},
    users::Account,
    virtual_path::VirtualPath,
};
use std::io::Write;
use std::{format, writeln};
const HELP: &str =
    "vfs mount PATH BYTES INODES (volatile memory; /mnt/NAME or a new name below an internal mount)
vfs pseudo proc|sys|dev PATH
vfs unmount PATH
vfs mounts (User or System provider)
vfs inode PATH
vfs link SOURCE DEST (same volume, or managed Host regular files in one resource domain)
vfs symlink TARGET DEST (internal: volume-relative; Host: link-parent-relative or virtual-root-absolute; max 40)
vfs symlink-global TARGET DEST (absolute internal namespace target; no host fallback)
vfs readlink PATH
vfs rename SOURCE DEST (same backend; managed Host files or empty target directory; old FDs survive)
vfs stats PATH (logical bytes, allocated data pages, COW copies and IO)
vfs quota PATH UID BYTES INODES
vfs dedup MOUNT (explicit in-memory page interning; not disk reclamation)
vfs snapshot SOURCE DEST
vfs clone SOURCE DEST
vfs overlay LOWER UPPER DEST [WHITEOUT...] (frozen merged view; paths relative to volume root)
vfs save MOUNT IMAGE
vfs load IMAGE MOUNT
vfs disk-create MOUNT IMAGE SLOTS CACHE_PAGES (new ordinary image only)
vfs disk-save MOUNT IMAGE CACHE_PAGES (incremental inode/page COW commit)
vfs disk-load IMAGE MOUNT CACHE_PAGES
vfs disk-stat IMAGE CACHE_PAGES
vfs disk-repair IMAGE CACHE_PAGES (republish verified survivor, not lost bytes)
vfs keygen KEYFILE (new root-owned 0600 random 256-bit key)
vfs seal MOUNT IMAGE KEYFILE (authenticated encrypted checkpoint)
vfs unseal IMAGE MOUNT KEYFILE
Ordinary file commands, ACL, find/du, exec and fd operate on mounted paths.
Cwd and nested-mount parent leases prevent rename/removal until released.
Snapshots/clones share immutable 4 KiB data pages; metadata is copied.
Saved snapshots retain frozen access after load/disk-load/unseal; clone explicitly to write.
Stats reports ReadWrite/FrozenSnapshot/RecoveryReadOnly/FrozenRecoveredReadOnly/VirtualProvider.
Default /proc, /sys and /dev are immutable virtual providers, not host devices.
Memory data is lost on unmount or exit unless explicitly saved.
";
pub(super) fn execute(
    root: &DataRoot,
    user: &Account,
    cwd: &VirtualPath,
    args: &[String],
) -> Result<CommandOutput, String> {
    let resolve = |s: &str| {
        cwd.resolve(s)
            .map(|p| p.to_string())
            .map_err(|e| e.to_string())
    };
    let number = |s: &str| {
        s.parse::<u64>()
            .map_err(|_| "expected unsigned integer".to_owned())
    };
    let mut stdout = Vec::new();
    match args {
        [verb, target, destination] if verb == "symlink-global" => {
            root.symlink_global_as(user, target, &resolve(destination)?)
                .map_err(|e| e.to_string())?;
        }

        [] => stdout.extend_from_slice(HELP.as_bytes()),
        [verb] if verb == "help" => stdout.extend_from_slice(HELP.as_bytes()),
        [verb, p, b, n] if verb == "mount" => root
            .mount_memory_as(
                user,
                &resolve(p)?,
                Limits {
                    bytes: number(b)?,
                    inodes: number(n)?,
                },
            )
            .map_err(|e| e.to_string())?,
        [verb, kind, point] if verb == "pseudo" => {
            let kind = match kind.as_str() {
                "proc" => crate::filesystem::vfs::PseudoKind::Proc,
                "sys" => crate::filesystem::vfs::PseudoKind::Sys,
                "dev" => crate::filesystem::vfs::PseudoKind::Dev,
                _ => return Err("pseudo kind must be proc|sys|dev".into()),
            };
            root.mount_pseudo_as(user, &resolve(point)?, kind)
                .map_err(|e| e.to_string())?;
        }
        [verb, key] if verb == "keygen" => {
            let receipt = root
                .create_image_key_as(user, &resolve(key)?)
                .map_err(|e| e.to_string())?;
            writeln!(stdout, "{receipt:?}").map_err(|e| e.to_string())?;
        }
        [verb, point, image, key] if verb == "seal" => {
            let receipt = root
                .seal_volume_as(user, &resolve(point)?, &resolve(image)?, &resolve(key)?)
                .map_err(|e| e.to_string())?;
            writeln!(stdout, "{receipt:?}").map_err(|e| e.to_string())?;
        }
        [verb, image, point, key] if verb == "unseal" => {
            let recovery = root
                .unseal_volume_as(user, &resolve(image)?, &resolve(point)?, &resolve(key)?)
                .map_err(|e| e.to_string())?;
            writeln!(stdout, "{recovery:?}").map_err(|e| e.to_string())?;
        }
        [verb, point, image, slots, cache] if verb == "disk-create" => {
            let stats = root
                .create_block_image_as(
                    user,
                    &resolve(point)?,
                    &resolve(image)?,
                    number(slots)?.try_into().map_err(|_| "slots overflow")?,
                    number(cache)?.try_into().map_err(|_| "cache overflow")?,
                )
                .map_err(|e| e.to_string())?;
            writeln!(stdout, "{stats:?}").map_err(|e| e.to_string())?;
        }
        [verb, point, image, cache] if verb == "disk-save" => {
            let stats = root
                .save_block_image_as(
                    user,
                    &resolve(point)?,
                    &resolve(image)?,
                    number(cache)?.try_into().map_err(|_| "cache overflow")?,
                )
                .map_err(|e| e.to_string())?;
            writeln!(stdout, "{stats:?}").map_err(|e| e.to_string())?;
        }
        [verb, image, point, cache] if verb == "disk-load" => {
            let recovery = root
                .load_block_image_as(
                    user,
                    &resolve(image)?,
                    &resolve(point)?,
                    number(cache)?.try_into().map_err(|_| "cache overflow")?,
                )
                .map_err(|e| e.to_string())?;
            writeln!(stdout, "{recovery:?}").map_err(|e| e.to_string())?;
        }
        [verb, image, cache] if verb == "disk-stat" => {
            let stats = root
                .block_image_stats_as(
                    user,
                    &resolve(image)?,
                    number(cache)?.try_into().map_err(|_| "cache overflow")?,
                )
                .map_err(|e| e.to_string())?;
            writeln!(stdout, "{stats:?}").map_err(|e| e.to_string())?;
        }
        [verb, image, cache] if verb == "disk-repair" => {
            let stats = root
                .repair_block_image_as(
                    user,
                    &resolve(image)?,
                    number(cache)?.try_into().map_err(|_| "cache overflow")?,
                )
                .map_err(|e| e.to_string())?;
            writeln!(stdout, "{stats:?}").map_err(|e| e.to_string())?;
        }
        [verb, point, image] if verb == "save" => {
            let (receipt, stats) = root
                .save_volume_as(user, &resolve(point)?, &resolve(image)?)
                .map_err(|e| e.to_string())?;
            writeln!(
                stdout,
                "{stats:?} durability={:?}",
                receipt.directory_durability
            )
            .map_err(|e| e.to_string())?;
        }
        [verb, image, point] if verb == "load" => writeln!(
            stdout,
            "{:?}",
            root.load_volume_as(user, &resolve(image)?, &resolve(point)?)
                .map_err(|e| e.to_string())?
        )
        .map_err(|e| e.to_string())?,
        [verb, point] if verb == "dedup" => {
            let stats = root
                .deduplicate_volume_as(user, &resolve(point)?)
                .map_err(|e| e.to_string())?;
            writeln!(stdout, "{stats:?}").map_err(|e| e.to_string())?;
        }
        [verb, p] if verb == "unmount" => root
            .unmount_as(user, &resolve(p)?)
            .map_err(|e| e.to_string())?,
        [verb] if verb == "mounts" => {
            for info in root.mount_info_as(user).map_err(|e| e.to_string())? {
                writeln!(stdout, "{} {:?}", info.path, info.kind).map_err(|e| e.to_string())?;
            }
        }
        [verb, p] if verb == "inode" => writeln!(
            stdout,
            "{:?}",
            root.inode_as(user, &resolve(p)?, false)
                .map_err(|e| e.to_string())?
        )
        .map_err(|e| e.to_string())?,
        [verb, source, dest] if verb == "link" => root
            .hardlink_as(user, &resolve(source)?, &resolve(dest)?)
            .map_err(|e| e.to_string())?,
        [verb, target, dest] if verb == "symlink" => root
            .symlink_as(user, target, &resolve(dest)?)
            .map_err(|e| e.to_string())?,
        [verb, p] if verb == "readlink" => writeln!(
            stdout,
            "{}",
            root.readlink_as(user, &resolve(p)?)
                .map_err(|e| e.to_string())?
        )
        .map_err(|e| e.to_string())?,
        [verb, source, dest] if verb == "rename" => root
            .rename_replace_as(user, &resolve(source)?, &resolve(dest)?)
            .map_err(|e| e.to_string())?,
        [verb, p] if verb == "stats" => writeln!(
            stdout,
            "{:?}",
            root.volume_stats_as(user, &resolve(p)?)
                .map_err(|e| e.to_string())?
        )
        .map_err(|e| e.to_string())?,
        [verb, p, uid, b, n] if verb == "quota" => root
            .volume_quota_as(
                user,
                &resolve(p)?,
                u32::try_from(number(uid)?).map_err(|_| "uid too large")?,
                Limits {
                    bytes: number(b)?,
                    inodes: number(n)?,
                },
            )
            .map_err(|e| e.to_string())?,
        [verb, lower, upper, dest, whiteouts @ ..] if verb == "overlay" => {
            root.overlay_mount_as(
                user,
                &resolve(lower)?,
                &resolve(upper)?,
                &resolve(dest)?,
                whiteouts,
            )
            .map_err(|e| e.to_string())?;
        }
        [verb, source, dest] if verb == "snapshot" || verb == "clone" => root
            .snapshot_mount_as(user, &resolve(source)?, &resolve(dest)?, verb == "snapshot")
            .map_err(|e| e.to_string())?,
        _ => return Err(format!("Usage:\n{HELP}")),
    }
    Ok(CommandOutput {
        status: 0,
        stdout,
        stderr: Vec::new(),
        input_use: InputUse::Untouched,
    })
}
