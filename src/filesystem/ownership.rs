//! Persistent ownership for explicitly managed regular files. Data generations
//! are pinned inside a marked capability store; a versioned intent bridges the
//! public name and the text catalog. Legacy files are never silently adopted.
use super::backend::linked;
use super::ownership_model::{key, Catalog, FileOwner, ObjectId, OwnedFileInfo, OwnedStorageClass, OwnershipState, VirtualMode, MAX_CATALOG_BYTES};
use super::transaction::{NamespaceGuard, read_magic, store_path as tx_path, TX_OWNER};
use super::{CleanupStatus, CommitReceipt, DataRoot, DirectoryDurability, FsError, ObjectBinding};
use crate::config::{write_class, WriteClass, ROOTFS_DEFAULTS};
use crate::permissions::{self, Access};
use crate::users::Account;
use crate::virtual_path::VirtualPath;
use cap_std::fs::{Dir, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::format;

pub(super) const OWNER_STORE: &str = crate::config::PRIVATE_OWNER_DIRECTORY;
const STORE_MAGIC: &[u8] = b"ELARA_OWNERSHIP_STORE_V1\n";
pub(super) const OWNED_TX_MAGIC: &[u8] = b"ELARA_OWNED_OBJECT_TX_V1\n";
const INTENT_MAGIC: &str = "ELARA_OWNERSHIP_INTENT_V1";
const OBJECT_MAGIC: &str = "ELARA_OWNED_STORAGE_V1";
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum OwnershipFault {
    None,
    #[cfg(test)] AfterStorage,
    #[cfg(test)] AfterPublicName,
    #[cfg(test)] AfterCatalog,
    #[cfg(test)] AfterOldPayloadRemoval,
}
#[derive(Clone, Debug)]
pub struct OwnedCommit {
    pub revision: u64,
    pub object: ObjectId,
    pub directory_durability: DirectoryDurability,
    pub cleanup: CleanupStatus,
}
#[derive(Clone)]
struct Intent { old: Catalog, new: Catalog, stage: String }
impl Intent {
    fn encode(&self) -> Result<Vec<u8>, FsError> {
        let old = self.old.encode()?;
        let new = self.new.encode()?;
        let mut bytes = format!("{INTENT_MAGIC}\n{} {} {}\n", old.len(), new.len(), self.stage).into_bytes();
        bytes.extend_from_slice(&old); bytes.extend_from_slice(&new);
        if bytes.len() > ROOTFS_DEFAULTS.max_transaction_bytes { return Err(FsError::OwnershipCapacity); }
        Ok(bytes)
    }
    fn decode(bytes: &[u8]) -> Result<Self, FsError> {
        let prefix = format!("{INTENT_MAGIC}\n");
        let body = bytes.strip_prefix(prefix.as_bytes()).ok_or(FsError::InvalidOwnershipRecord)?;
        let end = body.iter().position(|b| *b == b'\n').ok_or(FsError::InvalidOwnershipRecord)?;
        let line = std::str::from_utf8(&body[..end]).map_err(|_| FsError::InvalidOwnershipRecord)?;
        let fields: Vec<_> = line.split(' ').collect();
        if fields.len() != 3 { return Err(FsError::InvalidOwnershipRecord); }
        let old_len = fields[0].parse::<usize>().map_err(|_| FsError::InvalidOwnershipRecord)?;
        let new_len = fields[1].parse::<usize>().map_err(|_| FsError::InvalidOwnershipRecord)?;
        if old_len > MAX_CATALOG_BYTES || new_len > MAX_CATALOG_BYTES { return Err(FsError::InvalidOwnershipRecord); }
        let data = &body[end+1..];
        if old_len.checked_add(new_len) != Some(data.len()) { return Err(FsError::InvalidOwnershipRecord); }
        let stage = fields[2].to_owned();
        if stage != "-" && (!stage.starts_with("txn-") || !stage[4..].bytes().all(|b| b.is_ascii_digit() || b == b'-')) {
            return Err(FsError::InvalidOwnershipRecord);
        }
        let intent = Self { old: Catalog::decode(&data[..old_len])?, new: Catalog::decode(&data[old_len..])?, stage };
        if intent.new.revision != intent.old.revision.checked_add(1).ok_or(FsError::OwnershipIdentityExhausted)?
            || intent.encode()?.as_slice() != bytes { return Err(FsError::InvalidOwnershipRecord); }
        Ok(intent)
    }
}
fn store_path() -> PathBuf { Path::new("var").join(OWNER_STORE) }
pub(super) fn payload(info: &OwnedFileInfo) -> PathBuf { store_path().join(format!("payload-{}-{}", info.id.value(), info.storage_generation)) }
fn marker(info: &OwnedFileInfo) -> PathBuf { store_path().join(format!("owner-{}-{}", info.id.value(), info.storage_generation)) }
fn object_magic(info: &OwnedFileInfo) -> Vec<u8> { format!("{OBJECT_MAGIC}\n{} {}\n", info.id.value(), info.storage_generation).into_bytes() }
fn read_bounded(dir: &Dir, path: &Path, limit: usize) -> Result<Vec<u8>, FsError> {
    let meta = dir.symlink_metadata(path)?;
    if linked(&meta) || !meta.is_file() { return Err(FsError::InvalidOwnershipRecord); }
    let mut bytes = Vec::new(); dir.open(path)?.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit { return Err(FsError::OwnershipCapacity); }
    Ok(bytes)
}
pub(super) fn user_path(user: &Account, text: &str, access: Access) -> Result<VirtualPath, FsError> {
    let path = VirtualPath::strict(text).map_err(|_| FsError::InvalidPath)?;
    permissions::check(user, &path, access).map_err(|e| FsError::AccessDenied(e.to_string()))?;
    Ok(path)
}
pub(super) fn storage_class(path: &str) -> Result<OwnedStorageClass, FsError> {
    match write_class(path) {
        WriteClass::UserData => Ok(OwnedStorageClass::UserData),
        WriteClass::Temporary => Ok(OwnedStorageClass::Temporary),
        WriteClass::SystemReadOnly => Err(FsError::ReadOnlySystemPath),
        _ => Err(FsError::OwnershipContextRequired),
    }
}
pub(super) fn authorize(user: &Account, info: &OwnedFileInfo, read: bool, write: bool) -> Result<(), FsError> {
    if !info.mode.permits(info.owner, user.uid, read, write) { return Err(FsError::OwnershipDenied); }
    Ok(())
}
fn mutation(intent: &Intent) -> Result<(Option<OwnedFileInfo>, Option<OwnedFileInfo>), FsError> {
    let old: std::collections::BTreeMap<_, _> = intent.old.entries.values().map(|v| (v.id, v.clone())).collect();
    let new: std::collections::BTreeMap<_, _> = intent.new.entries.values().map(|v| (v.id, v.clone())).collect();
    let ids: std::collections::BTreeSet<_> = old.keys().chain(new.keys()).copied().collect();
    let changed: Vec<_> = ids.into_iter().filter(|id| old.get(id) != new.get(id)).collect();
    if changed.len() != 1 { return Err(FsError::InvalidOwnershipRecord); }
    let id = changed[0];
    let before = old.get(&id).cloned(); let after = new.get(&id).cloned();
    match (&before, &after) {
        (None, Some(info)) => {
            if info.id.value() != intent.old.next_id || info.storage_generation != 1
                || intent.new.next_id != intent.old.next_id.checked_add(1).ok_or(FsError::OwnershipIdentityExhausted)? {
                return Err(FsError::InvalidOwnershipRecord);
            }
        }
        (Some(old), Some(new)) => {
            if old.owner != new.owner || old.storage_class != new.storage_class || old.delete_condition != new.delete_condition
                || intent.new.next_id != intent.old.next_id { return Err(FsError::InvalidOwnershipRecord); }
            let path_change = old.path != new.path;
            let generation_change = old.storage_generation != new.storage_generation;
            let mode_change = old.mode != new.mode;
            if usize::from(path_change) + usize::from(generation_change) + usize::from(mode_change) != 1
                || (generation_change && new.storage_generation != old.storage_generation.checked_add(1).ok_or(FsError::OwnershipIdentityExhausted)?) {
                return Err(FsError::InvalidOwnershipRecord);
            }
        }
        (Some(_), None) if intent.new.next_id == intent.old.next_id => {}
        _ => return Err(FsError::InvalidOwnershipRecord),
    }
    Ok((before, after))
}
impl DataRoot {
    pub(super) fn existing_owner_store(&self) -> Result<Option<Dir>, FsError> {
        match self.dir.symlink_metadata(store_path()) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
            Ok(meta) if linked(&meta) || !meta.is_dir() => return Err(FsError::UnownedOwnershipStore),
            Ok(_) => {}
        }
        self.inspect_relative(&store_path())?;
        let store = self.dir.open_dir(store_path())?;
        let marker = store.symlink_metadata("store.owner").map_err(|_| FsError::UnownedOwnershipStore)?;
        if linked(&marker) || !marker.is_file() || !read_magic(&store, "store.owner", STORE_MAGIC)? {
            return Err(FsError::UnownedOwnershipStore);
        }
        Ok(Some(store))
    }
    fn ensure_owner_store(&self, guard: &NamespaceGuard<'_>) -> Result<Dir, FsError> {
        if let Some(store) = self.existing_owner_store()? { return Ok(store); }
        self.dir.create_dir(store_path())?;
        let store = self.dir.open_dir(store_path())?;
        let mut marker = store.open_with("store.owner", OpenOptions::new().write(true).create_new(true))?;
        marker.write_all(STORE_MAGIC)?; marker.sync_all()?; drop(marker);
        self.publish_protocol_locked(&store_path().join("catalog"), &Catalog::empty().encode()?, true, guard)?;
        Ok(store)
    }
    pub(super) fn load_owner_catalog(&self, store: &Dir) -> Result<Catalog, FsError> {
        Catalog::decode(&read_bounded(store, Path::new("catalog"), MAX_CATALOG_BYTES)?)
    }
    fn verify_storage(&self, info: &OwnedFileInfo) -> Result<(), FsError> {
        self.inspect_relative(&marker(info))?; self.inspect_relative(&payload(info))?;
        if read_bounded(&self.dir, &marker(info), 128)? != object_magic(info)
            || !self.dir.metadata(payload(info))?.is_file() { return Err(FsError::InvalidOwnershipRecord); }
        Ok(())
    }
    pub(super) fn verify_public_binding(&self, info: &OwnedFileInfo, guard: &NamespaceGuard<'_>) -> Result<(), FsError> {
        self.verify_storage(info)?;
        match self.compare_relative_objects(&VirtualPath::strict(&info.path).map_err(|_| FsError::InvalidPath)?.as_relative(), &payload(info), guard)? {
            ObjectBinding::SameObject => Ok(()),
            _ => Err(FsError::OwnershipBindingUnconfirmed),
        }
    }
    fn prepare_owned_payload(&self, info: &OwnedFileInfo, bytes: &[u8], guard: &NamespaceGuard<'_>) -> Result<String, FsError> {
        if bytes.len() > ROOTFS_DEFAULTS.max_transaction_bytes { return Err(FsError::IoRequestTooLarge(ROOTFS_DEFAULTS.max_transaction_bytes)); }
        let (staging, name) = self.create_protocol_stage(guard)?;
        let stage = staging.open_dir(&name)?;
        let mut owner = stage.open_with(TX_OWNER, OpenOptions::new().write(true).create_new(true))?;
        owner.write_all(OWNED_TX_MAGIC)?; owner.sync_all()?;
        let mut description = stage.open_with("object.owner", OpenOptions::new().write(true).create_new(true))?;
        description.write_all(&object_magic(info))?; description.sync_all()?;
        let mut data = stage.open_with("payload", OpenOptions::new().write(true).create_new(true))?;
        data.write_all(bytes)?; data.sync_all()?;
        Ok(name)
    }
    fn install_owned_payload(&self, info: &OwnedFileInfo, stage_name: &str, guard: &NamespaceGuard<'_>) -> Result<(), FsError> {
        let private_payload = payload(info); let private_marker = marker(info);
        let exists = self.dir.symlink_metadata(&private_payload).is_ok();
        if exists { return self.verify_storage(info); }
        if stage_name == "-" { return Err(FsError::OwnershipRecoveryBlocked); }
        let stage_path = tx_path().join(stage_name);
        self.inspect_relative(&stage_path)?;
        let stage = self.dir.open_dir(stage_path)?;
        if !read_magic(&stage, TX_OWNER, OWNED_TX_MAGIC)? || !read_magic(&stage, "object.owner", &object_magic(info))? {
            return Err(FsError::InvalidOwnershipRecord);
        }
        match self.dir.symlink_metadata(&private_marker) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => stage.hard_link("object.owner", &self.dir, &private_marker)?,
            Err(e) => return Err(e.into()),
            Ok(_) if read_bounded(&self.dir, &private_marker, 128)? == object_magic(info) => {}
            Ok(_) => return Err(FsError::OwnershipRecoveryBlocked),
        }
        stage.hard_link("payload", &self.dir, &private_payload)?;
        guard.require_root(self)?;
        self.verify_storage(info)
    }
    fn finish_owner_intent(&self, store: &Dir, intent: &Intent, guard: &NamespaceGuard<'_>, _fault: OwnershipFault) -> Result<CommitReceipt, FsError> {
        let current = self.load_owner_catalog(store)?;
        if current.encode()? != intent.old.encode()? && current.encode()? != intent.new.encode()? {
            return Err(FsError::OwnershipRecoveryBlocked);
        }
        let (old, new) = mutation(intent)?;
        if let Some(info) = &new {
            if old.as_ref().is_none_or(|v| v.storage_generation != info.storage_generation) {
                self.install_owned_payload(info, &intent.stage, guard)?;
            } else { self.verify_storage(info)?; }
        }
        #[cfg(test)]
        if _fault == OwnershipFault::AfterStorage { return Err(io::Error::other("injected after storage installation").into()); }
        if current.revision == intent.old.revision {
            match (&old, &new) {
                (None, Some(info)) => {
                    let path = VirtualPath::strict(&info.path).map_err(|_| FsError::InvalidPath)?.as_relative();
                    match self.dir.symlink_metadata(&path) {
                        Err(e) if e.kind() == io::ErrorKind::NotFound => { self.publish_protocol_link_exclusive_locked(&payload(info), &path, guard)?; }
                        Err(e) => return Err(e.into()),
                        Ok(_) => self.verify_public_binding(info, guard)?,
                    }
                }
                (Some(old), Some(new)) if old.path != new.path => {
                    let source = VirtualPath::strict(&old.path).map_err(|_| FsError::InvalidPath)?.as_relative();
                    let destination = VirtualPath::strict(&new.path).map_err(|_| FsError::InvalidPath)?.as_relative();
                    match self.dir.symlink_metadata(&destination) {
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {
                            self.verify_public_binding(old, guard)?;
                            self.dir.rename(source, &self.dir, destination).map_err(FsError::AtomicRenameUnavailable)?;
                        }
                        Err(e) => return Err(e.into()),
                        Ok(_) => {
                            self.verify_public_binding(new, guard)?;
                            if self.dir.symlink_metadata(source).is_ok() { return Err(FsError::OwnershipRecoveryBlocked); }
                        }
                    }
                }
                (Some(old), Some(new)) if old.storage_generation != new.storage_generation => {
                    let path = VirtualPath::strict(&new.path).map_err(|_| FsError::InvalidPath)?.as_relative();
                    let already_new = self.compare_relative_objects(&path, &payload(new), guard)? == ObjectBinding::SameObject;
                    if !already_new {
                        self.verify_public_binding(old, guard)?;
                        self.publish_protocol_link_locked(&payload(new), &path, guard)?;
                    }
                }
                (Some(old), None) => {
                    let path = VirtualPath::strict(&old.path).map_err(|_| FsError::InvalidPath)?.as_relative();
                    match self.dir.symlink_metadata(&path) {
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e.into()),
                        Ok(_) => { self.verify_public_binding(old, guard)?; self.dir.remove_file(path)?; }
                    }
                }
                (Some(_), Some(_)) => {}
                _ => return Err(FsError::InvalidOwnershipRecord),
            }
        }
        #[cfg(test)]
        if _fault == OwnershipFault::AfterPublicName { return Err(io::Error::other("injected after public name publication").into()); }
        let receipt = self.publish_protocol_locked(&store_path().join("catalog"), &intent.new.encode()?, false, guard)?;
        #[cfg(test)]
        if _fault == OwnershipFault::AfterCatalog { return Err(io::Error::other("injected after catalog commit").into()); }
        // Do not lose evidence until the public binding and catalog agree.
        if let Some(info) = &new { self.verify_public_binding(info, guard)?; }
        if let Some(old) = &old {
            if new.as_ref().is_none_or(|v| v.storage_generation != old.storage_generation) {
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
                        if read_bounded(&self.dir, &private_marker, 128)? != object_magic(old) { return Err(FsError::OwnershipRecoveryBlocked); }
                        if payload_exists { self.dir.remove_file(&private_payload)?; }
                        #[cfg(test)]
                        if _fault == OwnershipFault::AfterOldPayloadRemoval { return Err(io::Error::other("injected during retirement cleanup").into()); }
                        self.dir.remove_file(&private_marker)?;
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound && !payload_exists => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        if intent.stage != "-" {
            let staging = self.dir.open_dir(tx_path())?;
            if staging.symlink_metadata(&intent.stage).is_ok() {
                if !super::transaction::discard_owned(&staging, std::ffi::OsStr::new(&intent.stage))? {
                    return Err(FsError::OwnershipRecoveryBlocked);
                }
            }
        }
        store.remove_file("pending")?;
        Ok(receipt)
    }
    pub(super) fn recover_owner_intent_locked(&self, guard: &NamespaceGuard<'_>) -> Result<(), FsError> {
        let pending = store_path().join("pending");
        match self.dir.symlink_metadata(&pending) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
            Ok(_) => {}
        }
        let store = self.existing_owner_store()?.ok_or(FsError::UnownedOwnershipStore)?;
        let intent = Intent::decode(&read_bounded(&store, Path::new("pending"), ROOTFS_DEFAULTS.max_transaction_bytes)?)?;
        self.finish_owner_intent(&store, &intent, guard, OwnershipFault::None)?;
        Ok(())
    }
    fn commit_owned_change(&self, store: &Dir, intent: Intent, guard: &NamespaceGuard<'_>, fault: OwnershipFault) -> Result<OwnedCommit, FsError> {
        let (old, new) = mutation(&intent)?;
        let object = new.as_ref().or(old.as_ref()).ok_or(FsError::InvalidOwnershipRecord)?.id;
        self.publish_protocol_locked(&store_path().join("pending"), &intent.encode()?, true, guard)?;
        match self.finish_owner_intent(store, &intent, guard, fault) {
            Ok(receipt) => Ok(OwnedCommit { revision: intent.new.revision, object,
                directory_durability: receipt.directory_durability, cleanup: receipt.cleanup }),
            Err(error) => Err(FsError::OwnershipCommitPending { object: object.value(), error: Box::new(error) }),
        }
    }
    pub(super) fn ownership_requires_actor(&self, path: &str) -> Result<bool, FsError> {
        let path = key(path)?;
        let Some(store) = self.existing_owner_store()? else { return Ok(false); };
        let catalog = self.load_owner_catalog(&store)?;
        if catalog.entries.contains_key(&path) { return Ok(true); }
        match store.symlink_metadata("pending") {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
            Ok(_) => {
                let intent = Intent::decode(&read_bounded(&store, Path::new("pending"), ROOTFS_DEFAULTS.max_transaction_bytes)?)?;
                Ok(intent.old.entries.contains_key(&path) || intent.new.entries.contains_key(&path))
            }
        }
    }

    pub fn owned_file_info(&self, path: &str) -> Result<OwnershipState, FsError> {
        let path = key(path)?;
        let guard = self.namespace_guard()?;
        let Some(store) = self.existing_owner_store()? else { return Ok(OwnershipState::LegacyUnknown); };
        let catalog = self.load_owner_catalog(&store)?;
        let Some(info) = catalog.entries.get(&path) else { return Ok(OwnershipState::LegacyUnknown); };
        self.verify_storage(info)?;
        let relative = VirtualPath::strict(&info.path).map_err(|_| FsError::InvalidPath)?.as_relative();
        match self.compare_relative_objects(&relative, &payload(info), &guard) {
            Ok(ObjectBinding::SameObject) => Ok(OwnershipState::Managed(info.clone())),
            Ok(ObjectBinding::DifferentObject) => Ok(OwnershipState::Unconfirmed { id: info.id, reason: io::ErrorKind::InvalidData }),
            Ok(ObjectBinding::Unconfirmed(reason)) => Ok(OwnershipState::Unconfirmed { id: info.id, reason }),
            Err(FsError::Io(error)) if error.kind() == io::ErrorKind::NotFound =>
                Ok(OwnershipState::Unconfirmed { id: info.id, reason: io::ErrorKind::NotFound }),
            Err(error) => Err(error),
        }
    }
    fn owned_lookup(&self, store: &Dir, path: &str) -> Result<OwnedFileInfo, FsError> {
        self.load_owner_catalog(store)?.entries.get(&key(path)?).cloned().ok_or(FsError::LegacyOwnershipUnknown)
    }
    pub fn create_owned_file(&self, user: &Account, path: &str, bytes: &[u8], mode: VirtualMode) -> Result<OwnedCommit, FsError> {
        self.create_owned_file_with_fault(user, path, bytes, mode, OwnershipFault::None)
    }
    pub(super) fn create_owned_file_with_fault(&self, user: &Account, path: &str, bytes: &[u8], mode: VirtualMode, fault: OwnershipFault) -> Result<OwnedCommit, FsError> {
        let path = user_path(user, path, Access::Write)?.to_string();
        storage_class(&path)?;
        let guard = self.namespace_guard()?;
        self.create_owned_file_locked(user, &path, bytes, mode, fault, &guard)
    }
    pub(super) fn create_owned_file_locked(&self, user: &Account, path: &str, bytes: &[u8], mode: VirtualMode,
        fault: OwnershipFault, guard: &NamespaceGuard<'_>) -> Result<OwnedCommit, FsError> {
        guard.require_root(self)?;
        let class = storage_class(path)?;
        self.resolve_new(path)?; self.ensure_write_allowed(path)?;
        let store = self.ensure_owner_store(&guard)?;
        let old = self.load_owner_catalog(&store)?; let mut new = old.clone();
        let info = new.allocate(&path, FileOwner { uid: user.uid, gid: user.uid }, mode, class)?;
        let stage = self.prepare_owned_payload(&info, bytes, &guard)?;
        self.commit_owned_change(&store, Intent { old, new, stage }, &guard, fault)
    }
    pub fn read_owned_file(&self, user: &Account, path: &str, limit: u64) -> Result<Vec<u8>, FsError> {
        self.read_owned_file_checked(user, path, limit, |user, info| authorize(user, info, true, false))
    }
    fn read_owned_file_checked<F>(&self, user: &Account, path: &str, limit: u64, check: F) -> Result<Vec<u8>, FsError>
    where F: FnOnce(&Account, &OwnedFileInfo) -> Result<(), FsError> {
        if limit > ROOTFS_DEFAULTS.max_file_io_bytes as u64 {
            return Err(FsError::IoRequestTooLarge(ROOTFS_DEFAULTS.max_file_io_bytes));
        }
        let path = user_path(user, path, Access::Read)?.to_string();
        let guard = self.namespace_guard()?;
        let store = self.existing_owner_store()?.ok_or(FsError::LegacyOwnershipUnknown)?;
        let info = self.owned_lookup(&store, &path)?;
        check(user, &info)?; self.verify_public_binding(&info, &guard)?;
        let mut bytes = Vec::new();
        self.dir.open(payload(&info))?.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit { return Err(FsError::TooLarge(limit)); }
        Ok(bytes)
    }
    #[cfg(test)]
    pub(super) fn read_without_object_mode_guard(&self, user: &Account, path: &str, limit: u64) -> Result<Vec<u8>, FsError> {
        self.read_owned_file_checked(user, path, limit, |_, _| Ok(()))
    }
    pub fn append_owned_file(&self, user: &Account, path: &str, bytes: &[u8]) -> Result<usize, FsError> {
        let path = user_path(user, path, Access::Write)?.to_string();
        if bytes.len() > ROOTFS_DEFAULTS.max_file_io_bytes { return Err(FsError::IoRequestTooLarge(ROOTFS_DEFAULTS.max_file_io_bytes)); }
        let guard = self.namespace_guard()?;
        let store = self.existing_owner_store()?.ok_or(FsError::LegacyOwnershipUnknown)?;
        let info = self.owned_lookup(&store, &path)?;
        authorize(user, &info, false, true)?; self.verify_public_binding(&info, &guard)?;
        Ok(self.dir.open_with(payload(&info), OpenOptions::new().append(true))?.write(bytes)?)
    }
    pub fn replace_owned_file(&self, user: &Account, path: &str, bytes: &[u8]) -> Result<OwnedCommit, FsError> {
        self.replace_owned_file_with_fault(user, path, bytes, OwnershipFault::None)
    }
    pub(super) fn replace_owned_file_with_fault(&self, user: &Account, path: &str, bytes: &[u8], fault: OwnershipFault) -> Result<OwnedCommit, FsError> {
        let path = user_path(user, path, Access::Write)?.to_string();
        let guard = self.namespace_guard()?;
        let store = self.existing_owner_store()?.ok_or(FsError::LegacyOwnershipUnknown)?;
        let old = self.load_owner_catalog(&store)?;
        let original = old.entries.get(&key(&path)?).ok_or(FsError::LegacyOwnershipUnknown)?;
        authorize(user, original, false, true)?; self.verify_public_binding(original, &guard)?;
        let mut new = old.clone();
        new.revision = new.revision.checked_add(1).ok_or(FsError::OwnershipIdentityExhausted)?;
        let changed = new.entries.get_mut(&key(&path)?).ok_or(FsError::InvalidOwnershipRecord)?;
        changed.storage_generation = changed.storage_generation.checked_add(1).ok_or(FsError::OwnershipIdentityExhausted)?;
        let info = changed.clone(); let stage = self.prepare_owned_payload(&info, bytes, &guard)?;
        self.commit_owned_change(&store, Intent { old, new, stage }, &guard, fault)
    }
    pub fn rename_owned_file(&self, user: &Account, source: &str, destination: &str) -> Result<OwnedCommit, FsError> {
        let source = user_path(user, source, Access::Write)?.to_string();
        let destination = user_path(user, destination, Access::Write)?.to_string();
        let class = storage_class(&destination)?;
        let guard = self.namespace_guard()?; self.resolve_new(&destination)?;
        let store = self.existing_owner_store()?.ok_or(FsError::LegacyOwnershipUnknown)?;
        let old = self.load_owner_catalog(&store)?;
        let info = old.entries.get(&key(&source)?).ok_or(FsError::LegacyOwnershipUnknown)?;
        if user.uid != 0 && user.uid != info.owner.uid { return Err(FsError::OwnershipDenied); }
        if class != info.storage_class { return Err(FsError::OwnershipClassMismatch); }
        self.verify_public_binding(info, &guard)?;
        let mut new = old.clone(); new.revision = new.revision.checked_add(1).ok_or(FsError::OwnershipIdentityExhausted)?;
        let mut info = new.entries.remove(&key(&source)?).ok_or(FsError::InvalidOwnershipRecord)?;
        info.path = key(&destination)?; new.entries.insert(info.path.clone(), info);
        self.commit_owned_change(&store, Intent { old, new, stage: "-".into() }, &guard, OwnershipFault::None)
    }
    pub fn remove_owned_file(&self, user: &Account, path: &str) -> Result<OwnedCommit, FsError> {
        let path = user_path(user, path, Access::Write)?.to_string();
        let guard = self.namespace_guard()?;
        let store = self.existing_owner_store()?.ok_or(FsError::LegacyOwnershipUnknown)?;
        let old = self.load_owner_catalog(&store)?;
        let info = old.entries.get(&key(&path)?).ok_or(FsError::LegacyOwnershipUnknown)?;
        if user.uid != 0 && user.uid != info.owner.uid { return Err(FsError::OwnershipDenied); }
        self.verify_public_binding(info, &guard)?;
        let mut new = old.clone(); new.revision = new.revision.checked_add(1).ok_or(FsError::OwnershipIdentityExhausted)?;
        new.entries.remove(&key(&path)?);
        self.commit_owned_change(&store, Intent { old, new, stage: "-".into() }, &guard, OwnershipFault::None)
    }
    pub fn chmod_owned_file(&self, user: &Account, path: &str, mode: VirtualMode) -> Result<OwnedCommit, FsError> {
        let path = user_path(user, path, Access::Write)?.to_string();
        let guard = self.namespace_guard()?;
        let store = self.existing_owner_store()?.ok_or(FsError::LegacyOwnershipUnknown)?;
        let old = self.load_owner_catalog(&store)?;
        let info = old.entries.get(&key(&path)?).ok_or(FsError::LegacyOwnershipUnknown)?;
        if user.uid != 0 && user.uid != info.owner.uid { return Err(FsError::OwnershipDenied); }
        self.verify_public_binding(info, &guard)?;
        if info.mode == mode { return Ok(OwnedCommit { object: info.id, revision: old.revision,
            directory_durability: DirectoryDurability::Unconfirmed(io::ErrorKind::Unsupported), cleanup: CleanupStatus::Complete }); }
        let mut new = old.clone(); new.revision = new.revision.checked_add(1).ok_or(FsError::OwnershipIdentityExhausted)?;
        new.entries.get_mut(&key(&path)?).ok_or(FsError::InvalidOwnershipRecord)?.mode = mode;
        self.commit_owned_change(&store, Intent { old, new, stage: "-".into() }, &guard, OwnershipFault::None)
    }
}
