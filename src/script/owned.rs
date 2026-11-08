//! Explicit managed-object command surface; no legacy adoption or cleanup tool.
use super::modules::{CommandOutput, InputUse};
use super::runtime::MAX_OUTPUT;
use crate::filesystem::{DataRoot, FsError, OwnershipState, VirtualMode, DirectoryDurability, CleanupStatus, OwnedCommit};
use crate::permissions::{self, Access};
use crate::users::Account;
use crate::virtual_path::VirtualPath;
use std::io::Write;
use std::{format, writeln};

const HELP: &str = "owned create PATH [TEXT]\nowned read PATH\nowned write PATH TEXT\nowned append PATH TEXT\nowned mv SOURCE DESTINATION\nowned rm PATH\nowned chmod MODE PATH\nowned stat PATH\nNew files are private (0600). Unknown legacy files are never adopted.\n";
fn path(user: &Account, cwd: &VirtualPath, text: &str, access: Access) -> Result<String, String> {
    let path = cwd.resolve(text).map_err(|e| e.to_string())?;
    permissions::check(user, &path, access).map_err(|e| e.to_string())?;
    Ok(path.to_string())
}
fn commit(out: &mut Vec<u8>, errors: &mut Vec<u8>, commit: OwnedCommit) -> Result<(), String> {
    writeln!(out, "object: {}\nrevision: {}", commit.object.value(), commit.revision).map_err(|e| e.to_string())?;
    if let DirectoryDurability::Unconfirmed(kind) = commit.directory_durability {
        writeln!(errors, "Warning: committed; directory durability unconfirmed: {kind:?}").map_err(|e| e.to_string())?;
    }
    if let CleanupStatus::Deferred(kind) = commit.cleanup {
        writeln!(errors, "Warning: committed; owned cleanup deferred: {kind:?}").map_err(|e| e.to_string())?;
    }
    Ok(())
}
pub(super) fn execute(root: &DataRoot, user: &Account, cwd: &VirtualPath, args: &[String]) -> Result<CommandOutput, String> {
    let mut stdout = Vec::new(); let mut stderr = Vec::new();
    let result: Result<(), FsError> = (|| {
        match args {
            [] => { stdout.extend_from_slice(HELP.as_bytes()); }
            [verb] if verb == "help" => { stdout.extend_from_slice(HELP.as_bytes()); }
            [verb, name, text @ ..] if verb == "create" && text.len() <= 1 => {
                let name = path(user, cwd, name, Access::Write).map_err(FsError::AccessDenied)?;
                let body = text.first().map_or(&b""[..], |s| s.as_bytes());
                let receipt = root.create_owned_file(user, &name, body, VirtualMode::new(0o600)?)?;
                commit(&mut stdout, &mut stderr, receipt).map_err(FsError::AccessDenied)?;
            }
            [verb, name] if verb == "read" => {
                let name = path(user, cwd, name, Access::Read).map_err(FsError::AccessDenied)?;
                stdout = root.read_owned_file(user, &name, MAX_OUTPUT as u64)?;
            }
            [verb, name, text] if verb == "write" || verb == "append" => {
                let name = path(user, cwd, name, Access::Write).map_err(FsError::AccessDenied)?;
                if verb == "write" {
                    let receipt = root.replace_owned_file(user, &name, text.as_bytes())?;
                    commit(&mut stdout, &mut stderr, receipt).map_err(FsError::AccessDenied)?;
                } else { let count = root.append_owned_file(user, &name, text.as_bytes())?; writeln!(stdout, "{count}")?; }
            }
            [verb, from, to] if verb == "mv" => {
                let from = path(user, cwd, from, Access::Write).map_err(FsError::AccessDenied)?;
                let to = path(user, cwd, to, Access::Write).map_err(FsError::AccessDenied)?;
                let receipt = root.rename_owned_file(user, &from, &to)?;
                commit(&mut stdout, &mut stderr, receipt).map_err(FsError::AccessDenied)?;
            }
            [verb, name] if verb == "rm" => {
                let name = path(user, cwd, name, Access::Write).map_err(FsError::AccessDenied)?;
                let receipt = root.remove_owned_file(user, &name)?;
                commit(&mut stdout, &mut stderr, receipt).map_err(FsError::AccessDenied)?;
            }
            [verb, mode, name] if verb == "chmod" => {
                let bits = u16::from_str_radix(mode, 8).map_err(|_| FsError::InvalidOwnershipRecord)?;
                let name = path(user, cwd, name, Access::Write).map_err(FsError::AccessDenied)?;
                let receipt = root.chmod_owned_file(user, &name, VirtualMode::new(bits)?)?;
                commit(&mut stdout, &mut stderr, receipt).map_err(FsError::AccessDenied)?;
            }
            [verb, name] if verb == "stat" => {
                let name = path(user, cwd, name, Access::Read).map_err(FsError::AccessDenied)?;
                match root.ownership_as(user, &name)? {
                    OwnershipState::Managed(info) => writeln!(stdout, "object: {}\ngeneration: {}\npath: {}\nuid: {}\ngid: {}\nmode: {:03o}\nclass: {:?}\ndelete_condition: {:?}",
                        info.id.value(), info.storage_generation, info.path, info.owner.uid, info.owner.gid, info.mode.bits(), info.storage_class, info.delete_condition)?,
                    OwnershipState::LegacyUnknown => writeln!(stdout, "ownership: LegacyUnknown")?,
                    OwnershipState::Unconfirmed { id, reason } => writeln!(stdout, "object: {}\nownership: Unconfirmed({reason:?})", id.value())?,
                }
            }
            _ => return Err(FsError::AccessDenied(format!("Usage:\n{HELP}"))),
        }
        Ok(())
    })();
    result.map_err(|e| e.to_string())?;
    if stdout.len() > MAX_OUTPUT || stderr.len() > MAX_OUTPUT { return Err("owned output exceeds 64 KiB".into()); }
    Ok(CommandOutput { status: 0, stdout, stderr, input_use: InputUse::Untouched })
}
