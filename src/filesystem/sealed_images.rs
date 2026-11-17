//! Root-only encrypted checkpoints; key bytes never enter command output/log text.
use super::*;
use crate::{permissions::Access, users::Account};
use std::{io::Read, matches};
use zeroize::Zeroizing;
impl DataRoot {
    fn sealed_path(&self, actor: &Account, path: &str) -> Result<String, FsError> {
        if !actor.is_admin() {
            return Err(FsError::OwnershipDenied);
        }
        let path = super::ownership_model::key(path)?;
        if crate::config::write_class(&path) != crate::config::WriteClass::UserData
            || !matches!(self.vfs.route(&path)?, vfs::Route::Host)
        {
            return Err(FsError::OwnershipClassMismatch);
        }
        Ok(path)
    }
    fn image_key_as(&self, actor: &Account, path: &str) -> Result<Zeroizing<Vec<u8>>, FsError> {
        let path = self.sealed_path(actor, path)?;
        let guard = self.namespace_guard()?;
        let path = self.resolve_host_locked(actor, &path, Access::Read, true, &guard)?;
        let path = self.sealed_path(actor, &path)?;
        self.search_locked(actor, &path, &self.load_access_catalog()?)?;
        let store = self
            .existing_owner_store()?
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        let catalog = self.load_owner_catalog(&store)?;
        let info = catalog
            .entries
            .get(&path)
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        if info.owner.uid != actor.uid
            || info.mode.bits() & 0o7077 != 0
            || info.delete_condition != DeleteCondition::OwnerRequest
        {
            return Err(FsError::OwnershipDenied);
        }
        self.authorize_file(actor, &info, true, false)?;
        self.verify_public_binding(&info, &guard)?;
        self.check_host_io_locked(&info, None, false, &guard)?;
        let mut bytes = Zeroizing::new(Vec::with_capacity(33));
        self.dir
            .open(super::ownership::payload(&info))?
            .take(33)
            .read_to_end(&mut bytes)?;
        if bytes.len() != 32 {
            return Err(vfs::Error::Crypto.into());
        }
        Ok(bytes)
    }
    pub fn create_image_key_as(&self, actor: &Account, path: &str) -> Result<OwnedCommit, FsError> {
        let path = self.sealed_path(actor, path)?;
        let guard = self.namespace_guard()?;
        let path = self.resolve_host_locked(actor, &path, Access::Write, false, &guard)?;
        let path = self.sealed_path(actor, &path)?;
        // The consumer requires OwnerRequest. Reject resource-owned keys before
        // publishing bytes, under the same gate as resource registration.
        if !matches!(
            self.resource_condition_locked(&path)?,
            super::resource_model::CreationResource::Ordinary
        ) {
            return Err(FsError::OwnershipClassMismatch);
        }
        let secret = vfs::sealed::new_key()?;
        self.create_owned_file_locked(
            actor,
            &path,
            secret.as_ref(),
            VirtualMode::new(0o600)?,
            super::ownership::OwnershipFault::None,
            &guard,
        )
    }
    pub fn seal_volume_as(
        &self,
        actor: &Account,
        point: &str,
        image: &str,
        key_file: &str,
    ) -> Result<OwnedCommit, FsError> {
        let image = self.sealed_path(actor, image)?;
        let key_path = self.sealed_path(actor, key_file)?;
        if image == key_path {
            return Err(FsError::InvalidPath);
        }
        let key = self.image_key_as(actor, &key_path)?;
        let vfs::Route::Mounted { volume, path, .. } = self.mounted(actor, point, Access::Read)?
        else {
            return Err(vfs::Error::Unsupported.into());
        };
        if path != "/" {
            return Err(FsError::InvalidPath);
        }
        let bytes = volume.sealed_image(&self.credentials_as(actor)?, &key)?;
        match self.owned_file_info(&image)? {
            OwnershipState::Managed(_) => self.replace_owned_file(actor, &image, &bytes),
            OwnershipState::LegacyUnknown => {
                self.create_owned_file(actor, &image, &bytes, VirtualMode::new(0o600)?)
            }
            _ => Err(FsError::OwnershipBindingUnconfirmed),
        }
    }
    pub fn unseal_volume_as(
        &self,
        actor: &Account,
        image: &str,
        point: &str,
        key_file: &str,
    ) -> Result<vfs::Recovery, FsError> {
        let image = self.sealed_path(actor, image)?;
        let key = self.image_key_as(actor, key_file)?;
        let bytes = self.read_file_as(actor, &image, 1024 * 1024)?;
        let loaded = vfs::Volume::load_sealed_image(&bytes, &key)?;
        self.attach_volume_as(actor, point, loaded.volume)?;
        Ok(loaded.recovery)
    }
}
