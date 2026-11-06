use std::format;
use std::fs;
use std::path::{Path, PathBuf};

/// 固定的系统默认参数集中在配置模块，不从零散的顶层文件读取。
/// 标记内容属于根目录归属协议；修改它需要明确的版本迁移。
pub(crate) struct RootFsDefaults {
    pub marker_filename: &'static str,
    pub marker_contents: &'static [u8],
    pub directories: &'static [&'static str],
}

pub(crate) const ROOTFS_DEFAULTS: RootFsDefaults = RootFsDefaults {
    marker_filename: ".elara-root",
    marker_contents: b"ELARA_DATA_ROOT_V1\n",
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
};

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
