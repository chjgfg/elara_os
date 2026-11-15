//! Bounded canonical policy; virtual IDs, not host attributes, are authority.
use super::{DeleteCondition, FileOwner, FsError, VirtualMode};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::{format, matches};
pub(super) const MAX_ACCESS_BYTES: usize = 400 * 1024;
const MAGIC_ASSOCIATIONS: &[u8] = b"ELARA_ACCESS_CATALOG_V7
";
const MAGIC_RESOURCES: &[u8] = b"ELARA_ACCESS_CATALOG_V6
";
const MAGIC: &[u8] = b"ELARA_ACCESS_CATALOG_V1\n";
const MAGIC_DEFAULT: &[u8] = b"ELARA_ACCESS_CATALOG_V2\n";
const MAGIC_SPECIAL: &[u8] = b"ELARA_ACCESS_CATALOG_V3\n";
const MAGIC_RUNTIME: &[u8] = b"ELARA_ACCESS_CATALOG_V5
";
const MAGIC_STICKY: &[u8] = b"ELARA_ACCESS_CATALOG_V4\n";
/// Which authority granted removal of an existing virtual name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NameAuthority {
    ParentDirectory,
    TargetOwner,
    DirectoryOwner,
    VirtualRoot,
    LegacyTargetOwner,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskScopeId(pub(super) u64);
