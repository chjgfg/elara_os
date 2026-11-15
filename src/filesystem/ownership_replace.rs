//! A replacement intent changes exactly two inodes' name sets, never their data.
use super::*;
pub(super) enum Mutation {
    Single(Option<OwnedFileInfo>, Option<OwnedFileInfo>),
    Replace {
        source: OwnedFileInfo,
        victim: OwnedFileInfo,
    },
}
impl Mutation {
    pub(super) fn object(&self) -> Result<ObjectId, FsError> {
        match self {
            Self::Single(old, new) => new
                .as_ref()
                .or(old.as_ref())
                .map(|i| i.id)
                .ok_or(FsError::InvalidOwnershipRecord),
            Self::Replace { source, .. } => Ok(source.id),
        }
    }
}
pub(super) fn classify(intent: &Intent) -> Result<Mutation, FsError> {
    // A rebinding of an existing name is the discriminator, not a failed
    // single-object validation which might mask another malformed mutation.
    let rebound: Vec<_> = intent
        .old
        .entries
        .names
        .iter()
        .filter(|(name, id)| {
            intent
                .new
                .entries
                .names
                .get(*name)
                .is_some_and(|next| next != *id)
        })
        .collect();
    if rebound.is_empty() {
        let (old, new) = super::mutation(intent)?;
        return Ok(Mutation::Single(old, new));
    }
    if rebound.len() != 1 || intent.stage != "-" || intent.access.is_some() {
        return Err(FsError::InvalidOwnershipRecord);
    }
    let victim = intent
        .old
        .entries
        .get(rebound[0].0)
        .ok_or(FsError::InvalidOwnershipRecord)?;
    let replacement = intent
        .new
        .entries
        .get(rebound[0].0)
        .ok_or(FsError::InvalidOwnershipRecord)?;
    let removed: Vec<_> = intent
        .old
        .entries
        .values()
        .filter(|i| !intent.new.entries.contains_key(&i.path))
        .collect();
    if removed.len() != 1 {
        return Err(FsError::InvalidOwnershipRecord);
    }
    let source = removed[0].clone();
    if source.id != replacement.id || source.storage_class != victim.storage_class {
        return Err(FsError::InvalidOwnershipRecord);
    }
    let mut expected = intent.old.clone();
    expected.entries.remove(&source.path);
    expected.entries.remove(&victim.path);
    let mut moved = source.clone();
    moved.path = victim.path.clone();
    expected.entries.insert(moved.path.clone(), moved)?;
    expected.revision = expected
        .revision
        .checked_add(1)
        .ok_or(FsError::OwnershipIdentityExhausted)?;
    if expected.encode()? != intent.new.encode()? {
        return Err(FsError::InvalidOwnershipRecord);
    }
    Ok(Mutation::Replace { source, victim })
}
impl DataRoot {
    pub(in crate::filesystem) fn rename_replace_owned_with_fault(
        &self,
        user: &Account,
        source: &str,
        destination: &str,
        fault: OwnershipFault,
    ) -> Result<(), FsError> {
        let guard = self.namespace_guard()?;
        let source = self.resolve_host_locked(user, source, Access::Write, false, &guard)?;
        let destination =
            self.resolve_host_locked(user, destination, Access::Write, false, &guard)?;
        self.parent_mutation_locked(user, &source)?;
        self.parent_mutation_locked(user, &destination)?;
        self.resource_rename_locked(&source, &destination)?;
        if self.is_protected(&source)? || self.is_protected(&destination)? {
            return Err(FsError::ProtectedPath);
        }
        let store = self
            .existing_owner_store()?
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        let old = self.load_owner_catalog(&store)?;
        let source_info = old
            .entries
            .get(&key(&source)?)
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        self.verify_public_binding(&source_info, &guard)?;
        self.name_removal_locked(user, &source, source_info.owner)?;
        if storage_class(&destination)? != source_info.storage_class {
            return Err(FsError::OwnershipClassMismatch);
        }
        let mut new = old.clone();
        if let Some(victim) = old.entries.get(&key(&destination)?) {
            self.verify_public_binding(&victim, &guard)?;
            self.name_removal_locked(user, &destination, victim.owner)?;
            if source_info.id == victim.id {
                return Ok(());
            }
            new.entries.remove(&victim.path);
        } else {
            self.resolve_new(&destination)?;
        }
        new.entries.remove(&source_info.path);
        let mut moved = source_info;
        moved.path = key(&destination)?;
        new.entries.insert(moved.path.clone(), moved)?;
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
}

impl DataRoot {
    pub(super) fn finish_replace_intent(
        &self,
        store: &Dir,
        intent: &Intent,
        source: &OwnedFileInfo,
        victim: &OwnedFileInfo,
        guard: &NamespaceGuard<'_>,
        _fault: OwnershipFault,
    ) -> Result<CommitReceipt, FsError> {
        guard.require_root(self)?;
        self.verify_storage(source)?;
        let moved = intent
            .new
            .entries
            .get(&victim.path)
            .ok_or(FsError::InvalidOwnershipRecord)?;
        // Preflight surviving aliases before changing either public name.
        for info in intent
            .new
            .entries
            .values()
            .filter(|i| (i.id == source.id || i.id == victim.id) && i.path != victim.path)
        {
            self.verify_public_binding(&info, guard)?;
        }
        #[cfg(test)]
        if _fault == OwnershipFault::AfterStorage {
            return Err(io::Error::other("injected before replacement namespace").into());
        }
        let from = VirtualPath::strict(&source.path)
            .map_err(|_| FsError::InvalidPath)?
            .as_relative();
        let to = VirtualPath::strict(&victim.path)
            .map_err(|_| FsError::InvalidPath)?
            .as_relative();
        match self.dir.symlink_metadata(&from) {
            Ok(_) => {
                if self.load_owner_catalog(store)?.revision != intent.old.revision {
                    return Err(FsError::OwnershipRecoveryBlocked);
                }
                self.verify_public_binding(source, guard)?;
                self.verify_public_binding(victim, guard)?;
                // This is one native file rename, not unlink plus publication.
                self.dir
                    .rename(&from, &self.dir, &to)
                    .map_err(FsError::AtomicRenameUnavailable)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.verify_public_binding(&moved, guard)?;
            }
            Err(error) => return Err(error.into()),
        }
        self.verify_public_binding(&moved, guard)?;
        #[cfg(test)]
        if _fault == OwnershipFault::AfterPublicName {
            return Err(io::Error::other("injected after replacement namespace").into());
        }
        let receipt = self.publish_protocol_locked(
            &store_path().join("catalog"),
            &intent.new.encode()?,
            false,
            guard,
        )?;
        #[cfg(test)]
        if _fault == OwnershipFault::AfterCatalog {
            return Err(io::Error::other("injected after replacement catalog").into());
        }
        if !intent.new.entries.inodes.contains_key(&victim.id) {
            self.retire_owned_storage(victim, _fault)?;
            let previous = self.load_access_catalog()?;
            if previous.file_acl.contains_key(&victim.id.value()) {
                let mut next = previous.clone();
                next.file_acl.remove(&victim.id.value());
                next.bump()?;
                self.commit_policy_locked(previous, next, None, guard)?;
            }
        }
        store.remove_file("pending")?;
        Ok(receipt)
    }
}

#[cfg(test)]
#[path = "../../tests/filesystem/ownership_replace_model.rs"]
mod tests;
