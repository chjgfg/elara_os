use std::format;
use std::fs;
use std::path::{Path, PathBuf};

/// 固定的系统默认参数集中在配置模块，不从零散的顶层文件读取。
/// 标记内容属于根目录归属协议；修改它需要明确的版本迁移。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WriteClass {
    SystemReadOnly,
    UserData,
    Temporary,
    Cache,
    Log,
    RuntimeState,
    Configuration,
    Application,
    Unclassified,
}

pub(crate) struct DirectoryPolicy {
    pub path: &'static str,
    pub class: WriteClass,
}

pub(crate) struct RootFsDefaults {
    pub marker_filename: &'static str,
    pub marker_contents: &'static [u8],
    pub directories: &'static [&'static str],
    pub write_policies: &'static [DirectoryPolicy],
    pub transaction_directory: &'static str,
    pub transaction_store_magic: &'static [u8],
    pub max_transaction_bytes: usize,
    pub max_file_io_bytes: usize,
    pub max_open_files: usize,
    pub max_directory_entries: usize,
    pub max_directory_page: usize,
    pub max_walk_entries: usize,
    pub max_walk_depth: usize,
}

pub(crate) const PRIVATE_LOG_DIRECTORY: &str = ".elara-logs";
pub(crate) const PRIVATE_OWNER_DIRECTORY: &str = ".elara-objects";
pub(crate) const LOG_SEGMENT_BYTES: usize = 64 * 1024;
pub(crate) const LOG_ARCHIVES: usize = 4;
pub(crate) const LOG_TAIL_BYTES: usize = 1024 * 1024;

pub(crate) const ROOTFS_DEFAULTS: RootFsDefaults = RootFsDefaults {
    marker_filename: ".elara-root",
    marker_contents: b"ELARA_DATA_ROOT_V1\n",
    transaction_directory: ".elara-transactions",
    transaction_store_magic: b"ELARA_TRANSACTION_STORE_V1\n",
    max_transaction_bytes: 1024 * 1024,
    max_file_io_bytes: 1024 * 1024,
    max_open_files: 256,
    max_directory_entries: 4096,
    max_directory_page: 256,
    max_walk_entries: 16384,
    max_walk_depth: 64,
    directories: &[
        "bin",
        "boot",
        "dev",
        "etc",
        "home",
        "lib",
        "media",
        "mnt",
        "opt",
        "proc",
        "root",
        "run",
        "sbin",
        "snap",
        "srv",
        "sys",
        "tmp",
        "usr",
        "usr/bin",
        "usr/lib",
        "usr/sbin",
        "var",
        "var/log",
        "var/cache",
    ],
    write_policies: &[
        DirectoryPolicy {
            path: "/bin",
            class: WriteClass::SystemReadOnly,
        },
        DirectoryPolicy {
            path: "/sbin",
            class: WriteClass::SystemReadOnly,
        },
        DirectoryPolicy {
            path: "/lib",
            class: WriteClass::SystemReadOnly,
        },
        DirectoryPolicy {
            path: "/usr",
            class: WriteClass::SystemReadOnly,
        },
        DirectoryPolicy {
            path: "/boot",
            class: WriteClass::SystemReadOnly,
        },
        DirectoryPolicy {
            path: "/dev",
            class: WriteClass::SystemReadOnly,
        },
        DirectoryPolicy {
            path: "/proc",
            class: WriteClass::SystemReadOnly,
        },
        DirectoryPolicy {
            path: "/sys",
            class: WriteClass::SystemReadOnly,
        },
        DirectoryPolicy {
            path: "/home",
            class: WriteClass::UserData,
        },
        DirectoryPolicy {
            path: "/root",
            class: WriteClass::UserData,
        },
        DirectoryPolicy {
            path: "/tmp",
            class: WriteClass::Temporary,
        },
        DirectoryPolicy {
            path: "/run",
            class: WriteClass::RuntimeState,
        },
        DirectoryPolicy {
            path: "/var/run",
            class: WriteClass::RuntimeState,
        },
        DirectoryPolicy {
            path: "/var/cache",
            class: WriteClass::Cache,
        },
        DirectoryPolicy {
            path: "/var/log",
            class: WriteClass::Log,
        },
        DirectoryPolicy {
            path: "/etc",
            class: WriteClass::Configuration,
        },
        DirectoryPolicy {
            path: "/opt",
            class: WriteClass::Application,
        },
        DirectoryPolicy {
            path: "/srv",
            class: WriteClass::Application,
        },
        DirectoryPolicy {
            path: "/mnt",
            class: WriteClass::UserData,
        },
        DirectoryPolicy {
            path: "/media",
            class: WriteClass::UserData,
        },
    ],
};

/// 分类只描述用途；用户/任务权限仍由上层权限对象决定。
/// 未分类路径显式返回 Unclassified，不默认为可写。
pub(crate) fn write_class(path: &str) -> WriteClass {
    ROOTFS_DEFAULTS
        .write_policies
        .iter()
        .filter(|policy| {
            let matches = if std::cfg!(windows) {
                path.get(..policy.path.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(policy.path))
            } else {
                path.starts_with(policy.path)
            };
            matches
                && (path.len() == policy.path.len()
                    || path.as_bytes().get(policy.path.len()) == Some(&b'/'))
        })
        .max_by_key(|policy| policy.path.len())
        .map_or(WriteClass::Unclassified, |policy| policy.class)
}

/// 机器相关的运行配置由单一 .env 输入，不包含 Cargo 构建设置。
pub struct Config {
    pub data_root: PathBuf,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path)
            .map_err(|error| format!("Cannot read configuration {}: {error}", path.display()))?;
        Self::parse(&text)
    }

    fn parse(text: &str) -> Result<Self, String> {
        let entries: Vec<_> = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect();
        if entries.len() != 1 {
            return Err(
                "Expected exactly one ELARA_DATA_ROOT setting; no fallback is allowed.".into(),
            );
        }
        let value = entries[0]
            .strip_prefix("ELARA_DATA_ROOT=")
            .ok_or("Unknown configuration key.")?
            .trim();
        if value.is_empty()
            || value.contains('"')
            || value.contains('\'')
            || value.chars().any(char::is_control)
        {
            return Err("ELARA_DATA_ROOT must be a nonempty, unquoted path.".into());
        }
        Ok(Self {
            data_root: PathBuf::from(value),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Config;
    use std::path::PathBuf;
    use std::{assert, assert_eq};

    #[test]
    fn parses_comments_and_root() {
        let config = Config::parse("# comment\nELARA_DATA_ROOT=D:/os\n").unwrap();
        assert_eq!(config.data_root, PathBuf::from("D:/os"));
    }

    #[test]
    fn missing_key_has_no_default() {
        assert!(Config::parse("# empty\n").is_err());
        assert!(Config::parse("ELARA_DATA_ROOT=\n").is_err());
    }

    #[test]
    fn duplicate_or_unknown_keys_are_rejected() {
        assert!(Config::parse("ELARA_DATA_ROOT=a\nELARA_DATA_ROOT=b\n").is_err());
        assert!(Config::parse("OTHER=a\n").is_err());
    }
}
