//! One link budget across Host and mounted volumes. Host results are re-walked
//! under the mutator's guard; dispatch observations never grant mutation authority.
use super::{host_symlinks::validate_target, ownership::user_path, ownership_model::key,
    transaction::NamespaceGuard, vfs::{self, Route}, *};
use crate::{permissions::Access, users::Account, virtual_path::VirtualPath};
use std::{collections::VecDeque, format, matches};

pub(super) struct ResolvedPath {
    pub path: String,
    pub route: Route,
}
impl DataRoot {
    pub(super) fn resolve_namespace_locked(
        &self, actor: &Account, path: &str, access: Access, follow: bool,
        guard: &NamespaceGuard<'_>,
    ) -> Result<ResolvedPath, FsError> {
        guard.require_root(self)?;
        let path = user_path(actor, path, access)?.to_string();
        if path.len() > 4096 { return Err(FsError::InvalidPath); }
        let permissions = self.load_access_catalog()?;
        let credentials = permissions.actor_credentials(actor);
        let mut catalog = None;
        let mut catalog_loaded = false;
        let mut pins = Vec::new();
        let (mut current, remaining) = match self.vfs.route_as(&credentials, &path)? {
            Route::Host => (String::from("/"), path.clone()),
            Route::Mounted { volume, path: relative, mount } => {
                if matches!(access, Access::Write) { self.ensure_write_class(&path)?; }
                pins.push(volume);
                (mount, relative)
            }
        };
        let mut pending: VecDeque<String> = remaining.split('/').filter(|s| !s.is_empty()).map(str::to_owned).collect();
        let mut links = 0;
        let mut refreshed = [false; 3];
        self.refresh_resolution_providers(&current, &mut refreshed, guard)?;
        while let Some(name) = pending.pop_front() {
            // Check before dot reduction: file/../x and denied/../x never work.
            let boundary = match self.vfs.route_as(&credentials, &current)? {
                Route::Host => {
                    self.search_locked(actor, &current, &permissions)?;
                    self.validate_directory_unchecked(&current)?;
                    if let Some(info) = self.directory_at_locked(&current, &permissions)? {
                        self.directory_access_locked(actor, &info, 1, &permissions)?;
                    }
                    String::from("/")
                }
                Route::Mounted { volume, path, mount } => {
                    volume.search_directory(&credentials, &path)?;
                    pins.push(volume);
                    mount
                }
            };
            if name == "." { continue; }
            if name == ".." {
                if current == boundary { return Err(vfs::Error::InvalidPath.into()); }
                current.truncate(current.rfind('/').ok_or(FsError::InvalidPath)?);
                if current.is_empty() { current.push('/'); }
                continue;
            }
            let candidate = format!("{}/{name}", current.trim_end_matches('/'));
            VirtualPath::strict(&candidate).map_err(|_| FsError::InvalidPath)?;
            if candidate.len() > 4096 { return Err(FsError::InvalidPath); }
            user_path(actor, &candidate, Access::Read)?;
            self.refresh_resolution_providers(&candidate, &mut refreshed, guard)?;
            let route = self.vfs.route_as(&credentials, &candidate)?;
            let link = match &route {
                Route::Host => {
                    if !catalog_loaded {
                        catalog = match self.existing_owner_store()? {
                            Some(store) => Some(self.load_owner_catalog(&store)?),
                            None => None,
                        };
                        catalog_loaded = true;
                    }
                    match catalog.as_ref().and_then(|c| c.entries.get(&key(&candidate).ok()?)) {
                        Some(info) => match &info.kind {
                            HostFileKind::Symlink { target } => {
                                self.verify_public_binding(&info, guard)?;
                                Some((target.clone(), String::from("/")))
                            }
                            HostFileKind::Regular => None,
                        },
                        None => None,
                    }
                }
                Route::Mounted { volume, path, mount } => {
                    pins.push(volume.clone());
                    if matches!(access, Access::Write) { self.ensure_write_class(&candidate).map_err(|_| vfs::Error::Denied)?; }
                    if pending.is_empty() && !follow { None } else {
                        match volume.resolution_node(&credentials, path) {
                            Ok(vfs::ResolutionNode::Link { target, global }) => {
                                let base = if global { "/".to_owned() } else { mount.clone() };
                                Some((target, base))
                            }
                            Ok(vfs::ResolutionNode::Object) => None,
                            Err(vfs::Error::NotFound) if pending.is_empty() => None,
                            Err(e) => return Err(e.into()),
                        }
                    }
                }
            };
            if let Some((target, base)) = link.filter(|_| follow || !pending.is_empty()) {
                links += 1;
                if links > 40 { return Err(vfs::Error::LinkLoop.into()); }
                validate_target(&target)?;
                if target.starts_with('/') { current = base; }
                if target.ends_with('/') { pending.push_front(".".to_owned()); }
                for part in target.split('/').filter(|s| !s.is_empty()).rev() { pending.push_front(part.to_owned()); }
                if pending.len() > 4096 { return Err(FsError::InvalidPath); }
                self.refresh_resolution_providers(&current, &mut refreshed, guard)?;
            } else { current = candidate; }
        }
        user_path(actor, &current, access)?;
        let route = self.vfs.route_as(&credentials, &current)?;
        if matches!(access, Access::Write) && !matches!(route, Route::Host) {
            self.ensure_write_class(&current)?;
        }
        Ok(ResolvedPath { path: current, route })
    }
    fn refresh_resolution_providers(
        &self, path: &str, refreshed: &mut [bool; 3], guard: &NamespaceGuard<'_>,
    ) -> Result<(), FsError> {
        let path = if cfg!(windows) { path.to_ascii_lowercase() } else { path.to_owned() };
        for (i, prefix) in ["/proc/tasks", "/proc/jobs", "/proc/executions"].iter().enumerate() {
            if !refreshed[i] && (path == "/proc" || path == *prefix || path.starts_with(&format!("{prefix}/"))) {
                match i {
                    0 => self.refresh_proc_tasks_locked(guard)?,
                    1 => self.refresh_proc_jobs()?,
                    _ => self.refresh_proc_executions()?,
                }
                refreshed[i] = true;
            }
        }
        Ok(())
    }
}
