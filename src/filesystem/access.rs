//! Account directory authority and joint policy/namespace transactions.
//! Directory markers prove protocol membership, NOT host inode identity against
//! a malicious host that deliberately transplants those markers.
use super::access_model::{
    directory_magic, AccessCatalog, CreationAccess, CreationGroup, CreationMode, MAX_ACCESS_BYTES,
};
use super::backend::{linked, virtual_relative};
use super::ownership::{read_bounded, store_path};
use super::ownership_model::{key, Catalog};
use super::transaction::NamespaceGuard;
use super::{
    Acl, CleanupStatus, CommitReceipt, Credentials, DataRoot, DefaultAcl, DirectoryInfo, FileOwner,
    FsError, Group, OwnedFileInfo, VirtualMode,
};
use crate::permissions::Access;
use crate::users::Account;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::{format, matches};
#[path = "access_replace.rs"]
mod replacement;
pub(super) const DIRECTORY_MARKER: &str = ".elara-dir-owner";
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum AccessFault {
    None,
    #[cfg(test)]
    BeforeDirectoryPublish,
    #[cfg(test)]
    AfterNamespace,
    #[cfg(test)]
    AfterReplaceRetirement,
    #[cfg(test)]
    AfterFiles,
    #[cfg(test)]
    AfterCatalog,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) enum Change {
    Policy,
    Create {
        path: String,
        id: u64,
    },
    Remove {
        path: String,
        id: u64,
    },
    Rename {
        source: String,
        destination: String,
        id: u64,
    },
    RenameReplace {
        source: String,
        destination: String,
        id: u64,
        replaced: u64,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    old: AccessCatalog,
    new: AccessCatalog,
    files: Option<(String, String)>,
    change: Change,
}
impl Intent {
    fn validate(&self) -> Result<(), FsError> {
        let bad = || FsError::InvalidOwnershipRecord;
        let mut expected = self.old.directories.clone();
        let mut defaults = self.old.default_acl.clone();
        match &self.change {
            Change::Policy => {
                defaults = self.new.default_acl.clone();
                if self.new.next_dir != self.old.next_dir
                    || self.new.directories.len() != expected.len()
                {
                    return Err(bad());
                }
                for (path, info) in &self.new.directories {
                    let old = expected.get(path).ok_or_else(bad)?;
                    if info.id != old.id
                        || info.owner.uid != old.owner.uid
                        || info.delete_condition != old.delete_condition
                    {
                        return Err(bad());
                    }
                }
            }
            Change::Create { path, id } => {
                if *id != self.old.next_dir
                    || self.new.next_dir != id.checked_add(1).ok_or_else(bad)?
                    || expected.contains_key(path)
                {
                    return Err(bad());
                }
                expected.insert(
                    path.clone(),
                    self.new.directories.get(path).ok_or_else(bad)?,
                )?;
                if let Some(acl) = self.new.default_acl.get(id) {
                    if !self.old.default_acl.values().any(|parent| parent == acl) {
                        return Err(bad());
                    }
                    defaults.insert(*id, acl.clone());
                }
                if expected != self.new.directories {
                    return Err(bad());
                }
            }
            Change::Remove { path, id } => {
                let old = expected.remove(path).ok_or_else(bad)?;
                defaults.remove(id);
                if old.id != *id
                    || self.new.next_dir != self.old.next_dir
                    || expected != self.new.directories
                {
                    return Err(bad());
                }
            }
            Change::Rename {
                source,
                destination,
                id,
            }
            | Change::RenameReplace {
                source,
                destination,
                id,
                ..
            } => {
                if self.old.directories.get(source).is_none_or(|d| d.id != *id)
                    || within(destination, source)
                    || self.new.next_dir != self.old.next_dir
                {
                    return Err(bad());
                }
                if let Change::RenameReplace { replaced, .. } = &self.change {
                    let victim = expected.remove(destination).ok_or_else(bad)?;
                    defaults.remove(replaced);
                    if victim.id != *replaced
                        || id == replaced
                        || self.files.is_none()
                        || self
                            .old
                            .directories
                            .keys()
                            .any(|p| p != destination && within(p, destination))
                    {
                        return Err(bad());
                    }
                    let mut unchanged = self.old.clone();
                    unchanged.directories = self.new.directories.clone();
                    unchanged.default_acl = self.new.default_acl.clone();
                    unchanged.bump()?;
                    if unchanged != self.new {
                        return Err(bad());
                    }
                }
                for info in self
                    .old
                    .directories
                    .values()
                    .filter(|d| within(&d.path, source))
                {
                    expected.remove(&info.path);
                    let mut next = info.clone();
                    next.path = format!("{destination}{}", &info.path[source.len()..]);
                    if expected.contains_key(&next.path) {
                        return Err(bad());
                    }
                    expected.insert(next.path.clone(), next)?;
                }
                if expected != self.new.directories {
                    return Err(bad());
                }
            }
        }
        if defaults != self.new.default_acl {
            return Err(bad());
        }
        for (id, record) in &self.new.resources {
            if let Some(old) = self.old.resources.get(id) {
                if old.id != record.id
                    || old.path != record.path
                    || old.owner != record.owner
                    || old.kind != record.kind
                    || old.association != record.association
                {
                    return Err(bad());
                }
                if old.state == super::ResourceState::Removing && record.state != old.state {
                    return Err(bad());
                }
                if old.state == super::ResourceState::Evicting
                    && record.state == super::ResourceState::Removing
                    && !matches!(record.association, super::ResourceAssociation::Application(parent)
                        if self.new.resources.get(&parent).is_some_and(|app| app.state == super::ResourceState::Removing))
                {
                    return Err(bad());
                }
            } else if !matches!(&self.change, Change::Create { path, id: directory } if path == &record.path && *directory == id.value())
                || record.state != super::ResourceState::Active
            {
                return Err(bad());
            }
        }
        for (id, old) in &self.old.resources {
            if !self.new.resources.contains_key(id)
                && (old.state != super::ResourceState::Removing
                    || self
                        .new
                        .directories
                        .values()
                        .any(|d| d.delete_condition == super::DeleteCondition::ResourceDelete(*id)))
            {
                return Err(bad());
            }
        }
        let added: Vec<_> = self
            .new
            .tasks
            .keys()
            .filter(|id| !self.old.tasks.contains_key(id))
            .collect();
        if added.len() > 1
            || self.new.next_task
                != self
                    .old
                    .next_task
                    .checked_add(added.len() as u64)
                    .ok_or_else(bad)?
            || added
                .first()
                .is_some_and(|id| id.value() != self.old.next_task)
        {
            return Err(bad());
        }
        for (id, uid) in &self.old.tasks {
            if self.new.tasks.get(id).is_some_and(|next| next != uid) {
                return Err(bad());
            }
        }
        if let Some((old, new)) = &self.files {
            let old = Catalog::decode(old.as_bytes())?;
            let new = Catalog::decode(new.as_bytes())?;
            if new.revision != old.revision.checked_add(1).ok_or_else(bad)?
                || new.next_id != old.next_id
                || new.entries.len() != old.entries.len()
            {
                return Err(bad());
            }
            if let Change::RenameReplace { destination, .. } = &self.change {
                if old.entries.keys().any(|p| within(p, destination)) {
                    return Err(bad());
                }
            }
            for info in old.entries.values() {
                let mut path = info.path.clone();
                if let Change::Rename {
                    source,
                    destination,
                    ..
                }
                | Change::RenameReplace {
                    source,
                    destination,
                    ..
                } = &self.change
                {
                    if within(&path, source) {
                        path = format!("{destination}{}", &path[source.len()..]);
                    }
                }
                let next = new.entries.get(&path).ok_or_else(bad)?;
                if info.kind != next.kind
                    || info.id != next.id
                    || info.storage_generation != next.storage_generation
                    || info.owner.uid != next.owner.uid
                    || info.storage_class != next.storage_class
                    || info.delete_condition != next.delete_condition
                {
                    return Err(bad());
                }
                if !matches!(self.change, Change::Policy)
                    && (info.owner != next.owner || info.mode != next.mode)
                {
                    return Err(bad());
                }
            }
        }
        Ok(())
    }
}
fn directory_owner(id: u64) -> PathBuf {
    store_path().join(format!("directory-{id}.owner"))
}
fn stage(id: u64) -> PathBuf {
    store_path().join(format!("directory-{id}.stage"))
}
fn retired(id: u64) -> PathBuf {
    store_path().join(format!("directory-{id}.retired"))
}
fn parent(path: &str) -> &str {
    path.rsplit_once('/')
        .map_or("/", |(p, _)| if p.is_empty() { "/" } else { p })
}
fn within(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}
impl DataRoot {
    pub(super) fn load_access_catalog(&self) -> Result<AccessCatalog, FsError> {
        let Some(store) = self.existing_owner_store()? else {
            return Ok(AccessCatalog::default());
        };
        match store.symlink_metadata("access.catalog") {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(AccessCatalog::default()),
            Err(e) => Err(e.into()),
            Ok(meta) if linked(&meta) || !meta.is_file() => Err(FsError::InvalidOwnershipRecord),
            Ok(_) => AccessCatalog::decode(&read_bounded(
                &store,
                Path::new("access.catalog"),
                MAX_ACCESS_BYTES,
            )?),
        }
    }
    pub fn credentials_as(&self, user: &Account) -> Result<Credentials, FsError> {
        let _guard = self.namespace_guard()?;
        Ok(self.load_access_catalog()?.actor_credentials(user))
    }
    pub(super) fn authorize_file(
        &self,
        user: &Account,
        info: &OwnedFileInfo,
        read: bool,
        write: bool,
    ) -> Result<(), FsError> {
        let catalog = self.load_access_catalog()?;
        self.search_locked(user, &info.path, &catalog)?;
        let acl = catalog
            .file_acl
            .get(&info.id.value())
            .cloned()
            .unwrap_or_default();
        if acl.permits(
            info.owner,
            info.mode,
            &catalog.actor_credentials(user),
            (u8::from(read) * 4) | (u8::from(write) * 2),
        ) {
            Ok(())
        } else {
            Err(FsError::OwnershipDenied)
        }
    }
    pub(super) fn directory_at_locked(
        &self,
        path: &str,
        catalog: &AccessCatalog,
    ) -> Result<Option<DirectoryInfo>, FsError> {
        if let Some(info) = catalog.directories.get(&key(path)?) {
            return Ok(Some(info));
        }
        // Windows Unicode case aliases must not bypass policy by missing an
        // ASCII-canonical map key. Inspect the actual reserved marker, then
        // prove its binding to the registered ID; bytes alone are insufficient.
        let public = virtual_relative(path)?.join(DIRECTORY_MARKER);
        match self.dir.symlink_metadata(&public) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) if e.kind() == io::ErrorKind::NotADirectory => return Ok(None),
            Err(e) => return Err(e.into()),
            Ok(_) => {}
        }
        self.inspect_relative(&public)?;
        let bytes = read_bounded(&self.dir, &public, 128)?;
        let info = catalog
            .directories
            .values()
            .find(|info| directory_magic(info.id) == bytes)
            .ok_or(FsError::OwnershipBindingUnconfirmed)?;
        self.compare_directory_markers(&public, &directory_owner(info.id))?;
        Ok(Some(info))
    }
    pub(super) fn search_locked(
        &self,
        user: &Account,
        path: &str,
        catalog: &AccessCatalog,
    ) -> Result<(), FsError> {
        let relative = virtual_relative(path)?;
        for ancestor in relative.ancestors().skip(1) {
            if ancestor.as_os_str().is_empty() {
                continue;
            }
            let text = format!("/{}", ancestor.to_string_lossy().replace('\\', "/"));
            if let Some(info) = self.directory_at_locked(&text, catalog)? {
                self.directory_access_locked(user, &info, 1, catalog)?;
            }
        }
        Ok(())
    }
    pub(super) fn directory_access_locked(
        &self,
        user: &Account,
        info: &DirectoryInfo,
        bits: u8,
        catalog: &AccessCatalog,
    ) -> Result<(), FsError> {
        // Caller holds namespace guard. Binding probe uses that same guard; no
        // recursive lock acquisition through account-aware facade methods.
        if !info.acl.permits(
            info.owner,
            info.mode,
            &catalog.actor_credentials(user),
            bits,
        ) {
            return Err(FsError::OwnershipDenied);
        }
        self.verify_directory_unlocked(info)
    }
    fn verify_directory_unlocked(&self, info: &DirectoryInfo) -> Result<(), FsError> {
        let relative = virtual_relative(&info.path)?;
        self.inspect_relative(&relative)?;
        if !self.dir.metadata(&relative)?.is_dir() {
            return Err(FsError::NotDirectory);
        }
        let public = relative.join(DIRECTORY_MARKER);
        self.inspect_relative(&public)?;
        self.inspect_relative(&directory_owner(info.id))?;
        if read_bounded(&self.dir, &public, 128)? != directory_magic(info.id)
            || read_bounded(&self.dir, &directory_owner(info.id), 128)? != directory_magic(info.id)
        {
            return Err(FsError::OwnershipBindingUnconfirmed);
        }
        // Both marker handles remain open throughout the lock identity probe.
        self.compare_directory_markers(&public, &directory_owner(info.id))
    }
    fn compare_directory_markers(&self, public: &Path, private: &Path) -> Result<(), FsError> {
        // Reuse the OS-lock identity implementation with an already-held guard
        // token supplied by the caller, never a new namespace lock.
        super::binding::same_marker(self, public, private)
    }
    pub(super) fn parent_mutation_locked(&self, user: &Account, path: &str) -> Result<(), FsError> {
        let catalog = self.load_access_catalog()?;
        self.search_locked(user, path, &catalog)?;
        if let Some(info) = self.directory_at_locked(parent(path), &catalog)? {
            self.directory_access_locked(user, &info, 3, &catalog)?;
        }
        self.ensure_write_class(path)?;
        Ok(())
    }
    /// Caller holds the namespace lock and separately proves target ownership/binding.
    /// Unregistered parent directories retain the legacy target-owner restriction.
    pub(super) fn name_removal_locked(
        &self,
        user: &Account,
        path: &str,
        target: FileOwner,
    ) -> Result<super::access_model::NameAuthority, FsError> {
        use super::access_model::NameAuthority;
        let catalog = self.load_access_catalog()?;
        self.search_locked(user, path, &catalog)?;
        self.ensure_write_class(path)?;
        if let Some(info) = self.directory_at_locked(parent(path), &catalog)? {
            self.directory_access_locked(user, &info, 3, &catalog)?;
            if !info.mode.is_sticky() {
                return Ok(NameAuthority::ParentDirectory);
            }
            if user.uid == 0 {
                return Ok(NameAuthority::VirtualRoot);
            }
            if user.uid == target.uid {
                return Ok(NameAuthority::TargetOwner);
            }
            if user.uid == info.owner.uid {
                return Ok(NameAuthority::DirectoryOwner);
            }
        } else {
            if user.uid == 0 {
                return Ok(NameAuthority::VirtualRoot);
            }
            if user.uid == target.uid {
                return Ok(NameAuthority::LegacyTargetOwner);
            }
        }
        Err(FsError::OwnershipDenied)
    }
    pub(super) fn creation_group_locked(
        &self,
        user: &Account,
        path: &str,
        catalog: &AccessCatalog,
    ) -> Result<CreationGroup, FsError> {
        if let Some(info) = self.directory_at_locked(parent(path), catalog)? {
            self.verify_directory_unlocked(&info)?;
            if info.mode.is_setgid() {
                return Ok(CreationGroup::SetgidDirectory(info.owner.gid));
            }
        }
        Ok(CreationGroup::Primary(catalog.actor_credentials(user).gid))
    }
    pub fn directory_info_as(
        &self,
        user: &Account,
        path: &str,
    ) -> Result<Option<DirectoryInfo>, FsError> {
        if let super::vfs::Route::Mounted {
            volume,
            path: relative,
            ..
        } = self.mounted(user, path, Access::Read)?
        {
            let credentials = self.credentials_as(user)?;
            let stat = volume.stat(&credentials, &relative, true)?;
            if stat.kind != super::vfs::InodeKind::Directory {
                return Ok(None);
            }
            volume.search_directory(&credentials, &relative)?;
            return Ok(Some(DirectoryInfo {
                id: stat.inode,
                path: path.to_owned(),
                owner: stat.owner,
                mode: stat.mode,
                acl: volume.acl(&credentials, &relative)?,
                delete_condition: super::DeleteCondition::OwnerRequest,
            }));
        }
        super::ownership::user_path(user, path, Access::Read)?;
        let _guard = self.namespace_guard()?;
        let resolved_path = self.resolve_host_locked(user, path, Access::Read, true, &_guard)?;
        let path = resolved_path.as_str();
        let catalog = self.load_access_catalog()?;
        self.search_locked(user, path, &catalog)?;
        let info = self.directory_at_locked(path, &catalog)?;
        if let Some(info) = &info {
            self.directory_access_locked(user, info, 1, &catalog)?;
        }
        Ok(info)
    }
    pub fn validate_directory_as(&self, user: &Account, path: &str) -> Result<(), FsError> {
        if let super::vfs::Route::Mounted { volume, path, .. } =
            self.mounted(user, path, Access::Read)?
        {
            return Ok(volume.search_directory(&self.credentials_as(user)?, &path)?);
        }
        super::ownership::user_path(user, path, Access::Read)?;
        let _guard = self.namespace_guard()?;
        let resolved_path = self.resolve_host_locked(user, path, Access::Read, true, &_guard)?;
        let path = resolved_path.as_str();
        let catalog = self.load_access_catalog()?;
        self.search_locked(user, path, &catalog)?;
        if let Some(info) = self.directory_at_locked(path, &catalog)? {
            self.directory_access_locked(user, &info, 1, &catalog)?;
        }
        self.validate_directory_unchecked(path)
    }
    pub fn list_as(&self, user: &Account, path: &str) -> Result<Vec<super::Entry>, FsError> {
        let mounted = !std::matches!(self.vfs.route(path)?, super::vfs::Route::Host);
        let mut cursor = self.open_directory_as(user, path)?;
        let mut entries = Vec::new();
        while cursor.position() < cursor.len() {
            for entry in cursor.read(crate::config::ROOTFS_DEFAULTS.max_directory_page)? {
                entries.push(super::Entry {
                    name: entry.name,
                    kind: match entry.kind {
                        super::ObjectKind::Directory => super::EntryKind::Directory,
                        super::ObjectKind::BlockedLink => super::EntryKind::BlockedLink,
                        super::ObjectKind::Symlink => super::EntryKind::Symlink,
                        super::ObjectKind::Other if mounted => super::EntryKind::Device,
                        _ => super::EntryKind::File,
                    },
                });
            }
        }
        Ok(entries)
    }
    pub fn open_directory_as(
        &self,
        user: &Account,
        path: &str,
    ) -> Result<super::DirectoryCursor, FsError> {
        if let super::vfs::Route::Mounted {
            volume,
            path,
            mount,
        } = self.mounted(user, path, Access::Read)?
        {
            return super::DirectoryCursor::virtual_directory(
                volume.open_directory(&self.credentials_as(user)?, &path)?,
            )?
            .with_namespace(self.vfs.clone(), mount);
        }
        super::ownership::user_path(user, path, Access::Read)?;
        let _guard = self.namespace_guard()?;
        let resolved_path = self.resolve_host_locked(user, path, Access::Read, true, &_guard)?;
        let path = resolved_path.as_str();
        let catalog = self.load_access_catalog()?;
        self.search_locked(user, path, &catalog)?;
        if let Some(info) = self.directory_at_locked(path, &catalog)? {
            self.directory_access_locked(user, &info, 5, &catalog)?;
        }
        self.open_directory_unchecked(path)?
            .with_host_links(self, path, &_guard)?
            .with_namespace(self.vfs.clone(), path.to_owned())
    }
    pub fn create_directory_as(
        &self,
        user: &Account,
        path: &str,
        options: super::CreationOptions<'_>,
    ) -> Result<CommitReceipt, FsError> {
        self.create_directory_registered_as(user, path, options, None)
    }
    pub(super) fn create_directory_registered_as(
        &self,
        user: &Account,
        path: &str,
        options: super::CreationOptions<'_>,
        registration: Option<(super::ResourceKind, super::ResourceLimits, Option<&str>)>,
    ) -> Result<CommitReceipt, FsError> {
        if let super::vfs::Route::Mounted {
            volume,
            path: relative,
            ..
        } = self.mounted_name(user, path, Access::Write)?
        {
            if registration.is_some() {
                return Err(super::vfs::Error::Unsupported.into());
            }
            options.condition(self, user, path)?;
            volume.mkdir_with_umask(&self.credentials_as(user)?, &relative, options.umask)?;
            return Ok(super::vfs_bridge::volatile_receipt(0));
        }
        super::ownership::user_path(user, path, Access::Write)?;
        let guard = self.namespace_guard()?;
        let resolved_path = self.resolve_host_locked(user, path, Access::Write, false, &guard)?;
        let path = resolved_path.as_str();
        self.resolve_new(path)?;
        self.parent_mutation_locked(user, path)?;
        let old = self.load_access_catalog()?;
        let mut new = old.clone();
        if new.directories.len() >= 4096 {
            return Err(FsError::OwnershipCapacity);
        }
        let id = new.next_dir;
        new.next_dir = id
            .checked_add(1)
            .ok_or(FsError::OwnershipIdentityExhausted)?;
        new.bump()?;
        let policy = self.creation_access_locked(
            path,
            CreationMode::Requested(VirtualMode::new(0o777)?),
            options.umask,
            &old,
        )?;
        let group = self.creation_group_locked(user, path, &old)?;
        let path = key(path)?;
        let (mode, acl) = match policy {
            CreationAccess::Inherited { mode, acl, default } => {
                new.default_acl.insert(id, default);
                (mode, acl)
            }
            CreationAccess::Explicit(mode) | CreationAccess::Umask(mode) => (mode, Acl::default()),
        };
        let condition = if let Some((kind, limits, application)) = registration {
            if user.uid != 0 || !super::resource_model::matches_path(kind, &path) {
                return Err(FsError::OwnershipDenied);
            }
            limits.validate()?;
            if new.resources.len() >= 64 {
                return Err(FsError::OwnershipCapacity);
            }
            let association = match application {
                Some(application) => {
                    let application = key(application)?;
                    let app = old
                        .resources
                        .values()
                        .find(|r| r.path == application)
                        .ok_or(FsError::NotFound)?;
                    if app.kind != super::ResourceKind::Application {
                        return Err(FsError::OwnershipClassMismatch);
                    }
                    if app.state != super::ResourceState::Active {
                        return Err(FsError::ResourceInactive);
                    }
                    super::ResourceAssociation::Application(app.id)
                }
                None => super::ResourceAssociation::Independent,
            };
            let resource = super::ResourceRecord {
                id: super::ResourceId(id),
                path: path.clone(),
                owner: user.uid,
                kind,
                limits,
                state: super::ResourceState::Active,
                association,
            };
            new.resources.insert(resource.id, resource);
            super::DeleteCondition::ResourceDelete(super::ResourceId(id))
        } else {
            let condition = options.condition(self, user, &path)?;
            self.check_resource_directory_locked(condition)?;
            condition
        };
        let info = DirectoryInfo {
            id,
            path: path.clone(),
            owner: FileOwner {
                uid: user.uid,
                gid: group.gid(),
            },
            mode: group.directory_mode(mode),
            acl,
            delete_condition: condition,
        };
        new.directories.insert(path.clone(), info)?;
        let intent = Intent {
            old,
            new,
            files: None,
            change: Change::Create { path, id },
        };
        // Check capacity/canonical roundtrip BEFORE writing any object bytes.
        self.access_intent_bytes(&intent)?;
        self.ensure_owner_store(&guard)?;
        self.commit_access_intent(intent, &guard)
    }
    pub fn remove_directory_as(
        &self,
        user: &Account,
        path: &str,
    ) -> Result<CommitReceipt, FsError> {
        if let super::vfs::Route::Mounted { volume, path, .. } =
            self.mounted_name(user, path, Access::Write)?
        {
            volume.unlink(&self.credentials_as(user)?, &path, true)?;
            return Ok(super::vfs_bridge::volatile_receipt(0));
        }
        super::ownership::user_path(user, path, Access::Write)?;
        let guard = self.namespace_guard()?;
        let resolved_path = self.resolve_host_locked(user, path, Access::Write, false, &guard)?;
        let path = resolved_path.as_str();
        self.remove_directory_locked(path, user, &guard)
    }
    pub(super) fn remove_directory_locked(
        &self,
        path: &str,
        user: &Account,
        guard: &NamespaceGuard<'_>,
    ) -> Result<CommitReceipt, FsError> {
        if self.is_protected(path)? {
            return Err(FsError::ProtectedPath);
        }
        let relative = self
            .resolve(path)
            .map_err(super::open::classify_open_error)?;
        if !self.dir.metadata(relative)?.is_dir() {
            return Err(FsError::NotDirectory);
        }
        self.parent_mutation_locked(user, path)?;
        let old = self.load_access_catalog()?;
        let path = key(path)?;
        let info = old
            .directories
            .get(&path)
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        self.resource_directory_removal_locked(&info, &old)?;
        self.name_removal_locked(user, &info.path, info.owner)?;
        self.verify_directory_unlocked(&info)?;
        for result in self.dir.read_dir(virtual_relative(&path)?)? {
            if result?.file_name() != DIRECTORY_MARKER {
                return Err(FsError::DirectoryNotEmpty);
            }
        }
        let id = info.id;
        let mut new = old.clone();
        new.directories.remove(&path);
        new.default_acl.remove(&id);
        new.bump()?;
        self.commit_access_intent(
            Intent {
                old,
                new,
                files: None,
                change: Change::Remove { path, id },
            },
            guard,
        )
    }
    pub fn rename_directory_as(
        &self,
        user: &Account,
        source: &str,
        destination: &str,
    ) -> Result<CommitReceipt, FsError> {
        self.rename_directory_configured(user, source, destination, false)
    }
    pub(super) fn rename_directory_configured(
        &self,
        user: &Account,
        source: &str,
        destination: &str,
        replace: bool,
    ) -> Result<CommitReceipt, FsError> {
        super::ownership::user_path(user, source, Access::Write)?;
        super::ownership::user_path(user, destination, Access::Write)?;
        let guard = self.namespace_guard()?;
        let resolved_source =
            self.resolve_host_locked(user, source, Access::Write, false, &guard)?;
        let source = resolved_source.as_str();
        let resolved_destination =
            self.resolve_host_locked(user, destination, Access::Write, false, &guard)?;
        let destination = resolved_destination.as_str();
        self.parent_mutation_locked(user, source)?;
        self.parent_mutation_locked(user, destination)?;
        if self.is_protected(source)? || (replace && self.is_protected(destination)?) {
            return Err(FsError::ProtectedPath);
        }
        let source = key(source)?;
        let destination = key(destination)?;
        self.resource_rename_locked(&source, &destination)?;
        if source != destination && within(&destination, &source) {
            return Err(FsError::InvalidPath);
        }
        let old = self.load_access_catalog()?;
        let info = old
            .directories
            .get(&source)
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        self.name_removal_locked(user, &info.path, info.owner)?;
        let id = info.id;
        self.verify_directory_unlocked(&info)?;
        if replace && source == destination {
            return Ok(CommitReceipt {
                bytes: 0,
                directory_durability: super::DirectoryDurability::Unconfirmed(
                    io::ErrorKind::Unsupported,
                ),
                cleanup: CleanupStatus::Complete,
            });
        }
        let replaced = if replace {
            if let Some(victim) = old.directories.get(&destination) {
                self.verify_directory_unlocked(&victim)?;
                self.name_removal_locked(user, &destination, victim.owner)?;
                self.resource_directory_removal_locked(&victim, &old)?;
                self.require_empty_owned_directory(&destination)?;
                Some(victim.id)
            } else {
                self.resolve_new(&destination)?;
                None
            }
        } else {
            self.resolve_new(&destination)?;
            None
        };
        if super::ownership::storage_class(&source)?
            != super::ownership::storage_class(&destination)?
        {
            return Err(FsError::OwnershipClassMismatch);
        }
        let mut new = old.clone();
        new.bump()?;
        if let Some(id) = replaced {
            new.directories.remove(&destination);
            new.default_acl.remove(&id);
        }
        for info in old
            .directories
            .values()
            .filter(|info| within(&info.path, &source))
        {
            self.verify_directory_unlocked(&info)?;
            let mut changed = info.clone();
            changed.path = format!("{destination}{}", &info.path[source.len()..]);
            new.directories.remove(&info.path);
            new.directories.insert(changed.path.clone(), changed)?;
        }
        let store = self
            .existing_owner_store()?
            .ok_or(FsError::UnownedOwnershipStore)?;
        let files = self.load_owner_catalog(&store)?;
        let mut changed = files.clone();
        for info in files
            .entries
            .values()
            .filter(|info| within(&info.path, &source))
        {
            self.verify_public_binding(&info, &guard)?;
            let mut next = info.clone();
            next.path = format!("{destination}{}", &info.path[source.len()..]);
            changed.entries.remove(&info.path);
            changed.entries.insert(next.path.clone(), next)?;
        }
        changed.revision = changed
            .revision
            .checked_add(1)
            .ok_or(FsError::OwnershipIdentityExhausted)?;
        self.commit_access_intent(
            Intent {
                old,
                new,
                files: Some((
                    String::from_utf8(files.encode()?)
                        .map_err(|_| FsError::InvalidOwnershipRecord)?,
                    String::from_utf8(changed.encode()?)
                        .map_err(|_| FsError::InvalidOwnershipRecord)?,
                )),
                change: match replaced {
                    Some(replaced) => Change::RenameReplace {
                        source,
                        destination,
                        id,
                        replaced,
                    },
                    None => Change::Rename {
                        source,
                        destination,
                        id,
                    },
                },
            },
            &guard,
        )
    }
    fn access_intent_bytes(&self, intent: &Intent) -> Result<Vec<u8>, FsError> {
        intent.old.encode()?;
        intent.new.encode()?;
        intent.validate()?;
        if intent.new.revision
            != intent
                .old
                .revision
                .checked_add(1)
                .ok_or(FsError::OwnershipIdentityExhausted)?
        {
            return Err(FsError::InvalidOwnershipRecord);
        }
        let bytes = serde_json::to_vec(intent).map_err(|_| FsError::InvalidOwnershipRecord)?;
        if bytes.len() > crate::config::ROOTFS_DEFAULTS.max_transaction_bytes {
            return Err(FsError::OwnershipCapacity);
        }
        Ok(bytes)
    }
    pub(super) fn validate_policy_change(
        &self,
        old: &AccessCatalog,
        new: &AccessCatalog,
    ) -> Result<(), FsError> {
        self.access_intent_bytes(&Intent {
            old: old.clone(),
            new: new.clone(),
            files: None,
            change: Change::Policy,
        })?;
        Ok(())
    }
    pub(super) fn commit_policy_locked(
        &self,
        old: AccessCatalog,
        new: AccessCatalog,
        files: Option<(Catalog, Catalog)>,
        guard: &NamespaceGuard<'_>,
    ) -> Result<CommitReceipt, FsError> {
        let files = files
            .map(|(a, b)| {
                Ok::<_, FsError>((
                    String::from_utf8(a.encode()?).map_err(|_| FsError::InvalidOwnershipRecord)?,
                    String::from_utf8(b.encode()?).map_err(|_| FsError::InvalidOwnershipRecord)?,
                ))
            })
            .transpose()?;
        self.commit_access_intent(
            Intent {
                old,
                new,
                files,
                change: Change::Policy,
            },
            guard,
        )
    }
    fn commit_access_intent(
        &self,
        intent: Intent,
        guard: &NamespaceGuard<'_>,
    ) -> Result<CommitReceipt, FsError> {
        let bytes = self.access_intent_bytes(&intent)?;
        self.ensure_owner_store(guard)?;
        self.publish_protocol_locked(&store_path().join("access.pending"), &bytes, true, guard)?;
        #[cfg(test)]
        let fault = self.access_fault;
        #[cfg(not(test))]
        let fault = AccessFault::None;
        self.finish_access_intent(&intent, guard, fault)
            .map_err(|error| FsError::AccessCommitPending(Box::new(error)))
    }
    pub(super) fn recover_access_intent_locked(
        &self,
        guard: &NamespaceGuard<'_>,
    ) -> Result<(), FsError> {
        let Some(store) = self.existing_owner_store()? else {
            return Ok(());
        };
        match store.symlink_metadata("access.pending") {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
            Ok(meta) if linked(&meta) || !meta.is_file() => {
                return Err(FsError::InvalidOwnershipRecord)
            }
            Ok(_) => {}
        }
        let bytes = read_bounded(
            &store,
            Path::new("access.pending"),
            crate::config::ROOTFS_DEFAULTS.max_transaction_bytes,
        )?;
        let intent: Intent =
            serde_json::from_slice(&bytes).map_err(|_| FsError::InvalidOwnershipRecord)?;
        if self.access_intent_bytes(&intent)? != bytes {
            return Err(FsError::InvalidOwnershipRecord);
        }
        self.finish_access_intent(&intent, guard, AccessFault::None)?;
        Ok(())
    }
    fn finish_access_intent(
        &self,
        intent: &Intent,
        guard: &NamespaceGuard<'_>,
        _fault: AccessFault,
    ) -> Result<CommitReceipt, FsError> {
        let current = self.load_access_catalog()?;
        if current != intent.old && current != intent.new {
            return Err(FsError::OwnershipRecoveryBlocked);
        }
        if let Some((a, b)) = &intent.files {
            let old = Catalog::decode(a.as_bytes())?;
            let new = Catalog::decode(b.as_bytes())?;
            let store = self
                .existing_owner_store()?
                .ok_or(FsError::UnownedOwnershipStore)?;
            let current = self.load_owner_catalog(&store)?.encode()?;
            if current != old.encode()? && current != new.encode()? {
                return Err(FsError::OwnershipRecoveryBlocked);
            }
        }
        match &intent.change {
            Change::Policy => {}
            Change::Create { path, id } => {
                let info = intent
                    .new
                    .directories
                    .get(path)
                    .ok_or(FsError::InvalidOwnershipRecord)?;
                if info.id != *id || intent.old.directories.contains_key(path) {
                    return Err(FsError::InvalidOwnershipRecord);
                }
                let public = virtual_relative(path)?;
                if self
                    .dir
                    .symlink_metadata(&public)
                    .is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
                {
                    let private = directory_owner(*id);
                    match self.dir.symlink_metadata(&private) {
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {
                            self.publish_protocol_locked(
                                &private,
                                &directory_magic(*id),
                                true,
                                guard,
                            )?;
                        }
                        Err(e) => return Err(e.into()),
                        Ok(_) => {
                            if read_bounded(&self.dir, &private, 128)? != directory_magic(*id) {
                                return Err(FsError::OwnershipRecoveryBlocked);
                            }
                        }
                    }
                    match self.dir.symlink_metadata(stage(*id)) {
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {
                            self.dir.create_dir(stage(*id))?
                        }
                        Err(e) => return Err(e.into()),
                        Ok(meta) if linked(&meta) || !meta.is_dir() => {
                            return Err(FsError::OwnershipRecoveryBlocked)
                        }
                        Ok(_) => {}
                    }
                    for entry in self.dir.read_dir(stage(*id))? {
                        if entry?.file_name() != DIRECTORY_MARKER {
                            return Err(FsError::OwnershipRecoveryBlocked);
                        }
                    }
                    let marker = stage(*id).join(DIRECTORY_MARKER);
                    match self.dir.symlink_metadata(&marker) {
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {
                            self.dir.hard_link(&private, &self.dir, &marker)?
                        }
                        Err(e) => return Err(e.into()),
                        Ok(_) => {}
                    }
                    self.compare_directory_markers(&marker, &private)?;
                    #[cfg(test)]
                    if _fault == AccessFault::BeforeDirectoryPublish {
                        return Err(FsError::AtomicRenameUnavailable(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "injected directory publication denial",
                        )));
                    }
                    self.dir
                        .rename(stage(*id), &self.dir, &public)
                        .map_err(FsError::AtomicRenameUnavailable)?;
                }
                self.verify_directory_unlocked(&info)?;
            }
            Change::Rename {
                source,
                destination,
                id,
            }
            | Change::RenameReplace {
                source,
                destination,
                id,
                ..
            } => {
                let old = intent
                    .old
                    .directories
                    .get(source)
                    .ok_or(FsError::InvalidOwnershipRecord)?;
                let new = intent
                    .new
                    .directories
                    .get(destination)
                    .ok_or(FsError::InvalidOwnershipRecord)?;
                if old.id != *id || new.id != *id || within(destination, source) {
                    return Err(FsError::InvalidOwnershipRecord);
                }
                if let Change::RenameReplace { replaced, .. } = &intent.change {
                    self.prepare_directory_replace(intent, *replaced, guard, _fault)?;
                }
                if self
                    .dir
                    .symlink_metadata(virtual_relative(destination)?)
                    .is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
                {
                    self.verify_directory_unlocked(&old)?;
                    self.resolve_new(destination)?;
                    self.dir
                        .rename(
                            virtual_relative(source)?,
                            &self.dir,
                            virtual_relative(destination)?,
                        )
                        .map_err(FsError::from)?;
                } else if self.dir.symlink_metadata(virtual_relative(source)?).is_ok() {
                    return Err(FsError::OwnershipRecoveryBlocked);
                }
                self.verify_directory_unlocked(&new)?;
            }
            Change::Remove { path, id } => {
                let old = intent
                    .old
                    .directories
                    .get(path)
                    .ok_or(FsError::InvalidOwnershipRecord)?;
                if old.id != *id || intent.new.directories.contains_key(path) {
                    return Err(FsError::InvalidOwnershipRecord);
                }
                if self.dir.symlink_metadata(virtual_relative(path)?).is_ok() {
                    self.verify_directory_unlocked(&old)?;
                    for entry in self.dir.read_dir(virtual_relative(path)?)? {
                        if entry?.file_name() != DIRECTORY_MARKER {
                            return Err(FsError::DirectoryNotEmpty);
                        }
                    }
                    if self.dir.symlink_metadata(retired(*id)).is_ok() {
                        return Err(FsError::OwnershipRecoveryBlocked);
                    }
                    self.dir
                        .rename(virtual_relative(path)?, &self.dir, retired(*id))
                        .map_err(FsError::from)?;
                }
            }
        }
        self.verify_directory_replacement_contents(intent, true, guard)?;
        #[cfg(test)]
        if _fault == AccessFault::AfterNamespace {
            return Err(io::Error::other("injected after directory namespace").into());
        }
        if let Some((_, next)) = &intent.files {
            self.publish_protocol_locked(
                &store_path().join("catalog"),
                next.as_bytes(),
                false,
                guard,
            )?;
        }
        #[cfg(test)]
        if _fault == AccessFault::AfterFiles {
            return Err(io::Error::other("injected after joint file catalog").into());
        }
        let exists = self
            .dir
            .symlink_metadata(store_path().join("access.catalog"))
            .is_ok();
        let mut receipt = self.publish_protocol_locked(
            &store_path().join("access.catalog"),
            &intent.new.encode()?,
            !exists,
            guard,
        )?;
        #[cfg(test)]
        if _fault == AccessFault::AfterCatalog {
            return Err(io::Error::other("injected after access catalog").into());
        }
        if let Change::Remove { id, .. } | Change::RenameReplace { replaced: id, .. } =
            intent.change
        {
            match self.dir.symlink_metadata(retired(id)) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
                Ok(_) => {
                    let names: Vec<_> = self
                        .dir
                        .read_dir(retired(id))?
                        .map(|e| e.map(|e| e.file_name()))
                        .collect::<Result<_, _>>()?;
                    if names.iter().any(|n| n != DIRECTORY_MARKER) {
                        return Err(FsError::DirectoryNotEmpty);
                    }
                    if !names.is_empty() {
                        self.compare_directory_markers(
                            &retired(id).join(DIRECTORY_MARKER),
                            &directory_owner(id),
                        )?;
                        self.dir.remove_file(retired(id).join(DIRECTORY_MARKER))?;
                    }
                    self.dir.remove_dir(retired(id))?;
                }
            }
            if self.dir.symlink_metadata(directory_owner(id)).is_ok() {
                self.dir.remove_file(directory_owner(id))?;
            }
        }
        self.dir.remove_file(store_path().join("access.pending"))?;
        receipt.cleanup = CleanupStatus::Complete;
        Ok(receipt)
    }
    pub fn group_add_as(&self, user: &Account, gid: u32, name: &str) -> Result<(), FsError> {
        if user.uid != 0 {
            return Err(FsError::OwnershipDenied);
        }
        let guard = self.namespace_guard()?;
        let old = self.load_access_catalog()?;
        let mut new = old.clone();
        match crate::users::load(self) {
            Ok(accounts) if accounts.iter().any(|u| u.uid == gid) => {
                return Err(FsError::AlreadyExists)
            }
            Ok(_) => {}
            Err(crate::users::UserError::Fs(FsError::Io(e)))
                if e.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(FsError::InvalidOwnershipRecord),
        }
        if gid == 0 || new.groups.contains_key(&gid) || new.groups.values().any(|g| g.name == name)
        {
            return Err(FsError::AlreadyExists);
        }
        new.groups.insert(
            gid,
            Group {
                name: name.into(),
                members: BTreeSet::new(),
            },
        );
        new.bump()?;
        self.commit_policy_locked(old, new, None, &guard)?;
        Ok(())
    }
    pub fn group_member_as(
        &self,
        user: &Account,
        gid: u32,
        uid: u32,
        present: bool,
    ) -> Result<(), FsError> {
        if user.uid != 0 {
            return Err(FsError::OwnershipDenied);
        }
        let guard = self.namespace_guard()?;
        let old = self.load_access_catalog()?;
        let mut new = old.clone();
        let group = new.groups.get_mut(&gid).ok_or(FsError::NotFound)?;
        if present {
            group.members.insert(uid);
        } else {
            if new.primary.get(&uid) == Some(&gid) {
                return Err(FsError::OwnershipDenied);
            }
            group.members.remove(&uid);
        }
        new.bump()?;
        self.commit_policy_locked(old, new, None, &guard)?;
        Ok(())
    }
    pub fn primary_group_as(&self, user: &Account, uid: u32, gid: u32) -> Result<(), FsError> {
        if user.uid != 0 {
            return Err(FsError::OwnershipDenied);
        }
        let guard = self.namespace_guard()?;
        let old = self.load_access_catalog()?;
        let mut new = old.clone();
        if !new
            .groups
            .get(&gid)
            .is_some_and(|g| g.members.contains(&uid))
        {
            return Err(FsError::OwnershipDenied);
        }
        new.primary.insert(uid, gid);
        new.bump()?;
        self.commit_policy_locked(old, new, None, &guard)?;
        Ok(())
    }
    pub fn groups_as(&self, user: &Account) -> Result<BTreeMap<u32, Group>, FsError> {
        let _guard = self.namespace_guard()?;
        let catalog = self.load_access_catalog()?;
        Ok(catalog
            .groups
            .into_iter()
            .filter(|(_, g)| user.uid == 0 || g.members.contains(&user.uid))
            .collect())
    }
    pub fn acl_as(&self, user: &Account, path: &str) -> Result<Acl, FsError> {
        if let super::vfs::Route::Mounted { volume, path, .. } =
            self.mounted(user, path, Access::Read)?
        {
            return Ok(volume.acl(&self.credentials_as(user)?, &path)?);
        }
        super::ownership::user_path(user, path, Access::Read)?;
        let guard = self.namespace_guard()?;
        let resolved_path = self.resolve_host_locked(user, path, Access::Read, true, &guard)?;
        let path = resolved_path.as_str();
        let catalog = self.load_access_catalog()?;
        self.search_locked(user, path, &catalog)?;
        if let Some(info) = self.directory_at_locked(path, &catalog)? {
            self.directory_access_locked(user, &info, 1, &catalog)?;
            return Ok(info.acl.clone());
        }
        let store = self
            .existing_owner_store()?
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        let files = self.load_owner_catalog(&store)?;
        let info = files
            .entries
            .get(&key(path)?)
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        if user.uid != 0 && user.uid != info.owner.uid {
            self.authorize_file(user, &info, true, false)?;
        }
        self.verify_public_binding(&info, &guard)?;
        Ok(catalog
            .file_acl
            .get(&info.id.value())
            .cloned()
            .unwrap_or_default())
    }
    pub(super) fn creation_access_locked(
        &self,
        path: &str,
        input: CreationMode,
        umask: super::Umask,
        catalog: &AccessCatalog,
    ) -> Result<CreationAccess, FsError> {
        let requested = match input {
            CreationMode::Exact(mode) | CreationMode::Requested(mode) => mode,
        };
        if let Some(parent) = self.directory_at_locked(parent(path), catalog)? {
            if let Some(default) = catalog.default_acl.get(&parent.id) {
                let (mode, acl) = default.inherit(requested)?;
                return Ok(CreationAccess::Inherited {
                    mode,
                    acl,
                    default: default.clone(),
                });
            }
        }
        Ok(match input {
            CreationMode::Exact(mode) => CreationAccess::Explicit(mode),
            CreationMode::Requested(mode) => CreationAccess::Umask(umask.apply(mode)),
        })
    }
    fn require_directory_record_locked(
        &self,
        path: &str,
        catalog: &AccessCatalog,
    ) -> Result<DirectoryInfo, FsError> {
        // Physical directory type and protocol ownership are different refusals.
        // Callers must still verify binding before reading/changing its policy.
        self.validate_directory_unchecked(path)?;
        self.directory_at_locked(path, catalog)?
            .ok_or(FsError::LegacyOwnershipUnknown)
    }
    pub fn default_acl_as(
        &self,
        user: &Account,
        path: &str,
    ) -> Result<Option<DefaultAcl>, FsError> {
        if let super::vfs::Route::Mounted { volume, path, .. } =
            self.mounted(user, path, Access::Read)?
        {
            return Ok(volume.default_acl(&self.credentials_as(user)?, &path)?);
        }
        super::ownership::user_path(user, path, Access::Read)?;
        let _guard = self.namespace_guard()?;
        let resolved_path = self.resolve_host_locked(user, path, Access::Read, true, &_guard)?;
        let path = resolved_path.as_str();
        let catalog = self.load_access_catalog()?;
        self.search_locked(user, path, &catalog)?;
        let info = self.require_directory_record_locked(path, &catalog)?;
        self.directory_access_locked(user, &info, 1, &catalog)?;
        Ok(catalog.default_acl.get(&info.id).cloned())
    }
    /// Replace/remove the directory template without changing current access grants.
    pub fn set_default_acl_as(
        &self,
        user: &Account,
        path: &str,
        acl: Option<DefaultAcl>,
    ) -> Result<CommitReceipt, FsError> {
        if let super::vfs::Route::Mounted { volume, path, .. } =
            self.mounted(user, path, Access::Write)?
        {
            volume.set_default_acl(&self.credentials_as(user)?, &path, acl)?;
            return Ok(super::vfs_bridge::volatile_receipt(0));
        }
        super::ownership::user_path(user, path, Access::Write)?;
        if let Some(acl) = &acl {
            acl.validate()?;
        }
        let guard = self.namespace_guard()?;
        let resolved_path = self.resolve_host_locked(user, path, Access::Write, true, &guard)?;
        let path = resolved_path.as_str();
        let old = self.load_access_catalog()?;
        self.search_locked(user, path, &old)?;
        self.ensure_write_class(path)?;
        let info = self.require_directory_record_locked(path, &old)?;
        if user.uid != 0 && user.uid != info.owner.uid {
            return Err(FsError::OwnershipDenied);
        }
        self.verify_directory_unlocked(&info)?;
        let mut new = old.clone();
        if let Some(acl) = acl {
            new.default_acl.insert(info.id, acl);
        } else {
            new.default_acl.remove(&info.id);
        }
        new.bump()?;
        self.commit_policy_locked(old, new, None, &guard)
    }
    /// None retains that field. ACL replacement recalculates mask unless explicit.
    pub fn set_access_as(
        &self,
        user: &Account,
        path: &str,
        mode: Option<VirtualMode>,
        gid: Option<u32>,
        acl: Option<Acl>,
    ) -> Result<CommitReceipt, FsError> {
        if let super::vfs::Route::Mounted { volume, path, .. } =
            self.mounted(user, path, Access::Write)?
        {
            volume.set_access(&self.credentials_as(user)?, &path, mode, gid, acl)?;
            return Ok(super::vfs_bridge::volatile_receipt(0));
        }
        super::ownership::user_path(user, path, Access::Write)?;
        let guard = self.namespace_guard()?;
        let resolved_path = self.resolve_host_locked(user, path, Access::Write, true, &guard)?;
        let path = resolved_path.as_str();
        let old = self.load_access_catalog()?;
        self.search_locked(user, path, &old)?;
        self.ensure_write_class(path)?;
        let mut new = old.clone();
        new.bump()?;
        let canonical = self
            .directory_at_locked(path, &old)?
            .map_or_else(|| key(path), |d| Ok(d.path.clone()))?;
        let mut files = None;
        if let Some(info) = new.directories.get(&canonical) {
            self.verify_directory_unlocked(&info)?;
            let info = new
                .directories
                .inode_mut(&canonical)
                .ok_or(FsError::InvalidOwnershipRecord)?;
            update_access(
                &old.actor_credentials(user),
                (&mut info.owner, &mut info.mode, &mut info.acl),
                (mode, gid, acl),
                ModeTarget::Directory,
            )?;
        } else {
            let store = self
                .existing_owner_store()?
                .ok_or(FsError::LegacyOwnershipUnknown)?;
            let previous = self.load_owner_catalog(&store)?;
            let mut next = previous.clone();
            let info = previous
                .entries
                .get(&canonical)
                .ok_or(FsError::LegacyOwnershipUnknown)?;
            self.verify_public_binding(&info, &guard)?;
            let changed = next
                .entries
                .inode_mut(&canonical)
                .ok_or(FsError::LegacyOwnershipUnknown)?;
            let mut current_acl = old
                .file_acl
                .get(&info.id.value())
                .cloned()
                .unwrap_or_default();
            update_access(
                &old.actor_credentials(user),
                (&mut changed.owner, &mut changed.mode, &mut current_acl),
                (mode, gid, acl),
                ModeTarget::RegularFile,
            )?;
            new.file_acl.insert(info.id.value(), current_acl);
            next.revision = next
                .revision
                .checked_add(1)
                .ok_or(FsError::OwnershipIdentityExhausted)?;
            files = Some((previous, next));
        }
        self.commit_policy_locked(old, new, files, &guard)
    }
}
#[derive(Clone, Copy)]
pub(super) enum ModeTarget {
    Directory,
    RegularFile,
}
pub(super) fn update_access(
    actor: &Credentials,
    current: (&mut FileOwner, &mut VirtualMode, &mut Acl),
    changes: (Option<VirtualMode>, Option<u32>, Option<Acl>),
    target: ModeTarget,
) -> Result<(), FsError> {
    let (owner, current_mode, current_acl) = current;
    let (mode, gid, acl) = changes;
    if actor.uid != 0 && actor.uid != owner.uid {
        return Err(FsError::OwnershipDenied);
    }
    if let Some(gid) = gid {
        if actor.uid != 0 && !actor.groups.contains(&gid) {
            return Err(FsError::OwnershipDenied);
        }
        owner.gid = gid;
        if matches!(target, ModeTarget::RegularFile) {
            *current_mode = current_mode.without_privilege();
        }
    }
    if let Some(mut acl) = acl {
        acl.validate()?;
        if acl != Acl::default() {
            if let Some(mask) = acl.mask {
                *current_mode =
                    VirtualMode::new((current_mode.bits() & !0o070) | (u16::from(mask) << 3))?;
            } else {
                *current_mode = acl.recalculate_mask(*current_mode);
            }
        }
        *current_acl = acl;
    }
    if let Some(mut mode) = mode {
        match target {
            ModeTarget::RegularFile => mode.require_regular_file()?,
            ModeTarget::Directory => mode.require_directory()?,
        }
        if actor.uid != 0 && !actor.groups.contains(&owner.gid) {
            mode = VirtualMode::new(mode.bits() & !0o2000)?;
        }
        *current_mode = mode;
        if current_acl.mask.is_some() {
            current_acl.mask = Some(((mode.bits() >> 3) & 7) as u8);
        }
    }
    Ok(())
}
