use std::fmt;
use std::path::PathBuf;
use std::{matches, write};

/// A normalized path in ElaraOS's virtual namespace, never a host path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VirtualPath {
    components: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathError {
    HostSyntax,
    AboveRoot,
    UnnormalizedPath,
    InvalidComponent,
}

impl fmt::Display for PathError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HostSyntax => write!(
                output,
                "Host path syntax and control characters are refused."
            ),
            Self::AboveRoot => write!(output, "Cannot move above the virtual root."),
            Self::UnnormalizedPath => write!(
                output,
                "The filesystem backend requires a path without dot components."
            ),
            Self::InvalidComponent => {
                write!(output, "Reserved, protected or ambiguous path component.")
            }
        }
    }
}

impl VirtualPath {
    pub fn root() -> Self {
        Self {
            components: Vec::new(),
        }
    }

    /// Keep raw dot components out of the host-filesystem backend.
    pub fn strict(path: &str) -> Result<Self, PathError> {
        if path.split('/').any(|part| matches!(part, "." | "..")) {
            return Err(PathError::UnnormalizedPath);
        }
        Self::root().resolve(path)
    }

    /// Resolve in memory. An error leaves the original path unchanged.
    pub fn resolve(&self, requested: &str) -> Result<Self, PathError> {
        if requested.contains(['\\', ':']) || requested.chars().any(char::is_control) {
            return Err(PathError::HostSyntax);
        }
        let mut result = if requested.starts_with('/') {
            Self::root()
        } else {
            self.clone()
        };
        for component in requested.split('/').filter(|part| !part.is_empty()) {
            match component {
                "." => {}
                ".." => {
                    result.components.pop().ok_or(PathError::AboveRoot)?;
                }
                _ => {
                    validate_component(component)?;
                    result.components.push(component.to_owned());
                }
            }
        }
        Ok(result)
    }

    /// The only conversion needed for joining with the configured data root.
    pub fn as_relative(&self) -> PathBuf {
        let mut path = PathBuf::new();
        for component in &self.components {
            path.push(component);
        }
        path
    }
}

impl fmt::Display for VirtualPath {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(output, "/{}", self.components.join("/"))
    }
}

fn validate_component(component: &str) -> Result<(), PathError> {
    let device = component
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    let reserved = matches!(
        device.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        device.strip_prefix(*prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    });
    if component == crate::config::ROOTFS_DEFAULTS.marker_filename
        || component.ends_with(['.', ' '])
        || reserved
    {
        return Err(PathError::InvalidComponent);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{PathError, VirtualPath};
    use std::path::PathBuf;
    use std::{assert, assert_eq};

    #[test]
    fn root_has_no_host_prefix() {
        let root = VirtualPath::root();
        assert_eq!(root.to_string(), "/");
        assert_eq!(root.as_relative(), PathBuf::new());
    }

    #[test]
    fn relative_paths_use_the_virtual_current_directory() {
        let current = VirtualPath::strict("/usr").unwrap();
        assert_eq!(current.resolve("bin").unwrap().to_string(), "/usr/bin");
    }

    #[test]
    fn absolute_paths_ignore_the_current_directory() {
        let current = VirtualPath::strict("/usr/bin").unwrap();
        assert_eq!(current.resolve("/etc").unwrap().to_string(), "/etc");
    }

    #[test]
    fn parents_are_allowed_only_inside_the_virtual_root() {
        let current = VirtualPath::strict("/usr/bin").unwrap();
        assert_eq!(current.resolve("../..").unwrap(), VirtualPath::root());
        assert_eq!(current.resolve("../../.."), Err(PathError::AboveRoot));
        assert_eq!(VirtualPath::root().resolve(".."), Err(PathError::AboveRoot));
    }

    #[test]
    fn strict_backend_input_still_rejects_raw_dot_components() {
        for path in ["/usr/../etc", "./usr", "usr/."] {
            assert_eq!(VirtualPath::strict(path), Err(PathError::UnnormalizedPath));
        }
    }

    #[test]
    fn host_paths_and_control_characters_are_rejected() {
        for path in [
            "C:/Windows",
            "\\\\server\\share",
            "usr\\bin",
            "file:stream",
            "a\0b",
            "a\nb",
        ] {
            assert_eq!(
                VirtualPath::root().resolve(path),
                Err(PathError::HostSyntax)
            );
        }
    }

    #[test]
    fn device_names_and_the_root_marker_are_protected() {
        for path in [
            "NUL",
            "CON.txt",
            "COM¹",
            "LPT9",
            ".elara-root",
            "end.",
            "end ",
        ] {
            assert_eq!(
                VirtualPath::root().resolve(path),
                Err(PathError::InvalidComponent)
            );
        }
    }

    #[test]
    fn separators_and_current_directory_components_are_normalized() {
        assert_eq!(
            VirtualPath::root()
                .resolve("//usr///./bin/")
                .unwrap()
                .to_string(),
            "/usr/bin"
        );
    }

    #[test]
    fn unicode_paths_remain_inside_the_namespace() {
        let path = VirtualPath::root().resolve("/home/星澜").unwrap();
        assert_eq!(path.to_string(), "/home/星澜");
        assert_eq!(path.as_relative(), PathBuf::from("home").join("星澜"));
        assert!(!path.as_relative().is_absolute());
    }

    #[test]
    fn failed_resolution_does_not_change_the_previous_directory() {
        let current = VirtualPath::strict("/usr").unwrap();
        assert_eq!(current.resolve("../.."), Err(PathError::AboveRoot));
        assert_eq!(current.to_string(), "/usr");
    }
}
