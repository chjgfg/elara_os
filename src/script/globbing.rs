//! Authorized virtual-path wildcard enumeration, never host directory globbing.
use super::pattern;
use crate::filesystem::{DataRoot, FsError, ObjectKind};
use crate::permissions::{self, Access};
use crate::users::Account;
use crate::virtual_path::VirtualPath;
use std::{format, vec};

pub enum PathExpansion {
    Unmatched,
    Matched(Vec<String>),
}
pub fn expand(
    root: &DataRoot,
    user: &Account,
    cwd: &VirtualPath,
    mask: &str,
) -> Result<PathExpansion, String> {
    let absolute = mask.starts_with('/');
    let mut paths = vec![(
        if absolute {
            VirtualPath::root()
        } else {
            cwd.clone()
        },
        if absolute {
            "/".to_owned()
        } else {
            String::new()
        },
    )];
    let parts: Vec<_> = mask.split('/').filter(|part| !part.is_empty()).collect();
    let mut visited = 0usize;
    for (index, part) in parts.iter().enumerate() {
        let mut next = Vec::new();
        if pattern::has_wildcards(part)? {
            for (directory, display) in paths {
                permissions::check(user, &directory, Access::Read).map_err(|e| e.to_string())?;
                let mut cursor = match root.open_directory(&directory.to_string()) {
                    Ok(cursor) => cursor,
                    Err(FsError::NotFound | FsError::NotDirectory) => continue,
                    Err(FsError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                        continue
                    }
                    Err(error) => return Err(error.to_string()),
                };
                loop {
                    let entries = cursor.read(256).map_err(|e| e.to_string())?;
                    if entries.is_empty() {
                        break;
                    }
                    for entry in entries {
                        visited += 1;
                        if visited > 4096 {
                            return Err("glob enumeration exceeds 4096 entries".into());
                        }
                        let name = entry.name.to_str().ok_or("glob requires UTF-8 names")?;
                        if name.starts_with('.') && !part.starts_with('.') {
                            continue;
                        }
                        if !pattern::matches(part, name)? {
                            continue;
                        }
                        if index + 1 < parts.len()
                            && !std::matches!(entry.kind, ObjectKind::Directory)
                        {
                            continue;
                        }
                        let path = directory.resolve(name).map_err(|e| e.to_string())?;
                        permissions::check(user, &path, Access::Read).map_err(|e| e.to_string())?;
                        next.push((path, join(&display, name)));
                    }
                }
            }
        } else {
            let name = pattern::literal(part)?;
            for (directory, display) in paths {
                let path = directory.resolve(&name).map_err(|e| e.to_string())?;
                permissions::check(user, &path, Access::Read).map_err(|e| e.to_string())?;
                next.push((path, join(&display, &name)));
            }
        }
        if next.len() > 1024 {
            return Err("glob expansion exceeds 1024 paths".into());
        }
        paths = next;
    }
    if paths.is_empty() {
        return Ok(PathExpansion::Unmatched);
    }
    let mut output = Vec::new();
    let mut bytes = 0usize;
    for (path, mut display) in paths {
        let stat = match root.stat(&path.to_string()) {
            Ok(stat) => stat,
            Err(FsError::NotFound) => continue,
            Err(FsError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        if mask.ends_with('/') {
            if !std::matches!(stat.kind, ObjectKind::Directory) {
                continue;
            }
            display.push('/');
        }
        bytes += display.len();
        if bytes > 65536 {
            return Err("glob output exceeds 64 KiB".into());
        }
        output.push(display);
    }
    output.sort();
    if output.is_empty() {
        Ok(PathExpansion::Unmatched)
    } else {
        Ok(PathExpansion::Matched(output))
    }
}
fn join(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.into()
    } else if parent == "/" {
        format!("/{child}")
    } else {
        format!("{parent}/{child}")
    }
}
