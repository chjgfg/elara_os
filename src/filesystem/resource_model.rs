//! Persistent administrative application/cache ownership, never path adoption.
use super::{access_model::AccessCatalog, DeleteCondition, FsError};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResourceId(pub(super) u64);
impl ResourceId {
    pub fn value(self) -> u64 {
        self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceKind {
    Application,
    Cache,
    ApplicationData,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceLimits {
    pub bytes: u64,
    pub objects: u64,
}
impl ResourceLimits {
    pub fn validate(self) -> Result<(), FsError> {
        if self.bytes == 0
            || self.bytes > 64 * 1024 * 1024
            || self.objects == 0
            || self.objects > 4096
        {
            return Err(FsError::ResourceQuota);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceState {
    Active,
    Removing,
    Evicting,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceAssociation {
    #[default]
    Independent,
    Application(ResourceId),
}
impl ResourceAssociation {
    pub fn is_independent(&self) -> bool {
        *self == Self::Independent
    }
}
pub(super) enum CreationResource {
    Ordinary,
    Registered(DeleteCondition),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRecord {
    pub id: ResourceId,
    /// Monotonic grant revision. Zero preserves legacy catalog bytes.
    #[serde(default, skip_serializing_if = "generation_is_zero")]
    pub generation: u64,
    pub path: String,
    pub owner: u32,
    pub kind: ResourceKind,
    pub limits: ResourceLimits,
    pub state: ResourceState,
    #[serde(default, skip_serializing_if = "ResourceAssociation::is_independent")]
    pub association: ResourceAssociation,
}
fn generation_is_zero(value: &u64) -> bool { *value == 0 }

/// All policy/state changes go through the same namespace transaction boundary.
/// File IO does not change grants; restoring old policy still advances its epoch.
pub(super) fn advance_generations(old: &AccessCatalog, new: &mut AccessCatalog) -> Result<(), FsError> {
    for (id, record) in &mut new.resources {
        if let Some(previous) = old.resources.get(id) {
            if record != previous {
                record.generation = previous.generation.checked_add(1)
                    .ok_or(FsError::OwnershipIdentityExhausted)?;
            }
        }
    }
    Ok(())
}
pub(super) fn within(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|tail| tail.starts_with('/'))
}
pub(super) fn kind_at(path: &str) -> Option<ResourceKind> {
    match crate::config::write_class(path) {
        crate::config::WriteClass::Application => Some(ResourceKind::Application),
        crate::config::WriteClass::Cache => Some(ResourceKind::Cache),
        _ => None,
    }
}
pub(super) fn matches_path(kind: ResourceKind, path: &str) -> bool {
    if kind == ResourceKind::ApplicationData {
        crate::config::write_class(path) == crate::config::WriteClass::UserData
            && (path.starts_with("/root/")
                || path
                    .strip_prefix("/home/")
                    .is_some_and(|tail| tail.contains('/')))
    } else {
        kind_at(path) == Some(kind)
    }
}
pub(super) fn validate(catalog: &AccessCatalog) -> Result<(), FsError> {
    if catalog.resources.len() > 64 {
        return Err(FsError::OwnershipCapacity);
    }
    for (id, record) in &catalog.resources {
        record.limits.validate()?;
        if *id != record.id
            || id.0 == 0
            || id.0 >= catalog.next_dir
            || record.owner != 0
            || !matches_path(record.kind, &record.path)
            || super::ownership_model::key(&record.path)? != record.path
            || record.state == ResourceState::Evicting && record.kind != ResourceKind::Cache
        {
            return Err(FsError::InvalidOwnershipRecord);
        }
        match record.association {
            ResourceAssociation::Independent if record.kind == ResourceKind::ApplicationData => {
                return Err(FsError::InvalidOwnershipRecord);
            }
            ResourceAssociation::Application(parent) => {
                let app = catalog
                    .resources
                    .get(&parent)
                    .ok_or(FsError::InvalidOwnershipRecord)?;
                if record.kind == ResourceKind::Application
                    || app.kind != ResourceKind::Application
                    || app.association != ResourceAssociation::Independent
                    || app.state == ResourceState::Removing
                        && record.state != ResourceState::Removing
                {
                    return Err(FsError::InvalidOwnershipRecord);
                }
            }
            ResourceAssociation::Independent => {}
        }
        match catalog.directories.get(&record.path) {
            Some(dir)
                if dir.id == id.0
                    && dir.owner.uid == record.owner
                    && dir.delete_condition == DeleteCondition::ResourceDelete(*id) => {}
            None if record.state == ResourceState::Removing => {}
            _ => return Err(FsError::InvalidOwnershipRecord),
        }
        if catalog
            .resources
            .values()
            .any(|other| other.id != *id && within(&record.path, &other.path))
        {
            return Err(FsError::InvalidOwnershipRecord);
        }
    }
    for dir in catalog.directories.values() {
        if let DeleteCondition::ResourceDelete(id) = dir.delete_condition {
            let record = catalog
                .resources
                .get(&id)
                .ok_or(FsError::InvalidOwnershipRecord)?;
            if !within(&dir.path, &record.path) || dir.owner.uid != record.owner {
                return Err(FsError::InvalidOwnershipRecord);
            }
        }
    }
    Ok(())
}