impl TaskScopeId {
    pub fn value(self) -> u64 {
        self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Umask(u16);
impl Default for Umask {
    fn default() -> Self {
        Self(0o077)
    }
}
impl Umask {
    pub fn new(bits: u16) -> Result<Self, FsError> {
        if bits & !0o777 != 0 {
            return Err(FsError::InvalidOwnershipRecord);
        }
        Ok(Self(bits))
    }
    pub fn bits(self) -> u16 {
        self.0
    }
    pub fn apply(self, requested: VirtualMode) -> VirtualMode {
        VirtualMode::new(requested.bits() & !self.0).expect("validated mode")
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Acl {
    pub users: BTreeMap<u32, u8>,
    pub groups: BTreeMap<u32, u8>,
    pub group: Option<u8>,
    pub mask: Option<u8>,
}
impl Acl {
    pub fn validate(&self) -> Result<(), FsError> {
        if self.users.len() + self.groups.len() > 64
            || self
                .users
                .values()
                .chain(self.groups.values())
                .any(|b| *b > 7)
            || self.group.is_some_and(|b| b > 7)
            || self.mask.is_some_and(|b| b > 7)
        {
            return Err(FsError::InvalidOwnershipRecord);
        }
        Ok(())
    }
    pub fn permits(
        &self,
        owner: FileOwner,
        mode: VirtualMode,
        actor: &Credentials,
        needed: u8,
    ) -> bool {
        if actor.uid == 0 {
            return true;
        }
        if actor.uid == owner.uid {
            return ((mode.bits() >> 6) as u8 & needed) == needed;
        }
        let mask = self.mask.unwrap_or(((mode.bits() >> 3) & 7) as u8);
        if let Some(bits) = self.users.get(&actor.uid) {
            return (bits & mask & needed) == needed;
        }
        let mut matched = false;
        let mut bits = 0;
        if actor.groups.contains(&owner.gid) {
            matched = true;
            bits |= self.group.unwrap_or(((mode.bits() >> 3) & 7) as u8);
        }
        for (gid, grant) in &self.groups {
            if actor.groups.contains(gid) {
                matched = true;
                bits |= grant;
            }
        }
        if matched {
            return (bits & mask & needed) == needed;
        }
        (mode.bits() as u8 & needed) == needed
    }
    pub fn recalculate_mask(&mut self, mode: VirtualMode) -> VirtualMode {
        let group = *self.group.get_or_insert(((mode.bits() >> 3) & 7) as u8);
        let mask = self
            .users
            .values()
            .chain(self.groups.values())
            .fold(group, |m, b| m | b);
        self.mask = Some(mask);
        VirtualMode::new((mode.bits() & !0o070) | (u16::from(mask) << 3)).expect("validated ACL")
    }
}
/// Directory-only inheritance template. Owner/other are independent of access mode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultAcl {
    pub owner: u8,
    pub other: u8,
    pub acl: Acl,
}
impl DefaultAcl {
    pub fn from_mode(mode: VirtualMode) -> Self {
        Self {
            owner: ((mode.bits() >> 6) & 7) as u8,
            other: (mode.bits() & 7) as u8,
            acl: Acl {
                group: Some(((mode.bits() >> 3) & 7) as u8),
                ..Acl::default()
            },
        }
    }
    pub fn validate(&self) -> Result<(), FsError> {
        self.acl.validate()?;
        if self.owner > 7
            || self.other > 7
            || self.acl.group.is_none()
            || ((!self.acl.users.is_empty() || !self.acl.groups.is_empty())
                && self.acl.mask.is_none())
        {
            return Err(FsError::InvalidOwnershipRecord);
        }
        Ok(())
    }
    /// POSIX creation rule: clip owner/mask(or group)/other, not named entries.
    pub fn inherit(&self, requested: VirtualMode) -> Result<(VirtualMode, Acl), FsError> {
        self.validate()?;
        let mut acl = self.acl.clone();
        let group_limit = ((requested.bits() >> 3) & 7) as u8;
        let group = if let Some(mask) = &mut acl.mask {
            *mask &= group_limit;
            *mask
        } else {
            let group = acl.group.as_mut().ok_or(FsError::InvalidOwnershipRecord)?;
            *group &= group_limit;
            *group
        };
        let owner = self.owner & ((requested.bits() >> 6) & 7) as u8;
        let other = self.other & (requested.bits() & 7) as u8;
        let mode =
            VirtualMode::new((u16::from(owner) << 6) | (u16::from(group) << 3) | u16::from(other))?;
        Ok((mode, acl))
    }
}
#[derive(Clone, Copy)]
pub(super) enum CreationGroup {
    Primary(u32),
    SetgidDirectory(u32),
}
impl CreationGroup {
    pub fn gid(self) -> u32 {
        match self {
            Self::Primary(gid) | Self::SetgidDirectory(gid) => gid,
        }
    }
    pub fn directory_mode(self, mode: VirtualMode) -> VirtualMode {
        match self {
            Self::Primary(_) => mode,
            Self::SetgidDirectory(_) => {
                VirtualMode::new(mode.bits() | 0o2000).expect("validated directory mode")
            }
        }
    }
}
#[derive(Clone, Copy)]
pub(super) enum CreationMode {
    Exact(VirtualMode),
    Requested(VirtualMode),
}
pub(super) enum CreationAccess {
    Explicit(VirtualMode),
    Umask(VirtualMode),
    Inherited {
        mode: VirtualMode,
        acl: Acl,
        default: DefaultAcl,
    },
}
impl CreationAccess {
    pub fn mode(&self) -> VirtualMode {
        match self {
            Self::Explicit(mode) | Self::Umask(mode) | Self::Inherited { mode, .. } => *mode,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credentials {
    pub uid: u32,
    pub gid: u32,
    pub groups: BTreeSet<u32>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryInfo {
    pub id: u64,
    pub path: String,
    pub owner: FileOwner,
    pub mode: VirtualMode,
    pub acl: Acl,
    pub delete_condition: DeleteCondition,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub name: String,
    pub members: BTreeSet<u32>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AccessCatalog {
    pub revision: u64,
    pub next_dir: u64,
    pub next_task: u64,
    pub directories: super::directory_model::DirectoryEntries,
    pub groups: BTreeMap<u32, Group>,
    pub primary: BTreeMap<u32, u32>,
    pub file_acl: BTreeMap<u64, Acl>,
    pub tasks: BTreeMap<TaskScopeId, u32>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub default_acl: BTreeMap<u64, DefaultAcl>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resources: BTreeMap<super::ResourceId, super::ResourceRecord>,
}
impl Default for AccessCatalog {
    fn default() -> Self {
        Self {
            revision: 1,
            next_dir: 1,
            next_task: 1,
            directories: super::directory_model::DirectoryEntries::default(),
            groups: BTreeMap::new(),
            primary: BTreeMap::new(),
            file_acl: BTreeMap::new(),
            tasks: BTreeMap::new(),
            default_acl: BTreeMap::new(),
            resources: BTreeMap::new(),
        }
    }
}
impl AccessCatalog {
    pub fn bump(&mut self) -> Result<(), FsError> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(FsError::OwnershipIdentityExhausted)?;
        Ok(())
    }
    pub fn actor_credentials(&self, actor: &crate::users::Account) -> Credentials {
        match &actor.identity {
            crate::users::Identity::Login => self.credentials(actor.uid),
            crate::users::Identity::Executed(id) => {
                let mut groups = id.supplementary.clone();
                groups.insert(id.effective_gid);
                Credentials {
                    uid: actor.uid,
                    gid: id.effective_gid,
                    groups,
                }
            }
        }
    }
    pub fn credentials(&self, uid: u32) -> Credentials {
        let gid = self.primary.get(&uid).copied().unwrap_or(uid);
        let mut groups = BTreeSet::from([gid]);
        for (id, group) in &self.groups {
            if group.members.contains(&uid) {
                groups.insert(*id);
            }
        }
        Credentials { uid, gid, groups }
    }
    pub fn validate(&self) -> Result<(), FsError> {
        if self.revision == 0
            || self.next_dir == 0
            || self.next_task == 0
            || self.directories.len() > 4096
            || self.file_acl.len() > 4096
            || self.groups.len() > 256
            || self.primary.len() > 4096
            || self.tasks.len() > 64
        {
            return Err(FsError::InvalidOwnershipRecord);
        }
        let mut ids = BTreeSet::new();
        for (path, info) in &self.directories {
            if super::ownership_model::key(path)? != *path
                || path != &info.path
                || info.id == 0
                || info.id >= self.next_dir
                || !ids.insert(info.id)
            {
                return Err(FsError::InvalidOwnershipRecord);
            }
            info.mode.require_directory()?;
            info.acl.validate()?;
            // Older catalogs may contain owner-managed directories under /run.
            // Preserve them; only newly created runtime directories require a lease.
            if let DeleteCondition::TaskExit(id) = info.delete_condition {
                if self.tasks.get(&id) != Some(&info.owner.uid)
                    || !super::scope::ephemeral_path(path)
                {
                    return Err(FsError::InvalidOwnershipRecord);
                }
            }
        }
        for group in self.groups.values() {
            if group.name.is_empty()
                || group.name.len() > 32
                || !group
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                || group.members.len() > 256
            {
                return Err(FsError::InvalidOwnershipRecord);
            }
        }
        let names: BTreeSet<_> = self.groups.values().map(|g| &g.name).collect();
        if names.len() != self.groups.len()
            || self
                .primary
                .values()
                .any(|gid| !self.groups.contains_key(gid))
            || self
                .tasks
                .keys()
                .any(|id| id.0 == 0 || id.0 >= self.next_task)
        {
            return Err(FsError::InvalidOwnershipRecord);
        }
        if self.file_acl.keys().any(|id| *id == 0) {
            return Err(FsError::InvalidOwnershipRecord);
        }
        for acl in self.file_acl.values() {
            acl.validate()?;
        }
        for (id, acl) in &self.default_acl {
            if !ids.contains(id) {
                return Err(FsError::InvalidOwnershipRecord);
            }
            acl.validate()?;
        }
        super::resource_model::validate(self)
    }
    pub fn encode(&self) -> Result<Vec<u8>, FsError> {
        self.validate()?;
        let magic = if self
            .resources
            .values()
            .any(|r| !r.association.is_independent())
        {
            MAGIC_ASSOCIATIONS
        } else if !self.resources.is_empty() {
            MAGIC_RESOURCES
        } else if self.directories.values().any(|d| {
            super::scope::runtime_path(&d.path)
                && matches!(d.delete_condition, DeleteCondition::TaskExit(_))
        }) {
            MAGIC_RUNTIME
        } else if self.directories.values().any(|d| d.mode.is_sticky()) {
            MAGIC_STICKY
        } else if self.directories.values().any(|d| d.mode.is_setgid()) {
            MAGIC_SPECIAL
        } else if self.default_acl.is_empty() {
            MAGIC
        } else {
            MAGIC_DEFAULT
        };
        let mut bytes = magic.to_vec();
        bytes.extend(serde_json::to_vec(self).map_err(|_| FsError::InvalidOwnershipRecord)?);
        bytes.push(b'\n');
        if bytes.len() > MAX_ACCESS_BYTES {
            return Err(FsError::OwnershipCapacity);
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, FsError> {
        if bytes.len() > MAX_ACCESS_BYTES {
            return Err(FsError::OwnershipCapacity);
        }
        let body = bytes
            .strip_prefix(MAGIC)
            .or_else(|| bytes.strip_prefix(MAGIC_DEFAULT))
            .or_else(|| bytes.strip_prefix(MAGIC_SPECIAL))
            .or_else(|| bytes.strip_prefix(MAGIC_STICKY))
            .or_else(|| bytes.strip_prefix(MAGIC_RUNTIME))
            .or_else(|| bytes.strip_prefix(MAGIC_RESOURCES))
            .or_else(|| bytes.strip_prefix(MAGIC_ASSOCIATIONS))
            .ok_or(FsError::InvalidOwnershipRecord)?;
        let value: Self =
            serde_json::from_slice(body).map_err(|_| FsError::InvalidOwnershipRecord)?;
        if value.encode()? != bytes {
            return Err(FsError::InvalidOwnershipRecord);
        }
        Ok(value)
    }
}
pub(super) fn directory_magic(id: u64) -> Vec<u8> {
    format!("ELARA_DIRECTORY_OBJECT_V1\n{id}\n").into_bytes()
}
