//! Virtual links are registered inodes, never native host symlinks/reparse points.
//! Resolution and the caller's operation share one namespace guard.
use super::{ownership::user_path, ownership_model::key, *};
use crate::{permissions::Access, users::Account, virtual_path::VirtualPath};
use std::{collections::VecDeque, format, matches};

pub(super) fn validate_target(target: &str) -> Result<(), FsError> {
    if target.is_empty()
        || target.len() > 4096
        || target.contains([char::from(92), ':'])
        || target.chars().any(char::is_control)
    {
        return Err(FsError::InvalidPath);
    }
    for part in target
        .split('/')
        .filter(|s| !s.is_empty() && !matches!(*s, "." | ".."))
    {
        VirtualPath::strict(part).map_err(|_| FsError::InvalidPath)?;
    }
    Ok(())
}
impl DataRoot {
    pub(super) fn resolve_host_locked(
        &self,
        actor: &Account,
        path: &str,
        access: Access,
        follow: bool,
        guard: &super::transaction::NamespaceGuard<'_>,
    ) -> Result<String, FsError> {
        guard.require_root(self)?;
        let path = user_path(actor, path, access)?.to_string();
        if path.len() > 4096 {
            return Err(FsError::InvalidPath);
        }
        let catalog = match self.existing_owner_store()? {
            Some(store) => Some(self.load_owner_catalog(&store)?),
            None => None,
        };
        let permissions = self.load_access_catalog()?;
        let mut pending: VecDeque<String> = path
            .split('/')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        let mut parts = Vec::<String>::new();
        let mut links = 0;
        while let Some(part) = pending.pop_front() {
            if part == "." {
                continue;
            }
            if part == ".." {
                parts.pop().ok_or(FsError::InvalidPath)?;
                continue;
            }
            let candidate = format!("/{}/{}", parts.join("/"), part);
            let candidate = VirtualPath::strict(&candidate)
                .map_err(|_| FsError::InvalidPath)?
                .to_string();
            if candidate.len() > 4096 {
                return Err(FsError::InvalidPath);
            }
            // Ancestors need search/read visibility, not write authority on /home.
            user_path(actor, &candidate, Access::Read)?;
            if !matches!(self.vfs.route(&candidate)?, vfs::Route::Host) {
                return Err(vfs::Error::CrossDevice.into());
            }
            self.search_locked(actor, &candidate, &permissions)?;
            if let Some(info) = catalog
                .as_ref()
                .and_then(|c| c.entries.get(&key(&candidate).ok()?))
            {
                if let HostFileKind::Symlink { target } = &info.kind {
                    self.verify_public_binding(&info, guard)?;
                    if follow || !pending.is_empty() {
                        links += 1;
                        if links > 40 {
                            return Err(vfs::Error::LinkLoop.into());
                        }
                        validate_target(target)?;
                        if target.starts_with('/') {
                            parts.clear();
                        }
                        if target.ends_with('/') {
                            pending.push_front(".".to_owned());
                        }
                        for segment in target.split('/').filter(|s| !s.is_empty()).rev() {
                            pending.push_front(segment.to_owned());
                        }
                        if pending.len() > 4096 {
                            return Err(FsError::InvalidPath);
                        }
                        continue;
                    }
                }
            }
            if !pending.is_empty() {
                let relative = self.resolve(&candidate)?;
                if !self.dir.metadata(relative)?.is_dir() {
                    return Err(FsError::NotDirectory);
                }
                if let Some(info) = self.directory_at_locked(&candidate, &permissions)? {
                    self.directory_access_locked(actor, &info, 1, &permissions)?;
                }
            }
            parts.push(part);
        }
        let result = format!("/{}", parts.join("/"));
        user_path(actor, &result, access)?;
        Ok(result)
    }
    pub(super) fn create_host_symlink(
        &self,
        actor: &Account,
        target: &str,
        path: &str,
    ) -> Result<(), FsError> {
        validate_target(target)?;
        let guard = self.namespace_guard()?;
        self.create_owned_node_configured_locked(
            actor,
            path,
            target.as_bytes(),
            (
                super::access_model::CreationMode::Exact(VirtualMode::new(0o777)?),
                super::ownership::OwnershipFault::None,
                CreationOptions::default(),
                HostFileKind::Symlink {
                    target: target.to_owned(),
                },
            ),
            &guard,
        )?;
        Ok(())
    }
    pub(super) fn read_host_symlink(&self, actor: &Account, path: &str) -> Result<String, FsError> {
        let guard = self.namespace_guard()?;
        let path = self.resolve_host_locked(actor, path, Access::Read, false, &guard)?;
        let store = self
            .existing_owner_store()?
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        let info = self
            .load_owner_catalog(&store)?
            .entries
            .get(&key(&path)?)
            .ok_or(FsError::LegacyOwnershipUnknown)?;
        self.verify_public_binding(&info, &guard)?;
        match info.kind {
            HostFileKind::Symlink { target } => Ok(target),
            HostFileKind::Regular => Err(FsError::InvalidPath),
        }
    }
}
