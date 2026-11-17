//! Interpreter application domains. No host accounts, processes or mounts.
use super::{access_model::AccessCatalog, DataRoot, FsError, ResourceAssociation,
    ResourceId, ResourceKind, ResourceRecord, ResourceState};
use crate::{permissions::Access, users::{Account, ExecutionIdentity, Identity}};
use std::sync::Arc;
use std::{format, vec};

/// Unforgeable through the public API; equality includes the granted root family.
#[derive(Clone, Debug)]
pub struct ApplicationDomain {
    family: Arc<()>,
    application: ResourceId,
    locations: Vec<ResourceRecord>,
}
impl PartialEq for ApplicationDomain {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.family, &other.family)
            && self.application == other.application && self.locations == other.locations
    }
}
impl Eq for ApplicationDomain {}
impl ApplicationDomain {
    pub fn id(&self) -> ResourceId { self.application }
    pub fn home(&self) -> &str { &self.locations[0].path }
    pub(crate) fn permits(&self, path: &str, access: Access, search: bool) -> bool {
        let Ok(path) = super::ownership_model::key(path) else { return false; };
        self.locations.iter().any(|location| {
            let inside = super::resource_model::within(&path, &location.path);
            (inside && (access == Access::Read || location.kind != ResourceKind::Application))
                || (search && (path == "/" || super::resource_model::within(&location.path, &path)))
        })
    }
}
impl DataRoot {
    /// Mint a bounded domain from active, already registered resources only.
    /// A later resource change revokes this snapshot; it never expands a live grant.
    pub fn application_account_as(&self, actor: &Account, path: &str) -> Result<Account, FsError> {
        if !actor.is_admin() { return Err(FsError::OwnershipDenied); }
        let _guard = self.namespace_guard()?;
        let catalog = self.load_access_catalog()?;
        let path = super::ownership_model::key(path)?;
        let app = catalog.resources.values().find(|r| r.path == path
            && r.kind == ResourceKind::Application).ok_or(FsError::NotFound)?;
        if app.state != ResourceState::Active { return Err(FsError::ResourceInactive); }
        let mut locations = vec![app.clone()];
        for location in catalog.resources.values().filter(|r|
            r.association == ResourceAssociation::Application(app.id)) {
            if location.state != ResourceState::Active { return Err(FsError::ResourceInactive); }
            locations.push(location.clone());
        }
        let credentials = catalog.actor_credentials(actor);
        let mut supplementary = credentials.groups;
        supplementary.remove(&credentials.gid);
        let domain = ApplicationDomain { family: self.family.clone(), application: app.id, locations };
        Ok(Account {
            name: format!("app-{}", app.id.value()), uid: actor.uid, home: path,
            identity: Identity::Application {
                credentials: ExecutionIdentity { real_uid: actor.session_uid(), real_gid: credentials.gid,
                    effective_gid: credentials.gid, saved_uid: actor.uid, saved_gid: credentials.gid, supplementary },
                domain,
            },
        })
    }
    pub(crate) fn validate_application(&self, actor: &Account) -> Result<(), FsError> {
        if actor.application_domain().is_none() { return Ok(()); }
        let _guard = self.namespace_guard()?;
        self.validate_application_locked(actor, &self.load_access_catalog()?)
    }
    pub(super) fn validate_application_locked(&self, actor: &Account, catalog: &AccessCatalog) -> Result<(), FsError> {
        let Some(domain) = actor.application_domain() else { return Ok(()); };
        if !Arc::ptr_eq(&self.family, &domain.family) { return Err(FsError::InvalidRoot); }
        for location in &domain.locations {
            if catalog.resources.get(&location.id) != Some(location) || location.state != ResourceState::Active {
                return Err(FsError::ResourceInactive);
            }
            let info = self.directory_at_locked(&location.path, catalog)?.ok_or(FsError::ResourceInactive)?;
            if info.id != location.id.value() { return Err(FsError::ResourceInactive); }
            // An ID in the catalog is not proof that the public directory still
            // names that resource. Reuse the existing marker identity gate.
            self.directory_access_locked(actor, &info, 1, catalog)?;
        }
        Ok(())
    }
}
