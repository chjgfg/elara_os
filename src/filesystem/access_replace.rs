//! Directory replacement retires only a verified empty destination. The durable
//! joint intent covers the gap between retirement and source publication.
use super::*;
impl DataRoot {
    pub(super) fn require_empty_owned_directory(&self, path: &str) -> Result<(), FsError> {
        for entry in self.dir.read_dir(virtual_relative(path)?)? {
            if entry?.file_name() != DIRECTORY_MARKER {
                return Err(FsError::DirectoryNotEmpty);
            }
        }
        Ok(())
    }
    pub(super) fn prepare_directory_replace(
        &self,
        intent: &Intent,
        replaced: u64,
        guard: &NamespaceGuard<'_>,
        fault: AccessFault,
    ) -> Result<(), FsError> {
        guard.require_root(self)?;
        let Change::RenameReplace {
            source,
            destination,
            ..
        } = &intent.change
        else {
            return Err(FsError::InvalidOwnershipRecord);
        };
        let moved = intent
            .new
            .directories
            .get(destination)
            .ok_or(FsError::InvalidOwnershipRecord)?;
        let victim = intent
            .old
            .directories
            .get(destination)
            .ok_or(FsError::InvalidOwnershipRecord)?;
        if victim.id != replaced {
            return Err(FsError::InvalidOwnershipRecord);
        }
        if self.verify_directory_unlocked(&moved).is_ok() {
            return Ok(());
        }
        // Validate the entire source subtree before removing any destination name.
        self.verify_directory_replacement_contents(intent, false, guard)?;
        let target = virtual_relative(destination)?;
        let retired = retired(replaced);
        match self.dir.symlink_metadata(&target) {
            Ok(_) => {
                self.verify_directory_unlocked(&victim)?;
                self.require_empty_owned_directory(destination)?;
                match self.dir.symlink_metadata(&retired) {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                    Ok(_) => return Err(FsError::OwnershipRecoveryBlocked),
                }
                self.dir
                    .rename(&target, &self.dir, &retired)
                    .map_err(FsError::AtomicRenameUnavailable)?;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.inspect_relative(&retired)?;
        self.compare_directory_markers(
            &retired.join(DIRECTORY_MARKER),
            &directory_owner(replaced),
        )?;
        for entry in self.dir.read_dir(&retired)? {
            if entry?.file_name() != DIRECTORY_MARKER {
                return Err(FsError::DirectoryNotEmpty);
            }
        }
        #[cfg(test)]
        if fault == AccessFault::AfterReplaceRetirement {
            return Err(io::Error::other(
                "injected between destination retirement and source rename",
            )
            .into());
        }
        let _ = (source, fault);
        Ok(())
    }
    pub(super) fn verify_directory_replacement_contents(
        &self,
        intent: &Intent,
        after: bool,
        guard: &NamespaceGuard<'_>,
    ) -> Result<(), FsError> {
        let Change::RenameReplace {
            source,
            destination,
            ..
        } = &intent.change
        else {
            return Ok(());
        };
        let (catalog, prefix) = if after {
            (&intent.new, destination)
        } else {
            (&intent.old, source)
        };
        for info in catalog
            .directories
            .values()
            .filter(|d| within(&d.path, prefix))
        {
            self.verify_directory_unlocked(&info)?;
        }
        let (old, new) = intent
            .files
            .as_ref()
            .ok_or(FsError::InvalidOwnershipRecord)?;
        let files = Catalog::decode(if after {
            new.as_bytes()
        } else {
            old.as_bytes()
        })?;
        for info in files.entries.values().filter(|i| within(&i.path, prefix)) {
            self.verify_public_binding(&info, guard)?;
        }
        Ok(())
    }
}
