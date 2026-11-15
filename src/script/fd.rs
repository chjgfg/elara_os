//! Descriptor command facade: parse requests, authorize opens, delegate all IO to filesystem.
use super::modules::{CommandOutput, InputUse};
use super::runtime::MAX_OUTPUT;
use crate::filesystem::{
    DataRoot, FileAccess, FileBehavior, FileCreation, FileHandle, FileMode, FilePosition, FsError,
    HandleTable,
};
use crate::permissions::Access;
use crate::users::Account;
use crate::virtual_path::VirtualPath;
use std::io::{SeekFrom, Write};
use std::{format, writeln};

pub struct FdSession {
    owner_uid: u32,
    table: HandleTable,
    scopes: std::collections::HashMap<FileHandle, crate::filesystem::TaskScopeId>,
}
impl FdSession {
    pub fn new(owner_uid: u32) -> Result<Self, FsError> {
        Ok(Self {
            owner_uid,
            table: HandleTable::new()?,
            scopes: Default::default(),
        })
    }
    pub fn with_limit(owner_uid: u32, limit: usize) -> Result<Self, FsError> {
        Ok(Self {
            owner_uid,
            table: HandleTable::with_limit(limit)?,
            scopes: Default::default(),
        })
    }
    pub fn owner_uid(&self) -> u32 {
        self.owner_uid
    }
    pub fn len(&self) -> usize {
        self.table.len()
    }
    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }
    /// Keep this table and its generations; do not replace it with a fresh 0:1 table.
    pub fn switch_owner(&mut self, uid: u32) -> usize {
        let closed = self.table.close_all();
        self.scopes.clear();
        self.owner_uid = uid;
        closed
    }
    pub fn finish_scope(&mut self, id: crate::filesystem::TaskScopeId) -> usize {
        let handles: Vec<_> = self
            .scopes
            .iter()
            .filter(|(_, scope)| **scope == id)
            .map(|(h, _)| *h)
            .collect();
        let mut closed = 0;
        for h in handles {
            self.scopes.remove(&h);
            if self.table.close(h).is_ok() {
                closed += 1;
            }
        }
        closed
    }
    fn handle(&self, token: &str) -> Result<FileHandle, String> {
        let (slot, generation) = token.split_once(':').ok_or("handle must be SLOT:GEN")?;
        if slot.is_empty()
            || generation.is_empty()
            || !slot.bytes().all(|b| b.is_ascii_digit())
            || !generation.bytes().all(|b| b.is_ascii_digit())
        {
            return Err("handle must contain two unsigned decimal numbers".into());
        }
        let slot = slot.parse::<usize>().map_err(|_| "slot is out of range")?;
        let generation = generation
            .parse::<u64>()
            .map_err(|_| "generation is out of range")?;
        self.table
            .resolve_token(slot, generation)
            .map_err(|e| e.to_string())
    }
    pub fn execute(
        &mut self,
        root: &DataRoot,
        user: &Account,
        cwd: &VirtualPath,
        args: &[String],
        input: &[u8],
    ) -> Result<CommandOutput, String> {
        self.execute_with_options(
            root,
            user,
            cwd,
            args,
            input,
            crate::filesystem::CreationOptions::default(),
        )
    }
    pub fn execute_with_options(
        &mut self,
        root: &DataRoot,
        user: &Account,
        cwd: &VirtualPath,
        args: &[String],
        input: &[u8],
        options: crate::filesystem::CreationOptions<'_>,
    ) -> Result<CommandOutput, String> {
        if self.owner_uid != user.session_uid() {
            return Err(
                "descriptor table belongs to another identity; close it on account transition"
                    .into(),
            );
        }
        let mut out = Vec::new();
        match args {
            [] => return Ok(output(HELP.as_bytes().to_vec())),
            [action] if action == "help" => return Ok(output(HELP.as_bytes().to_vec())),
            [action, path, access, sharing @ ..] if action == "open" && sharing.len() <= 1 => {
                let mode = mode(access)?;
                let share = match sharing.first().map(String::as_str) {
                    None | Some("shared") => crate::filesystem::vfs::ShareMode::Shared,
                    Some("exclusive") => crate::filesystem::vfs::ShareMode::Exclusive,
                    _ => return Err("sharing must be shared|exclusive".into()),
                };
                let path = cwd.resolve(path).map_err(|e| e.to_string())?;
                if mode.access() != FileAccess::WriteOnly {
                    root.check_io_path_as(user, &path.to_string(), Access::Read)
                        .map_err(|e| e.to_string())?;
                }
                if mode.access() != FileAccess::ReadOnly {
                    root.check_io_path_as(user, &path.to_string(), Access::Write)
                        .map_err(|e| e.to_string())?;
                }
                let handle = self
                    .table
                    .open_shared_with_options(root, user, &path.to_string(), mode, share, options)
                    .map_err(|e| e.to_string())?;
                if let Some(scope) = options.scope {
                    self.scopes.insert(handle, scope.id());
                }
                return Ok(output(token(handle).into_bytes()));
            }
            [action, handle, mode] if action == "lock" => {
                let mode = match mode.as_str() {
                    "shared" => crate::filesystem::vfs::LockMode::Shared,
                    "exclusive" => crate::filesystem::vfs::LockMode::Exclusive,
                    _ => return Err("lock must be shared|exclusive".into()),
                };
                self.table
                    .lock(self.handle(handle)?, mode)
                    .map_err(|e| e.to_string())?;
            }
            [action, handle] if action == "unlock" => {
                self.table
                    .unlock(self.handle(handle)?)
                    .map_err(|e| e.to_string())?;
            }
            [action, handle, count] if action == "read" => {
                let count = read_count(count)?;
                let handle = self.handle(handle)?;
                return Ok(output(
                    self.table.read(handle, count).map_err(|e| e.to_string())?,
                ));
            }
            [action, handle, offset, count] if action == "readat" => {
                let count = read_count(count)?;
                let offset = unsigned(offset)?;
                let handle = self.handle(handle)?;
                return Ok(output(
                    self.table
                        .read_at(handle, offset, count)
                        .map_err(|e| e.to_string())?,
                ));
            }
            [action, handle, text] if action == "write" => {
                let handle = self.handle(handle)?;
                return write_result(self.table.write(handle, text.as_bytes()), false);
            }
            [action, handle] if action == "write" => {
                let handle = self.handle(handle)?;
                return write_result(self.table.write(handle, input), true);
            }
            [action, handle, offset, text] if action == "writeat" => {
                let offset = unsigned(offset)?;
                let handle = self.handle(handle)?;
                return write_result(self.table.write_at(handle, offset, text.as_bytes()), false);
            }
            [action, handle, offset] if action == "writeat" => {
                let offset = unsigned(offset)?;
                let handle = self.handle(handle)?;
                return write_result(self.table.write_at(handle, offset, input), true);
            }
            [action, handle, offset, origin] if action == "seek" => {
                let position = match origin.as_str() {
                    "start" => SeekFrom::Start(unsigned(offset)?),
                    "current" => SeekFrom::Current(
                        offset
                            .parse()
                            .map_err(|_| "relative offset must be an i64")?,
                    ),
                    "end" => SeekFrom::End(
                        offset
                            .parse()
                            .map_err(|_| "relative offset must be an i64")?,
                    ),
                    _ => return Err("seek origin must be start|current|end".into()),
                };
                let handle = self.handle(handle)?;
                let offset = self
                    .table
                    .seek(handle, position)
                    .map_err(|e| e.to_string())?;
                writeln!(out, "{offset}").map_err(|e| e.to_string())?;
            }
            [action, handle, length] if action == "truncate" => {
                let length = unsigned(length)?;
                let handle = self.handle(handle)?;
                self.table
                    .truncate(handle, length)
                    .map_err(|e| e.to_string())?;
            }
            [action, left, right] if action == "same" => {
                let binding = self
                    .table
                    .compare_identity(self.handle(left)?, self.handle(right)?)
                    .map_err(|e| e.to_string())?;
                writeln!(out, "{binding:?}").map_err(|e| e.to_string())?;
            }
            [action, handle] if action == "stat" => {
                let handle = self.handle(handle)?;
                let info = self.table.info(handle).map_err(|e| e.to_string())?;
                writeln!(
                    out,
                    "path: {}\naccess: {:?}\ncreation: {:?}\nbehavior: {:?}",
                    info.opened_path,
                    info.mode.access(),
                    info.mode.creation(),
                    info.mode.behavior()
                )
                .map_err(|e| e.to_string())?;
                match info.position {
                    FilePosition::Known(offset) => writeln!(out, "position: {offset}"),
                    FilePosition::Unconfirmed => writeln!(out, "position: unconfirmed"),
                }
                .map_err(|e| e.to_string())?;
                writeln!(
                    out,
                    "length: {}\nowner_uid: {}",
                    info.length, self.owner_uid
                )
                .map_err(|e| e.to_string())?;
                match info.ownership {
                    crate::filesystem::OwnershipState::Managed(owner) => writeln!(
                        out,
                        "object: {}
generation: {}
uid: {}
gid: {}
mode: {:03o}",
                        owner.id.value(),
                        owner.storage_generation,
                        owner.owner.uid,
                        owner.owner.gid,
                        owner.mode.bits()
                    ),
                    crate::filesystem::OwnershipState::Virtual { filesystem, inode } => writeln!(
                        out,
                        "filesystem: {filesystem}
inode: {inode:?}"
                    ),
                    crate::filesystem::OwnershipState::LegacyUnknown => {
                        writeln!(out, "ownership: LegacyUnknown")
                    }
                    crate::filesystem::OwnershipState::Unconfirmed { id, reason } => writeln!(
                        out,
                        "object: {}
ownership: Unconfirmed({reason:?})",
                        id.value()
                    ),
                }
                .map_err(|e| e.to_string())?;
            }
            [action, handle] if action == "dup" => {
                let handle = self.handle(handle)?;
                let duplicate = self.table.duplicate(handle).map_err(|e| e.to_string())?;
                if let Some(scope) = options.scope {
                    self.scopes.insert(duplicate, scope.id());
                }
                return Ok(output(token(duplicate).into_bytes()));
            }
            [action, handle, mode] if action == "sync" => {
                let handle = self.handle(handle)?;
                match mode.as_str() {
                    "data" => self.table.sync_data(handle),
                    "all" => self.table.sync_all(handle),
                    _ => return Err("sync mode must be data|all".into()),
                }
                .map_err(|e| e.to_string())?;
            }
            [action, handle] if action == "close" => {
                let handle = self.handle(handle)?;
                self.table.close(handle).map_err(|e| e.to_string())?;
                self.scopes.remove(&handle);
            }
            [action] if action == "closeall" => {
                let closed = if let Some(scope) = options.scope {
                    self.finish_scope(scope.id())
                } else {
                    self.scopes.clear();
                    self.table.close_all()
                };
                writeln!(out, "{closed}").map_err(|e| e.to_string())?;
            }
            _ => return Err("invalid fd request; use 'fd help'".into()),
        }
        if out.len() > MAX_OUTPUT {
            return Err("descriptor output exceeds 64 KiB".into());
        }
        Ok(output(out))
    }
}
fn mode(name: &str) -> Result<FileMode, String> {
    let result = match name {
        "read" => return Ok(FileMode::READ),
        "create" => return Ok(FileMode::CREATE),
        "write" => FileMode::new(
            FileAccess::WriteOnly,
            FileCreation::Existing,
            FileBehavior::Preserve,
        ),
        "rw" => FileMode::new(
            FileAccess::ReadWrite,
            FileCreation::Existing,
            FileBehavior::Preserve,
        ),
        "append" => FileMode::new(
            FileAccess::WriteOnly,
            FileCreation::IfMissing,
            FileBehavior::Append,
        ),
        "truncate" => FileMode::new(
            FileAccess::WriteOnly,
            FileCreation::Existing,
            FileBehavior::Truncate,
        ),
        _ => return Err("open mode must be read|write|rw|create|append|truncate".into()),
    };
    result.map_err(|e| e.to_string())
}
fn unsigned(number: &str) -> Result<u64, String> {
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return Err("expected an unsigned decimal number".into());
    }
    number
        .parse()
        .map_err(|_| "number is out of u64 range".into())
}
fn read_count(number: &str) -> Result<usize, String> {
    let count = unsigned(number)?;
    if count > MAX_OUTPUT as u64 {
        return Err("fd read limit is 65536 bytes; cursor was not advanced".into());
    }
    Ok(count as usize)
}
fn token(handle: FileHandle) -> String {
    format!("{}:{}\n", handle.slot(), handle.generation())
}
fn output(stdout: Vec<u8>) -> CommandOutput {
    CommandOutput {
        status: 0,
        stdout,
        stderr: Vec::new(),
        input_use: InputUse::Untouched,
    }
}

