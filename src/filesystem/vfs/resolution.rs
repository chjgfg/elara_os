//! Internal namespace link walking. Host links and host fallback are forbidden.
use super::{Error, Registry, Route};
use crate::filesystem::Credentials;
use std::collections::VecDeque;
use std::{format, unreachable, vec};

pub(super) enum Node {
    Object,
    Link { target: String, global: bool },
}
impl Registry {
    pub fn resolve_as(
        &self,
        actor: &Credentials,
        path: &str,
        follow_final: bool,
        mut authorize: impl FnMut(&str) -> Result<(), Error>,
    ) -> Result<Route, Error> {
        authorize(path)?;
        let initial = self.route_as(actor, path)?;
        let Route::Mounted {
            volume,
            path: relative,
            mount,
        } = initial
        else {
            return Ok(Route::Host);
        };
        // Keep visited mounts pinned until the walk finishes; target remains
        // pinned in the returned Route. No registry lock is held across IO.
        let mut pins = vec![volume];
        let mut current = mount;
        let mut pending: VecDeque<String> = relative
            .split('/')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        let mut links = 0;
        while let Some(name) = pending.pop_front() {
            let Route::Mounted {
                volume,
                path: parent,
                mount,
            } = self.route_as(actor, &current)?
            else {
                return Err(Error::CrossDevice);
            };
            volume.search_directory(actor, &parent)?;
            if name == "." {
                continue;
            }
            if name == ".." {
                // Ordinary links retain the existing volume-root boundary.
                if current == mount {
                    return Err(Error::InvalidPath);
                }
                current.truncate(current.rfind('/').ok_or(Error::InvalidPath)?);
                continue;
            }
            if !super::model::valid_name(&name) {
                return Err(Error::InvalidPath);
            }
            let candidate = format!("{current}/{name}");
            if candidate.len() > 4096 {
                return Err(Error::InvalidPath);
            }
            authorize(&candidate)?;
            let route = self.route_as(actor, &candidate)?;
            let Route::Mounted {
                volume,
                path: relative,
                ..
            } = &route
            else {
                return Err(Error::CrossDevice);
            };
            if pending.is_empty() && !follow_final {
                return Ok(route);
            }
            match volume.resolution_node(actor, relative) {
                Ok(Node::Object) => current = candidate,
                Err(Error::NotFound) if pending.is_empty() => return Ok(route),
                Err(error) => return Err(error),
                Ok(Node::Link { target, global }) => {
                    links += 1;
                    if links > 40 {
                        return Err(Error::LinkLoop);
                    }
                    let relative_target = if global {
                        authorize(&target)?;
                        let Route::Mounted {
                            volume,
                            path,
                            mount,
                        } = self.route_as(actor, &target)?
                        else {
                            return Err(Error::CrossDevice);
                        };
                        pins.push(volume);
                        current = mount;
                        path
                    } else {
                        if target.starts_with('/') {
                            let Route::Mounted { mount, .. } = &route else {
                                unreachable!()
                            };
                            current = mount.clone();
                        }
                        target
                    };
                    for part in relative_target.split('/').filter(|s| !s.is_empty()).rev() {
                        pending.push_front(part.to_owned());
                    }
                    if pending.len() > 4096 {
                        return Err(Error::LinkLoop);
                    }
                }
            }
            pins.push(volume.clone());
        }
        authorize(&current)?;
        self.route_as(actor, &current)
    }
}
