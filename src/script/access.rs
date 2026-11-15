//! Literal command adapter for virtual modes, shared groups and access ACLs.
use super::modules::{CommandOutput, InputUse};
use crate::filesystem::{Acl, DataRoot, DefaultAcl, FsError, VirtualMode};
use crate::users::Account;
use crate::virtual_path::VirtualPath;
use std::io::Write;
use std::{format, writeln};
const HELP: &str = "chmod MODE PATH\nchgrp GID PATH\ngetfacl PATH\nsetfacl u:UID:BITS|g:GID:BITS|g::BITS|m::BITS[,ENTRY...] PATH\nsetfacl -b PATH\nsetfacl -d u::BITS|u:UID:BITS|g::BITS|g:GID:BITS|o::BITS|m::BITS[,ENTRY...] PATH\nsetfacl -k PATH\ngroupadd GID NAME\ngroupmem add|del GID UID\ngroupmem primary GID UID\ngroups\nBITS is octal 0..7; ACL grants remain below path/category limits. MODE supports rwx, directory sticky/setgid and virtual file setuid/setgid; never host permissions.\n";
pub(super) fn execute(root: &DataRoot, user: &Account, cwd: &VirtualPath, name: &str, args: &[String]) -> Result<CommandOutput, String> {
    let mut stdout = Vec::new();
    let resolve = |text: &str| cwd.resolve(text).map(|p| p.to_string()).map_err(|e| e.to_string());
    match (name, args) {
        ("chmod", [bits, path]) => { let mode = VirtualMode::new(octal(bits)?).map_err(|e| e.to_string())?; root.set_access_as(user, &resolve(path)?, Some(mode), None, None).map_err(|e| e.to_string())?; }
        ("chgrp", [gid, path]) => { root.set_access_as(user, &resolve(path)?, None, Some(number(gid)?), None).map_err(|e| e.to_string())?; }
        ("getfacl", [path]) => {
            let path = resolve(path)?; let acl = root.acl_as(user, &path).map_err(|e| e.to_string())?;
            let (owner, mode) = if let Some(info) = root.directory_info_as(user, &path).map_err(|e| e.to_string())? { (info.owner, info.mode) }
                else { file_access(root, user, &path)? };
            writeln!(stdout, "path: {path}\nuid: {}\ngid: {}\nmode: {:03o}\nu::{}\ng::{}\no::{}", owner.uid, owner.gid, mode.bits(), (mode.bits() >> 6) & 7, acl.group.unwrap_or(((mode.bits() >> 3) & 7) as u8), mode.bits() & 7).map_err(|e| e.to_string())?;
            for (uid,bits) in acl.users { writeln!(stdout, "u:{uid}:{bits}").map_err(|e| e.to_string())?; }
            for (gid,bits) in acl.groups { writeln!(stdout, "g:{gid}:{bits}").map_err(|e| e.to_string())?; }
            if let Some(mask) = acl.mask { writeln!(stdout, "m::{mask}").map_err(|e| e.to_string())?; }
            if root.directory_info_as(user, &path).map_err(|e| e.to_string())?.is_some() {
                if let Some(default) = root.default_acl_as(user, &path).map_err(|e| e.to_string())? {
                    writeln!(stdout, "default:u::{}", default.owner).map_err(|e| e.to_string())?;
                    writeln!(stdout, "default:g::{}", default.acl.group.ok_or_else(|| FsError::InvalidOwnershipRecord.to_string())?).map_err(|e| e.to_string())?;
                    writeln!(stdout, "default:o::{}", default.other).map_err(|e| e.to_string())?;
                    for (uid, bits) in default.acl.users { writeln!(stdout, "default:u:{uid}:{bits}").map_err(|e| e.to_string())?; }
                    for (gid, bits) in default.acl.groups { writeln!(stdout, "default:g:{gid}:{bits}").map_err(|e| e.to_string())?; }
                    if let Some(mask) = default.acl.mask { writeln!(stdout, "default:m::{mask}").map_err(|e| e.to_string())?; }
                }
            }
        }
        ("setfacl", [flag, spec, path]) if flag == "-d" => {
            let path = resolve(path)?;
            let info = root.directory_info_as(user, &path).map_err(|e| e.to_string())?.ok_or_else(|| FsError::NotDirectory.to_string())?;
            let mut default = root.default_acl_as(user, &path).map_err(|e| e.to_string())?.unwrap_or_else(|| {
                let mut base = DefaultAcl::from_mode(info.mode);
                base.acl.group = info.acl.group.or(base.acl.group); base
            });
            let had_mask = default.acl.mask.is_some(); default.acl.mask = None;
            for text in spec.split(',') {
                match entry(text)? {
                    AclEntry::Owner(bits) => default.owner = bits, AclEntry::Other(bits) => default.other = bits,
                    AclEntry::User(uid, bits) => { default.acl.users.insert(uid, bits); },
                    AclEntry::Group(gid, bits) => { default.acl.groups.insert(gid, bits); },
                    AclEntry::GroupObj(bits) => default.acl.group = Some(bits), AclEntry::Mask(bits) => default.acl.mask = Some(bits),
                }
            }
            default.acl.validate().map_err(|e| e.to_string())?;
            if default.acl.mask.is_none() && (had_mask || !default.acl.users.is_empty() || !default.acl.groups.is_empty()) { default.acl.recalculate_mask(info.mode); }
            root.set_default_acl_as(user, &path, Some(default)).map_err(|e| e.to_string())?;
        }
        ("setfacl", [flag, path]) if flag == "-k" => { root.set_default_acl_as(user, &resolve(path)?, None).map_err(|e| e.to_string())?; }
        ("setfacl", [spec, path]) => {
            let path = resolve(path)?;
            if spec == "-b" {
                // -b removes access extensions only; retain the actual owning-group
                // entry, not the mask displayed in mode. Default ACL is unchanged.
                let acl = root.acl_as(user, &path).map_err(|e| e.to_string())?;
                let current = if let Some(info) = root.directory_info_as(user, &path).map_err(|e| e.to_string())? { info.mode }
                    else { file_access(root, user, &path)?.1 };
                let group = acl.group.unwrap_or(((current.bits() >> 3) & 7) as u8);
                let mode = VirtualMode::new((current.bits() & !0o070) | (u16::from(group) << 3)).map_err(|e| e.to_string())?;
                root.set_access_as(user, &path, Some(mode), None, Some(Acl::default())).map_err(|e| e.to_string())?;
            } else {
                let mut acl = root.acl_as(user, &path).map_err(|e| e.to_string())?; acl.mask = None;
                for text in spec.split(',') {
                    match entry(text)? {
                        AclEntry::User(uid, bits) => { acl.users.insert(uid, bits); }, AclEntry::Group(gid, bits) => { acl.groups.insert(gid, bits); },
                        AclEntry::GroupObj(bits) => acl.group = Some(bits), AclEntry::Mask(bits) => acl.mask = Some(bits),
                        AclEntry::Owner(_) | AclEntry::Other(_) => return Err(HELP.into()),
                    }
                }
                root.set_access_as(user, &path, None, None, Some(acl)).map_err(|e| e.to_string())?;
            }
        }
        ("groupadd", [gid, name]) => { root.group_add_as(user, number(gid)?, name).map_err(|e| e.to_string())?; }
        ("groupmem", [action, gid, uid]) => {
            let gid = number(gid)?; let uid = number(uid)?;
            match action.as_str() {
                "add" | "del" => root.group_member_as(user, gid, uid, action == "add").map_err(|e| e.to_string())?,
                "primary" => root.primary_group_as(user, uid, gid).map_err(|e| e.to_string())?, _ => return Err(HELP.into()),
            }
        }
        ("groups", []) => {
            let credentials = root.credentials_as(user).map_err(|e| e.to_string())?;
            writeln!(stdout, "uid: {}\nprimary_gid: {}", credentials.uid, credentials.gid).map_err(|e| e.to_string())?;
            for (gid, group) in root.groups_as(user).map_err(|e| e.to_string())? { writeln!(stdout, "{gid}: {} {:?}", group.name, group.members).map_err(|e| e.to_string())?; }
        }
        _ => return Err(format!("Usage:\n{HELP}")),
    }
    if stdout.len() > super::runtime::MAX_OUTPUT { return Err("access output exceeds 64 KiB".into()); }
    Ok(CommandOutput { status: 0, stdout, stderr: Vec::new(), input_use: InputUse::Untouched })
}
fn file_access(root: &DataRoot, user: &Account, path: &str) -> Result<(crate::filesystem::FileOwner, VirtualMode), String> {
    match root.ownership_as(user, path).map_err(|e| e.to_string())? {
        crate::filesystem::OwnershipState::Managed(info) => Ok((info.owner, info.mode)),
        crate::filesystem::OwnershipState::Virtual { inode, .. } => Ok((inode.owner, inode.mode)),
        _ => Err(FsError::LegacyOwnershipUnknown.to_string()),
    }
}
fn number(text: &str) -> Result<u32, String> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) { return Err("ID must be unsigned decimal".into()); }
    text.parse().map_err(|_| "ID out of range".into())
}
fn octal(text: &str) -> Result<u16, String> {
    if text.is_empty() || !text.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return Err("mode must be octal 0000..7777; special bits depend on object type".into());
    }
    u16::from_str_radix(text, 8).map_err(|_| "mode must be octal 0000..7777; special bits depend on object type".into())
}

