use super::{DirectoryAnchor, Error, PseudoKind, Volume};
use crate::filesystem::Credentials;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// Per-data-root registry. A resolved Arc pins a mount against unmount.
#[derive(Default)]
pub struct Registry {
    mounts: Mutex<BTreeMap<String, Mount>>,
}
struct Mount {
    volume: Arc<Volume>,
    kind: MountKind,
    // Kept alive until unmount; prevents renaming/removing the underlying parent.
    _attachment: Attachment,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountKind {
    User,
    System(PseudoKind),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountInfo {
    pub path: String,
    pub kind: MountKind,
}
enum Attachment {
    Host,
    Nested { _parent: DirectoryAnchor },
}
pub enum Route {
    Host,
    Mounted {
        volume: Arc<Volume>,
        path: String,
        mount: String,
    },
}
impl Registry {
    pub(crate) fn install_system(&self, point: &str, kind: PseudoKind) -> Result<(), Error> {
        let volume = Volume::pseudo(kind)?;
        let mut mounts = self.mounts.lock().map_err(|_| Error::Busy)?;
        if mounts.contains_key(point) {
            return Err(Error::Exists);
        }
        mounts.insert(
            point.to_owned(),
            Mount {
                volume,
                kind: MountKind::System(kind),
                _attachment: Attachment::Host,
            },
        );
        Ok(())
    }
    pub fn mount_info(&self) -> Result<Vec<MountInfo>, Error> {
        let mounts = self.mounts.lock().map_err(|_| Error::Busy)?;
        Ok(mounts
            .iter()
            .map(|(path, entry)| MountInfo {
                path: path.clone(),
                kind: entry.kind,
            })
            .collect())
    }
    pub fn mount(
        &self,
        actor: &Credentials,
        point: &str,
        volume: Arc<Volume>,
    ) -> Result<(), Error> {
        if actor.uid != 0 {
            return Err(Error::Denied);
        }
        let point = canonical(point)?;
        if point == "/" {
            return Err(Error::InvalidPath);
        }
        let mut mounts = self.mounts.lock().map_err(|_| Error::Busy)?;
        if mounts.len() >= 32 {
            return Err(Error::Quota);
        }
        if mounts.contains_key(&point) {
            return Err(Error::Exists);
        }
        if mounts
            .iter()
            .any(|(p, v)| within(p, &point) || Arc::ptr_eq(&v.volume, &volume))
        {
            return Err(Error::Busy);
        }
        let attachment = if let Some((prefix, parent)) = mounts
            .iter()
            .filter(|(p, _)| within(&point, p))
            .max_by_key(|(p, _)| p.len())
        {
            let relative = &point[prefix.len()..];
            match parent.volume.stat(actor, relative, false) {
                Ok(_) => return Err(Error::Exists),
                Err(Error::NotFound) => {}
                Err(error) => return Err(error),
            }
            let parent_path = relative.rsplit_once('/').ok_or(Error::InvalidPath)?.0;
            let parent_path = if parent_path.is_empty() {
                "/"
            } else {
                parent_path
            };
            let anchor = parent.volume.anchor_directory(actor, parent_path)?;
            // Lexical mount prefixes must not bypass a symlinked parent.
            if anchor.path() != parent_path {
                return Err(Error::InvalidPath);
            }
            Attachment::Nested { _parent: anchor }
        } else {
            Attachment::Host
        };
        mounts.insert(
            point,
            Mount {
                volume,
                kind: MountKind::User,
                _attachment: attachment,
            },
        );
        Ok(())
    }
    pub fn unmount(&self, actor: &Credentials, point: &str) -> Result<(), Error> {
        if actor.uid != 0 {
            return Err(Error::Denied);
        }
        let point = canonical(point)?;
        let mut mounts = self.mounts.lock().map_err(|_| Error::Busy)?;
        let entry = mounts.get(&point).ok_or(Error::NotFound)?;
        if entry.kind != MountKind::User {
            return Err(Error::ReadOnly);
        }
        let volume = &entry.volume;
        if Arc::strong_count(volume) != 1
            || volume.stats()?.open_files != 0
            || mounts.keys().any(|p| p != &point && within(p, &point))
        {
            return Err(Error::Busy);
        }
        mounts.remove(&point);
        Ok(())
    }
    pub fn route(&self, path: &str) -> Result<Route, Error> {
        let path = canonical(path)?;
        let mounts = self.mounts.lock().map_err(|_| Error::Busy)?;
        let selected = mounts
            .iter()
            .filter(|(point, _)| within(&path, point))
            .max_by_key(|(point, _)| point.len());
        match selected {
            None => Ok(Route::Host),
            Some((point, volume)) => Ok(Route::Mounted {
                volume: volume.volume.clone(),
                path: if path == *point {
                    "/".into()
                } else {
                    path[point.len()..].into()
                },
                mount: point.clone(),
            }),
        }
    }
    pub fn route_as(&self, actor: &Credentials, path: &str) -> Result<Route, Error> {
        let route = self.route(path)?;
        let mounts = self.mounts.lock().map_err(|_| Error::Busy)?;
        let mut ancestors: Vec<_> = mounts.iter().filter(|(p, _)| within(path, p)).collect();
        ancestors.sort_by_key(|(point, _)| point.len());
        for pair in ancestors.windows(2) {
            let (prefix, parent) = pair[0];
            let (child, _) = pair[1];
            let relative = &child[prefix.len()..];
            let parent_path = relative.rsplit_once('/').ok_or(Error::InvalidPath)?.0;
            parent.volume.search_directory(
                actor,
                if parent_path.is_empty() {
                    "/"
                } else {
                    parent_path
                },
            )?;
        }
        Ok(route)
    }
    pub fn mounts(&self) -> Result<Vec<String>, Error> {
        Ok(self
            .mounts
            .lock()
            .map_err(|_| Error::Busy)?
            .keys()
            .cloned()
            .collect())
    }
    /// A bounded snapshot of immediate mount children, retaining their volumes.
    pub fn children(&self, path: &str) -> Result<Vec<(String, Arc<Volume>)>, Error> {
        let path = canonical(path)?;
        let mounts = self.mounts.lock().map_err(|_| Error::Busy)?;
        Ok(mounts
            .iter()
            .filter_map(|(point, mount)| {
                let (parent, name) = point.rsplit_once('/')?;
                let parent = if parent.is_empty() { "/" } else { parent };
                (parent == path).then(|| (name.to_owned(), mount.volume.clone()))
            })
            .collect())
    }
    pub fn rename(
        &self,
        actor: &Credentials,
        source: &str,
        destination: &str,
        replace: bool,
    ) -> Result<(), Error> {
        match (
            self.resolve_as(actor, source, false, |_| Ok(()))?,
            self.resolve_as(actor, destination, false, |_| Ok(()))?,
        ) {
            (
                Route::Mounted {
                    volume: a,
                    path: source,
                    ..
                },
                Route::Mounted {
                    volume: b,
                    path: destination,
                    ..
                },
            ) if Arc::ptr_eq(&a, &b) => a.rename(actor, &source, &destination, replace),
            _ => Err(Error::CrossDevice),
        }
    }
    pub fn link(&self, actor: &Credentials, source: &str, destination: &str) -> Result<(), Error> {
        match (
            self.resolve_as(actor, source, false, |_| Ok(()))?,
            self.resolve_as(actor, destination, false, |_| Ok(()))?,
        ) {
            (
                Route::Mounted {
                    volume: a,
                    path: source,
                    ..
                },
                Route::Mounted {
                    volume: b,
                    path: destination,
                    ..
                },
            ) if Arc::ptr_eq(&a, &b) => a.link(actor, &source, &destination),
            _ => Err(Error::CrossDevice),
        }
    }
}
fn within(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|s| s.starts_with('/'))
}
fn canonical(path: &str) -> Result<String, Error> {
    let path = crate::virtual_path::VirtualPath::strict(path)
        .map_err(|_| Error::InvalidPath)?
        .to_string();
    // Host layouts are case-insensitive on Windows; aliases must never route
    // around a built-in provider into the otherwise empty backing directory.
    #[cfg(windows)]
    {
        let end = path[1..].find('/').map_or(path.len(), |i| i + 1);
        let first = &path[..end];
        for system in ["/proc", "/sys", "/dev"] {
            if first.eq_ignore_ascii_case(system) {
                return Ok(std::format!("{system}{}", &path[end..]));
            }
        }
    }
    Ok(path)
}
