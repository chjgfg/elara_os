//! Default providers never read host processes/devices or adopt backing files.
use super::{
    vfs::{self, PseudoKind, Route},
    DataRoot, FsError,
};
use crate::{permissions::Access, users::Account};
use std::matches;
use std::sync::Arc;

pub(super) enum DeviceTarget {
    Ordinary,
    Virtual {
        volume: Arc<vfs::Volume>,
        path: String,
    },
}
impl DataRoot {
    pub(super) fn install_system_providers(&self) -> Result<(), FsError> {
        let providers = [
            ("/proc", PseudoKind::Proc),
            ("/sys", PseudoKind::Sys),
            ("/dev", PseudoKind::Dev),
        ];
        // Validate every backing directory before publishing any default mount.
        // Unknown objects cause an explicit startup conflict, never deletion.
        for (point, _) in &providers {
            let path = super::backend::virtual_relative(point)?;
            self.inspect_relative(&path)?;
            let directory = self.dir.open_dir(path)?;
            if let Some(entry) = directory.entries()?.next() {
                entry?;
                return Err(FsError::SystemProviderConflict((*point).to_owned()));
            }
        }
        for (point, kind) in providers {
            self.vfs.install_system(point, kind)?;
        }
        Ok(())
    }
    pub fn mount_info_as(&self, actor: &Account) -> Result<Vec<vfs::MountInfo>, FsError> {
        if !actor.is_admin() {
            return Err(FsError::OwnershipDenied);
        }
        Ok(self.vfs.mount_info()?)
    }
    pub(super) fn system_device_as(
        &self,
        actor: &Account,
        path: &str,
    ) -> Result<DeviceTarget, FsError> {
        crate::virtual_path::VirtualPath::strict(path).map_err(|_| FsError::InvalidPath)?;
        let Route::Mounted {
            volume,
            path: relative,
            mount,
        } = self.vfs.route(path)?
        else {
            return Ok(DeviceTarget::Ordinary);
        };
        if mount != "/dev" || !volume.is_device_namespace() {
            return Ok(DeviceTarget::Ordinary);
        }
        super::ownership::user_path(actor, path, Access::Read)?;
        match volume.stat(&self.credentials_as(actor)?, &relative, false) {
            Ok(stat) if stat.kind == vfs::InodeKind::Device => Ok(DeviceTarget::Virtual {
                volume,
                path: relative,
            }),
            Ok(_) | Err(vfs::Error::NotFound) => Ok(DeviceTarget::Ordinary),
            Err(error) => Err(error.into()),
        }
    }
    /// IO preflight only, not namespace authority. Only a built-in device gets
    /// an exception to the ordinary path write policy; open still checks ACL.
    pub fn check_io_path_as(
        &self,
        actor: &Account,
        path: &str,
        access: Access,
    ) -> Result<(), FsError> {
        if matches!(access, Access::Write)
            && matches!(
                self.system_device_as(actor, path)?,
                DeviceTarget::Virtual { .. }
            )
        {
            return Ok(());
        }
        super::ownership::user_path(actor, path, access)?;
        Ok(())
    }
}
