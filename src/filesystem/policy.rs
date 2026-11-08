//! Normal-write policy. Privilege checks and directory classes are separate:
//! even root cannot bypass a read-only system path through the ordinary APIs.
use super::backend::{virtual_relative, DataRoot};
use super::types::FsError;
use crate::config::{write_class, WriteClass, ROOTFS_DEFAULTS};
use crate::virtual_path::VirtualPath;

impl DataRoot {
    pub(super) fn is_protected(&self, virtual_path: &str) -> Result<bool, FsError> {
        let relative = virtual_relative(virtual_path)?;
        if relative.as_os_str().is_empty() {
            return Ok(true);
        }
        let text = relative.to_string_lossy().replace('\\', "/");
        Ok(ROOTFS_DEFAULTS.directories.iter().any(|directory| {
            if std::cfg!(windows) {
                text.eq_ignore_ascii_case(directory)
            } else {
                text == *directory
            }
        }))
    }

    pub(super) fn ensure_write_allowed(&self, virtual_path: &str) -> Result<WriteClass, FsError> {
        let path = VirtualPath::strict(virtual_path).map_err(|_| FsError::InvalidPath)?;
        if path.as_relative().as_os_str().is_empty() {
            return Err(FsError::ProtectedPath);
        }
        let text = path.to_string();
        let class = write_class(&text);
        #[cfg(test)]
        if self.bypass_write_policy {
            return Ok(class);
        }
        let log_view = if std::cfg!(windows) {
            text.eq_ignore_ascii_case("/var/log/elara.log")
        } else {
            text == "/var/log/elara.log"
        };
        if log_view {
            let store = std::path::Path::new("var/log").join(crate::config::PRIVATE_LOG_DIRECTORY);
            match self.dir.symlink_metadata(store) {
                Ok(_) => return Err(FsError::ServiceOwnedPath),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        if self.ownership_requires_actor(&text)? {
            return Err(FsError::OwnershipContextRequired);
        }
        match class {
            WriteClass::SystemReadOnly => Err(FsError::ReadOnlySystemPath),
            WriteClass::Unclassified => Err(FsError::UnclassifiedWritePath),
            _ => Ok(class),
        }
    }
}
