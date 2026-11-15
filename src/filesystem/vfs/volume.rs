use super::archive::{self, ArchiveKind};
use super::data::Data;
use super::model::{Content, Inode, Tree};
use super::VolumeAccess;
use super::{Error, InodeStat, Limits, LockMode, ShareMode, Stats};
use crate::filesystem::{
    Acl, Credentials, DefaultAcl, FileAccess, FileBehavior, FileCreation, FileMode, FileOwner,
    Umask, VirtualMode,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::{matches, vec};

#[derive(Clone)]
struct Open {
    inode: u64,
    mode: FileMode,
    share: ShareMode,
    lock: Option<LockMode>,
    generated: Option<Vec<u8>>,
    anchor: Option<String>,
}
pub(super) struct State {
    pub tree: Tree,
    opens: BTreeMap<u64, Open>,
    next_open: u64,
    pub access: VolumeAccess,
    pub stats: Stats,
}
static NEXT_VOLUME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
pub struct Volume {
    id: u64,
    pub(super) state: Mutex<State>,
    pub(super) pseudo: Option<super::PseudoKind>,
    pub(super) started: std::time::Instant,
}
impl Volume {
    pub fn memory(owner: FileOwner, limits: Limits) -> Result<Arc<Self>, Error> {
        Self::from_tree(Tree::new(owner, limits)?, false)
    }
    pub(super) fn from_tree(tree: Tree, readonly: bool) -> Result<Arc<Self>, Error> {
        Self::from_tree_kind(tree, readonly, None)
    }
    pub(super) fn from_tree_kind(
        tree: Tree,
        readonly: bool,
        pseudo: Option<super::PseudoKind>,
    ) -> Result<Arc<Self>, Error> {
        let access = if pseudo.is_some() {
            VolumeAccess::VirtualProvider
        } else {
            VolumeAccess::restored(ArchiveKind::Ordinary, readonly)
        };
        Self::from_tree_access_kind(tree, access, pseudo)
    }
    pub(super) fn from_tree_access(tree: Tree, access: VolumeAccess) -> Result<Arc<Self>, Error> {
        Self::from_tree_access_kind(tree, access, None)
    }
    fn from_tree_access_kind(
        tree: Tree,
        access: VolumeAccess,
        pseudo: Option<super::PseudoKind>,
    ) -> Result<Arc<Self>, Error> {
        let id = NEXT_VOLUME
            .fetch_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |id| id.checked_add(1),
            )
            .map_err(|_| Error::Overflow)?;
        Ok(Arc::new(Self {
            id,
            pseudo,
            started: std::time::Instant::now(),
            state: Mutex::new(State {
                tree,
                opens: BTreeMap::new(),
                next_open: 1,
                access,
                stats: Stats::default(),
            }),
        }))
    }
    pub fn revision(&self) -> Result<u64, Error> {
        Ok(self.guard()?.tree.clock)
    }
    pub(crate) fn is_device_namespace(&self) -> bool {
        self.pseudo == Some(super::PseudoKind::Dev)
    }
    pub fn id(&self) -> u64 {
        self.id
    }
    pub(super) fn guard(&self) -> Result<MutexGuard<'_, State>, Error> {
        self.state.lock().map_err(|_| Error::Busy)
    }
    fn edit<T>(&self, change: impl FnOnce(&mut Tree) -> Result<T, Error>) -> Result<T, Error> {
        let mut state = self.guard()?;
        if state.access.is_readonly() {
            return Err(Error::ReadOnly);
        }
        let mut next = state.tree.clone();
        let result = change(&mut next)?;
        next.check_limits()?;
        // Cwd and mount-parent leases protect names, unlike directory cursors.
        // Check against the candidate tree while still holding the mutation lock.
        for open in state.opens.values() {
            if let Some(path) = &open.anchor {
                if next.directory_at(path) != Ok(open.inode) {
                    return Err(Error::Busy);
                }
            }
        }
        state.tree = next;
        Ok(result)
    }
    pub fn stat(&self, actor: &Credentials, path: &str, follow: bool) -> Result<InodeStat, Error> {
        let state = self.guard()?;
        let id = state.tree.resolve(actor, path, follow)?;
        let node = state.tree.node(id)?;
        let mut stat = node.stat(id);
        if let Content::Pseudo(kind) = node.content {
            if !kind.is_device() {
                stat.length = kind.render(actor, self.started.elapsed()).len() as u64;
            }
        }
        Ok(stat)
    }
    // Execute-only bytes stay interpreter-internal, never a public read bypass.
    pub(crate) fn executable(
        &self,
        actor: &Credentials,
        path: &str,
        limit: u64,
        check: impl FnOnce(&InodeStat, &Acl) -> Result<(), crate::filesystem::FsError>,
    ) -> Result<(InodeStat, Vec<u8>), crate::filesystem::FsError> {
        let state = self.guard()?;
        let id = state.tree.resolve(actor, path, true)?;
        let node = state.tree.node(id)?;
        let Content::File(bytes) = &node.content else {
            return Err(Error::IsDirectory.into());
        };
        let stat = node.stat(id);
        check(&stat, &node.acl)?;
        check_lock(&state, id, None, false)?;
        if bytes.len() as u64 > limit {
            return Err(crate::filesystem::FsError::TooLarge(limit));
        }
        Ok((stat, bytes.read(0, bytes.len())))
    }
    pub fn search_directory(&self, actor: &Credentials, path: &str) -> Result<(), Error> {
        let state = self.guard()?;
        let id = state.tree.resolve(actor, path, true)?;
        let node = state.tree.node(id)?;
        node.entries()?;
        node.permits(actor, 1)
    }
    pub fn list(&self, actor: &Credentials, path: &str) -> Result<Vec<(String, InodeStat)>, Error> {
        let state = self.guard()?;
        let id = state.tree.resolve(actor, path, true)?;
        let node = state.tree.node(id)?;
        node.permits(actor, 5)?;
        node.entries()?
            .iter()
            .map(|(name, id)| Ok((name.clone(), state.tree.node(*id)?.stat(*id))))
            .collect()
    }
    pub(crate) fn open_directory(
        self: &Arc<Self>,
        actor: &Credentials,
        path: &str,
    ) -> Result<Directory, Error> {
        let mut state = self.guard()?;
        if state.opens.len() >= 1024 {
            return Err(Error::Busy);
        }
        let id = state.tree.resolve(actor, path, true)?;
        let node = state.tree.node(id)?;
        node.entries()?;
        node.permits(actor, 5)?;
        let token = state.next_open;
        state.next_open = token.checked_add(1).ok_or(Error::Overflow)?;
        state.opens.insert(
            token,
            Open {
                inode: id,
                mode: FileMode::READ,
                share: ShareMode::Shared,
                lock: None,
                generated: None,
                anchor: None,
            },
        );
        Ok(Directory {
            lease: Arc::new(Lease {
                volume: self.clone(),
                token,
            }),
        })
    }
    pub fn anchor_directory(
        self: &Arc<Self>,
        actor: &Credentials,
        path: &str,
    ) -> Result<DirectoryAnchor, Error> {
        let mut state = self.guard()?;
        if state.opens.len() >= 1024 {
            return Err(Error::Busy);
        }
        let id = state.tree.resolve(actor, path, true)?;
        let node = state.tree.node(id)?;
        node.entries()?;
        node.permits(actor, 1)?;
        let path = state.tree.directory_path(id)?;
        let token = state.next_open;
        state.next_open = token.checked_add(1).ok_or(Error::Overflow)?;
        state.opens.insert(
            token,
            Open {
                inode: id,
                mode: FileMode::READ,
                share: ShareMode::Shared,
                lock: None,
                generated: None,
                anchor: Some(path.clone()),
            },
        );
        Ok(DirectoryAnchor {
            path,
            lease: Arc::new(Lease {
                volume: self.clone(),
                token,
            }),
        })
    }
    fn birth(
        tree: &mut Tree,
        actor: &Credentials,
        path: &str,
        mode: VirtualMode,
        content: Content,
        umask: Option<Umask>,
    ) -> Result<u64, Error> {
        let (parent, name) = tree.parent(actor, path)?;
        let directory = tree.node(parent)?;
        if directory.entries()?.contains_key(&name) {
            return Err(Error::Exists);
        }
        let gid = if directory.mode.is_setgid() {
            directory.owner.gid
        } else {
            actor.gid
        };
        let default = if matches!(content, Content::Symlink(_) | Content::GlobalSymlink(_)) {
            None
        } else {
            directory.default_acl.clone()
        };
        let (mode, acl) = if let Some(default) = &default {
            default.inherit(mode).map_err(|_| Error::Denied)?
        } else {
            (umask.map_or(mode, |mask| mask.apply(mode)), Acl::default())
        };
        let inherited_default = if matches!(content, Content::Directory(_)) {
            default
        } else {
            None
        };
        let mut bits = mode.bits();
        if matches!(content, Content::Directory(_)) {
            if bits & 0o4000 != 0 {
                return Err(Error::Denied);
            }
            if directory.mode.is_setgid() {
                bits |= 0o2000;
            }
        } else {
            if bits & 0o1000 != 0 {
                return Err(Error::Denied);
            }
            if actor.uid != 0 && !actor.groups.contains(&gid) {
                bits &= !0o2000;
            }
        }
        let now = tree.tick()?;
        let id = tree.next;
        tree.next = id.checked_add(1).ok_or(Error::Overflow)?;
        tree.nodes.insert(
            id,
            Inode {
                owner: FileOwner {
                    uid: actor.uid,
                    gid,
                },
                mode: VirtualMode::new(bits).map_err(|_| Error::Denied)?,
                acl,
                default_acl: inherited_default,
                links: 1,
                created: now,
                modified: now,
                changed: now,
                content,
            },
        );
        let parent = tree.node_mut(parent)?;
        parent.entries_mut()?.insert(name, id);
        parent.modified = now;
        parent.changed = now;
        Ok(id)
    }
    pub fn create(
        &self,
        actor: &Credentials,
        path: &str,
        bytes: &[u8],
        mode: VirtualMode,
    ) -> Result<u64, Error> {
        if bytes.len() > 64 * 1024 * 1024 {
            return Err(Error::Quota);
        }
        self.edit(|tree| {
            Self::birth(
                tree,
                actor,
                path,
                mode,
                Content::File(Data::new(bytes)?),
                None,
            )
        })
    }
    pub fn mkdir(&self, actor: &Credentials, path: &str, mode: VirtualMode) -> Result<u64, Error> {
        self.edit(|tree| {
            Self::birth(
                tree,
                actor,
                path,
                mode,
                Content::Directory(BTreeMap::new()),
                None,
            )
        })
    }
    pub fn create_with_umask(
        &self,
        actor: &Credentials,
        path: &str,
        bytes: &[u8],
        umask: Umask,
    ) -> Result<u64, Error> {
        if bytes.len() > 64 * 1024 * 1024 {
            return Err(Error::Quota);
        }
        self.edit(|tree| {
            Self::birth(
                tree,
                actor,
                path,
                VirtualMode::new(0o666).map_err(|_| Error::Denied)?,
                Content::File(Data::new(bytes)?),
                Some(umask),
            )
        })
    }
    pub fn mkdir_with_umask(
        &self,
        actor: &Credentials,
        path: &str,
        umask: Umask,
    ) -> Result<u64, Error> {
        self.edit(|tree| {
            Self::birth(
                tree,
                actor,
                path,
                VirtualMode::new(0o777).map_err(|_| Error::Denied)?,
                Content::Directory(BTreeMap::new()),
                Some(umask),
            )
        })
    }
    pub fn symlink(&self, actor: &Credentials, target: &str, path: &str) -> Result<u64, Error> {
        if target.is_empty() || target.len() > 4096 || target.contains(['\0', '\\', ':']) {
            return Err(Error::InvalidPath);
        }
        self.edit(|tree| {
            Self::birth(
                tree,
                actor,
                path,
                VirtualMode::new(0o777).map_err(|_| Error::Denied)?,
                Content::Symlink(target.to_owned()),
                None,
            )
        })
    }
    pub fn symlink_global(
        &self,
        actor: &Credentials,
        target: &str,
        path: &str,
    ) -> Result<u64, Error> {
        if !target.starts_with('/') || target.len() > 4096 {
            return Err(Error::InvalidPath);
        }
        crate::virtual_path::VirtualPath::strict(target).map_err(|_| Error::InvalidPath)?;
        self.edit(|tree| {
            Self::birth(
                tree,
                actor,
                path,
                VirtualMode::new(0o777).map_err(|_| Error::Denied)?,
                Content::GlobalSymlink(target.to_owned()),
                None,
            )
        })
    }
    pub(super) fn resolution_node(
        &self,
        actor: &Credentials,
        path: &str,
    ) -> Result<super::resolution::Node, Error> {
        let state = self.guard()?;
        let id = state.tree.resolve(actor, path, false)?;
        Ok(match &state.tree.node(id)?.content {
            Content::Symlink(target) => super::resolution::Node::Link {
                target: target.clone(),
                global: false,
            },
            Content::GlobalSymlink(target) => super::resolution::Node::Link {
                target: target.clone(),
                global: true,
            },
            _ => super::resolution::Node::Object,
        })
    }
    pub fn readlink(&self, actor: &Credentials, path: &str) -> Result<String, Error> {
        let state = self.guard()?;
        let id = state.tree.resolve(actor, path, false)?;
        match &state.tree.node(id)?.content {
            Content::Symlink(target) | Content::GlobalSymlink(target) => Ok(target.clone()),
            _ => Err(Error::InvalidPath),
        }
    }
    pub fn link(&self, actor: &Credentials, source: &str, destination: &str) -> Result<(), Error> {
        self.edit(|tree| {
            let id = tree.resolve(actor, source, false)?;
            let node = tree.node(id)?;
            if matches!(node.content, Content::Directory(_)) {
                return Err(Error::IsDirectory);
            }
            if actor.uid != 0 && actor.uid != node.owner.uid {
                node.permits(actor, 6)?;
            }
            let (parent, name) = tree.parent(actor, destination)?;
            if tree.node(parent)?.entries()?.contains_key(&name) {
                return Err(Error::Exists);
            }
            let now = tree.tick()?;
            let node = tree.node_mut(id)?;
            node.links = node.links.checked_add(1).ok_or(Error::Overflow)?;
            node.changed = now;
            let parent = tree.node_mut(parent)?;
            parent.entries_mut()?.insert(name, id);
            parent.modified = now;
            parent.changed = now;
            Ok(())
        })
    }
    pub fn unlink(&self, actor: &Credentials, path: &str, directory: bool) -> Result<(), Error> {
        self.edit(|tree| {
            let (parent, name) = tree.parent(actor, path)?;
            let id = *tree
                .node(parent)?
                .entries()?
                .get(&name)
                .ok_or(Error::NotFound)?;
            tree.naming(actor, parent, id)?;
            match &tree.node(id)?.content {
                Content::Directory(entries) => {
                    if !directory {
                        return Err(Error::IsDirectory);
                    }
                    if !entries.is_empty() {
                        return Err(Error::NotEmpty);
                    }
                }
                _ if directory => return Err(Error::NotDirectory),
                _ => {}
            }
            let now = tree.tick()?;
            let dir = tree.node_mut(parent)?;
            dir.entries_mut()?.remove(&name);
            dir.modified = now;
            dir.changed = now;
            let node = tree.node_mut(id)?;
            node.links -= 1;
            node.changed = now;
            Ok(())
        })?;
        self.collect()
    }
    pub fn rename(
        &self,
        actor: &Credentials,
        source: &str,
        destination: &str,
        replace: bool,
    ) -> Result<(), Error> {
        self.edit(|tree| {
            let (sp, sn) = tree.parent(actor, source)?;
            let (dp, dn) = tree.parent(actor, destination)?;
            let id = *tree.node(sp)?.entries()?.get(&sn).ok_or(Error::NotFound)?;
            tree.naming(actor, sp, id)?;
            let old = tree.node(dp)?.entries()?.get(&dn).copied();
            if old == Some(id) {
                return Ok(());
            }
            let source_dir = matches!(tree.node(id)?.content, Content::Directory(_));
            if source_dir {
                let mut pending = vec![id];
                while let Some(next) = pending.pop() {
                    if next == dp {
                        return Err(Error::InvalidPath);
                    }
                    if let Content::Directory(entries) = &tree.node(next)?.content {
                        pending.extend(
                            entries
                                .values()
                                .filter(|id| {
                                    matches!(
                                        tree.nodes.get(id).map(|n| &n.content),
                                        Some(Content::Directory(_))
                                    )
                                })
                                .copied(),
                        );
                    }
                }
            }
            if let Some(old) = old {
                if !replace {
                    return Err(Error::Exists);
                }
                tree.naming(actor, dp, old)?;
                match &tree.node(old)?.content {
                    Content::Directory(e) => {
                        if !source_dir {
                            return Err(Error::IsDirectory);
                        }
                        if !e.is_empty() {
                            return Err(Error::NotEmpty);
                        }
                    }
                    _ if source_dir => return Err(Error::NotDirectory),
                    _ => {}
                }
            }
            let now = tree.tick()?;
            if let Some(old) = old {
                let node = tree.node_mut(old)?;
                node.links -= 1;
                node.changed = now;
            }
            let parent = tree.node_mut(sp)?;
            parent.entries_mut()?.remove(&sn);
            parent.modified = now;
            parent.changed = now;
            let parent = tree.node_mut(dp)?;
            parent.entries_mut()?.insert(dn, id);
            parent.modified = now;
            parent.changed = now;
            tree.node_mut(id)?.changed = now;
            Ok(())
        })?;
        self.collect()
    }
    /// Replace payload on the same inode: names and existing handles retain identity.
    /// The complete write and quota check commit together, never truncate-then-write.
    pub fn replace(&self, actor: &Credentials, path: &str, bytes: &[u8]) -> Result<u64, Error> {
        if bytes.len() > 1024 * 1024 {
            return Err(Error::Quota);
        }
        let mut state = self.guard()?;
        if state.access.is_readonly() {
            return Err(Error::ReadOnly);
        }
        let mut tree = state.tree.clone();
        let id = tree.resolve(actor, path, true)?;
        let node = tree.node(id)?;
        node.permits(actor, 2)?;
        if !matches!(node.content, Content::File(_)) {
            return Err(Error::IsDirectory);
        }
        check_lock(&state, id, None, true)?;
        if state
            .opens
            .values()
            .any(|open| open.inode == id && open.share == ShareMode::Exclusive)
        {
            return Err(Error::Busy);
        }
        let now = tree.tick()?;
        let node = tree.node_mut(id)?;
        node.content = Content::File(Data::new(bytes)?);
        node.mode = node.mode.without_privilege();
        node.modified = now;
        node.changed = now;
        tree.check_limits()?;
        state.tree = tree;
        state.stats.writes = state.stats.writes.saturating_add(1);
        state.stats.bytes_written = state.stats.bytes_written.saturating_add(bytes.len() as u64);
        Ok(id)
    }
    pub fn chmod(&self, actor: &Credentials, path: &str, mode: VirtualMode) -> Result<(), Error> {
        self.set_access(actor, path, Some(mode), None, None)
    }
    pub fn set_access(
        &self,
        actor: &Credentials,
        path: &str,
        mode: Option<VirtualMode>,
        gid: Option<u32>,
        acl: Option<Acl>,
    ) -> Result<(), Error> {
        self.edit(|tree| {
            let id = tree.resolve(actor, path, true)?;
            let now = tree.tick()?;
            let node = tree.node_mut(id)?;
            use crate::filesystem::access::{update_access, ModeTarget};
            let target = if matches!(node.content, Content::Directory(_)) {
                ModeTarget::Directory
            } else {
                ModeTarget::RegularFile
            };
            update_access(
                actor,
                (&mut node.owner, &mut node.mode, &mut node.acl),
                (mode, gid, acl),
                target,
            )
            .map_err(|_| Error::Denied)?;
            node.changed = now;
            Ok(())
        })
    }
    pub fn acl(&self, actor: &Credentials, path: &str) -> Result<Acl, Error> {
        let state = self.guard()?;
        let id = state.tree.resolve(actor, path, true)?;
        let node = state.tree.node(id)?;
        if matches!(node.content, Content::Directory(_)) {
            node.permits(actor, 1)?;
        } else if actor.uid != 0 && actor.uid != node.owner.uid {
            node.permits(actor, 4)?;
        }
        Ok(node.acl.clone())
    }
    pub fn default_acl(
        &self,
        actor: &Credentials,
        path: &str,
    ) -> Result<Option<DefaultAcl>, Error> {
        let state = self.guard()?;
        let id = state.tree.resolve(actor, path, true)?;
        let node = state.tree.node(id)?;
        node.entries()?;
        node.permits(actor, 1)?;
        Ok(node.default_acl.clone())
    }
    pub fn set_default_acl(
        &self,
        actor: &Credentials,
        path: &str,
        default: Option<DefaultAcl>,
    ) -> Result<(), Error> {
        if let Some(default) = &default {
            default.validate().map_err(|_| Error::Denied)?;
        }
        self.edit(|tree| {
            let id = tree.resolve(actor, path, true)?;
            let now = tree.tick()?;
            let node = tree.node_mut(id)?;
            node.entries()?;
            if actor.uid != 0 && actor.uid != node.owner.uid {
                return Err(Error::Denied);
            }
            node.default_acl = default;
            node.changed = now;
            Ok(())
        })
    }
    pub fn quota(&self, actor: &Credentials, uid: u32, limits: Limits) -> Result<(), Error> {
        if actor.uid != 0 {
            return Err(Error::Denied);
        }
        limits.validate()?;
        self.edit(|tree| {
            tree.quotas.insert(uid, limits);
            Ok(())
        })
    }
    fn collect(&self) -> Result<(), Error> {
        let mut state = self.guard()?;
        let open = retained_open_nodes(&state)?;
        state
            .tree
            .nodes
            .retain(|id, node| node.links != 0 || open.contains(id));
        Ok(())
    }
    pub fn stats(&self) -> Result<Stats, Error> {
        let state = self.guard()?;
        let mut stats = state.stats.clone();
        stats.access = state.access;
        stats.bytes = state.tree.nodes.values().map(Inode::size).sum();
        stats.inodes = state.tree.nodes.len() as u64;
        for node in state.tree.nodes.values() {
            if let Content::File(data) = &node.content {
                stats.allocated_data_bytes += data.allocated() as u64;
                stats.shared_data_pages += data.shared_pages() as u64;
            }
        }
        stats.directory_entries = state
            .tree
            .nodes
            .values()
            .map(|node| match &node.content {
                Content::Directory(entries) => entries.len() as u64,
                _ => 0,
            })
            .sum();
        stats.open_files = state.opens.len() as u64;
        Ok(stats)
    }
    pub fn open(
        self: &Arc<Self>,
        actor: &Credentials,
        path: &str,
        mode: FileMode,
        share: ShareMode,
    ) -> Result<File, Error> {
        self.open_with_umask(actor, path, mode, share, Umask::default())
    }
    pub fn open_with_umask(
        self: &Arc<Self>,
        actor: &Credentials,
        path: &str,
        mode: FileMode,
        share: ShareMode,
        umask: Umask,
    ) -> Result<File, Error> {
        let mut state = self.guard()?;
        if state.opens.len() >= 1024 {
            return Err(Error::Busy);
        }
        let mut tree = state.tree.clone();
        let mut created = false;
        let id = match tree.resolve(actor, path, mode.creation() != FileCreation::Exclusive) {
            Ok(_) if mode.creation() == FileCreation::Exclusive => return Err(Error::Exists),
            Ok(id) => id,
            Err(Error::NotFound) if mode.creation() != FileCreation::Existing => {
                if state.access.is_readonly() {
                    return Err(Error::ReadOnly);
                }
                created = true;
                Self::birth(
                    &mut tree,
                    actor,
                    path,
                    VirtualMode::new(0o666).map_err(|_| Error::Denied)?,
                    Content::File(Data::default()),
                    Some(umask),
                )?
            }
            Err(e) => return Err(e),
        };
        let node = tree.node(id)?;
        if !matches!(node.content, Content::File(_) | Content::Pseudo(_)) {
            return Err(Error::IsDirectory);
        }
        let bits = match mode.access() {
            FileAccess::ReadOnly => 4,
            FileAccess::WriteOnly => 2,
            FileAccess::ReadWrite => 6,
        };
        if !created {
            node.permits(actor, bits)?;
        }
        let device = matches!(node.content,Content::Pseudo(kind) if kind.is_device());
        let generated = match &node.content {
            Content::Pseudo(kind) if !kind.is_device() => {
                Some(kind.render(actor, self.started.elapsed()))
            }
            Content::File(data) if self.pseudo == Some(super::PseudoKind::Proc) => {
                Some(data.read(0, data.len()))
            }
            _ => None,
        };
        if bits & 2 != 0 && state.access.is_readonly() && !device {
            return Err(Error::ReadOnly);
        }
        if state.opens.values().any(|o| {
            o.inode == id && (o.share == ShareMode::Exclusive || share == ShareMode::Exclusive)
        }) {
            return Err(Error::Busy);
        }
        if mode.behavior() == FileBehavior::Truncate && !device {
            check_lock(&state, id, None, true)?;
            let now = tree.tick()?;
            let node = tree.node_mut(id)?;
            node.content = Content::File(Data::default());
            node.mode = node.mode.without_privilege();
            node.modified = now;
            node.changed = now;
        }
        tree.check_limits()?;
        let token = state.next_open;
        let next = token.checked_add(1).ok_or(Error::Overflow)?;
        state.tree = tree;
        state.next_open = next;
        state.opens.insert(
            token,
            Open {
                inode: id,
                mode,
                share,
                lock: None,
                generated,
                anchor: None,
            },
        );
        Ok(File {
            lease: Arc::new(Lease {
                volume: self.clone(),
                token,
            }),
            position: Arc::new(Mutex::new(0)),
        })
    }
    /// Snapshot includes named objects only; open-but-unlinked inodes are runtime state.
    pub fn snapshot(&self, actor: &Credentials, readonly: bool) -> Result<Arc<Self>, Error> {
        let (tree, _) = self.archived_tree(actor)?;
        Self::from_tree_access(
            tree,
            if readonly {
                VolumeAccess::FrozenSnapshot
            } else {
                VolumeAccess::ReadWrite
            },
        )
    }
    pub(super) fn archived_tree(&self, actor: &Credentials) -> Result<(Tree, ArchiveKind), Error> {
        if self.pseudo.is_some() {
            return Err(Error::Unsupported);
        }
        if actor.uid != 0 {
            return Err(Error::Denied);
        }
        let state = self.guard()?;
        let mut tree = state.tree.clone();
        tree.nodes.retain(|_, n| n.links != 0);
        tree.validate()?;
        Ok((tree, state.access.archive_kind()))
    }
    pub fn encode(&self, actor: &Credentials) -> Result<Vec<u8>, Error> {
        let (tree, kind) = self.archived_tree(actor)?;
        archive::encode(&tree, kind)
    }
    pub fn decode(bytes: &[u8], readonly: bool) -> Result<Arc<Self>, Error> {
        if bytes.len() > 256 * 1024 * 1024 {
            return Err(Error::Quota);
        }
        let (tree, kind) = archive::decode(bytes)?;
        tree.validate()?;
        Self::from_tree_access(tree, VolumeAccess::restored(kind, readonly))
    }
}
fn retained_open_nodes(state: &State) -> Result<std::collections::BTreeSet<u64>, Error> {
    let mut retained = std::collections::BTreeSet::new();
    let mut pending: Vec<_> = state.opens.values().map(|o| o.inode).collect();
    while let Some(id) = pending.pop() {
        if !retained.insert(id) {
            continue;
        }
        if let Content::Directory(entries) = &state.tree.node(id)?.content {
            pending.extend(entries.values().copied());
        }
    }
    Ok(retained)
}
fn check_lock(state: &State, inode: u64, token: Option<u64>, write: bool) -> Result<(), Error> {
    if state.opens.iter().any(|(id, o)| {
        Some(*id) != token
            && o.inode == inode
            && (o.lock == Some(LockMode::Exclusive) || write && o.lock == Some(LockMode::Shared))
    }) {
        return Err(Error::LockConflict);
    }
    Ok(())
}
struct Lease {
    volume: Arc<Volume>,
    token: u64,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.volume.guard() {
            state.opens.remove(&self.token);
        }
        let _ = self.volume.collect();
    }
}
/// Search-only name lease. Renaming/removing this directory or an ancestor is
/// Busy until its last clone is dropped. This is not a directory-read grant.
#[derive(Clone)]
pub struct DirectoryAnchor {
    path: String,
    lease: Arc<Lease>,
}
impl DirectoryAnchor {
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn volume_id(&self) -> u64 {
        self.lease.volume.id()
    }
}
impl std::fmt::Debug for DirectoryAnchor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectoryAnchor")
            .field("volume", &self.volume_id())
            .field("path", &self.path)
            .finish()
    }
}
pub(crate) struct Directory {
    lease: Arc<Lease>,
}
impl Directory {
    pub(crate) fn namespace_path(&self) -> Result<String, Error> {
        let state = self.lease.volume.guard()?;
        let open = state
            .opens
            .get(&self.lease.token)
            .ok_or(Error::InvalidHandle)?;
        state.tree.directory_path(open.inode)
    }
    pub(crate) fn entries(&self) -> Result<Vec<(String, InodeStat)>, Error> {
        let state = self.lease.volume.guard()?;
        let open = state
            .opens
            .get(&self.lease.token)
            .ok_or(Error::InvalidHandle)?;
        state
            .tree
            .node(open.inode)?
            .entries()?
            .iter()
            .map(|(name, id)| Ok((name.clone(), state.tree.node(*id)?.stat(*id))))
            .collect()
    }
}
#[derive(Clone)]
pub struct File {
    lease: Arc<Lease>,
    position: Arc<Mutex<u64>>,
}
impl File {
    pub fn position(&self) -> Result<u64, Error> {
        Ok(*self.position.lock().map_err(|_| Error::Busy)?)
    }
    pub fn volume_id(&self) -> u64 {
        self.lease.volume.id()
    }
    pub fn references(&self) -> usize {
        Arc::strong_count(&self.lease)
    }
    pub fn stat(&self) -> Result<InodeStat, Error> {
        let state = self.lease.volume.guard()?;
        let open = state
            .opens
            .get(&self.lease.token)
            .ok_or(Error::InvalidHandle)?;
        let mut stat = state.tree.node(open.inode)?.stat(open.inode);
        if let Some(bytes) = &open.generated {
            stat.length = bytes.len() as u64;
        }
        Ok(stat)
    }
    pub fn lock(&self, mode: LockMode) -> Result<(), Error> {
        let mut state = self.lease.volume.guard()?;
        let open = state
            .opens
            .get(&self.lease.token)
            .ok_or(Error::InvalidHandle)?
            .clone();
        if mode == LockMode::Exclusive && open.mode.access() == FileAccess::ReadOnly {
            return Err(Error::Denied);
        }
        check_lock(
            &state,
            open.inode,
            Some(self.lease.token),
            mode == LockMode::Exclusive,
        )?;
        state
            .opens
            .get_mut(&self.lease.token)
            .ok_or(Error::InvalidHandle)?
            .lock = Some(mode);
        Ok(())
    }
    pub fn unlock(&self) -> Result<(), Error> {
        self.lease
            .volume
            .guard()?
            .opens
            .get_mut(&self.lease.token)
            .ok_or(Error::InvalidHandle)?
            .lock = None;
        Ok(())
    }
    pub fn read_at(&self, offset: u64, maximum: usize) -> Result<Vec<u8>, Error> {
        if maximum > 1024 * 1024 {
            return Err(Error::Quota);
        }
        let mut state = self.lease.volume.guard()?;
        let open = state
            .opens
            .get(&self.lease.token)
            .ok_or(Error::InvalidHandle)?
            .clone();
        if open.mode.access() == FileAccess::WriteOnly {
            return Err(Error::Denied);
        }
        check_lock(&state, open.inode, Some(self.lease.token), false)?;
        let slice = |bytes: &[u8]| {
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(bytes.len());
            bytes[start..start.saturating_add(maximum).min(bytes.len())].to_vec()
        };
        // An open-time diagnostic snapshot is authoritative for both stat and
        // every read path, including Content::File-backed dynamic proc rows.
        let result = if let Some(bytes) = &open.generated {
            slice(bytes)
        } else {
            match &state.tree.node(open.inode)?.content {
                Content::File(bytes) => {
                    bytes.read(usize::try_from(offset).unwrap_or(usize::MAX), maximum)
                }
                Content::Pseudo(super::pseudo::Node::Null) => Vec::new(),
                Content::Pseudo(super::pseudo::Node::Zero | super::pseudo::Node::Full) => {
                    vec![0; maximum]
                }
                _ => return Err(Error::InvalidHandle),
            }
        };
        offset
            .checked_add(result.len() as u64)
            .ok_or(Error::Overflow)?;
        state.stats.reads = state.stats.reads.saturating_add(1);
        state.stats.bytes_read = state.stats.bytes_read.saturating_add(result.len() as u64);
        Ok(result)
    }
    fn change(
        &self,
        offset: u64,
        bytes: &[u8],
        truncate: bool,
        append: bool,
    ) -> Result<(usize, u64), Error> {
        let mut state = self.lease.volume.guard()?;
        let open = state
            .opens
            .get(&self.lease.token)
            .ok_or(Error::InvalidHandle)?
            .clone();
        if open.mode.access() == FileAccess::ReadOnly {
            return Err(Error::Denied);
        }
        if !truncate && !append && open.mode.behavior() == FileBehavior::Append {
            return Err(Error::Unsupported);
        }
        check_lock(&state, open.inode, Some(self.lease.token), true)?;
        if let Content::Pseudo(kind) = state.tree.node(open.inode)?.content {
            if !kind.is_device() {
                return Err(Error::ReadOnly);
            }
            if truncate {
                return Err(Error::Unsupported);
            }
            if bytes.len() > 1024 * 1024 {
                return Err(Error::Quota);
            }
            if matches!(kind, super::pseudo::Node::Full) && !bytes.is_empty() {
                return Err(Error::NoSpace);
            }
            let end = offset
                .checked_add(bytes.len() as u64)
                .ok_or(Error::Overflow)?;
            state.stats.writes = state.stats.writes.saturating_add(1);
            state.stats.bytes_written =
                state.stats.bytes_written.saturating_add(bytes.len() as u64);
            return Ok((bytes.len(), end));
        }
        if state.access.is_readonly() {
            return Err(Error::ReadOnly);
        }
        let mut tree = state.tree.clone();
        let offset = if append {
            tree.node(open.inode)?.size()
        } else {
            offset
        };
        let end = offset
            .checked_add(bytes.len() as u64)
            .ok_or(Error::Overflow)?;
        if end > tree.limits.bytes || bytes.len() > 1024 * 1024 {
            return Err(Error::Quota);
        }
        if !truncate && bytes.is_empty() {
            return Ok((0, offset));
        }
        let now = tree.tick()?;
        let node = tree.node_mut(open.inode)?;
        let Content::File(data) = &mut node.content else {
            return Err(Error::InvalidHandle);
        };
        let copied = if truncate {
            data.resize(end as usize)?
        } else {
            data.write(offset as usize, bytes)?
        };
        node.mode = node.mode.without_privilege();
        node.modified = now;
        node.changed = now;
        tree.check_limits()?;
        state.tree = tree;
        state.stats.cow_copied_bytes = state.stats.cow_copied_bytes.saturating_add(copied);
        state.stats.writes = state.stats.writes.saturating_add(1);
        state.stats.bytes_written = state.stats.bytes_written.saturating_add(bytes.len() as u64);
        Ok((bytes.len(), end))
    }
    pub fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<usize, Error> {
        self.change(offset, bytes, false, false).map(|v| v.0)
    }
    pub fn truncate(&self, length: u64) -> Result<(), Error> {
        self.change(length, &[], true, false).map(|_| ())
    }
    pub fn read(&self, maximum: usize) -> Result<Vec<u8>, Error> {
        let mut position = self.position.lock().map_err(|_| Error::Busy)?;
        let bytes = self.read_at(*position, maximum)?;
        *position += bytes.len() as u64;
        Ok(bytes)
    }
    pub fn write(&self, bytes: &[u8]) -> Result<usize, Error> {
        let mut position = self.position.lock().map_err(|_| Error::Busy)?;
        let append = {
            let state = self.lease.volume.guard()?;
            state
                .opens
                .get(&self.lease.token)
                .ok_or(Error::InvalidHandle)?
                .mode
                .behavior()
                == FileBehavior::Append
        };
        let (count, end) = self.change(*position, bytes, false, append)?;
        *position = end;
        Ok(count)
    }
    pub fn seek(&self, position: std::io::SeekFrom) -> Result<u64, Error> {
        let mut cursor = self.position.lock().map_err(|_| Error::Busy)?;
        let next = match position {
            std::io::SeekFrom::Start(n) => Some(n),
            std::io::SeekFrom::Current(n) => cursor.checked_add_signed(n),
            std::io::SeekFrom::End(n) => self.stat()?.length.checked_add_signed(n),
        }
        .ok_or(Error::Overflow)?;
        *cursor = next;
        Ok(next)
    }
}

