//! Cooperative Host locks; payload locks remain exclusively identity probes.
//! Every catalog decision and associated IO runs under the namespace gate.
use super::ownership::store_path;
use super::transaction::{self, NamespaceGuard};
use super::vfs::{Error, LockMode, ShareMode};
use super::{DataRoot, FsError, OwnedFileInfo};
use cap_std::fs::OpenOptions;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::format;
use std::fs::{File, TryLockError};
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::Arc;

const MAGIC: &[u8] = b"ELARA_OPEN_CATALOG_V4\n";
const V3_MAGIC: &[u8] = b"ELARA_OPEN_CATALOG_V3\n";
const LEGACY_MAGIC: &[u8] = b"ELARA_OPEN_CATALOG_V1\n";
pub(super) const CONTAINER_MAGIC: &[u8] = b"ELARA_OPEN_CONTAINER_V3\n";
#[cfg(test)]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum HostOpenFault {
    #[default]
    None,
    BeforeContainerPublish,
    AfterContainerCreate,
    AfterOwner,
    AfterPayload,
    AfterWitness,
}
const LEGACY_V2_MAGIC: &[u8] = b"ELARA_OPEN_CATALOG_V2\n";
const MAX_OPENS: usize = 256;
const MAX_BYTES: u64 = 128 * 1024;
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Phase {
    Pending,
    Held,
    Retiring,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Lock {
    None,
    Shared,
    Exclusive,
}
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
enum ObjectDomain {
    #[default]
    File,
    Directory,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    #[serde(default)]
    domain: ObjectDomain,
    object: u64,
    generation: u64,
    exclusive: bool,
    lock: Lock,
    phase: Phase,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    next: u64,
    rows: BTreeMap<u64, Row>,
}
fn catalog_path() -> PathBuf {
    store_path().join("open.catalog")
}
fn lease_path(id: u64) -> PathBuf {
    store_path().join(format!("open-{id}.lease"))
}
fn marker(id: u64, row: &Row) -> Vec<u8> {
    if row.domain == ObjectDomain::Directory {
        return format!("ELARA_CWD_LEASE_V1
{id} {}
", row.object).into_bytes();
    }
    format!(
        "ELARA_OPEN_LEASE_V3\n{id} {} {}\n",
        row.object, row.generation
    )
    .into_bytes()
}
fn invalid() -> FsError {
    FsError::OwnershipBindingUnconfirmed
}

/// Explicit recovery cost: never silently promote an IO check to a full sweep.
enum Scan<'a> {
    All,
    One(u64),
    Directories(&'a BTreeSet<u64>),
    Io {
        info: &'a OwnedFileInfo,
        own: Option<u64>,
        write: bool,
    },
    Open {
        info: &'a OwnedFileInfo,
        share: ShareMode,
        truncate: bool,
    },
}
impl Scan<'_> {
    fn includes(&self, id: u64, row: &Row) -> bool {
        match self {
            Self::All => true,
            Self::One(own) => *own == id,
            Self::Directories(directories) => row.domain == ObjectDomain::Directory && directories.contains(&row.object),
            Self::Io { info, own, write } => {
                *own == Some(id)
                    || (row.domain == ObjectDomain::File
                        && row.object == info.id.value()
                        && row.generation == info.storage_generation
                        && (row.phase != Phase::Held
                            || (own.is_none() && row.exclusive)
                            || row.lock == Lock::Exclusive
                            || (*write && row.lock == Lock::Shared)))
            }
            Self::Open {
                info,
                share,
                truncate,
            } => {
                row.domain == ObjectDomain::File
                    && row.object == info.id.value()
                    && row.generation == info.storage_generation
                    && (row.phase != Phase::Held
                        || row.exclusive
                        || *share == ShareMode::Exclusive
                        || (*truncate && row.lock != Lock::None))
            }
        }
    }
}

pub(super) struct OpenLease {
    pub(super) root: Arc<DataRoot>,
    pub(super) id: u64,
    pub(super) file: Option<File>,
}
impl Drop for OpenLease {
    fn drop(&mut self) {
        // Failure leaves a registered lease, recovered at the next gate.
        if let Ok(guard) = self.root.namespace_guard() {
            self.file.take();
            let _ = self
                .root
                .recover_host_scan_locked(Scan::One(self.id), &guard);
        }
    }
}
impl Catalog {
    fn check(&self, info: &OwnedFileInfo, own: Option<u64>, write: bool) -> Result<(), FsError> {
        if let Some(id) = own {
            let row = self.rows.get(&id).ok_or_else(invalid)?;
            if row.domain != ObjectDomain::File
                || row.object != info.id.value()
                || row.generation != info.storage_generation
                || row.phase != Phase::Held
            {
                return Err(invalid());
            }
        }
        for (id, row) in &self.rows {
            if row.domain != ObjectDomain::File
                || Some(*id) == own
                || row.object != info.id.value()
                || row.generation != info.storage_generation
            {
                continue;
            }
            if row.phase != Phase::Held {
                return Err(invalid());
            }
            if own.is_none() && row.exclusive {
                return Err(Error::Busy.into());
            }
            if row.lock == Lock::Exclusive || write && row.lock == Lock::Shared {
                return Err(Error::LockConflict.into());
            }
        }
        Ok(())
    }
}
impl DataRoot {
    fn load_open_catalog(&self) -> Result<Catalog, FsError> {
        let empty = || Catalog {
            next: 1,
            rows: BTreeMap::new(),
        };
        if self.existing_owner_store()?.is_none() {
            return Ok(empty());
        }
        let path = catalog_path();
        match self.dir.symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(empty()),
            Err(e) => return Err(e.into()),
            Ok(meta) if !meta.is_file() || meta.len() > MAX_BYTES => return Err(invalid()),
            Ok(_) => {}
        }
        self.inspect_relative(&path)?;
        let mut bytes = Vec::new();
        self.dir
            .open(path)?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid());
        }
        let legacy = bytes.starts_with(LEGACY_MAGIC) || bytes.starts_with(LEGACY_V2_MAGIC);
        let data = bytes
            .strip_prefix(MAGIC)
            .or_else(|| bytes.strip_prefix(V3_MAGIC))
            .or_else(|| bytes.strip_prefix(LEGACY_MAGIC))
            .or_else(|| bytes.strip_prefix(LEGACY_V2_MAGIC))
            .ok_or_else(invalid)?;
        let catalog: Catalog = serde_json::from_slice(data).map_err(|_| invalid())?;
        if (legacy && !catalog.rows.is_empty())
            || (!bytes.starts_with(MAGIC) && catalog.rows.values().any(|r| r.domain != ObjectDomain::File))
        {
            return Err(invalid());
        }
        if catalog.next == 0
            || catalog.rows.len() > MAX_OPENS
            || catalog.rows.iter().any(|(id, r)| {
                *id == 0 || *id >= catalog.next || r.object == 0 || r.generation == 0
                    || (r.domain == ObjectDomain::Directory && (r.generation != 1 || r.exclusive || r.lock != Lock::None))
            })
        {
            return Err(invalid());
        }
        Ok(catalog)
    }
    fn save_open_catalog(
        &self,
        catalog: &Catalog,
        guard: &NamespaceGuard<'_>,
    ) -> Result<(), FsError> {
        let mut bytes = MAGIC.to_vec();
        serde_json::to_writer(&mut bytes, catalog).map_err(|_| invalid())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(FsError::OwnershipCapacity);
        }
        self.publish_protocol_locked(&catalog_path(), &bytes, false, guard)?;
        Ok(())
    }
    pub(super) fn recover_host_opens_locked(
        &self,
        guard: &NamespaceGuard<'_>,
    ) -> Result<(), FsError> {
        self.recover_host_scan_locked(Scan::All, guard).map(|_| ())
    }
    fn recover_host_scan_locked(
        &self,
        scan: Scan<'_>,
        guard: &NamespaceGuard<'_>,
    ) -> Result<Catalog, FsError> {
        guard.require_root(self)?;
        let mut catalog = self.load_open_catalog()?;
        let ids: Vec<_> = catalog
            .rows
            .iter()
            .filter(|(id, row)| scan.includes(**id, row))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let row = &catalog.rows[&id];
            let path = lease_path(id);
            let expected = marker(id, row);
            let exists = match self.dir.symlink_metadata(&path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound && row.phase != Phase::Held => false,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(invalid()),
                Err(e) => return Err(e.into()),
                Ok(meta) if !meta.is_dir() => return Err(invalid()),
                Ok(_) => true,
            };
            if exists {
                self.inspect_relative(&path)?;
                let container = self.dir.open_dir(&path)?;
                let mut names = Vec::new();
                for entry in container.entries()? {
                    let name = entry?.file_name();
                    if names.len() >= 3
                        || (name != transaction::TX_OWNER && name != "payload" && name != "witness")
                    {
                        return Err(invalid());
                    }
                    self.inspect_relative(&path.join(&name))?;
                    if !container.symlink_metadata(&name)?.is_file() {
                        return Err(invalid());
                    }
                    names.push(name);
                }
                let has = |name: &str| names.iter().any(|v| v == name);
                if row.phase == Phase::Pending
                    && ((!names.is_empty() && !has(transaction::TX_OWNER))
                        || (has("witness") && !has("payload")))
                {
                    return Err(invalid());
                }
                if row.phase == Phase::Held
                    && (!has(transaction::TX_OWNER) || !has("payload") || !has("witness"))
                {
                    return Err(invalid());
                }
                if has(transaction::TX_OWNER)
                    && !transaction::read_magic(&container, transaction::TX_OWNER, CONTAINER_MAGIC)?
                {
                    return Err(invalid());
                }
                for name in ["payload", "witness"] {
                    if has(name) && container.symlink_metadata(name)?.len() != expected.len() as u64
                    {
                        return Err(invalid());
                    }
                }
                let options = OpenOptions::new().read(true).write(true).to_owned();
                if has("payload") && has("witness") {
                    let a = container.open_with("payload", &options)?.into_std();
                    let b = container.open_with("witness", &options)?.into_std();
                    match a.try_lock() {
                        Err(TryLockError::WouldBlock) => match b.try_lock() {
                            Err(TryLockError::WouldBlock) if row.phase == Phase::Held => continue,
                            Ok(()) => {
                                b.unlock()?;
                                return Err(invalid());
                            }
                            _ => return Err(invalid()),
                        },
                        Err(TryLockError::Error(e)) => return Err(e.into()),
                        Ok(()) => a.unlock()?,
                    }
                    if super::binding::controlled_binding(&a, &b)
                        != super::ObjectBinding::SameObject
                    {
                        return Err(invalid());
                    }
                }
                // Retiring is durable proof that the complete pair was checked
                // while dead. It permits finishing an interrupted bounded unlink.
                for name in &names {
                    let expected_bytes = if name == transaction::TX_OWNER {
                        CONTAINER_MAGIC
                    } else {
                        expected.as_slice()
                    };
                    let mut file = container.open_with(name, &options)?.into_std();
                    file.try_lock().map_err(|_| invalid())?;
                    let mut bytes = Vec::new();
                    (&mut file)
                        .take(expected_bytes.len() as u64 + 1)
                        .read_to_end(&mut bytes)?;
                    if bytes != expected_bytes {
                        return Err(invalid());
                    }
                }
                catalog.rows.get_mut(&id).ok_or_else(invalid)?.phase = Phase::Retiring;
                self.save_open_catalog(&catalog, guard)?;
                for name in names {
                    container.remove_file(name)?;
                }
                drop(container);
                self.dir.remove_dir(&path)?;
            }
            catalog.rows.remove(&id);
            self.save_open_catalog(&catalog, guard)?;
        }
        Ok(catalog)
    }
    fn publish_host_lease_container(
        &self,
        id: u64,
        bytes: &[u8],
        guard: &NamespaceGuard<'_>,
    ) -> Result<(), FsError> {
        // The Pending catalog already owns this birth. No open capability is
        // granted until all names exist and Held is durably published. Keeping
        // the registered directory at its final location avoids renaming a
        // just-created directory on hosts where that operation is unavailable.
        #[cfg(test)]
        self.host_open_checkpoint(HostOpenFault::BeforeContainerPublish)?;
        let path = lease_path(id);
        self.dir.create_dir(&path)?;
        #[cfg(test)]
        self.host_open_checkpoint(HostOpenFault::AfterContainerCreate)?;
        self.publish_protocol_locked(
            &path.join(transaction::TX_OWNER),
            CONTAINER_MAGIC,
            true,
            guard,
        )?;
        #[cfg(test)]
        self.host_open_checkpoint(HostOpenFault::AfterOwner)?;
        self.publish_protocol_locked(&path.join("payload"), bytes, true, guard)?;
        #[cfg(test)]
        self.host_open_checkpoint(HostOpenFault::AfterPayload)?;
        self.dir
            .hard_link(path.join("payload"), &self.dir, path.join("witness"))?;
        #[cfg(test)]
        self.host_open_checkpoint(HostOpenFault::AfterWitness)?;
        Ok(())
    }
    #[cfg(test)]
    fn host_open_checkpoint(&self, point: HostOpenFault) -> Result<(), FsError> {
        if self.host_open_fault == point {
            Err(FsError::AtomicPublishUnavailable(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected lease publication denial",
            )))
        } else {
            Ok(())
        }
    }
    pub(super) fn check_host_io_locked(
        &self,
        info: &OwnedFileInfo,
        own: Option<u64>,
        write: bool,
        guard: &NamespaceGuard<'_>,
    ) -> Result<(), FsError> {
        self.recover_host_scan_locked(Scan::Io { info, own, write }, guard)?
            .check(info, own, write)
    }
    fn capacity_catalog_locked(&self, guard: &NamespaceGuard<'_>) -> Result<Catalog, FsError> {
        guard.require_root(self)?;
        let catalog = self.load_open_catalog()?;
        if catalog.rows.len() >= MAX_OPENS {
            self.recover_host_scan_locked(Scan::All, guard)
        } else {
            Ok(catalog)
        }
    }
    pub(super) fn check_host_open_capacity_locked(
        &self,
        guard: &NamespaceGuard<'_>,
    ) -> Result<(), FsError> {
        let catalog = self.capacity_catalog_locked(guard)?;
        if catalog.rows.len() >= MAX_OPENS {
            return Err(FsError::OwnershipCapacity);
        }
        catalog
            .next
            .checked_add(1)
            .ok_or(FsError::OwnershipIdentityExhausted)?;
        match self.dir.symlink_metadata(lease_path(catalog.next)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
            Ok(_) => Err(invalid()),
        }
    }
    pub(super) fn register_host_open_locked(
        &self,
        info: &OwnedFileInfo,
        share: ShareMode,
        truncate: bool,
        guard: &NamespaceGuard<'_>,
    ) -> Result<(u64, File), FsError> {
        self.check_host_open_capacity_locked(guard)?;
        let catalog = self.recover_host_scan_locked(
            Scan::Open {
                info,
                share,
                truncate,
            },
            guard,
        )?;
        // Opens without truncation grant a descriptor; locks gate its later IO.
        for row in catalog.rows.values() {
            if row.domain == ObjectDomain::File
                && row.object == info.id.value()
                && row.generation == info.storage_generation
                && (row.exclusive || share == ShareMode::Exclusive)
            {
                return Err(Error::Busy.into());
            }
        }
        if truncate {
            catalog.check(info, None, true)?;
        }
        let row = Row {
            domain: ObjectDomain::File,
            object: info.id.value(),
            generation: info.storage_generation,
            exclusive: share == ShareMode::Exclusive,
            lock: Lock::None,
            phase: Phase::Pending,
        };
        self.register_lease_row(catalog, row, guard)
    }
    pub(super) fn register_host_cwd_locked(
        &self,
        directory: u64,
        guard: &NamespaceGuard<'_>,
    ) -> Result<(u64, File), FsError> {
        self.check_host_open_capacity_locked(guard)?;
        let catalog = self.recover_host_scan_locked(Scan::Directories(&BTreeSet::from([directory])), guard)?;
        self.register_lease_row(catalog, Row {
            domain: ObjectDomain::Directory,
            object: directory,
            generation: 1,
            exclusive: false,
            lock: Lock::None,
            phase: Phase::Pending,
        }, guard)
    }
    pub(super) fn check_host_cwd_unpinned_locked(
        &self,
        directories: &BTreeSet<u64>,
        guard: &NamespaceGuard<'_>,
    ) -> Result<(), FsError> {
        if directories.is_empty() { return Ok(()); }
        let catalog = self.recover_host_scan_locked(Scan::Directories(directories), guard)?;
        if catalog.rows.values().any(|r| r.domain == ObjectDomain::Directory && directories.contains(&r.object)) {
            return Err(Error::Busy.into());
        }
        Ok(())
    }
    fn register_lease_row(
        &self,
        mut catalog: Catalog,
        row: Row,
        guard: &NamespaceGuard<'_>,
    ) -> Result<(u64, File), FsError> {
        if catalog.rows.len() >= MAX_OPENS {
            return Err(FsError::OwnershipCapacity);
        }
        let id = catalog.next;
        catalog.next = id.checked_add(1).ok_or(FsError::OwnershipIdentityExhausted)?;
        match self.dir.symlink_metadata(lease_path(id)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
            Ok(_) => return Err(invalid()),
        }
        let bytes = marker(id, &row);
        catalog.rows.insert(id, row);
        self.save_open_catalog(&catalog, guard)?;
        self.publish_host_lease_container(id, &bytes, guard)?;
        let file = self
            .dir
            .open_with(
                lease_path(id).join("payload"),
                OpenOptions::new().read(true).write(true),
            )?
            .into_std();
        file.try_lock().map_err(|e| match e {
            TryLockError::WouldBlock => invalid(),
            TryLockError::Error(e) => e.into(),
        })?;
        catalog.rows.get_mut(&id).ok_or_else(invalid)?.phase = Phase::Held;
        self.save_open_catalog(&catalog, guard)?;
        Ok((id, file))
    }
    pub(super) fn set_host_lock_locked(
        &self,
        info: &OwnedFileInfo,
        id: u64,
        mode: Option<LockMode>,
        guard: &NamespaceGuard<'_>,
    ) -> Result<(), FsError> {
        let scan = match mode {
            Some(mode) => Scan::Io {
                info,
                own: Some(id),
                write: mode == LockMode::Exclusive,
            },
            None => Scan::One(id),
        };
        let mut catalog = self.recover_host_scan_locked(scan, guard)?;
        let own = catalog.rows.get(&id).ok_or_else(invalid)?;
        if own.domain != ObjectDomain::File
            || own.object != info.id.value()
            || own.generation != info.storage_generation
            || own.phase != Phase::Held
        {
            return Err(invalid());
        }
        if let Some(mode) = mode {
            catalog.check(info, Some(id), mode == LockMode::Exclusive)?;
        }
        catalog.rows.get_mut(&id).ok_or_else(invalid)?.lock = match mode {
            None => Lock::None,
            Some(LockMode::Shared) => Lock::Shared,
            Some(LockMode::Exclusive) => Lock::Exclusive,
        };
        self.save_open_catalog(&catalog, guard)
    }
}
