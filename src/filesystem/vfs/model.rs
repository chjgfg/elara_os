use crate::filesystem::{Acl, Credentials, DefaultAcl, FileOwner, VirtualMode};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::{fmt, matches, vec, write};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    NotFound,
    Exists,
    NotDirectory,
    IsDirectory,
    NotEmpty,
    Denied,
    ReadOnly,
    InvalidPath,
    LinkLoop,
    CrossDevice,
    Busy,
    Quota,
    InvalidImage,
    Overflow,
    InvalidHandle,
    LockConflict,
    Unsupported,
    NoSpace,
    Crypto,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "virtual filesystem: {self:?}")
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub bytes: u64,
    pub inodes: u64,
}
impl Limits {
    pub fn validate(self) -> Result<(), Error> {
        if self.bytes == 0 || self.bytes > 64 * 1024 * 1024 || self.inodes < 2 || self.inodes > 4096
        {
            return Err(Error::Quota);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InodeKind {
    File,
    Directory,
    Symlink,
    Device,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockMode {
    Shared,
    Exclusive,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShareMode {
    Shared,
    Exclusive,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InodeStat {
    pub inode: u64,
    pub kind: InodeKind,
    pub owner: FileOwner,
    pub mode: VirtualMode,
    pub links: u64,
    pub length: u64,
    pub created: u64,
    pub modified: u64,
    pub changed: u64,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub access: super::VolumeAccess,
    pub bytes: u64,
    pub inodes: u64,
    pub open_files: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
    /// Namespace entries are bounded independently from logical payload bytes.
    pub directory_entries: u64,
    /// Data-page allocations referenced by this volume, excluding metadata.
    pub allocated_data_bytes: u64,
    /// Page references shared with another transaction, snapshot or clone.
    pub shared_data_pages: u64,
    /// Bytes copied while detaching shared data pages for successful mutations.
    pub cow_copied_bytes: u64,
    pub reads: u64,
    pub writes: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) enum Content {
    File(super::data::Data),
    Directory(BTreeMap<String, u64>),
    Symlink(String),
    GlobalSymlink(String),
    Pseudo(super::pseudo::Node),
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Inode {
    pub owner: FileOwner,
    pub mode: VirtualMode,
    pub acl: Acl,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_acl: Option<DefaultAcl>,
    pub links: u64,
    pub created: u64,
    pub modified: u64,
    pub changed: u64,
    pub content: Content,
}
impl Inode {
    pub fn size(&self) -> u64 {
        match &self.content {
            Content::File(b) => b.len() as u64,
            Content::Symlink(s) | Content::GlobalSymlink(s) => s.len() as u64,
            Content::Directory(_) | Content::Pseudo(_) => 0,
        }
    }
    pub fn permits(&self, actor: &Credentials, bits: u8) -> Result<(), Error> {
        if self.acl.permits(self.owner, self.mode, actor, bits) {
            Ok(())
        } else {
            Err(Error::Denied)
        }
    }
    pub fn entries(&self) -> Result<&BTreeMap<String, u64>, Error> {
        match &self.content {
            Content::Directory(e) => Ok(e),
            _ => Err(Error::NotDirectory),
        }
    }
    pub fn entries_mut(&mut self) -> Result<&mut BTreeMap<String, u64>, Error> {
        match &mut self.content {
            Content::Directory(e) => Ok(e),
            _ => Err(Error::NotDirectory),
        }
    }
    pub fn stat(&self, id: u64) -> InodeStat {
        InodeStat {
            inode: id,
            kind: match self.content {
                Content::File(_) => InodeKind::File,
                Content::Directory(_) => InodeKind::Directory,
                Content::Symlink(_) | Content::GlobalSymlink(_) => InodeKind::Symlink,
                Content::Pseudo(node) if node.is_device() => InodeKind::Device,
                Content::Pseudo(_) => InodeKind::File,
            },
            owner: self.owner,
            mode: self.mode,
            links: self.links,
            length: self.size(),
            created: self.created,
            modified: self.modified,
            changed: self.changed,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Tree {
    pub next: u64,
    pub clock: u64,
    pub limits: Limits,
    pub quotas: BTreeMap<u32, Limits>,
    pub nodes: BTreeMap<u64, Inode>,
}
pub(super) fn components(path: &str) -> Result<VecDeque<String>, Error> {
    if path.len() > 4096 || !path.starts_with('/') || path.contains(['\0', '\\', ':']) {
        return Err(Error::InvalidPath);
    }
    Ok(path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect())
}
pub(super) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.len() <= 255
        && !name.contains(['/', '\\', '\0', ':'])
}
impl Tree {
    pub fn new(owner: FileOwner, limits: Limits) -> Result<Self, Error> {
        limits.validate()?;
        let root = Inode {
            owner,
            mode: VirtualMode::new(0o755).map_err(|_| Error::Denied)?,
            acl: Acl::default(),
            default_acl: None,
            links: 1,
            created: 1,
            modified: 1,
            changed: 1,
            content: Content::Directory(BTreeMap::new()),
        };
        Ok(Self {
            next: 2,
            clock: 1,
            limits,
            quotas: BTreeMap::new(),
            nodes: BTreeMap::from([(1, root)]),
        })
    }
    pub fn tick(&mut self) -> Result<u64, Error> {
        self.clock = self.clock.checked_add(1).ok_or(Error::Overflow)?;
        Ok(self.clock)
    }
    pub fn node(&self, id: u64) -> Result<&Inode, Error> {
        self.nodes.get(&id).ok_or(Error::NotFound)
    }
    pub fn node_mut(&mut self, id: u64) -> Result<&mut Inode, Error> {
        self.nodes.get_mut(&id).ok_or(Error::NotFound)
    }
    pub fn resolve(&self, actor: &Credentials, path: &str, follow: bool) -> Result<u64, Error> {
        let mut pending = components(path)?;
        let mut stack = vec![1];
        let mut links = 0;
        while let Some(name) = pending.pop_front() {
            let current = *stack.last().ok_or(Error::InvalidPath)?;
            self.node(current)?.permits(actor, 1)?;
            self.node(current)?.entries()?;
            if name == "." {
                continue;
            }
            if name == ".." {
                if stack.len() == 1 {
                    return Err(Error::InvalidPath);
                }
                stack.pop();
                continue;
            }
            if !valid_name(&name) {
                return Err(Error::InvalidPath);
            }
            let next = *self
                .node(current)?
                .entries()?
                .get(&name)
                .ok_or(Error::NotFound)?;
            match &self.node(next)?.content {
                Content::GlobalSymlink(_) if follow || !pending.is_empty() => {
                    return Err(Error::CrossDevice)
                }
                Content::Symlink(target) if follow || !pending.is_empty() => {
                    links += 1;
                    if links > 40 {
                        return Err(Error::LinkLoop);
                    }
                    if target.is_empty()
                        || target.len() > 4096
                        || target.contains(['\0', '\\', ':'])
                    {
                        return Err(Error::InvalidPath);
                    }
                    if target.starts_with('/') {
                        stack.truncate(1);
                    }
                    for part in target.split('/').filter(|s| !s.is_empty()).rev() {
                        pending.push_front(part.to_owned());
                    }
                    if pending.len() > 4096 {
                        return Err(Error::LinkLoop);
                    }
                }
                _ => stack.push(next),
            }
        }
        stack.last().copied().ok_or(Error::InvalidPath)
    }
    /// Direct directory names only: no permission grants and no link expansion.
    pub fn directory_at(&self, path: &str) -> Result<u64, Error> {
        let mut id = 1;
        for name in path.split('/').filter(|s| !s.is_empty()) {
            id = *self.node(id)?.entries()?.get(name).ok_or(Error::NotFound)?;
        }
        self.node(id)?.entries()?;
        Ok(id)
    }
    pub fn directory_path(&self, target: u64) -> Result<String, Error> {
        let mut pending = vec![(1, String::from("/"))];
        while let Some((id, path)) = pending.pop() {
            if id == target {
                return Ok(path);
            }
            for (name, child) in self.node(id)?.entries()? {
                if matches!(self.node(*child)?.content, Content::Directory(_)) {
                    pending.push((
                        *child,
                        std::format!(
                            "{}{name}",
                            if path == "/" {
                                path.clone()
                            } else {
                                std::format!("{path}/")
                            }
                        ),
                    ));
                }
            }
        }
        Err(Error::NotFound)
    }
    pub fn parent(&self, actor: &Credentials, path: &str) -> Result<(u64, String), Error> {
        components(path)?;
        let (parent, name) = path.rsplit_once('/').ok_or(Error::InvalidPath)?;
        if !valid_name(name) {
            return Err(Error::InvalidPath);
        }
        let parent = self.resolve(actor, if parent.is_empty() { "/" } else { parent }, true)?;
        self.node(parent)?.entries()?;
        self.node(parent)?.permits(actor, 3)?;
        Ok((parent, name.to_owned()))
    }
    pub fn naming(&self, actor: &Credentials, parent: u64, target: u64) -> Result<(), Error> {
        let dir = self.node(parent)?;
        dir.permits(actor, 3)?;
        if dir.mode.is_sticky()
            && actor.uid != 0
            && actor.uid != dir.owner.uid
            && actor.uid != self.node(target)?.owner.uid
        {
            return Err(Error::Denied);
        }
        Ok(())
    }
    pub fn check_limits(&self) -> Result<(), Error> {
        let mut usage: BTreeMap<u32, (u64, u64)> = BTreeMap::new();
        let mut bytes = 0u64;
        let mut names = 0usize;
        if self.quotas.len() > 4096 {
            return Err(Error::Quota);
        }
        for node in self.nodes.values() {
            if let Content::Directory(entries) = &node.content {
                names = names.checked_add(entries.len()).ok_or(Error::Overflow)?;
                if names > 4096 {
                    return Err(Error::Quota);
                }
            }
            bytes = bytes.checked_add(node.size()).ok_or(Error::Overflow)?;
            let entry = usage.entry(node.owner.uid).or_default();
            entry.0 += node.size();
            entry.1 += 1;
        }
        if bytes > self.limits.bytes || self.nodes.len() as u64 > self.limits.inodes {
            return Err(Error::Quota);
        }
        for (uid, (bytes, inodes)) in usage {
            if self
                .quotas
                .get(&uid)
                .is_some_and(|q| bytes > q.bytes || inodes > q.inodes)
            {
                return Err(Error::Quota);
            }
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<(), Error> {
        self.limits.validate()?;
        self.check_limits()?;
        if self.quotas.len() > 4096 {
            return Err(Error::Quota);
        }
        for quota in self.quotas.values() {
            quota.validate()?;
        }
        if self.clock == 0
            || self.next < 2
            || !matches!(self.node(1)?.content, Content::Directory(_))
        {
            return Err(Error::InvalidImage);
        }
        let mut links = BTreeMap::<u64, u64>::from([(1, 1)]);
        let mut visited = BTreeSet::new();
        let mut pending = vec![1];
        while let Some(id) = pending.pop() {
            if !visited.insert(id) {
                continue;
            }
            let node = self.node(id)?;
            if let Content::Directory(entries) = &node.content {
                for (name, target) in entries {
                    if !valid_name(name) || *target == 1 {
                        return Err(Error::InvalidImage);
                    }
                    let count = links.entry(*target).or_default();
                    *count += 1;
                    if matches!(self.node(*target)?.content, Content::Directory(_)) && *count > 1 {
                        return Err(Error::InvalidImage);
                    }
                    pending.push(*target);
                }
            }
        }
        for (id, node) in &self.nodes {
            if *id == 0
                || *id >= self.next
                || !visited.contains(id)
                || links.get(id) != Some(&node.links)
                || node.created == 0
                || node.created > node.modified
                || node.created > node.changed
                || node.modified > self.clock
                || node.changed > self.clock
            {
                return Err(Error::InvalidImage);
            }
            node.acl.validate().map_err(|_| Error::InvalidImage)?;
            if let Some(default) = &node.default_acl {
                node.entries().map_err(|_| Error::InvalidImage)?;
                default.validate().map_err(|_| Error::InvalidImage)?;
            }
            match &node.content {
                Content::Pseudo(_) => return Err(Error::InvalidImage),
                Content::Directory(_) if node.mode.is_setuid() => return Err(Error::InvalidImage),
                Content::File(_) | Content::Symlink(_) | Content::GlobalSymlink(_)
                    if node.mode.is_sticky() =>
                {
                    return Err(Error::InvalidImage)
                }
                Content::GlobalSymlink(target)
                    if !target.starts_with('/')
                        || crate::virtual_path::VirtualPath::strict(target).is_err() =>
                {
                    return Err(Error::InvalidImage)
                }
                Content::Symlink(target) | Content::GlobalSymlink(target)
                    if target.is_empty()
                        || target.len() > 4096
                        || target.chars().any(|c| [0, 92, 58].contains(&(c as u32))) =>
                {
                    return Err(Error::InvalidImage)
                }
                _ => {}
            }
        }
        Ok(())
    }
}