#[derive(Debug, PartialEq, Eq)]
enum AclEntry { Owner(u8), User(u32, u8), GroupObj(u8), Group(u32, u8), Other(u8), Mask(u8) }
fn entry(text: &str) -> Result<AclEntry, String> {
    let mut parts = text.split(':');
    let kind = parts.next().ok_or(HELP)?; let id = parts.next().ok_or(HELP)?; let bits = parts.next().ok_or(HELP)?;
    if parts.next().is_some() { return Err(HELP.into()); }
    if bits.is_empty() || !bits.bytes().all(|b| (b'0'..=b'7').contains(&b)) { return Err("ACL bits must be 0..7".into()); }
    let bits = u8::from_str_radix(bits, 8).map_err(|_| "ACL bits must be 0..7")?;
    if bits > 7 { return Err("ACL bits must be 0..7".into()); }
    Ok(match (kind, id) {
        ("u", "") => AclEntry::Owner(bits), ("g", "") => AclEntry::GroupObj(bits), ("o", "") => AclEntry::Other(bits), ("m", "") => AclEntry::Mask(bits),
        ("u", id) => AclEntry::User(number(id)?, bits), ("g", id) => AclEntry::Group(number(id)?, bits), _ => return Err(HELP.into()),
    })
}

#[cfg(test)]
#[path = "../../tests/script/access.rs"]
mod tests;
