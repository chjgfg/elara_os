//! Shared, bounded filesystem command adapter for the real shell and scripts.
use super::modules::{CommandOutput, InputUse};
use super::runtime::MAX_OUTPUT;
use crate::filesystem::{
    CleanupStatus, DataRoot, DirectoryDurability, FsError, WalkEntry, WalkOptions,
};
use crate::permissions::{self, Access};
use crate::users::Account;
use crate::virtual_path::VirtualPath;
use std::io::Write;
use std::{format, writeln};

fn authorized(user: &Account, path: &VirtualPath, access: Access) -> Result<(), String> {
    permissions::check(user, path, access).map_err(|error| error.to_string())
}
fn check_tree(user: &Account, text: &str) -> Result<(), FsError> {
    let path = VirtualPath::strict(text).map_err(|_| FsError::InvalidPath)?;
    permissions::check(user, &path, Access::Read)
        .map_err(|error| FsError::AccessDenied(error.to_string()))
}
fn bounded(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_OUTPUT {
        Err("filesystem command output exceeds 64 KiB; no complete result is available".into())
    } else {
        Ok(())
    }
}
fn durability(bytes: &mut Vec<u8>, label: &str, state: DirectoryDurability) -> Result<(), String> {
    if let DirectoryDurability::Unconfirmed(error) = state {
        writeln!(
            bytes,
            "Warning: committed; {label} durability unconfirmed: {error:?}"
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(super) fn execute(
    root: &DataRoot,
    user: &Account,
    cwd: &VirtualPath,
    command: &str,
    args: &[String],
) -> Result<CommandOutput, String> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    match command {
        "mv" | "cp" => {
            let [source, destination] = args else {
                return Err(format!(
                    "Usage: {command} <source> <destination> (no overwrite)"
                ));
            };
            let source = cwd.resolve(source).map_err(|error| error.to_string())?;
            let destination = cwd
                .resolve(destination)
                .map_err(|error| error.to_string())?;
            authorized(
                user,
                &source,
                if command == "mv" {
                    Access::Write
                } else {
                    Access::Read
                },
            )?;
            authorized(user, &destination, Access::Write)?;
            if command == "mv" {
                let receipt = root
                    .rename(&source.to_string(), &destination.to_string())
                    .map_err(|error| error.to_string())?;
                durability(&mut stderr, "source directory", receipt.source_directory)?;
                durability(
                    &mut stderr,
                    "destination directory",
                    receipt.destination_directory,
                )?;
            } else {
                let receipt = root
                    .copy_file(&source.to_string(), &destination.to_string())
                    .map_err(|error| error.to_string())?;
                durability(
                    &mut stderr,
                    "destination directory",
                    receipt.directory_durability,
                )?;
                if let CleanupStatus::Deferred(error) = receipt.cleanup {
                    writeln!(
                        stderr,
                        "Warning: committed; transaction cleanup deferred: {error:?}"
                    )
                    .map_err(|error| error.to_string())?;
                }
            }
        }
        "stat" | "find" | "du" => {
            let requested = match (command, args) {
                ("stat", [path]) => path.as_str(),
                ("stat", _) => return Err("Usage: stat <path>".into()),
                (_, []) => "",
                (_, [path]) => path.as_str(),
                _ => return Err(format!("Usage: {command} [path]")),
            };
            let path = cwd.resolve(requested).map_err(|error| error.to_string())?;
            authorized(user, &path, Access::Read)?;
            let path = path.to_string();
            match command {
                "stat" => {
                    let stat = root.stat(&path).map_err(|error| error.to_string())?;
                    writeln!(stdout, "path: {path}\nkind: {:?}\nlength: {}\nhost_readonly: {}\ncreated: {:?}\nmodified: {:?}\naccessed: {:?}",
                        stat.kind, stat.length, stat.host_readonly, stat.created, stat.modified, stat.accessed)
                        .map_err(|error| error.to_string())?;
                }
                "find" => {
                    let tree = root
                        .walk_checked(&path, WalkOptions::default(), |text| check_tree(user, text))
                        .map_err(|error| error.to_string())?;
                    for entry in tree {
                        match entry {
                            WalkEntry::Accessible { path, .. } => writeln!(stdout, "{path}"),
                            WalkEntry::BlockedName { parent, name, .. } => {
                                writeln!(stdout, "blocked-name parent={parent} name={name:?}")
                            }
                        }
                        .map_err(|error| error.to_string())?;
                        bounded(&stdout)?;
                    }
                }
                _ => {
                    let usage = root
                        .usage_checked(&path, WalkOptions::default(), |text| check_tree(user, text))
                        .map_err(|error| error.to_string())?;
                    writeln!(stdout, "logical_file_bytes: {}\nfiles: {}\ndirectories: {}\nblocked_links: {}\nblocked_names: {}\nother: {}\nphysical_usage: {:?}",
                        usage.logical_file_bytes, usage.files, usage.directories, usage.blocked_links,
                        usage.blocked_names, usage.other, usage.physical_usage)
                        .map_err(|error| error.to_string())?;
                }
            }
        }
        _ => return Err("unsupported filesystem command".into()),
    }
    bounded(&stdout)?;
    bounded(&stderr)?;
    Ok(CommandOutput {
        status: 0,
        stdout,
        stderr,
        input_use: InputUse::Untouched,
    })
}
