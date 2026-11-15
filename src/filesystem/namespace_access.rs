//! Account-aware rename dispatch is naming authority, not target data access.
use super::{DataRoot, FsError, RenameReceipt};
use crate::permissions::Access;
use crate::users::Account;

/// Dispatch observations are not grants: every selected mutator reacquires the
/// namespace lock and revalidates its current object, parent policy and binding.
enum RenameTarget {
    ManagedFile,
    ManagedDirectory,
    Legacy,
}
impl DataRoot {
    pub(super) fn rename_replace_host_as(
        &self,
        user: &Account,
        source: &str,
        destination: &str,
    ) -> Result<(), FsError> {
        let directory = {
            let guard = self.namespace_guard()?;
            let path = self.resolve_host_locked(user, source, Access::Write, false, &guard)?;
            self.directory_at_locked(&path, &self.load_access_catalog()?)?
                .is_some()
        };
        // Classification grants no mutation authority: both branches re-resolve
        // and revalidate the namespace under their own guard.
        if directory {
            self.rename_directory_configured(user, source, destination, true)
                .map(|_| ())
        } else {
            self.rename_replace_owned_with_fault(
                user,
                source,
                destination,
                super::ownership::OwnershipFault::None,
            )
        }
    }

    pub fn rename_as(
        &self,
        user: &Account,
        source: &str,
        destination: &str,
    ) -> Result<RenameReceipt, FsError> {
        let a = self.mounted_name(user, source, Access::Write)?;
        let b = self.mounted_name(user, destination, Access::Write)?;
        if !std::matches!((&a, &b), (super::vfs::Route::Host, super::vfs::Route::Host)) {
            self.vfs
                .rename(&self.credentials_as(user)?, source, destination, false)?;
            let receipt = super::vfs_bridge::volatile_receipt(0);
            return Ok(RenameReceipt {
                source_directory: receipt.directory_durability,
                destination_directory: receipt.directory_durability,
            });
        }
        super::ownership::user_path(user, source, Access::Write)?;
        super::ownership::user_path(user, destination, Access::Write)?;
        let target = {
            let guard = self.namespace_guard()?;
            let resolved_source =
                self.resolve_host_locked(user, source, Access::Write, false, &guard)?;
            let source = resolved_source.as_str();
            let resolved_destination =
                self.resolve_host_locked(user, destination, Access::Write, false, &guard)?;
            let destination = resolved_destination.as_str();
            self.parent_mutation_locked(user, source)?;
            self.parent_mutation_locked(user, destination)?;
            let key = super::ownership_model::key(source)?;
            let catalog = self.load_access_catalog()?;
            if let Some(info) = self.directory_at_locked(source, &catalog)? {
                self.name_removal_locked(user, &info.path, info.owner)?;
                RenameTarget::ManagedDirectory
            } else if let Some(store) = self.existing_owner_store()? {
                let catalog = self.load_owner_catalog(&store)?;
                if let Some(info) = catalog.entries.get(&key) {
                    self.name_removal_locked(user, &info.path, info.owner)?;
                    self.verify_public_binding(&info, &guard)?;
                    RenameTarget::ManagedFile
                } else {
                    RenameTarget::Legacy
                }
            } else {
                RenameTarget::Legacy
            }
        };
        match target {
            RenameTarget::ManagedFile => self.rename_file_as(user, source, destination),
            RenameTarget::ManagedDirectory => self
                .rename_directory_as(user, source, destination)
                .map(|r| RenameReceipt {
                    source_directory: r.directory_durability,
                    destination_directory: r.directory_durability,
                }),
            // The raw legacy mutator refuses newly managed names/ancestors and
            // descendants, so a changed classification never grants a bypass.
            RenameTarget::Legacy => self.rename(source, destination),
        }
    }
}
