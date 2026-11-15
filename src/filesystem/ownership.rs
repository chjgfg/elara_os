//! Persistent ownership for explicitly managed regular files. Data generations
//! are pinned inside a marked capability store; a versioned intent bridges the
//! public name and the text catalog. Legacy files are never silently adopted.
use super::access_model::{AccessCatalog, CreationAccess, CreationMode, MAX_ACCESS_BYTES};
use super::backend::linked;
use super::ownership_model::{
    key, Catalog, FileOwner, ObjectId, OwnedFileInfo, OwnedStorageClass, OwnershipState,
    VirtualMode, MAX_CATALOG_BYTES,
};
use super::transaction::{read_magic, store_path as tx_path, NamespaceGuard, TX_OWNER};
use super::{CleanupStatus, CommitReceipt, DataRoot, DirectoryDurability, FsError, ObjectBinding};
use crate::config::{write_class, WriteClass, ROOTFS_DEFAULTS};
use crate::permissions::{self, Access};
use crate::users::Account;
use crate::virtual_path::VirtualPath;
use cap_std::fs::{Dir, OpenOptions};
use std::format;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub(super) const OWNER_STORE: &str = crate::config::PRIVATE_OWNER_DIRECTORY;
const STORE_MAGIC: &[u8] = b"ELARA_OWNERSHIP_STORE_V1\n";
pub(super) const OWNED_TX_MAGIC: &[u8] = b"ELARA_OWNED_OBJECT_TX_V1\n";
const INTENT_MAGIC: &str = "ELARA_OWNERSHIP_INTENT_V1";
const INTENT_MAGIC_REPLACE: &str = "ELARA_OWNERSHIP_INTENT_V3";
#[path = "ownership_replace.rs"]
mod replacement;
use replacement::Mutation;
const INTENT_MAGIC_ACCESS: &str = "ELARA_OWNERSHIP_INTENT_V2";
const OBJECT_MAGIC: &str = "ELARA_OWNED_STORAGE_V1";
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum OwnershipFault {
    None,
    #[cfg(test)]
    AfterStorage,
    #[cfg(test)]
    AfterPublicName,
    #[cfg(test)]
    AfterFirstPublicName,
    #[cfg(test)]
    AfterCreationAcl,
    #[cfg(test)]
    AfterCatalog,
    #[cfg(test)]
    AfterOldPayloadRemoval,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitObject {
    Host(ObjectId),
    Virtual { filesystem: u64, inode: u64 },
}
#[derive(Clone, Debug)]
pub struct OwnedCommit {
    pub revision: u64,
    pub object: CommitObject,
    pub directory_durability: DirectoryDurability,
    pub cleanup: CleanupStatus,
}
#[derive(Clone)]
struct Intent {
    old: Catalog,
    new: Catalog,
    stage: String,
    access: Option<(AccessCatalog, AccessCatalog)>,
}
impl Intent {
    fn encode(&self) -> Result<Vec<u8>, FsError> {
        let operation = replacement::classify(self)?;
        let magic = if std::matches!(operation, Mutation::Replace { .. }) {
            INTENT_MAGIC_REPLACE
        } else {
            INTENT_MAGIC
        };
        let old = self.old.encode()?;
        let new = self.new.encode()?;
        let newline = char::from(10);
        let bytes = if let Some((before, after)) = &self.access {
            let before = before.encode()?;
            let after = after.encode()?;
            let mut bytes = format!(
                "{INTENT_MAGIC_ACCESS}{newline}{} {} {} {} {}{newline}",
                old.len(),
                new.len(),
                self.stage,
                before.len(),
                after.len()
            )
            .into_bytes();
            bytes.extend_from_slice(&old);
            bytes.extend_from_slice(&new);
            bytes.extend_from_slice(&before);
            bytes.extend_from_slice(&after);
            bytes
        } else {
            let mut bytes = format!(
                "{magic}{newline}{} {} {}{newline}",
                old.len(),
                new.len(),
                self.stage
            )
            .into_bytes();
            bytes.extend_from_slice(&old);
            bytes.extend_from_slice(&new);
            bytes
        };
        if bytes.len() > ROOTFS_DEFAULTS.max_transaction_bytes {
            return Err(FsError::OwnershipCapacity);
        }
        // Framing always contains complete canonical catalogs, never a partial policy.
        Ok(bytes)
    }
    fn decode(bytes: &[u8]) -> Result<Self, FsError> {
        if bytes.len() > ROOTFS_DEFAULTS.max_transaction_bytes {
            return Err(FsError::OwnershipCapacity);
        }
        let newline = char::from(10);
        let legacy = format!("{INTENT_MAGIC}{newline}");
        let joint = format!("{INTENT_MAGIC_ACCESS}{newline}");
        let (body, joint) = if let Some(body) = bytes
            .strip_prefix(legacy.as_bytes())
            .or_else(|| bytes.strip_prefix(format!("{INTENT_MAGIC_REPLACE}{newline}").as_bytes()))
        {
            (body, false)
        } else {
            (
                bytes
                    .strip_prefix(joint.as_bytes())
                    .ok_or(FsError::InvalidOwnershipRecord)?,
                true,
            )
        };
        let end = body
            .iter()
            .position(|b| *b == 10)
            .ok_or(FsError::InvalidOwnershipRecord)?;
        let line =
            std::str::from_utf8(&body[..end]).map_err(|_| FsError::InvalidOwnershipRecord)?;
        let fields: Vec<_> = line.split(' ').collect();
        if fields.len() != if joint { 5 } else { 3 } {
            return Err(FsError::InvalidOwnershipRecord);
        }
        let length = |index: usize| {
            fields[index]
                .parse::<usize>()
                .map_err(|_| FsError::InvalidOwnershipRecord)
        };
        let old_len = length(0)?;
        let new_len = length(1)?;
        let (before_len, after_len) = if joint {
            (length(3)?, length(4)?)
        } else {
            (0, 0)
        };
        if old_len > MAX_CATALOG_BYTES
            || new_len > MAX_CATALOG_BYTES
            || before_len > MAX_ACCESS_BYTES
            || after_len > MAX_ACCESS_BYTES
        {
            return Err(FsError::InvalidOwnershipRecord);
        }
        let data = &body[end + 1..];
        let total = [old_len, new_len, before_len, after_len]
            .into_iter()
            .try_fold(0usize, |n, v| n.checked_add(v))
            .ok_or(FsError::InvalidOwnershipRecord)?;
        if total != data.len() {
            return Err(FsError::InvalidOwnershipRecord);
        }
        let stage = fields[2].to_owned();
        if stage != "-"
            && (!stage.starts_with("txn-")
                || !stage[4..].bytes().all(|b| b.is_ascii_digit() || b == b'-'))
        {
            return Err(FsError::InvalidOwnershipRecord);
        }
        let split = old_len + new_len;
        let access = if joint {
            Some((
                AccessCatalog::decode(&data[split..split + before_len])?,
                AccessCatalog::decode(&data[split + before_len..])?,
            ))
        } else {
            None
        };
        let intent = Self {
            old: Catalog::decode(&data[..old_len])?,
            new: Catalog::decode(&data[old_len..split])?,
            stage,
            access,
        };
        if intent.new.revision
            != intent
                .old
                .revision
                .checked_add(1)
                .ok_or(FsError::OwnershipIdentityExhausted)?
            || intent.encode()?.as_slice() != bytes
        {
            return Err(FsError::InvalidOwnershipRecord);
        }
        Ok(intent)
    }
}
pub(super) fn store_path() -> PathBuf {
    Path::new("var").join(OWNER_STORE)
}
pub(super) fn payload(info: &OwnedFileInfo) -> PathBuf {
    store_path().join(format!(
        "payload-{}-{}",
        info.id.value(),
        info.storage_generation
    ))
}
fn marker(info: &OwnedFileInfo) -> PathBuf {
    store_path().join(format!(
        "owner-{}-{}",
        info.id.value(),
        info.storage_generation
    ))
}
fn object_magic(info: &OwnedFileInfo) -> Vec<u8> {
    format!(
        "{OBJECT_MAGIC}\n{} {}\n",
        info.id.value(),
        info.storage_generation
    )
    .into_bytes()
}
pub(super) fn read_bounded(dir: &Dir, path: &Path, limit: usize) -> Result<Vec<u8>, FsError> {
    let meta = dir.symlink_metadata(path)?;
    if linked(&meta) || !meta.is_file() {
        return Err(FsError::InvalidOwnershipRecord);
    }
    let mut bytes = Vec::new();
    dir.open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(FsError::OwnershipCapacity);
    }
    Ok(bytes)
}
pub(super) fn user_path(
    user: &Account,
    text: &str,
    access: Access,
) -> Result<VirtualPath, FsError> {
    let path = VirtualPath::strict(text).map_err(|_| FsError::InvalidPath)?;
    permissions::check(user, &path, access).map_err(|e| FsError::AccessDenied(e.to_string()))?;
    Ok(path)
}
pub(super) fn storage_class(path: &str) -> Result<OwnedStorageClass, FsError> {
    match write_class(path) {
        WriteClass::UserData => Ok(OwnedStorageClass::UserData),
        WriteClass::Temporary => Ok(OwnedStorageClass::Temporary),
        WriteClass::RuntimeState => Ok(OwnedStorageClass::RuntimeState),
        WriteClass::Application => Ok(OwnedStorageClass::Application),
        WriteClass::Cache => Ok(OwnedStorageClass::Cache),
        WriteClass::SystemReadOnly => Err(FsError::ReadOnlySystemPath),
        _ => Err(FsError::OwnershipContextRequired),
    }
}
fn mutation(intent: &Intent) -> Result<(Option<OwnedFileInfo>, Option<OwnedFileInfo>), FsError> {
    // Compare all names instead of silently folding aliases by object id.
    let group = |catalog: &Catalog| {
        let mut objects = std::collections::BTreeMap::<ObjectId, Vec<OwnedFileInfo>>::new();
        for info in catalog.entries.values() {
            objects.entry(info.id).or_default().push(info);
        }
        objects
    };
    let old = group(&intent.old);
    let new = group(&intent.new);
    let ids: std::collections::BTreeSet<_> = old.keys().chain(new.keys()).copied().collect();
    let changed: Vec<_> = ids
        .into_iter()
        .filter(|id| old.get(id) != new.get(id))
        .collect();
    if changed.len() != 1
        || intent.new.revision
            != intent
                .old
                .revision
                .checked_add(1)
                .ok_or(FsError::OwnershipIdentityExhausted)?
    {
        return Err(FsError::InvalidOwnershipRecord);
    }
    let id = changed[0];
    let before = old.get(&id).and_then(|names| names.first()).cloned();
    let after = new.get(&id).and_then(|names| names.first()).cloned();
    let names = |objects: &std::collections::BTreeMap<ObjectId, Vec<OwnedFileInfo>>| {
        objects
            .get(&id)
            .into_iter()
            .flatten()
            .map(|info| info.path.clone())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let old_names = names(&old);
    let new_names = names(&new);
    match (&before, &after) {
        (None, Some(info)) => {
            if new_names.len() != 1
                || info.id.value() != intent.old.next_id
                || info.storage_generation != 1
                || intent.new.next_id
                    != intent
                        .old
                        .next_id
                        .checked_add(1)
                        .ok_or(FsError::OwnershipIdentityExhausted)?
            {
                return Err(FsError::InvalidOwnershipRecord);
            }
        }
        (Some(old), Some(new)) => {
            if old.kind != new.kind
                || old.owner != new.owner
                || old.storage_class != new.storage_class
                || old.delete_condition != new.delete_condition
                || intent.new.next_id != intent.old.next_id
            {
                return Err(FsError::InvalidOwnershipRecord);
            }
            let added = new_names.difference(&old_names).count();
            let removed = old_names.difference(&new_names).count();
            let generation = old.storage_generation != new.storage_generation;
            let mode = old.mode != new.mode;
            if added != 0 || removed != 0 {
                if added > 1 || removed > 1 || generation || mode {
                    return Err(FsError::InvalidOwnershipRecord);
                }
            } else if usize::from(generation) + usize::from(mode) != 1
                || generation
                    && new.storage_generation
                        != old
                            .storage_generation
                            .checked_add(1)
                            .ok_or(FsError::OwnershipIdentityExhausted)?
            {
                return Err(FsError::InvalidOwnershipRecord);
            }
        }
        (Some(_), None) if old_names.len() == 1 && intent.new.next_id == intent.old.next_id => {}
        _ => return Err(FsError::InvalidOwnershipRecord),
    }
    let storage_change = before
        .as_ref()
        .zip(after.as_ref())
        .is_some_and(|(a, b)| a.storage_generation != b.storage_generation);
    if before.is_some() && !storage_change && intent.stage != "-" {
        return Err(FsError::InvalidOwnershipRecord);
    }
    if let Some((old_access, new_access)) = &intent.access {
        let (None, Some(info)) = (&before, &after) else {
            return Err(FsError::InvalidOwnershipRecord);
        };
        old_access.validate()?;
        new_access.validate()?;
        let id = info.id.value();
        if old_access.file_acl.contains_key(&id) {
            return Err(FsError::InvalidOwnershipRecord);
        }
        let acl = new_access
            .file_acl
            .get(&id)
            .ok_or(FsError::InvalidOwnershipRecord)?;
        if acl.group.is_none()
            || ((!acl.users.is_empty() || !acl.groups.is_empty()) && acl.mask.is_none())
            || acl.mask.or(acl.group) != Some(((info.mode.bits() >> 3) & 7) as u8)
        {
            return Err(FsError::InvalidOwnershipRecord);
        }
        let mut expected = old_access.clone();
        expected.file_acl.insert(id, acl.clone());
        expected.bump()?;
        if expected != *new_access {
            return Err(FsError::InvalidOwnershipRecord);
        }
    }
    Ok((before, after))
}
impl DataRoot {
    pub(super) fn existing_owner_store(&self) -> Result<Option<Dir>, FsError> {
        match self.dir.symlink_metadata(store_path()) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
            Ok(meta) if linked(&meta) || !meta.is_dir() => {
                return Err(FsError::UnownedOwnershipStore)
            }
            Ok(_) => {}
        }
        self.inspect_relative(&store_path())?;
        let store = self.dir.open_dir(store_path())?;
        let marker = store
            .symlink_metadata("store.owner")
            .map_err(|_| FsError::UnownedOwnershipStore)?;
        if linked(&marker) || !marker.is_file() || !read_magic(&store, "store.owner", STORE_MAGIC)?
        {
            return Err(FsError::UnownedOwnershipStore);
        }
        Ok(Some(store))
    }
    pub(super) fn ensure_owner_store(&self, guard: &NamespaceGuard<'_>) -> Result<Dir, FsError> {
        if let Some(store) = self.existing_owner_store()? {
            return Ok(store);
        }
        self.dir.create_dir(store_path())?;
        let store = self.dir.open_dir(store_path())?;
        let mut marker = store.open_with(
            "store.owner",
            OpenOptions::new().write(true).create_new(true),
        )?;
        marker.write_all(STORE_MAGIC)?;
        marker.sync_all()?;
        drop(marker);
        self.publish_protocol_locked(
            &store_path().join("catalog"),
            &Catalog::empty().encode()?,
            true,
            guard,
        )?;
        Ok(store)
    }
    pub(super) fn load_owner_catalog(&self, store: &Dir) -> Result<Catalog, FsError> {
        let catalog = Catalog::decode(&read_bounded(
            store,
            Path::new("catalog"),
            MAX_CATALOG_BYTES,
        )?)?;
        let access = self.load_access_catalog()?;
        for info in catalog.entries.values() {
            if let super::DeleteCondition::TaskExit(id) = info.delete_condition {
                if access.tasks.get(&id) != Some(&info.owner.uid) {
                    return Err(FsError::InvalidOwnershipRecord);
                }
            }
        }
        Ok(catalog)
    }
    pub(super) fn verify_storage(&self, info: &OwnedFileInfo) -> Result<(), FsError> {
        self.inspect_relative(&marker(info))?;
        self.inspect_relative(&payload(info))?;
        if read_bounded(&self.dir, &marker(info), 128)? != object_magic(info)
            || !self.dir.metadata(payload(info))?.is_file()
        {
            return Err(FsError::InvalidOwnershipRecord);
        }
        Ok(())
    }
    pub(super) fn verify_public_binding(
        &self,
        info: &OwnedFileInfo,
        guard: &NamespaceGuard<'_>,
    ) -> Result<(), FsError> {
        self.verify_storage(info)?;
        match self.compare_relative_objects(
            &VirtualPath::strict(&info.path)
                .map_err(|_| FsError::InvalidPath)?
                .as_relative(),
            &payload(info),
            guard,
        )? {
            ObjectBinding::SameObject => Ok(()),
            _ => Err(FsError::OwnershipBindingUnconfirmed),
        }
    }
    fn prepare_owned_payload(
        &self,
        info: &OwnedFileInfo,
        bytes: &[u8],
        guard: &NamespaceGuard<'_>,
    ) -> Result<String, FsError> {
        if bytes.len() > ROOTFS_DEFAULTS.max_transaction_bytes {
            return Err(FsError::IoRequestTooLarge(
                ROOTFS_DEFAULTS.max_transaction_bytes,
            ));
        }
        let (staging, name) = self.create_protocol_stage(guard)?;
        let stage = staging.open_dir(&name)?;
        let mut owner =
            stage.open_with(TX_OWNER, OpenOptions::new().write(true).create_new(true))?;
        owner.write_all(OWNED_TX_MAGIC)?;
        owner.sync_all()?;
        let mut description = stage.open_with(
            "object.owner",
            OpenOptions::new().write(true).create_new(true),
        )?;
        description.write_all(&object_magic(info))?;
        description.sync_all()?;
        let mut data =
            stage.open_with("payload", OpenOptions::new().write(true).create_new(true))?;
        data.write_all(bytes)?;
        data.sync_all()?;
        Ok(name)
    }
    fn install_owned_payload(
        &self,
        info: &OwnedFileInfo,
        stage_name: &str,
        guard: &NamespaceGuard<'_>,
    ) -> Result<(), FsError> {
        let private_payload = payload(info);
        let private_marker = marker(info);
        let exists = self.dir.symlink_metadata(&private_payload).is_ok();
        if exists {
            return self.verify_storage(info);
        }
        if stage_name == "-" {
            return Err(FsError::OwnershipRecoveryBlocked);
        }
        let stage_path = tx_path().join(stage_name);
        self.inspect_relative(&stage_path)?;
        let stage = self.dir.open_dir(stage_path)?;
        if !read_magic(&stage, TX_OWNER, OWNED_TX_MAGIC)?
            || !read_magic(&stage, "object.owner", &object_magic(info))?
        {
            return Err(FsError::InvalidOwnershipRecord);
        }
        match self.dir.symlink_metadata(&private_marker) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                stage.hard_link("object.owner", &self.dir, &private_marker)?
            }
            Err(e) => return Err(e.into()),
            Ok(_) if read_bounded(&self.dir, &private_marker, 128)? == object_magic(info) => {}
            Ok(_) => return Err(FsError::OwnershipRecoveryBlocked),
        }
        stage.hard_link("payload", &self.dir, &private_payload)?;
        guard.require_root(self)?;
        self.verify_storage(info)
    }
    fn publish_owner_names(
        &self,
        before: &std::collections::BTreeMap<String, OwnedFileInfo>,
        after: &std::collections::BTreeMap<String, OwnedFileInfo>,
        guard: &NamespaceGuard<'_>,
    ) -> Result<(), FsError> {
        let removed: Vec<_> = before
            .values()
            .filter(|info| !after.contains_key(&info.path))
            .collect();
        let added: Vec<_> = after
            .values()
            .filter(|info| !before.contains_key(&info.path))
            .collect();
        let relative = |path: &str| {
            VirtualPath::strict(path)
                .map(|p| p.as_relative())
                .map_err(|_| FsError::InvalidPath)
        };
        if let ([source], [destination]) = (removed.as_slice(), added.as_slice()) {
            let from = relative(&source.path)?;
            let to = relative(&destination.path)?;
            match self.dir.symlink_metadata(&to) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    self.verify_public_binding(source, guard)?;
                    self.dir
                        .rename(from, &self.dir, to)
                        .map_err(FsError::AtomicRenameUnavailable)?;
                }
                Err(e) => return Err(e.into()),
                Ok(_) => {
                    self.verify_public_binding(destination, guard)?;
                    match self.dir.symlink_metadata(from) {
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e.into()),
                        Ok(_) => return Err(FsError::OwnershipRecoveryBlocked),
                    }
                }
            }
        } else {
            for info in added {
                let path = relative(&info.path)?;
                match self.dir.symlink_metadata(&path) {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        self.publish_protocol_link_exclusive_locked(&payload(info), &path, guard)?;
                    }
                    Err(e) => return Err(e.into()),
                    Ok(_) => self.verify_public_binding(info, guard)?,
                }
            }
            for info in removed {
                let path = relative(&info.path)?;
                match self.dir.symlink_metadata(&path) {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                    Ok(_) => {
                        self.verify_public_binding(info, guard)?;
                        self.dir.remove_file(path)?;
                    }
                }
            }
        }
        Ok(())
    }
    fn finish_owner_intent(
        &self,
        store: &Dir,
        intent: &Intent,
        guard: &NamespaceGuard<'_>,
        _fault: OwnershipFault,
    ) -> Result<CommitReceipt, FsError> {
        let current = self.load_owner_catalog(store)?;
        if current.encode()? != intent.old.encode()? && current.encode()? != intent.new.encode()? {
            return Err(FsError::OwnershipRecoveryBlocked);
        }
        let (old, new) = match replacement::classify(intent)? {
            Mutation::Single(old, new) => (old, new),
            Mutation::Replace { source, victim } => {
                return self.finish_replace_intent(store, intent, &source, &victim, guard, _fault);
            }
        };
        let id = new
            .as_ref()
            .or(old.as_ref())
            .ok_or(FsError::InvalidOwnershipRecord)?
            .id;
        let before: std::collections::BTreeMap<_, _> = intent
            .old
            .entries
            .values()
            .filter(|info| info.id == id)
            .map(|info| (info.path.clone(), info))
            .collect();
        let after: std::collections::BTreeMap<_, _> = intent
            .new
            .entries
            .values()
            .filter(|info| info.id == id)
            .map(|info| (info.path.clone(), info))
            .collect();
        if let Some(info) = &new {
            if old
                .as_ref()
                .is_none_or(|v| v.storage_generation != info.storage_generation)
            {
                self.install_owned_payload(info, &intent.stage, guard)?;
            } else {
                self.verify_storage(info)?;
            }
        }
        #[cfg(test)]
        if _fault == OwnershipFault::AfterStorage {
            return Err(io::Error::other("injected after storage installation").into());
        }
        if let Some((before, after)) = &intent.access {
            let policy = self.load_access_catalog()?;
            if policy == *before {
                self.commit_policy_locked(before.clone(), after.clone(), None, guard)?;
            } else if policy != *after {
                return Err(FsError::OwnershipRecoveryBlocked);
            }
        }
        #[cfg(test)]
        if _fault == OwnershipFault::AfterCreationAcl {
            return Err(io::Error::other("injected after creation ACL installation").into());
        }
        if current.revision == intent.old.revision {
            self.publish_owner_names(&before, &after, guard)?;
            if old
                .as_ref()
                .zip(new.as_ref())
                .is_some_and(|(a, b)| a.storage_generation != b.storage_generation)
            {
                // Recover only a verified old/new mixture, never unknown names.
                let mut pending = Vec::new();
                for (name, info) in &after {
                    let path = VirtualPath::strict(name)
                        .map_err(|_| FsError::InvalidPath)?
                        .as_relative();
                    if self.compare_relative_objects(&path, &payload(info), guard)?
                        != ObjectBinding::SameObject
                    {
                        self.verify_public_binding(
                            before.get(name).ok_or(FsError::InvalidOwnershipRecord)?,
                            guard,
                        )?;
                        pending.push((path, info));
                    }
                }
                for (path, info) in pending {
                    self.publish_protocol_link_locked(&payload(info), &path, guard)?;
                    #[cfg(test)]
                    if _fault == OwnershipFault::AfterFirstPublicName {
                        return Err(io::Error::other("injected after one alias publication").into());
                    }
                }
            }
        }
        #[cfg(test)]
        if _fault == OwnershipFault::AfterPublicName {
            return Err(io::Error::other("injected after public name publication").into());
        }
        let receipt = self.publish_protocol_locked(
            &store_path().join("catalog"),
            &intent.new.encode()?,
            false,
            guard,
        )?;
        #[cfg(test)]
        if _fault == OwnershipFault::AfterCatalog {
            return Err(io::Error::other("injected after catalog commit").into());
        }
        // Do not lose evidence until the public binding and catalog agree.
        for info in after.values() {
            self.verify_public_binding(info, guard)?;
        }
        if let Some(old) = &old {
            if new
                .as_ref()
                .is_none_or(|v| v.storage_generation != old.storage_generation)
            {
                self.retire_owned_storage(old, _fault)?;
            }
        }
        if intent.stage != "-" {
            let staging = self.dir.open_dir(tx_path())?;
            if staging.symlink_metadata(&intent.stage).is_ok()
                && !super::transaction::discard_owned(
                    &staging,
                    std::ffi::OsStr::new(&intent.stage),
                )?
            {
                return Err(FsError::OwnershipRecoveryBlocked);
            }
        }
        if new.is_none() {
            if let Some(info) = &old {
                let previous = self.load_access_catalog()?;
                if previous.file_acl.contains_key(&info.id.value()) {
                    let mut next = previous.clone();
                    next.file_acl.remove(&info.id.value());
                    next.bump()?;
                    self.commit_policy_locked(previous, next, None, guard)?;
                }
            }
        }
        store.remove_file("pending")?;
        Ok(receipt)
    }
    pub(super) fn retire_owned_storage(
        &self,
        old: &OwnedFileInfo,
        _fault: OwnershipFault,
    ) -> Result<(), FsError> {
        let private_payload = payload(old);
        let private_marker = marker(old);
        let payload_exists = match self.dir.symlink_metadata(&private_payload) {
            Ok(meta) if !linked(&meta) && meta.is_file() => true,
            Ok(_) => return Err(FsError::OwnershipRecoveryBlocked),
            Err(e) if e.kind() == io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.into()),
        };
        match self.dir.symlink_metadata(&private_marker) {
            Ok(_) => {
                if read_bounded(&self.dir, &private_marker, 128)? != object_magic(old) {
                    return Err(FsError::OwnershipRecoveryBlocked);
                }
                if payload_exists {
                    self.dir.remove_file(&private_payload)?;
                }
                #[cfg(test)]
                if _fault == OwnershipFault::AfterOldPayloadRemoval {
                    return Err(io::Error::other("injected during retirement cleanup").into());
                }
                self.dir.remove_file(&private_marker)?;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound && !payload_exists => {}
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }
    pub(super) fn recover_owner_intent_locked(
        &self,
        guard: &NamespaceGuard<'_>,
    ) -> Result<(), FsError> {
        let pending = store_path().join("pending");
        match self.dir.symlink_metadata(&pending) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
            Ok(_) => {}
        }
        let store = self
            .existing_owner_store()?
            .ok_or(FsError::UnownedOwnershipStore)?;
        let intent = Intent::decode(&read_bounded(
            &store,
            Path::new("pending"),
            ROOTFS_DEFAULTS.max_transaction_bytes,
        )?)?;
        self.finish_owner_intent(&store, &intent, guard, OwnershipFault::None)?;
        Ok(())
    }
    fn commit_owned_change(
        &self,
        store: &Dir,
        intent: Intent,
        guard: &NamespaceGuard<'_>,
        fault: OwnershipFault,
    ) -> Result<OwnedCommit, FsError> {
        let object = replacement::classify(&intent)?.object()?;
        self.publish_protocol_locked(
            &store_path().join("pending"),
            &intent.encode()?,
            true,
            guard,
        )?;
        match self.finish_owner_intent(store, &intent, guard, fault) {
            Ok(receipt) => Ok(OwnedCommit {
                revision: intent.new.revision,
                object: CommitObject::Host(object),
                directory_durability: receipt.directory_durability,
                cleanup: receipt.cleanup,
            }),
            Err(error) => Err(FsError::OwnershipCommitPending {
                object: object.value(),
                error: Box::new(error),
            }),
        }
    }
    pub(super) fn ownership_requires_actor(&self, path: &str) -> Result<bool, FsError> {
        // Preserve the public path-validation contract before backend dispatch.
        let canonical = key(path)?;
        if !std::matches!(self.vfs.route(path)?, super::vfs::Route::Host) {
            return Ok(true);
        }
        let path = canonical;
        let Some(store) = self.existing_owner_store()? else {
            return Ok(false);
        };
        if store.symlink_metadata("access.pending").is_ok() {
            return Ok(true);
        }
        let catalog = self.load_owner_catalog(&store)?;
        if catalog.entries.contains_key(&path) {
            return Ok(true);
        }
        let access = self.load_access_catalog()?;
        let relative = super::backend::virtual_relative(&path)?;
        for ancestor in relative.ancestors() {
            if ancestor.as_os_str().is_empty() {
                continue;
            }
            let text = std::format!(
                "/{}",
                ancestor.to_string_lossy().replace(char::from(92), "/")
            );
            if self.directory_at_locked(&text, &access)?.is_some() {
                return Ok(true);
            }
        }
        if access
            .directories
            .keys()
            .any(|d| path == *d || path.strip_prefix(d).is_some_and(|r| r.starts_with("/")))
        {
            return Ok(true);
        }
        match store.symlink_metadata("pending") {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
            Ok(_) => {
                let intent = Intent::decode(&read_bounded(
                    &store,
                    Path::new("pending"),
                    ROOTFS_DEFAULTS.max_transaction_bytes,
                )?)?;
                Ok(
                    intent.old.entries.contains_key(&path)
                        || intent.new.entries.contains_key(&path),
                )
            }
        }
    }

    pub fn owned_file_info(&self, path: &str) -> Result<OwnershipState, FsError> {
        let guard = self.namespace_guard()?;
        self.owned_file_info_locked(path, &guard)
    }
    pub(super) fn owned_file_info_locked(
        &self,
        path: &str,
        guard: &NamespaceGuard<'_>,
    ) -> Result<OwnershipState, FsError> {
        guard.require_root(self)?;
        let path = key(path)?;
        let Some(store) = self.existing_owner_store()? else {
            return Ok(OwnershipState::LegacyUnknown);
        };
        let catalog = self.load_owner_catalog(&store)?;
        let Some(info) = catalog.entries.get(&path) else {
            return Ok(OwnershipState::LegacyUnknown);
        };
        self.verify_storage(&info)?;
        let relative = VirtualPath::strict(&info.path)
            .map_err(|_| FsError::InvalidPath)?
            .as_relative();
        match self.compare_relative_objects(&relative, &payload(&info), guard) {
            Ok(ObjectBinding::SameObject) => Ok(OwnershipState::Managed(info.clone())),
            Ok(ObjectBinding::DifferentObject) => Ok(OwnershipState::Unconfirmed {
                id: info.id,
                reason: io::ErrorKind::InvalidData,
            }),
            Ok(ObjectBinding::Unconfirmed(reason)) => Ok(OwnershipState::Unconfirmed {
                id: info.id,
                reason,
            }),
            Err(FsError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                Ok(OwnershipState::Unconfirmed {
                    id: info.id,
                    reason: io::ErrorKind::NotFound,
                })
            }
            Err(error) => Err(error),
        }
    }
    fn owned_lookup(&self, store: &Dir, path: &str) -> Result<OwnedFileInfo, FsError> {
        self.load_owner_catalog(store)?
            .entries
            .get(&key(path)?)
            .ok_or(FsError::LegacyOwnershipUnknown)
    }
    pub fn create_owned_file(
        &self,
        user: &Account,
        path: &str,
        bytes: &[u8],
        mode: VirtualMode,
    ) -> Result<OwnedCommit, FsError> {
        if let super::vfs::Route::Mounted { volume, path, .. } =
            self.mounted_name(user, path, Access::Write)?
        {
            let id = volume.create(&self.credentials_as(user)?, &path, bytes, mode)?;
            return self.virtual_commit(&volume, id);
        }
        self.create_owned_file_with_fault(user, path, bytes, mode, OwnershipFault::None)
    }
    pub(super) fn create_owned_file_with_fault(
        &self,
        user: &Account,
        path: &str,
        bytes: &[u8],
        mode: VirtualMode,
        fault: OwnershipFault,
    ) -> Result<OwnedCommit, FsError> {
        let path = user_path(user, path, Access::Write)?.to_string();
        storage_class(&path)?;
        let guard = self.namespace_guard()?;
        self.create_owned_file_locked(user, &path, bytes, mode, fault, &guard)
    }
    pub(super) fn create_owned_file_locked(
        &self,
        user: &Account,
        path: &str,
        bytes: &[u8],
        mode: VirtualMode,
        fault: OwnershipFault,
        guard: &NamespaceGuard<'_>,
    ) -> Result<OwnedCommit, FsError> {
        self.create_owned_file_configured_locked(
            user,
            path,
            bytes,
            (
                CreationMode::Exact(mode),
                fault,
                super::CreationOptions::default(),
            ),
            guard,
        )
    }
    pub(super) fn create_owned_file_configured_locked(
        &self,
        user: &Account,
        path: &str,
        bytes: &[u8],
        configuration: (CreationMode, OwnershipFault, super::CreationOptions<'_>),
        guard: &NamespaceGuard<'_>,
    ) -> Result<OwnedCommit, FsError> {
        self.create_owned_node_configured_locked(
            user,
            path,
            bytes,
            (
                configuration.0,
                configuration.1,
                configuration.2,
                super::HostFileKind::Regular,
            ),
            guard,
        )
    }
    pub(super) fn create_owned_node_configured_locked(
        &self,
        user: &Account,
        path: &str,
        bytes: &[u8],
        configuration: (
            CreationMode,
            OwnershipFault,
            super::CreationOptions<'_>,
            super::HostFileKind,
        ),
        guard: &NamespaceGuard<'_>,
    ) -> Result<OwnedCommit, FsError> {
        let resolved_path = self.resolve_host_locked(user, path, Access::Write, false, guard)?;
        let path = resolved_path.as_str();
        let (input, fault, options, kind) = configuration;
        // An unprivileged creator cannot install a setgid image for a group
        // inherited from a directory but not held by the creator.
        let input = match input {
            CreationMode::Exact(mode) if user.uid != 0 && mode.is_setgid() => {
                let access = self.load_access_catalog()?;
                let gid = self.creation_group_locked(user, path, &access)?.gid();
                let mode = if access.actor_credentials(user).groups.contains(&gid) {
                    mode
                } else {
                    VirtualMode::new(mode.bits() & !0o2000)?
                };
                CreationMode::Exact(mode)
            }
            other => other,
        };
        let requested = match input {
            CreationMode::Exact(mode) | CreationMode::Requested(mode) => mode,
        };
        requested.require_regular_file()?;
        guard.require_root(self)?;
        let class = storage_class(path)?;
        let condition = options.condition(self, user, path)?;
        self.resolve_new(path)?;
        self.parent_mutation_locked(user, path)?;
        let store = self.ensure_owner_store(guard)?;
        let old = self.load_owner_catalog(&store)?;
        let mut new = old.clone();
        let old_access = self.load_access_catalog()?;
        let policy = self.creation_access_locked(path, input, options.umask, &old_access)?;
        let gid = self.creation_group_locked(user, path, &old_access)?.gid();
        let mut info =
            new.allocate(path, FileOwner { uid: user.uid, gid }, policy.mode(), class)?;
        info.kind = kind;
        info.delete_condition = condition;
        self.check_resource_write_locked(&info, bytes.len() as u64, true)?;
        new.entries.insert(info.path.clone(), info.clone())?;
        let access = match policy {
            CreationAccess::Inherited { acl, .. } => {
                if old_access.file_acl.len() >= 4096 {
                    return Err(FsError::OwnershipCapacity);
                }
                let mut next = old_access.clone();
                next.file_acl.insert(info.id.value(), acl);
                next.bump()?;
                self.validate_policy_change(&old_access, &next)?;
                Some((old_access, next))
            }
            CreationAccess::Explicit(_) | CreationAccess::Umask(_) => None,
        };
        // Reserve the longest generated stage name; reject catalog/intent capacity
        // BEFORE staging payload bytes. Actual publication checks exact framing again.
        let mut intent = Intent {
            old,
            new,
            stage: format!("txn-{}-{}", u32::MAX, u64::MAX),
            access,
        };
        intent.encode()?;
        intent.stage = self.prepare_owned_payload(&info, bytes, guard)?;
        self.commit_owned_change(&store, intent, guard, fault)
    }
    pub fn read_owned_file(
        &self,
        user: &Account,
        path: &str,
        limit: u64,
    ) -> Result<Vec<u8>, FsError> {
        if !std::matches!(
            self.mounted(user, path, Access::Read)?,
            super::vfs::Route::Host
        ) {
            return self.read_file_as(user, path, limit);
        }

        self.read_owned_file_checked(user, path, limit, |user, info| {
            self.authorize_file(user, info, true, false)
        })
    }
    fn read_owned_file_checked<F>(
        &self,
        user: &Account,
        path: &str,
        limit: u64,
        check: F,
    ) -> Result<Vec<u8>, FsError>
    where
        F: FnOnce(&Account, &OwnedFileInfo) -> Result<(), FsError>,
    {
        if limit > ROOTFS_DEFAULTS.max_file_io_bytes as u64 {
            return Err(FsError::IoRequestTooLarge(
                ROOTFS_DEFAULTS.max_file_io_bytes,
            ));
        }
        let path = user_path(user, path, Access::Read)?.to_string();
        let guard = self.namespace_guard()?;
        let path = self.resolve_host_locked(user, &path, Access::Read, true, &guard)?;
        let store = self
            .existing_owner_store()?
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        let info = self.owned_lookup(&store, &path)?;
        self.search_locked(user, &path, &self.load_access_catalog()?)?;
        check(user, &info)?;
        self.verify_public_binding(&info, &guard)?;
        self.check_host_io_locked(&info, None, false, &guard)?;
        let mut bytes = Vec::new();
        self.dir
            .open(payload(&info))?
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(FsError::TooLarge(limit));
        }
        Ok(bytes)
    }
    #[cfg(test)]
    pub(super) fn read_without_object_mode_guard(
        &self,
        user: &Account,
        path: &str,
        limit: u64,
    ) -> Result<Vec<u8>, FsError> {
        self.read_owned_file_checked(user, path, limit, |_, _| Ok(()))
    }
    pub fn append_owned_file(
        &self,
        user: &Account,
        path: &str,
        bytes: &[u8],
    ) -> Result<usize, FsError> {
        if let super::vfs::Route::Mounted { volume, path, .. } =
            self.mounted(user, path, Access::Write)?
        {
            let mode = super::FileMode::new(
                super::FileAccess::WriteOnly,
                super::FileCreation::Existing,
                super::FileBehavior::Append,
            )?;
            return Ok(volume
                .open(
                    &self.credentials_as(user)?,
                    &path,
                    mode,
                    super::vfs::ShareMode::Shared,
                )?
                .write(bytes)?);
        }

        let path = user_path(user, path, Access::Write)?.to_string();
        if bytes.len() > ROOTFS_DEFAULTS.max_file_io_bytes {
            return Err(FsError::IoRequestTooLarge(
                ROOTFS_DEFAULTS.max_file_io_bytes,
            ));
        }
        let guard = self.namespace_guard()?;
        let path = self.resolve_host_locked(user, &path, Access::Write, true, &guard)?;
        let store = self
            .existing_owner_store()?
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        let info = self.owned_lookup(&store, &path)?;
        self.authorize_file(user, &info, false, true)?;
        self.verify_public_binding(&info, &guard)?;
        self.check_host_io_locked(&info, None, true, &guard)?;
        let length = self
            .dir
            .metadata(payload(&info))?
            .len()
            .checked_add(bytes.len() as u64)
            .ok_or(FsError::ResourceQuota)?;
        self.check_resource_write_locked(&info, length, false)?;
        if !bytes.is_empty() {
            self.clear_privilege_locked(info.id, info.storage_generation, &guard)?;
        }
        Ok(self
            .dir
            .open_with(payload(&info), OpenOptions::new().append(true))?
            .write(bytes)?)
    }
    pub fn replace_owned_file(
        &self,
        user: &Account,
        path: &str,
        bytes: &[u8],
    ) -> Result<OwnedCommit, FsError> {
        if let super::vfs::Route::Mounted { volume, path, .. } =
            self.mounted(user, path, Access::Write)?
        {
            let id = volume.replace(&self.credentials_as(user)?, &path, bytes)?;
            return self.virtual_commit(&volume, id);
        }

        self.replace_owned_file_with_fault(user, path, bytes, OwnershipFault::None)
    }
    pub(super) fn replace_owned_file_with_fault(
        &self,
        user: &Account,
        path: &str,
        bytes: &[u8],
        fault: OwnershipFault,
    ) -> Result<OwnedCommit, FsError> {
        let path = user_path(user, path, Access::Write)?.to_string();
        let guard = self.namespace_guard()?;
        let path = self.resolve_host_locked(user, &path, Access::Write, true, &guard)?;
        let store = self
            .existing_owner_store()?
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        let old = self.load_owner_catalog(&store)?;
        let original = old
            .entries
            .get(&key(&path)?)
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        self.authorize_file(user, &original, false, true)?;
        self.verify_public_binding(&original, &guard)?;
        self.check_host_io_locked(&original, None, true, &guard)?;
        self.check_resource_write_locked(&original, bytes.len() as u64, false)?;
        for alias in old.entries.values().filter(|info| info.id == original.id) {
            self.verify_public_binding(&alias, &guard)?;
        }
        self.clear_privilege_locked(original.id, original.storage_generation, &guard)?;
        let old = self.load_owner_catalog(&store)?;
        let mut new = old.clone();
        new.revision = new
            .revision
            .checked_add(1)
            .ok_or(FsError::OwnershipIdentityExhausted)?;
        let changed = new
            .entries
            .inode_mut(&key(&path)?)
            .ok_or(FsError::InvalidOwnershipRecord)?;
        changed.storage_generation = changed
            .storage_generation
            .checked_add(1)
            .ok_or(FsError::OwnershipIdentityExhausted)?;
        let info = new
            .entries
            .get(&key(&path)?)
            .ok_or(FsError::InvalidOwnershipRecord)?;
        let stage = self.prepare_owned_payload(&info, bytes, &guard)?;
        self.commit_owned_change(
            &store,
            Intent {
                old,
                new,
                stage,
                access: None,
            },
            &guard,
            fault,
        )
    }
    pub(super) fn hardlink_owned_file(
        &self,
        user: &Account,
        source: &str,
        destination: &str,
    ) -> Result<(), FsError> {
        self.hardlink_owned_file_with_fault(user, source, destination, OwnershipFault::None)
    }
    pub(super) fn hardlink_owned_file_with_fault(
        &self,
        user: &Account,
        source: &str,
        destination: &str,
        fault: OwnershipFault,
    ) -> Result<(), FsError> {
        let source = user_path(user, source, Access::Read)?.to_string();
        let destination = user_path(user, destination, Access::Write)?.to_string();
        let guard = self.namespace_guard()?;
        let source = self.resolve_host_locked(user, &source, Access::Write, false, &guard)?;
        let destination =
            self.resolve_host_locked(user, &destination, Access::Write, false, &guard)?;
        let class = storage_class(&destination)?;
        self.resource_rename_locked(&source, &destination)?;
        self.parent_mutation_locked(user, &destination)?;
        self.resolve_new(&destination)?;
        let store = self
            .existing_owner_store()?
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        let old = self.load_owner_catalog(&store)?;
        let mut alias = old
            .entries
            .get(&key(&source)?)
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        if !alias.kind.is_regular() {
            return Err(super::vfs::Error::Unsupported.into());
        }
        self.authorize_file(user, &alias, false, false)?;
        // Only the owner or administrator may extend the namespace lifetime.
        if user.uid != 0 && user.uid != alias.owner.uid {
            return Err(FsError::OwnershipDenied);
        }
        if class != alias.storage_class {
            return Err(FsError::OwnershipClassMismatch);
        }
        self.verify_public_binding(&alias, &guard)?;
        let mut new = old.clone();
        alias.path = key(&destination)?;
        new.entries.insert(alias.path.clone(), alias)?;
        new.revision = new
            .revision
            .checked_add(1)
            .ok_or(FsError::OwnershipIdentityExhausted)?;
        self.commit_owned_change(
            &store,
            Intent {
                old,
                new,
                stage: "-".into(),
                access: None,
            },
            &guard,
            fault,
        )?;
        Ok(())
    }
    pub fn rename_owned_file(
        &self,
        user: &Account,
        source: &str,
        destination: &str,
    ) -> Result<OwnedCommit, FsError> {
        let from = self.mounted_name(user, source, Access::Write)?;
        let to = self.mounted_name(user, destination, Access::Write)?;
        if !std::matches!(
            (&from, &to),
            (super::vfs::Route::Host, super::vfs::Route::Host)
        ) {
            let (
                super::vfs::Route::Mounted {
                    volume: a,
                    path: source,
                    ..
                },
                super::vfs::Route::Mounted {
                    volume: b,
                    path: destination,
                    ..
                },
            ) = (from, to)
            else {
                return Err(super::vfs::Error::CrossDevice.into());
            };
            if !std::sync::Arc::ptr_eq(&a, &b) {
                return Err(super::vfs::Error::CrossDevice.into());
            }
            let actor = self.credentials_as(user)?;
            let inode = a.stat(&actor, &source, false)?;
            if inode.kind == super::vfs::InodeKind::Directory {
                return Err(FsError::IsDirectory);
            }
            a.rename(&actor, &source, &destination, false)?;
            return self.virtual_commit(&a, inode.inode);
        }

        let source = user_path(user, source, Access::Write)?.to_string();
        let destination = user_path(user, destination, Access::Write)?.to_string();
        let guard = self.namespace_guard()?;
        let source = self.resolve_host_locked(user, &source, Access::Write, false, &guard)?;
        let destination =
            self.resolve_host_locked(user, &destination, Access::Write, false, &guard)?;
        let class = storage_class(&destination)?;
        self.resource_rename_locked(&source, &destination)?;
        self.resolve_new(&destination)?;
        self.parent_mutation_locked(user, &source)?;
        self.parent_mutation_locked(user, &destination)?;
        let store = self
            .existing_owner_store()?
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        let old = self.load_owner_catalog(&store)?;
        let info = old
            .entries
            .get(&key(&source)?)
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        self.name_removal_locked(user, &info.path, info.owner)?;
        if class != info.storage_class {
            return Err(FsError::OwnershipClassMismatch);
        }
        self.verify_public_binding(&info, &guard)?;
        let mut new = old.clone();
        new.revision = new
            .revision
            .checked_add(1)
            .ok_or(FsError::OwnershipIdentityExhausted)?;
        let mut info = new
            .entries
            .remove(&key(&source)?)
            .ok_or(FsError::InvalidOwnershipRecord)?;
        info.path = key(&destination)?;
        new.entries.insert(info.path.clone(), info)?;
        self.commit_owned_change(
            &store,
            Intent {
                old,
                new,
                stage: "-".into(),
                access: None,
            },
            &guard,
            OwnershipFault::None,
        )
    }
    pub fn remove_owned_file(&self, user: &Account, path: &str) -> Result<OwnedCommit, FsError> {
        if let super::vfs::Route::Mounted { volume, path, .. } =
            self.mounted_name(user, path, Access::Write)?
        {
            let actor = self.credentials_as(user)?;
            let inode = volume.stat(&actor, &path, false)?;
            volume.unlink(&actor, &path, false)?;
            return self.virtual_commit(&volume, inode.inode);
        }

        let path = user_path(user, path, Access::Write)?.to_string();
        let guard = self.namespace_guard()?;
        let path = self.resolve_host_locked(user, &path, Access::Write, false, &guard)?;
        self.remove_owned_file_locked(user, &path, &guard)
    }
    pub(super) fn remove_owned_file_locked(
        &self,
        user: &Account,
        path: &str,
        guard: &NamespaceGuard<'_>,
    ) -> Result<OwnedCommit, FsError> {
        if self.is_protected(path)? {
            return Err(FsError::ProtectedPath);
        }
        let relative = self
            .resolve(path)
            .map_err(super::open::classify_open_error)?;
        let metadata = self.dir.metadata(relative)?;
        if metadata.is_dir() {
            return Err(FsError::IsDirectory);
        }
        if !metadata.is_file() {
            return Err(FsError::NotFile);
        }
        self.parent_mutation_locked(user, path)?;
        let store = self
            .existing_owner_store()?
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        let old = self.load_owner_catalog(&store)?;
        let info = old
            .entries
            .get(&key(path)?)
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        self.name_removal_locked(user, &info.path, info.owner)?;
        self.verify_public_binding(&info, guard)?;
        let mut new = old.clone();
        new.revision = new
            .revision
            .checked_add(1)
            .ok_or(FsError::OwnershipIdentityExhausted)?;
        new.entries.remove(&key(path)?);
        self.commit_owned_change(
            &store,
            Intent {
                old,
                new,
                stage: "-".into(),
                access: None,
            },
            guard,
            OwnershipFault::None,
        )
    }
    pub fn chmod_owned_file(
        &self,
        user: &Account,
        path: &str,
        mode: VirtualMode,
    ) -> Result<OwnedCommit, FsError> {
        self.chmod_as(user, path, mode)
    }
}