impl Volume {
    pub(crate) fn publish_tasks(
        &self,
        rows: &[crate::filesystem::TaskLeaseStatus],
    ) -> Result<(), Error> {
        if rows.len() > 64 {
            return Err(Error::Quota);
        }
        let rows: Vec<_> = rows
            .iter()
            .map(|row| {
                (
                    row.id.value(),
                    row.owner_uid,
                    std::format!(
                        "task_scope_id: {}
owner_uid: {}
lease_state: {:?}
kind: resource-lease
",
                        row.id.value(),
                        row.owner_uid,
                        row.state,
                    )
                    .into_bytes(),
                )
            })
            .collect();
        self.publish_diagnostic_rows("tasks", 0, &rows)
    }
    pub(crate) fn publish_job_rows(&self, rows: &[(u64, u32, Vec<u8>)]) -> Result<(), Error> {
        if rows.len() > 256 {
            return Err(Error::Quota);
        }
        self.publish_diagnostic_rows("jobs", 2, rows)
    }
    pub(crate) fn publish_execution_rows(&self, rows: &[(u64, u32, Vec<u8>)]) -> Result<(), Error> {
        if rows.len() > 32 { return Err(Error::Quota); }
        self.publish_diagnostic_rows("executions", 4, rows)
    }
    fn publish_diagnostic_rows(
        &self,
        group: &str,
        lane: u64,
        rows: &[(u64, u32, Vec<u8>)],
    ) -> Result<(), Error> {
        if self.pseudo != Some(super::PseudoKind::Proc) {
            return Err(Error::Unsupported);
        }
        let mut state = self.guard()?;
        let mut tree = state.tree.clone();
        let parent = *tree.node(1)?.entries()?.get(group).ok_or(Error::NotFound)?;
        let mut names = BTreeMap::new();
        let old: Vec<_> = tree.node(parent)?.entries()?.values().copied().collect();
        for dir in old {
            let children: Vec<_> = tree.node(dir)?.entries()?.values().copied().collect();
            for child in children {
                tree.node_mut(child)?.links = 0;
            }
            tree.node_mut(dir)?.links = 0;
        }
        for (number, uid, bytes) in rows {
            // Separate inode pairs for leases, jobs and executions; IDs never reuse.
            let id = number
                .checked_mul(8)
                .and_then(|id| id.checked_add(64 + lane))
                .ok_or(Error::Overflow)?;
            let status_id = id.checked_add(1).ok_or(Error::Overflow)?;
            tree.next = tree.next.max(id.checked_add(2).ok_or(Error::Overflow)?);
            let mut directory = tree.node(parent)?.clone();
            directory.owner = FileOwner {
                uid: *uid,
                gid: *uid,
            };
            directory.mode = VirtualMode::new(0o500).map_err(|_| Error::Denied)?;
            directory.content = Content::Directory(BTreeMap::from([("status".into(), status_id)]));
            directory.links = 1;
            let mut status = directory.clone();
            status.mode = VirtualMode::new(0o400).map_err(|_| Error::Denied)?;
            status.content = Content::File(Data::new(bytes)?);
            tree.nodes.insert(id, directory);
            tree.nodes.insert(status_id, status);
            if names.insert(number.to_string(), id).is_some() {
                return Err(Error::Exists);
            }
        }
        tree.node_mut(parent)?.content = Content::Directory(names);
        let retained = retained_open_nodes(&state)?;
        tree.nodes
            .retain(|id, n| n.links != 0 || retained.contains(id));
        tree.check_limits()?;
        state.tree = tree;
        Ok(())
    }
}
