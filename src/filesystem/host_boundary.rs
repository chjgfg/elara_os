//! Shared pre-IO gate for the only ambient grant: a dedicated local data root.
//! This safety rail is not an OS sandbox or a permit for raw devices/mounts.
use super::FsError;
use crate::virtual_path::VirtualPath;
use std::matches;
use std::path::{Component, Path};

pub(crate) fn validate_root_location(root: &Path) -> Result<(), FsError> {
    let invalid = || FsError::InvalidRoot;
    let text = root.to_str().ok_or_else(invalid)?;
    if !root.is_absolute()
        || root.parent().is_none()
        || text.chars().any(char::is_control)
        || text.split(['/', '\\']).any(|p| matches!(p, "." | ".."))
    {
        return Err(invalid());
    }
    #[cfg(windows)]
    if !matches!(root.components().next(), Some(Component::Prefix(prefix))
        if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
    {
        return Err(invalid());
    }
    for component in root.components() {
        if let Component::Normal(name) = component {
            let name = name.to_str().ok_or_else(invalid)?;
            VirtualPath::strict(name).map_err(|_| invalid())?;
            if name.contains(['<', '>', '"', '|', '?', '*']) {
                return Err(invalid());
            }
        }
    }
    #[cfg(windows)]
    {
        // Refuse conventional OS/program trees on every drive. ProgramData and
        // user-profile descendants may hold explicitly selected application data.
        let first = root
            .components()
            .find_map(|c| match c {
                Component::Normal(name) => name.to_str(),
                _ => None,
            })
            .ok_or_else(invalid)?;
        if ["Windows", "Program Files", "Program Files (x86)"]
            .iter()
            .any(|name| first.eq_ignore_ascii_case(name))
        {
            return Err(invalid());
        }
        // Also cover a nonstandard configured Windows system directory.
        if let Some(system) = std::env::var_os("SystemRoot") {
            // Drop disk-prefix spelling so verbatim/local aliases cannot
            // defeat this conservative system-tree restriction.
            let system = Path::new(&system).components().skip(1).collect::<Vec<_>>();
            let selected = root.components().skip(1).collect::<Vec<_>>();
            if !system.is_empty()
                && selected.len() >= system.len()
                && selected.iter().zip(&system).all(|(a, b)| {
                    a.as_os_str()
                        .to_string_lossy()
                        .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
                })
            {
                return Err(invalid());
            }
        }
    }
    #[cfg(unix)]
    if [
        "/bin", "/boot", "/dev", "/etc", "/lib", "/lib64", "/proc", "/sbin", "/sys", "/usr",
    ]
    .iter()
    .any(|path| root.starts_with(path))
    {
        return Err(invalid());
    }
    Ok(())
}
