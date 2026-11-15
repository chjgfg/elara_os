//! Versioned text ownership records. IDs belong to this virtual filesystem,
//! never to a guessed host inode, pathname, mtime, or account directory prefix.
use super::FsError;
use crate::virtual_path::VirtualPath;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::{format, matches, write, writeln};

pub(super) const MAX_OWNED_OBJECTS: usize = 4096;
pub(super) const MAX_CATALOG_BYTES: usize = 400 * 1024;
const MAGIC: &str = "ELARA_OWNERSHIP_CATALOG_V1";
const MAGIC_V7: &str = "ELARA_OWNERSHIP_CATALOG_V7";
const MAGIC_V6: &str = "ELARA_OWNERSHIP_CATALOG_V6";
const MAGIC_V2: &str = "ELARA_OWNERSHIP_CATALOG_V2";
const MAGIC_V5: &str = "ELARA_OWNERSHIP_CATALOG_V5";
const MAGIC_V4: &str = "ELARA_OWNERSHIP_CATALOG_V4";
const MAGIC_V3: &str = "ELARA_OWNERSHIP_CATALOG_V3";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObjectId(pub(super) u64);
impl ObjectId {
    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct VirtualMode(u16);
impl<'de> Deserialize<'de> for VirtualMode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bits = u16::deserialize(deserializer)?;
        Self::new(bits).map_err(serde::de::Error::custom)
    }
}
impl VirtualMode {
    pub fn new(bits: u16) -> Result<Self, FsError> {
        if bits & !0o7777 != 0 {
            return Err(FsError::InvalidOwnershipRecord);
        }
        Ok(Self(bits))
    }
    pub fn bits(self) -> u16 {
        self.0
    }
    pub fn is_setgid(self) -> bool {
        self.0 & 0o2000 != 0
    }
    pub fn is_setuid(self) -> bool {
        self.0 & 0o4000 != 0
    }
    pub(super) fn without_privilege(self) -> Self {
        Self(self.0 & !0o6000)
    }
    pub(super) fn require_directory(self) -> Result<(), FsError> {
        if self.is_setuid() {
            return Err(FsError::InvalidOwnershipRecord);
        }
        Ok(())
    }
    pub fn is_sticky(self) -> bool {
        self.0 & 0o1000 != 0
    }
    pub(super) fn require_regular_file(self) -> Result<(), FsError> {
        if self.0 & 0o1000 != 0 {
            return Err(FsError::DirectoryModeOnFile);
        }
        Ok(())
    }
    pub fn permits(self, owner: FileOwner, uid: u32, read: bool, write: bool) -> bool {
        if uid == 0 {
            return true;
        }
        // Initial virtual groups are private primary groups, gid == account uid.
        let shift = if uid == owner.uid {
            6
        } else if uid == owner.gid {
            3
        } else {
            0
        };
        let granted = ((self.0 >> shift) & 7) as u8;
        (!read || granted & 4 != 0) && (!write || granted & 2 != 0)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileOwner {
    pub uid: u32,
    pub gid: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeleteCondition {
    OwnerRequest,
    TaskExit(super::access_model::TaskScopeId),
    ResourceDelete(super::ResourceId),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnedStorageClass {
    UserData,
    Temporary,
    RuntimeState,
    Application,
    Cache,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostFileKind {
    #[default]
    Regular,
    Symlink {
        target: String,
    },
}
impl HostFileKind {
    pub fn is_regular(&self) -> bool {
        matches!(self, Self::Regular)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedFileInfo {
    pub kind: HostFileKind,
    pub id: ObjectId,
    pub storage_generation: u64,
    pub path: String,
    pub owner: FileOwner,
    pub mode: VirtualMode,
    pub storage_class: OwnedStorageClass,
    pub delete_condition: DeleteCondition,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnershipState {
    Virtual {
        filesystem: u64,
        inode: super::vfs::InodeStat,
    },
    LegacyUnknown,
    Managed(OwnedFileInfo),
    Unconfirmed {
        id: ObjectId,
        reason: std::io::ErrorKind,
    },
}

pub(super) fn key(path: &str) -> Result<String, FsError> {
    let path = VirtualPath::strict(path)
        .map_err(|_| FsError::InvalidPath)?
        .to_string();
    if std::cfg!(windows) {
        Ok(path.to_ascii_lowercase())
    } else {
        Ok(path)
    }
}
pub(super) fn hex(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len() * 2);
    for byte in text.as_bytes() {
        write!(encoded, "{byte:02x}").expect("string formatting");
    }
    encoded
}
pub(super) fn unhex(text: &str) -> Result<String, FsError> {
    if !text.len().is_multiple_of(2) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(FsError::InvalidOwnershipRecord);
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    for chunk in text.as_bytes().chunks_exact(2) {
        let digit = |b: u8| match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            _ => None,
        };
        let high = digit(chunk[0]).ok_or(FsError::InvalidOwnershipRecord)?;
        let low = digit(chunk[1]).ok_or(FsError::InvalidOwnershipRecord)?;
        bytes.push((high << 4) | low);
    }
    String::from_utf8(bytes).map_err(|_| FsError::InvalidOwnershipRecord)
}
// The directory owns names only. Authorization and lifecycle metadata live once
// per object; OwnedFileInfo is a detached path projection, never an authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OwnedInode {
    #[serde(default, skip_serializing_if = "HostFileKind::is_regular")]
    pub kind: HostFileKind,
    pub storage_generation: u64,
    pub owner: FileOwner,
    pub mode: VirtualMode,
    pub storage_class: OwnedStorageClass,
    pub delete_condition: DeleteCondition,
}
impl OwnedInode {
    fn from_info(info: &OwnedFileInfo) -> Self {
        Self {
            kind: info.kind.clone(),
            storage_generation: info.storage_generation,
            owner: info.owner,
            mode: info.mode,
            storage_class: info.storage_class,
            delete_condition: info.delete_condition,
        }
    }
    fn project(&self, id: ObjectId, path: &str) -> OwnedFileInfo {
        OwnedFileInfo {
            kind: self.kind.clone(),
            id,
            path: path.to_owned(),
            storage_generation: self.storage_generation,
            owner: self.owner,
            mode: self.mode,
            storage_class: self.storage_class,
            delete_condition: self.delete_condition,
        }
    }
}
pub(super) type OwnedEntries = super::inode_table::InodeTable<ObjectId, OwnedInode>;
impl OwnedEntries {
    pub fn get(&self, path: &str) -> Option<OwnedFileInfo> {
        let id = *self.names.get(path)?;
        Some(self.inodes.get(&id)?.project(id, path))
    }
    pub fn values(&self) -> impl Iterator<Item = OwnedFileInfo> + '_ {
        self.names
            .iter()
            .map(|(path, id)| self.inodes[id].project(*id, path))
    }
    pub fn insert(&mut self, path: String, info: OwnedFileInfo) -> Result<(), FsError> {
        if path != info.path || info.id.0 == 0 {
            return Err(FsError::InvalidOwnershipRecord);
        }
        self.bind(path, info.id, OwnedInode::from_info(&info), true)
    }
    pub fn remove(&mut self, path: &str) -> Option<OwnedFileInfo> {
        let (id, inode) = self.unbind(path)?;
        Some(inode.project(id, path))
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Catalog {
    pub revision: u64,
    pub next_id: u64,
    pub entries: OwnedEntries,
}
impl Catalog {
    pub fn empty() -> Self {
        Self {
            revision: 1,
            next_id: 1,
            entries: OwnedEntries::default(),
        }
    }
    pub fn allocate(
        &mut self,
        path: &str,
        owner: FileOwner,
        mode: VirtualMode,
        storage_class: OwnedStorageClass,
    ) -> Result<OwnedFileInfo, FsError> {
        mode.require_regular_file()?;
        if self.entries.len() >= MAX_OWNED_OBJECTS {
            return Err(FsError::OwnershipCapacity);
        }
        let path = key(path)?;
        if self.entries.contains_key(&path) {
            return Err(FsError::AlreadyExists);
        }
        let id = self.next_id;
        let next_id = id
            .checked_add(1)
            .ok_or(FsError::OwnershipIdentityExhausted)?;
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(FsError::OwnershipIdentityExhausted)?;
        self.next_id = next_id;
        self.revision = revision;
        let info = OwnedFileInfo {
            kind: HostFileKind::Regular,
            id: ObjectId(id),
            storage_generation: 1,
            path: path.clone(),
            owner,
            mode,
            storage_class,
            delete_condition: DeleteCondition::OwnerRequest,
        };
        self.entries.insert(path, info.clone())?;
        Ok(info)
    }
    pub fn encode(&self) -> Result<Vec<u8>, FsError> {
        let mut named = std::collections::BTreeMap::<ObjectId, usize>::new();
        for id in self.entries.names.values() {
            *named.entry(*id).or_default() += 1;
        }
        if self.revision == 0
            || self.next_id == 0
            || self
                .entries
                .names
                .values()
                .any(|id| !self.entries.inodes.contains_key(id))
            || self
                .entries
                .inodes
                .keys()
                .any(|id| id.0 == 0 || id.0 >= self.next_id || !named.contains_key(id))
            || self
                .entries
                .names
                .keys()
                .any(|path| key(path).map_or(true, |canonical| canonical != *path))
            || self
                .entries
                .inodes
                .values()
                .any(|inode| inode.storage_generation == 0)
        {
            return Err(FsError::InvalidOwnershipRecord);
        }
        if self.entries.len() > MAX_OWNED_OBJECTS {
            return Err(FsError::OwnershipCapacity);
        }
        let links = self
            .entries
            .inodes
            .values()
            .any(|inode| !inode.kind.is_regular());
        for (id, inode) in &self.entries.inodes {
            if let HostFileKind::Symlink { target } = &inode.kind {
                super::host_symlinks::validate_target(target)?;
                if named.get(id) != Some(&1) {
                    return Err(FsError::InvalidOwnershipRecord);
                }
            }
        }
        let multiple = self.entries.names.len() != self.entries.inodes.len();
        let v5 = self
            .entries
            .values()
            .any(|i| matches!(i.delete_condition, DeleteCondition::ResourceDelete(_)));
        let v4 = v5
            || self
                .entries
                .values()
                .any(|i| i.storage_class == OwnedStorageClass::RuntimeState);
        let v3 = v4 || self.entries.values().any(|i| i.mode.bits() & 0o6000 != 0);
        let v2 = v3
            || self
                .entries
                .values()
                .any(|i| i.delete_condition != DeleteCondition::OwnerRequest);
        let magic = if v5 {
            MAGIC_V5
        } else if v4 {
            MAGIC_V4
        } else if v3 {
            MAGIC_V3
        } else if v2 {
            MAGIC_V2
        } else {
            MAGIC
        };
        let mut text = format!("{magic}\n{} {}\n", self.revision, self.next_id);
        for info in self.entries.values() {
            info.mode
                .require_regular_file()
                .map_err(|_| FsError::InvalidOwnershipRecord)?;
            if (info.storage_class == OwnedStorageClass::RuntimeState)
                != super::scope::runtime_path(&info.path)
                || info.storage_class == OwnedStorageClass::RuntimeState
                    && !matches!(info.delete_condition, DeleteCondition::TaskExit(_))
            {
                return Err(FsError::InvalidOwnershipRecord);
            }
            let resource_kind = match info.storage_class {
                OwnedStorageClass::Application => Some(super::ResourceKind::Application),
                OwnedStorageClass::Cache => Some(super::ResourceKind::Cache),
                OwnedStorageClass::UserData
                    if matches!(info.delete_condition, DeleteCondition::ResourceDelete(_)) =>
                {
                    Some(super::ResourceKind::ApplicationData)
                }
                _ => None,
            };
            if resource_kind.is_some()
                && (!resource_kind
                    .is_some_and(|kind| super::resource_model::matches_path(kind, &info.path))
                    || !matches!(info.delete_condition, DeleteCondition::ResourceDelete(_)))
            {
                return Err(FsError::InvalidOwnershipRecord);
            }
            let condition = match info.delete_condition {
                DeleteCondition::ResourceDelete(id) => {
                    if id.value() == 0 || resource_kind.is_none() {
                        return Err(FsError::InvalidOwnershipRecord);
                    }
                    format!("resource:{}", id.value())
                }
                DeleteCondition::OwnerRequest => String::from("owner"),
                DeleteCondition::TaskExit(id) => {
                    if id.value() == 0
                        || !matches!(
                            info.storage_class,
                            OwnedStorageClass::Temporary | OwnedStorageClass::RuntimeState
                        )
                        || !super::scope::ephemeral_path(&info.path)
                    {
                        return Err(FsError::InvalidOwnershipRecord);
                    }
                    format!("task:{}", id.value())
                }
            };
            let suffix = if v2 {
                format!(" {condition}")
            } else {
                String::new()
            };
            writeln!(
                text,
                "{} {} {} {} {:03o} {} {}{}",
                info.id.0,
                info.storage_generation,
                info.owner.uid,
                info.owner.gid,
                info.mode.0,
                match info.storage_class {
                    OwnedStorageClass::UserData => "user",
                    OwnedStorageClass::Temporary => "tmp",
                    OwnedStorageClass::RuntimeState => "run",
                    OwnedStorageClass::Application => "app",
                    OwnedStorageClass::Cache => "cache",
                },
                hex(&info.path),
                suffix
            )
            .map_err(|_| FsError::InvalidOwnershipRecord)?;
            if text.len() > MAX_CATALOG_BYTES {
                return Err(FsError::OwnershipCapacity);
            }
        }
        if multiple || links {
            let magic = if links { MAGIC_V7 } else { MAGIC_V6 };
            let mut bytes = format!(
                "{magic}
"
            )
            .into_bytes();
            bytes.extend(serde_json::to_vec(self).map_err(|_| FsError::InvalidOwnershipRecord)?);
            bytes.push(10);
            if bytes.len() > MAX_CATALOG_BYTES {
                return Err(FsError::OwnershipCapacity);
            }
            Ok(bytes)
        } else {
            Ok(text.into_bytes())
        }
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, FsError> {
        if bytes.len() > MAX_CATALOG_BYTES || !bytes.ends_with(b"\n") {
            return Err(FsError::InvalidOwnershipRecord);
        }
        let newline = char::from(10);
        if let Some(body) = bytes
            .strip_prefix(format!("{MAGIC_V6}{newline}").as_bytes())
            .or_else(|| bytes.strip_prefix(format!("{MAGIC_V7}{newline}").as_bytes()))
        {
            let catalog: Self =
                serde_json::from_slice(body).map_err(|_| FsError::InvalidOwnershipRecord)?;
            if catalog.encode()?.as_slice() != bytes {
                return Err(FsError::InvalidOwnershipRecord);
            }
            return Ok(catalog);
        }
        let text = std::str::from_utf8(bytes).map_err(|_| FsError::InvalidOwnershipRecord)?;
        let mut lines = text.lines();
        let v2 = match lines.next() {
            Some(MAGIC) => false,
            Some(MAGIC_V2) | Some(MAGIC_V3) | Some(MAGIC_V4) | Some(MAGIC_V5) => true,
            _ => return Err(FsError::InvalidOwnershipRecord),
        };
        let fields: Vec<_> = lines
            .next()
            .ok_or(FsError::InvalidOwnershipRecord)?
            .split(' ')
            .collect();
        if fields.len() != 2 {
            return Err(FsError::InvalidOwnershipRecord);
        }
        let revision = fields[0]
            .parse::<u64>()
            .map_err(|_| FsError::InvalidOwnershipRecord)?;
        let next_id = fields[1]
            .parse::<u64>()
            .map_err(|_| FsError::InvalidOwnershipRecord)?;
        if revision == 0 || next_id == 0 {
            return Err(FsError::InvalidOwnershipRecord);
        }
        let mut entries = OwnedEntries::default();
        let mut ids = std::collections::BTreeSet::new();
        for line in lines {
            if entries.len() >= MAX_OWNED_OBJECTS {
                return Err(FsError::OwnershipCapacity);
            }
            let fields: Vec<_> = line.split(' ').collect();
            if fields.len() != if v2 { 8 } else { 7 } {
                return Err(FsError::InvalidOwnershipRecord);
            }
            let id = fields[0]
                .parse::<u64>()
                .map_err(|_| FsError::InvalidOwnershipRecord)?;
            let generation = fields[1]
                .parse::<u64>()
                .map_err(|_| FsError::InvalidOwnershipRecord)?;
            if generation == 0 {
                return Err(FsError::InvalidOwnershipRecord);
            }
            let uid = fields[2]
                .parse::<u32>()
                .map_err(|_| FsError::InvalidOwnershipRecord)?;
            let gid = fields[3]
                .parse::<u32>()
                .map_err(|_| FsError::InvalidOwnershipRecord)?;
            let mode = VirtualMode::new(
                u16::from_str_radix(fields[4], 8).map_err(|_| FsError::InvalidOwnershipRecord)?,
            )?;
            mode.require_regular_file()
                .map_err(|_| FsError::InvalidOwnershipRecord)?;
            let storage_class = match fields[5] {
                "user" => OwnedStorageClass::UserData,
                "tmp" => OwnedStorageClass::Temporary,
                "run" => OwnedStorageClass::RuntimeState,
                "app" => OwnedStorageClass::Application,
                "cache" => OwnedStorageClass::Cache,
                _ => return Err(FsError::InvalidOwnershipRecord),
            };
            let path = unhex(fields[6])?;
            if id == 0
                || id >= next_id
                || key(&path)? != path
                || !ids.insert(id)
                || entries.contains_key(&path)
            {
                return Err(FsError::InvalidOwnershipRecord);
            }
            let delete_condition = if !v2 || fields[7] == "owner" {
                DeleteCondition::OwnerRequest
            } else if let Some(value) = fields[7].strip_prefix("resource:") {
                DeleteCondition::ResourceDelete(super::ResourceId(
                    value.parse().map_err(|_| FsError::InvalidOwnershipRecord)?,
                ))
            } else {
                let id = fields[7]
                    .strip_prefix("task:")
                    .ok_or(FsError::InvalidOwnershipRecord)?
                    .parse::<u64>()
                    .map_err(|_| FsError::InvalidOwnershipRecord)?;
                if id == 0
                    || !matches!(
                        storage_class,
                        OwnedStorageClass::Temporary | OwnedStorageClass::RuntimeState
                    )
                    || !super::scope::ephemeral_path(&path)
                {
                    return Err(FsError::InvalidOwnershipRecord);
                }
                DeleteCondition::TaskExit(super::TaskScopeId(id))
            };
            entries.insert(
                path.clone(),
                OwnedFileInfo {
                    kind: HostFileKind::Regular,
                    id: ObjectId(id),
                    storage_generation: generation,
                    path,
                    owner: FileOwner { uid, gid },
                    mode,
                    storage_class,
                    delete_condition,
                },
            )?;
        }
        let catalog = Self {
            revision,
            next_id,
            entries,
        };
        if catalog.encode()?.as_slice() != bytes {
            return Err(FsError::InvalidOwnershipRecord);
        }
        Ok(catalog)
    }
}

#[cfg(test)]
#[path = "../../tests/filesystem/ownership_model.rs"]
mod inode_model_tests;
