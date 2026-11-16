//! One authoritative Host catalog after an explicit, recoverable V1 -> V2 upgrade.
//! Legacy transaction checkpoints remain in force, under the same namespace lock.
use super::access_model::{AccessCatalog, MAX_ACCESS_BYTES};
use super::ownership::{read_bounded, store_path};
use super::ownership_model::{Catalog, MAX_CATALOG_BYTES};
use super::transaction::NamespaceGuard;
use super::{CommitReceipt, DataRoot, FsError};
use crate::{config::ROOTFS_DEFAULTS, users::Account};
use cap_std::fs::Dir;
use std::{io, path::Path};

pub(super) const JOINT_STORE_MAGIC: &[u8] = b"ELARA_OWNERSHIP_STORE_V2\n";
const JOINT_MAGIC: &[u8] = b"ELARA_NAMESPACE_CATALOG_V1\n";
const MIGRATION_MAGIC: &[u8] = b"ELARA_NAMESPACE_MIGRATION_V1\n";
const JOINT_NAME: &str = "namespace.catalog";
const MIGRATION_NAME: &str = "catalog.migration";
const STORE_OWNER: &str = "store.owner";
const FILE_CATALOG: &str = "catalog";
const ACCESS_CATALOG: &str = "access.catalog";

/// Visibility and host directory durability are distinct outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogUpgrade {
    Migrated {
        directory_durability: super::DirectoryDurability,
    },
    AlreadyCurrent {
        directory_durability: super::DirectoryDurability,
    },
}

/// A marker observation belongs to this open, not to a cached DataRoot.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CatalogFormat {
    Legacy,
    Joint,
}
pub(super) fn read_store_format(store: &Dir) -> Result<CatalogFormat, FsError> {
    use std::io::Read;
    let metadata = store
        .symlink_metadata(STORE_OWNER)
        .map_err(|_| FsError::UnownedOwnershipStore)?;
    if super::backend::linked(&metadata) || !metadata.is_file() {
        return Err(FsError::UnownedOwnershipStore);
    }
    let mut bytes = Vec::new();
    store.open(STORE_OWNER)?.take(33).read_to_end(&mut bytes)?;
    match bytes.as_slice() {
        super::ownership::STORE_MAGIC => Ok(CatalogFormat::Legacy),
        JOINT_STORE_MAGIC => Ok(CatalogFormat::Joint),
        _ => Err(FsError::UnownedOwnershipStore),
    }
}