/// Pure reporting adapter; a reported partial write is never retried.
pub fn write_result(
    result: Result<usize, FsError>,
    from_stdin: bool,
) -> Result<CommandOutput, String> {
    let consumed = |count| {
        if from_stdin {
            InputUse::Consumed(count)
        } else {
            InputUse::Untouched
        }
    };
    match result {
        Ok(bytes) => Ok(CommandOutput { status: 0, stdout: format!("{bytes}\n").into_bytes(), stderr: Vec::new(), input_use: consumed(bytes) }),
        Err(FsError::WritePositionUnconfirmed { bytes, error }) => {
            Ok(CommandOutput { status: 1, stdout: format!("{bytes}\n").into_bytes(), stderr: format!("Error: {bytes} bytes were written, but position is unconfirmed: {error}; no retry performed\n").into_bytes(), input_use: consumed(bytes) })
        }
        Err(error) => Err(error.to_string()),
    }
}
const HELP: &str = "fd open PATH read|write|rw|create|append|truncate [shared|exclusive]
fd same LEFT RIGHT (opened object identity: SameObject/DifferentObject/Unconfirmed)
fd lock SLOT:GEN shared|exclusive
fd unlock SLOT:GEN
fd read SLOT:GEN COUNT
fd readat SLOT:GEN OFFSET COUNT
fd write SLOT:GEN [TEXT] (stdin when TEXT omitted)
fd writeat SLOT:GEN OFFSET [TEXT]
fd seek SLOT:GEN OFFSET start|current|end
fd truncate SLOT:GEN LENGTH
fd stat SLOT:GEN
fd dup SLOT:GEN
fd sync SLOT:GEN data|all
fd close SLOT:GEN
fd closeall
Tokens are local to this shell's owner-bound table.
";
