//! Administrator-only block images in registered ordinary UserData objects.
//! Serialize image IO under the existing namespace gate, without reentrant FD IO.
use super::{
    vfs::{self, block_store},
    *,
};
use crate::{permissions::Access, users::Account};
use std::matches;
fn error(error: block_store::Error) -> FsError {
    match error {
        block_store::Error::Io(e) => e.into(),
        block_store::Error::NoSpace => vfs::Error::NoSpace.into(),
        block_store::Error::ReadOnly => vfs::Error::ReadOnly.into(),
        block_store::Error::Invalid | block_store::Error::Bounds => vfs::Error::InvalidImage.into(),
    }
}
impl DataRoot {
    fn block_image_path(&self, actor: &Account, image: &str, write: bool) -> Result<(), FsError> {
        if actor.uid != 0 {
            return Err(FsError::OwnershipDenied);
        }
        let path = super::ownership_model::key(image)?;
        super::ownership::user_path(
            actor,
            &path,
            if write { Access::Write } else { Access::Read },
        )?;
        if crate::config::write_class(&path) != crate::config::WriteClass::UserData
            || !matches!(self.vfs.route(&path)?, vfs::Route::Host)
        {
            return Err(FsError::OwnershipClassMismatch);
        }
        Ok(())
    }
    fn block_image_file_locked(
        &self,
        actor: &Account,
        image: &str,
        write: bool,
        guard: &super::transaction::NamespaceGuard<'_>,
    ) -> Result<std::fs::File, FsError> {
        let resolved = self.resolve_host_locked(actor, image,
            if write { Access::Write } else { Access::Read }, true, guard)?;
        self.block_image_path(actor, &resolved, write)?;
        let path = super::ownership_model::key(&resolved)?;
        self.search_locked(actor, &path, &self.load_access_catalog()?)?;
        let store = self
            .existing_owner_store()?
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        let catalog = self.load_owner_catalog(&store)?;
        let info = catalog
            .entries
            .get(&path)
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        if info.storage_class != OwnedStorageClass::UserData
            || info.delete_condition != DeleteCondition::OwnerRequest
        {
            return Err(FsError::OwnershipClassMismatch);
        }
        self.authorize_file(actor, &info, true, write)?;
        self.verify_public_binding(&info, guard)?;
        self.check_host_io_locked(&info, None, write, guard)?;
        if write {
            self.clear_privilege_locked(info.id, info.storage_generation, guard)?;
        }
        let mut options = cap_std::fs::OpenOptions::new();
        options.read(true).write(write);
        Ok(self
            .dir
            .open_with(super::ownership::payload(&info), &options)?
            .into_std())
    }
    fn volume_block_records_as(
        &self,
        actor: &Account,
        point: &str,
    ) -> Result<block_store::Records, FsError> {
        if actor.uid != 0 {
            return Err(FsError::OwnershipDenied);
        }
        let vfs::Route::Mounted { volume, path, .. } = self.mounted(actor, point, Access::Read)?
        else {
            return Err(vfs::Error::Unsupported.into());
        };
        if path != "/" {
            return Err(FsError::InvalidPath);
        }
        Ok(volume.block_records(&self.credentials_as(actor)?)?)
    }
    pub fn create_block_image_as(
        &self,
        actor: &Account,
        point: &str,
        image: &str,
        slots: u32,
        cache_pages: usize,
    ) -> Result<block_store::Stats, FsError> {
        self.block_image_path(actor, image, true)?;
        if !(8..=65536).contains(&slots) || !(1..=512).contains(&cache_pages) {
            return Err(vfs::Error::Quota.into());
        }
        let records = self.volume_block_records_as(actor, point)?;
        block_store::Store::<std::fs::File>::validate_initial(&records, slots, cache_pages)
            .map_err(error)?;
        let guard = self.namespace_guard()?;
        let resolved = self.resolve_host_locked(actor, image, Access::Write, false, &guard)?;
        let image = resolved.as_str();
        self.block_image_path(actor, image, true)?;
        // Root-only ordinary ownership: an application quota is never bypassed.
        if !matches!(
            self.resource_condition_locked(image)?,
            super::resource_model::CreationResource::Ordinary
        ) {
            return Err(FsError::OwnershipClassMismatch);
        }
        self.create_owned_file_locked(
            actor,
            image,
            b"",
            VirtualMode::new(0o600)?,
            super::ownership::OwnershipFault::None,
            &guard,
        )?;
        let file = self.block_image_file_locked(actor, image, true, &guard)?;
        let mut disk = block_store::Store::format(file, slots, cache_pages).map_err(error)?;
        disk.commit(&records).map_err(error)?;
        Ok(disk.stats())
    }
    pub fn save_block_image_as(
        &self,
        actor: &Account,
        point: &str,
        image: &str,
        cache_pages: usize,
    ) -> Result<block_store::Stats, FsError> {
        self.block_image_path(actor, image, true)?;
        let records = self.volume_block_records_as(actor, point)?;
        let guard = self.namespace_guard()?;
        let file = self.block_image_file_locked(actor, image, true, &guard)?;
        let mut disk = block_store::Store::open(file, cache_pages).map_err(error)?;
        disk.commit(&records).map_err(error)?;
        Ok(disk.stats())
    }
    pub fn load_block_image_as(
        &self,
        actor: &Account,
        image: &str,
        point: &str,
        cache_pages: usize,
    ) -> Result<block_store::Recovery, FsError> {
        self.block_image_path(actor, image, false)?;
        let (volume, recovery) = {
            let guard = self.namespace_guard()?;
            let file = self.block_image_file_locked(actor, image, false, &guard)?;
            let mut disk = block_store::Store::open(file, cache_pages).map_err(error)?;
            let recovery = disk.recovery();
            let volume = vfs::Volume::load_block_records(
                &disk.records().map_err(error)?,
                recovery != block_store::Recovery::Clean,
            )?;
            (volume, recovery)
        };
        self.attach_volume_as(actor, point, volume)?;
        Ok(recovery)
    }
    pub fn repair_block_image_as(
        &self,
        actor: &Account,
        image: &str,
        cache_pages: usize,
    ) -> Result<block_store::Stats, FsError> {
        self.block_image_path(actor, image, true)?;
        let guard = self.namespace_guard()?;
        let file = self.block_image_file_locked(actor, image, true, &guard)?;
        let mut disk = block_store::Store::open(file, cache_pages).map_err(error)?;
        // Validate the filesystem graph as well as physical checksums before repair.
        vfs::Volume::load_block_records(&disk.records().map_err(error)?, true)?;
        disk.repair().map_err(error)?;
        Ok(disk.stats())
    }
    pub fn block_image_stats_as(
        &self,
        actor: &Account,
        image: &str,
        cache_pages: usize,
    ) -> Result<(block_store::Recovery, block_store::Stats), FsError> {
        self.block_image_path(actor, image, false)?;
        let guard = self.namespace_guard()?;
        let file = self.block_image_file_locked(actor, image, false, &guard)?;
        let disk = block_store::Store::open(file, cache_pages).map_err(error)?;
        Ok((disk.recovery(), disk.stats()))
    }
}