#[derive(Clone, Copy)]
pub(super) enum CatalogComponent {
    Files,
    Access,
}
impl CatalogComponent {
    fn name(self) -> &'static str {
        match self {
            Self::Files => FILE_CATALOG,
            Self::Access => ACCESS_CATALOG,
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CatalogFault {
    None,
    #[cfg(test)]
    AfterIntent,
    #[cfg(test)]
    AfterJoint,
    #[cfg(test)]
    AfterMarker,
    #[cfg(test)]
    AfterFilesRetired,
    #[cfg(test)]
    AfterAccessRetired,
}

// Missing is a persisted state, not equivalent to a present empty catalog.
enum LegacyAccess<'a> {
    Missing,
    Present(&'a [u8]),
}
struct Migration<'a> {
    files: &'a [u8],
    access: LegacyAccess<'a>,
}

fn frame(magic: &[u8], left: &[u8], right: &[u8]) -> Result<Vec<u8>, FsError> {
    if left.len() > MAX_CATALOG_BYTES || right.len() > MAX_ACCESS_BYTES {
        return Err(FsError::OwnershipCapacity);
    }
    let size = magic.len() + 16 + left.len() + right.len();
    if size > ROOTFS_DEFAULTS.max_transaction_bytes {
        return Err(FsError::OwnershipCapacity);
    }
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(magic);
    out.extend_from_slice(&(left.len() as u64).to_le_bytes());
    out.extend_from_slice(&(right.len() as u64).to_le_bytes());
    out.extend_from_slice(left);
    out.extend_from_slice(right);
    Ok(out)
}
fn unframe<'a>(bytes: &'a [u8], magic: &[u8]) -> Result<(&'a [u8], &'a [u8]), FsError> {
    if bytes.len() > ROOTFS_DEFAULTS.max_transaction_bytes {
        return Err(FsError::OwnershipCapacity);
    }
    let body = bytes
        .strip_prefix(magic)
        .ok_or(FsError::InvalidOwnershipRecord)?;
    let size = |offset| -> Result<usize, FsError> {
        let part = body
            .get(offset..offset + 8)
            .ok_or(FsError::InvalidOwnershipRecord)?;
        usize::try_from(u64::from_le_bytes(
            part.try_into()
                .map_err(|_| FsError::InvalidOwnershipRecord)?,
        ))
        .map_err(|_| FsError::InvalidOwnershipRecord)
    };
    let left = size(0)?;
    let right = size(8)?;
    if left > MAX_CATALOG_BYTES || right > MAX_ACCESS_BYTES {
        return Err(FsError::OwnershipCapacity);
    }
    if body.len() != 16 + left + right {
        return Err(FsError::InvalidOwnershipRecord);
    }
    Ok((&body[16..16 + left], &body[16 + left..]))
}
fn joint_bytes(files: &Catalog, access: &AccessCatalog) -> Result<Vec<u8>, FsError> {
    frame(JOINT_MAGIC, &files.encode()?, &access.encode()?)
}
fn decode_joint(bytes: &[u8]) -> Result<(Catalog, AccessCatalog), FsError> {
    let (files, access) = unframe(bytes, JOINT_MAGIC)?;
    let pair = (Catalog::decode(files)?, AccessCatalog::decode(access)?);
    if joint_bytes(&pair.0, &pair.1)? != bytes {
        return Err(FsError::InvalidOwnershipRecord);
    }
    Ok(pair)
}
impl Migration<'_> {
    fn joint(&self) -> Result<Vec<u8>, FsError> {
        let access = match self.access {
            LegacyAccess::Missing => AccessCatalog::default(),
            LegacyAccess::Present(bytes) => AccessCatalog::decode(bytes)?,
        };
        let files = Catalog::decode(self.files)?;
        for info in files.entries.values() {
            if let super::DeleteCondition::TaskExit(id) = info.delete_condition {
                if access.tasks.get(&id) != Some(&info.owner.uid) {
                    return Err(FsError::InvalidOwnershipRecord);
                }
            }
        }
        joint_bytes(&files, &access)
    }
    fn encode(&self) -> Result<Vec<u8>, FsError> {
        let (tag, access) = match self.access {
            LegacyAccess::Missing => (0, &[][..]),
            LegacyAccess::Present(bytes) => (1, bytes),
        };
        let mut magic = MIGRATION_MAGIC.to_vec();
        magic.push(tag);
        frame(&magic, self.files, access)
    }
    fn decode(bytes: &[u8]) -> Result<Migration<'_>, FsError> {
        let tag = *bytes
            .get(MIGRATION_MAGIC.len())
            .ok_or(FsError::InvalidOwnershipRecord)?;
        let mut magic = MIGRATION_MAGIC.to_vec();
        magic.push(tag);
        let (files, access) = unframe(bytes, &magic)?;
        let access = match tag {
            0 if access.is_empty() => LegacyAccess::Missing,
            1 if !access.is_empty() => LegacyAccess::Present(access),
            _ => return Err(FsError::InvalidOwnershipRecord),
        };
        Ok(Migration { files, access })
    }
}
fn exists(store: &Dir, name: &str) -> Result<bool, FsError> {
    match store.symlink_metadata(name) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn absent(store: &Dir, name: &str) -> Result<(), FsError> {
    if exists(store, name)? {
        return Err(FsError::OwnershipRecoveryBlocked);
    }
    Ok(())
}
fn exact_file(store: &Dir, name: &str, expected: &[u8]) -> Result<(), FsError> {
    if read_bounded(store, Path::new(name), expected.len())? != expected {
        return Err(FsError::OwnershipRecoveryBlocked);
    }
    Ok(())
}
impl DataRoot {
    pub(super) fn joint_store(&self, store: &Dir) -> Result<bool, FsError> {
        Ok(read_store_format(store)? == CatalogFormat::Joint)
    }
    pub(super) fn joint_catalogs(&self, store: &Dir) -> Result<(Catalog, AccessCatalog), FsError> {
        decode_joint(&read_bounded(
            store,
            Path::new(JOINT_NAME),
            ROOTFS_DEFAULTS.max_transaction_bytes,
        )?)
    }
    /// Explicit catalog call sites use this adapter. Generic publication is unchanged.
    pub(super) fn publish_catalog_locked(
        &self,
        component: CatalogComponent,
        contents: &[u8],
        exclusive: bool,
        guard: &NamespaceGuard<'_>,
    ) -> Result<CommitReceipt, FsError> {
        guard.require_root(self)?;
        let observed = self
            .owner_store_state()?
            .ok_or(FsError::UnownedOwnershipStore)?;
        let store = observed.dir;
        if observed.format == CatalogFormat::Legacy {
            return self.publish_protocol_locked(
                &store_path().join(component.name()),
                contents,
                exclusive,
                guard,
            );
        }
        let (mut files, mut access) = self.joint_catalogs(&store)?;
        match component {
            CatalogComponent::Files => files = Catalog::decode(contents)?,
            CatalogComponent::Access => access = AccessCatalog::decode(contents)?,
        }
        self.publish_protocol_locked(
            &store_path().join(JOINT_NAME),
            &joint_bytes(&files, &access)?,
            false,
            guard,
        )
    }
    /// Root-only, idempotent upgrade. Host directory-sync limitations still apply;
    /// this does not promise power-loss durability on an unsupported host.
    pub fn upgrade_catalog_as(&self, actor: &Account) -> Result<CatalogUpgrade, FsError> {
        if actor.uid != 0 {
            return Err(FsError::OwnershipDenied);
        }
        let guard = self.namespace_guard()?;
        self.upgrade_catalog_locked(&guard, CatalogFault::None)
    }
    pub(super) fn upgrade_catalog_locked(
        &self,
        guard: &NamespaceGuard<'_>,
        fault: CatalogFault,
    ) -> Result<CatalogUpgrade, FsError> {
        guard.require_root(self)?;
        let store = self.ensure_owner_store(guard)?;
        if self.joint_store(&store)? {
            self.joint_catalogs(&store)?;
            return Ok(CatalogUpgrade::AlreadyCurrent {
                directory_durability: super::metadata::sync_relative_directory(self, &store_path()),
            });
        }
        for name in ["pending", "access.pending", JOINT_NAME, MIGRATION_NAME] {
            absent(&store, name)?;
        }
        let files = read_bounded(&store, Path::new(FILE_CATALOG), MAX_CATALOG_BYTES)?;
        let access = if exists(&store, ACCESS_CATALOG)? {
            Some(read_bounded(
                &store,
                Path::new(ACCESS_CATALOG),
                MAX_ACCESS_BYTES,
            )?)
        } else {
            None
        };
        let migration = Migration {
            files: &files,
            access: match &access {
                Some(bytes) => LegacyAccess::Present(bytes),
                None => LegacyAccess::Missing,
            },
        };
        migration.joint()?;
        let intent = migration.encode()?;
        self.publish_protocol_locked(&store_path().join(MIGRATION_NAME), &intent, true, guard)?;
        #[cfg(test)]
        if fault == CatalogFault::AfterIntent {
            return Err(io::Error::other("after migration intent").into());
        }
        self.finish_catalog_migration_locked(guard, fault)
            .map(|directory_durability| CatalogUpgrade::Migrated {
                directory_durability,
            })
    }
    fn finish_catalog_migration_locked(
        &self,
        guard: &NamespaceGuard<'_>,
        _fault: CatalogFault,
    ) -> Result<super::DirectoryDurability, FsError> {
        guard.require_root(self)?;
        let store = self
            .existing_owner_store()?
            .ok_or(FsError::UnownedOwnershipStore)?;
        let intent = read_bounded(
            &store,
            Path::new(MIGRATION_NAME),
            ROOTFS_DEFAULTS.max_transaction_bytes,
        )?;
        let migration = Migration::decode(&intent)?;
        let joint = migration.joint()?;
        let switched = self.joint_store(&store)?;
        // Validate every old file before retiring any. In V1, missing is data loss.
        if !switched || exists(&store, FILE_CATALOG)? {
            exact_file(&store, FILE_CATALOG, migration.files)?;
        }
        match migration.access {
            LegacyAccess::Missing => absent(&store, ACCESS_CATALOG)?,
            LegacyAccess::Present(bytes) if !switched || exists(&store, ACCESS_CATALOG)? => {
                exact_file(&store, ACCESS_CATALOG, bytes)?
            }
            LegacyAccess::Present(_) => {}
        }
        if switched || exists(&store, JOINT_NAME)? {
            exact_file(&store, JOINT_NAME, &joint)?;
        } else {
            self.publish_protocol_locked(&store_path().join(JOINT_NAME), &joint, true, guard)?;
        }
        #[cfg(test)]
        if _fault == CatalogFault::AfterJoint {
            return Err(io::Error::other("after joint catalog").into());
        }
        if !switched {
            self.publish_protocol_locked(
                &store_path().join(STORE_OWNER),
                JOINT_STORE_MAGIC,
                false,
                guard,
            )?;
        }
        #[cfg(test)]
        if _fault == CatalogFault::AfterMarker {
            return Err(io::Error::other("after V2 marker").into());
        }
        for name in [FILE_CATALOG, ACCESS_CATALOG] {
            if exists(&store, name)? {
                store.remove_file(name)?;
            }
            #[cfg(test)]
            if (name == FILE_CATALOG && _fault == CatalogFault::AfterFilesRetired)
                || (name == ACCESS_CATALOG && _fault == CatalogFault::AfterAccessRetired)
            {
                return Err(io::Error::other("after legacy retirement").into());
            }
        }
        super::metadata::sync_relative_directory(self, &store_path());
        store.remove_file(MIGRATION_NAME)?;
        Ok(super::metadata::sync_relative_directory(
            self,
            &store_path(),
        ))
    }
    /// Each recovery pass shares one validated directory, never a cross-lock cache.
    pub(super) fn recover_namespace_catalogs_locked(
        &self,
        guard: &NamespaceGuard<'_>,
    ) -> Result<(), FsError> {
        guard.require_root(self)?;
        let Some(observed) = self.owner_store_state()? else {
            return Ok(());
        };
        self.recover_catalog_migration_locked(&observed, guard)?;
        self.recover_access_intent_locked(&observed.dir, guard)?;
        self.recover_owner_intent_locked(&observed.dir, guard)?;
        Ok(())
    }
    fn recover_catalog_migration_locked(
        &self,
        observed: &super::ownership::OwnerStore,
        guard: &NamespaceGuard<'_>,
    ) -> Result<(), FsError> {
        guard.require_root(self)?;
        let store = &observed.dir;
        if exists(store, MIGRATION_NAME)? {
            return self
                .finish_catalog_migration_locked(guard, CatalogFault::None)
                .map(|_| ());
        }
        if observed.format == CatalogFormat::Joint {
            absent(store, FILE_CATALOG)?;
            absent(store, ACCESS_CATALOG)?;
            self.joint_catalogs(store)?;
        } else {
            absent(store, JOINT_NAME)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../tests/filesystem/namespace_catalog.rs"]
mod tests;
