//! Durable registration and exact reclaim of administrative application/cache data.
//! All callers with a `_locked` suffix already hold the namespace transaction gate.
use super::{
    access_model::AccessCatalog,
    ownership::payload,
    resource_model::{kind_at, matches_path, within, CreationResource},
    *,
};
use crate::users::Account;
use std::matches;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResourceUsage {
    pub bytes: u64,
    pub objects: u64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResourceRecovery {
    pub resources: usize,
    pub files: usize,
    pub directories: usize,
    pub deferred: usize,
}
fn admin(actor: &Account) -> Result<(), FsError> {
    if actor.uid == 0 {
        Ok(())
    } else {
        Err(FsError::OwnershipDenied)
    }
}
fn lookup<'a>(catalog: &'a AccessCatalog, path: &str) -> Result<&'a ResourceRecord, FsError> {
    let path = super::ownership_model::key(path)?;
    catalog
        .resources
        .values()
        .find(|r| r.path == path)
        .ok_or(FsError::NotFound)
}
fn active(record: &ResourceRecord) -> Result<(), FsError> {
    if record.state == ResourceState::Active {
        Ok(())
    } else {
        Err(FsError::ResourceInactive)
    }
}
fn bound(record: &ResourceRecord, info: &OwnedFileInfo) -> Result<(), FsError> {
    if within(&info.path, &record.path)
        && info.owner.uid == record.owner
        && matches_path(record.kind, &info.path)
    {
        Ok(())
    } else {
        Err(FsError::InvalidOwnershipRecord)
    }
}
impl DataRoot {
    pub fn register_resource_as(
        &self,
        actor: &Account,
        path: &str,
        kind: ResourceKind,
        limits: ResourceLimits,
    ) -> Result<CommitReceipt, FsError> {
        admin(actor)?;
        limits.validate()?;
        if kind_at(path) != Some(kind) {
            return Err(FsError::OwnershipClassMismatch);
        }
        if !matches!(self.vfs.route(path)?, vfs::Route::Host) {
            return Err(vfs::Error::Unsupported.into());
        }
        self.create_directory_registered_as(
            actor,
            path,
            CreationOptions::default(),
            Some((kind, limits, None)),
        )
    }
    pub fn register_application_location_as(
        &self,
        actor: &Account,
        application: &str,
        path: &str,
        kind: ResourceKind,
        limits: ResourceLimits,
    ) -> Result<CommitReceipt, FsError> {
        admin(actor)?;
        limits.validate()?;
        let canonical = super::ownership_model::key(path)?;
        if !matches!(kind, ResourceKind::Cache | ResourceKind::ApplicationData)
            || !matches_path(kind, &canonical)
        {
            return Err(FsError::OwnershipClassMismatch);
        }
        self.create_directory_registered_as(
            actor,
            path,
            CreationOptions::default(),
            Some((kind, limits, Some(application))),
        )
    }
    pub fn application_locations_as(
        &self,
        actor: &Account,
        application: &str,
    ) -> Result<Vec<ResourceRecord>, FsError> {
        admin(actor)?;
        let _guard = self.namespace_guard()?;
        let catalog = self.load_access_catalog()?;
        let app = lookup(&catalog, application)?;
        if app.kind != ResourceKind::Application {
            return Err(FsError::OwnershipClassMismatch);
        }
        Ok(catalog
            .resources
            .values()
            .filter(|r| r.association == ResourceAssociation::Application(app.id))
            .cloned()
            .collect())
    }
    pub fn resources_as(&self, actor: &Account) -> Result<Vec<ResourceRecord>, FsError> {
        admin(actor)?;
        let _guard = self.namespace_guard()?;
        Ok(self
            .load_access_catalog()?
            .resources
            .into_values()
            .collect())
    }
    pub fn resource_info_as(
        &self,
        actor: &Account,
        path: &str,
    ) -> Result<(ResourceRecord, ResourceUsage), FsError> {
        admin(actor)?;
        let _guard = self.namespace_guard()?;
        let catalog = self.load_access_catalog()?;
        let record = lookup(&catalog, path)?.clone();
        let usage = self.resource_usage_locked(&catalog, &record)?;
        Ok((record, usage))
    }
    pub fn resource_quota_as(
        &self,
        actor: &Account,
        path: &str,
        limits: ResourceLimits,
    ) -> Result<CommitReceipt, FsError> {
        admin(actor)?;
        limits.validate()?;
        let guard = self.namespace_guard()?;
        let old = self.load_access_catalog()?;
        let record = lookup(&old, path)?;
        active(record)?;
        let usage = self.resource_usage_locked(&old, record)?;
        if usage.bytes > limits.bytes || usage.objects > limits.objects {
            return Err(FsError::ResourceQuota);
        }
        let mut new = old.clone();
        new.resources
            .get_mut(&record.id)
            .ok_or(FsError::NotFound)?
            .limits = limits;
        new.bump()?;
        self.commit_policy_locked(old, new, None, &guard)
    }
    pub(super) fn resource_condition_locked(
        &self,
        path: &str,
    ) -> Result<CreationResource, FsError> {
        let catalog = self.load_access_catalog()?;
        let path = super::ownership_model::key(path)?;
        match catalog.resources.values().find(|r| within(&path, &r.path)) {
            Some(record) => {
                active(record)?;
                Ok(CreationResource::Registered(
                    DeleteCondition::ResourceDelete(record.id),
                ))
            }
            None if kind_at(&path).is_some() => Err(FsError::OwnershipContextRequired),
            None => Ok(CreationResource::Ordinary),
        }
    }
    fn resource_usage_locked(
        &self,
        access: &AccessCatalog,
        record: &ResourceRecord,
    ) -> Result<ResourceUsage, FsError> {
        let mut usage = ResourceUsage {
            objects: access
                .directories
                .values()
                .filter(|d| d.delete_condition == DeleteCondition::ResourceDelete(record.id))
                .count() as u64,
            bytes: 0,
        };
        let mut counted = std::collections::BTreeSet::new();
        if let Some(store) = self.existing_owner_store()? {
            for info in self
                .load_owner_catalog(&store)?
                .entries
                .values()
                .filter(|f| f.delete_condition == DeleteCondition::ResourceDelete(record.id))
            {
                bound(record, &info)?;
                if !counted.insert(info.id) { continue; }
                let metadata = self.dir.symlink_metadata(payload(&info))?;
                if super::backend::linked(&metadata) || !metadata.is_file() {
                    return Err(FsError::ResourceDetached);
                }
                usage.bytes = usage
                    .bytes
                    .checked_add(metadata.len())
                    .ok_or(FsError::ResourceQuota)?;
                usage.objects += 1;
            }
        }
        Ok(usage)
    }
    pub(super) fn check_resource_directory_locked(
        &self,
        condition: DeleteCondition,
    ) -> Result<(), FsError> {
        let DeleteCondition::ResourceDelete(id) = condition else {
            return Ok(());
        };
        let access = self.load_access_catalog()?;
        let record = access.resources.get(&id).ok_or(FsError::ResourceDetached)?;
        active(record)?;
        if self.resource_usage_locked(&access, record)?.objects >= record.limits.objects {
            return Err(FsError::ResourceQuota);
        }
        Ok(())
    }
    pub(super) fn check_resource_write_locked(
        &self,
        info: &OwnedFileInfo,
        length: u64,
        creating: bool,
    ) -> Result<(), FsError> {
        let DeleteCondition::ResourceDelete(id) = info.delete_condition else {
            return Ok(());
        };
        let access = self.load_access_catalog()?;
        let record = access.resources.get(&id).ok_or(FsError::ResourceDetached)?;
        active(record)?;
        bound(record, info)?;
        let mut usage = self.resource_usage_locked(&access, record)?;
        if creating {
            usage.objects += 1;
        } else {
            let store = self
                .existing_owner_store()?
                .ok_or(FsError::ResourceDetached)?;
            let files = self.load_owner_catalog(&store)?;
            let current = files
                .entries
                .values()
                .find(|f| f.id == info.id && f.storage_generation == info.storage_generation)
                .ok_or(FsError::ResourceDetached)?;
            if current.delete_condition != info.delete_condition {
                return Err(FsError::ResourceDetached);
            }
            bound(record, &current)?;
            usage.bytes = usage
                .bytes
                .checked_sub(self.dir.metadata(payload(&current))?.len())
                .ok_or(FsError::ResourceDetached)?;
        }
        usage.bytes = usage
            .bytes
            .checked_add(length)
            .ok_or(FsError::ResourceQuota)?;
        if usage.bytes > record.limits.bytes || usage.objects > record.limits.objects {
            return Err(FsError::ResourceQuota);
        }
        Ok(())
    }
    pub(super) fn resource_rename_locked(
        &self,
        source: &str,
        destination: &str,
    ) -> Result<(), FsError> {
        let access = self.load_access_catalog()?;
        let source = super::ownership_model::key(source)?;
        let destination = super::ownership_model::key(destination)?;
        // Moving an ordinary ancestor must not strand a registered location.
        if access.resources.values().any(|r| within(&r.path, &source)) {
            return Err(FsError::ServiceOwnedPath);
        }
        let from = access.resources.values().find(|r| within(&source, &r.path));
        let to = access
            .resources
            .values()
            .find(|r| within(&destination, &r.path));
        if from.map(|r| r.id) != to.map(|r| r.id) {
            return Err(FsError::OwnershipClassMismatch);
        }
        if let Some(record) = from {
            active(record)?;
        }
        Ok(())
    }
    pub(super) fn resource_directory_removal_locked(
        &self,
        info: &DirectoryInfo,
        catalog: &AccessCatalog,
    ) -> Result<(), FsError> {
        if let DeleteCondition::ResourceDelete(id) = info.delete_condition {
            let record = catalog
                .resources
                .get(&id)
                .ok_or(FsError::ResourceDetached)?;
            if info.path == record.path && record.state != ResourceState::Removing {
                return Err(FsError::ServiceOwnedPath);
            }
        }
        Ok(())
    }
    pub fn remove_resource_as(
        &self,
        actor: &Account,
        path: &str,
    ) -> Result<ResourceRecovery, FsError> {
        self.change_resource_state_as(actor, path, ResourceState::Removing)
    }
    pub fn clear_cache_as(&self, actor: &Account, path: &str) -> Result<ResourceRecovery, FsError> {
        self.change_resource_state_as(actor, path, ResourceState::Evicting)
    }
    fn change_resource_state_as(
        &self,
        actor: &Account,
        path: &str,
        state: ResourceState,
    ) -> Result<ResourceRecovery, FsError> {
        admin(actor)?;
        let guard = self.namespace_guard()?;
        let old = self.load_access_catalog()?;
        let record = lookup(&old, path)?;
        if state == ResourceState::Evicting && record.kind != ResourceKind::Cache {
            return Err(FsError::OwnershipClassMismatch);
        }
        if record.state != ResourceState::Active && record.state != state {
            return Err(FsError::ResourceInactive);
        }
        let id = record.id;
        if record.state != state {
            let mut new = old.clone();
            new.resources.get_mut(&id).ok_or(FsError::NotFound)?.state = state;
            if record.kind == ResourceKind::Application && state == ResourceState::Removing {
                for child in new
                    .resources
                    .values_mut()
                    .filter(|r| r.association == ResourceAssociation::Application(id))
                {
                    child.state = ResourceState::Removing;
                }
            }
            new.bump()?;
            self.commit_policy_locked(old, new, None, &guard)?;
        }
        self.reap_resource_group_locked(id, &guard)
    }
    pub fn recover_resources(&self) -> Result<ResourceRecovery, FsError> {
        let guard = self.namespace_guard()?;
        let catalog = self.load_access_catalog()?;
        let mut result = ResourceRecovery::default();
        for record in catalog
            .resources
            .values()
            .filter(|r| r.state != ResourceState::Active)
            .filter(|r| !matches!(r.association, ResourceAssociation::Application(parent)
                if catalog.resources.get(&parent).is_some_and(|app| app.state == ResourceState::Removing)))
        {
            if !self
                .load_access_catalog()?
                .resources
                .contains_key(&record.id)
            {
                continue;
            }
            let report = self.reap_resource_group_locked(record.id, &guard)?;
            result.resources += report.resources;
            result.files += report.files;
            result.directories += report.directories;
            result.deferred += report.deferred;
        }
        Ok(result)
    }
    fn reap_resource_group_locked(
        &self,
        id: ResourceId,
        guard: &super::transaction::NamespaceGuard<'_>,
    ) -> Result<ResourceRecovery, FsError> {
        let catalog = self.load_access_catalog()?;
        let mut result = ResourceRecovery::default();
        for child in catalog
            .resources
            .values()
            .filter(|r| r.association == ResourceAssociation::Application(id))
        {
            let report = self.reap_resource_locked(child.id, guard)?;
            result.resources += report.resources;
            result.files += report.files;
            result.directories += report.directories;
            result.deferred += report.deferred;
        }
        if result.deferred == 0 {
            let report = self.reap_resource_locked(id, guard)?;
            result.resources += report.resources;
            result.files += report.files;
            result.directories += report.directories;
            result.deferred += report.deferred;
        }
        Ok(result)
    }
    fn reap_resource_locked(
        &self,
        id: ResourceId,
        guard: &super::transaction::NamespaceGuard<'_>,
    ) -> Result<ResourceRecovery, FsError> {
        let catalog = self.load_access_catalog()?;
        let record = catalog.resources.get(&id).ok_or(FsError::NotFound)?;
        if record.state == ResourceState::Active {
            return Err(FsError::ResourceInactive);
        }
        let reaper = Account {
            identity: Default::default(),
            uid: 0,
            name: "resource-reaper".into(),
            home: "/root".into(),
        };
        let mut result = ResourceRecovery::default();
        if let Some(store) = self.existing_owner_store()? {
            let files = self.load_owner_catalog(&store)?;
            for info in files
                .entries
                .values()
                .filter(|f| f.delete_condition == DeleteCondition::ResourceDelete(id))
            {
                bound(record, &info)?;
                if self
                    .remove_owned_file_locked(&reaper, &info.path, guard)
                    .is_ok()
                {
                    result.files += 1;
                } else {
                    result.deferred += 1;
                }
            }
        }
        let mut directories: Vec<_> = catalog
            .directories
            .values()
            .filter(|d| {
                d.delete_condition == DeleteCondition::ResourceDelete(id)
                    && (record.state == ResourceState::Removing || d.path != record.path)
            })
            .collect();
        directories.sort_by(|a, b| b.path.len().cmp(&a.path.len()));
        for info in directories {
            if self
                .remove_directory_locked(&info.path, &reaper, guard)
                .is_ok()
            {
                result.directories += 1;
            } else {
                result.deferred += 1;
            }
        }
        if result.deferred == 0 {
            let old = self.load_access_catalog()?;
            let mut new = old.clone();
            if record.state == ResourceState::Removing {
                new.resources.remove(&id);
            } else {
                new.resources.get_mut(&id).ok_or(FsError::NotFound)?.state = ResourceState::Active;
            }
            new.bump()?;
            self.commit_policy_locked(old, new, None, guard)?;
            result.resources = 1;
        }
        Ok(result)
    }
}
