//! System log stored inside the virtual root at `/var/log/elara.log`.
//! Entries record startup and command execution. The log never touches a
//! host log location, and a logging failure is reported but never aborts
//! the command that triggered it.
use crate::filesystem::{
    CleanupStatus, DataRoot, FsError, LogCommit, LogPolicy, LogStats, MirrorStatus,
};
use std::format;
use std::time::{SystemTime, UNIX_EPOCH};

pub const LOG_PATH: &str = "/var/log/elara.log";
pub const DEFAULT_TAIL: usize = 20;

pub struct Logger {
    policy: LogPolicy,
}

impl Logger {
    /// New stores are managed; pre-existing unowned logs remain read-only.
    pub fn open(data_root: &DataRoot) -> Result<Self, FsError> {
        Self::with_policy(data_root, LogPolicy::default())
    }
    pub fn with_policy(data_root: &DataRoot, policy: LogPolicy) -> Result<Self, FsError> {
        data_root.initialize_log(policy)?;
        Ok(Self { policy })
    }
    pub fn record_receipt(
        &self,
        data_root: &DataRoot,
        user: &str,
        event: &str,
    ) -> Result<LogCommit, FsError> {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0);
        let event = sanitize(event);
        let user = sanitize(user);
        data_root.append_log(
            self.policy,
            format!("{seconds} {user} {event}\n").as_bytes(),
        )
    }
    pub fn record(&self, data_root: &DataRoot, user: &str, event: &str) -> Result<(), FsError> {
        let receipt = self.record_receipt(data_root, user, event)?;
        if let MirrorStatus::Deferred(error) = receipt.mirror {
            return Err(FsError::LogFollowupUnconfirmed {
                generation: receipt.generation,
                error,
            });
        }
        if let CleanupStatus::Deferred(error) = receipt.cleanup {
            return Err(FsError::LogCleanupUnconfirmed {
                generation: receipt.generation,
                error,
            });
        }
        Ok(())
    }
    pub fn tail(&self, data_root: &DataRoot, count: usize) -> Result<Vec<String>, FsError> {
        data_root.log_tail(self.policy, count)
    }
    pub fn stats(&self, data_root: &DataRoot) -> Result<LogStats, FsError> {
        data_root.log_stats(self.policy)
    }
}

/// Keep one event per line and drop control characters so the log stays parseable.
fn sanitize(event: &str) -> String {
    let mut text: String = event
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if text.len() > 512 {
        let mut end = 512;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("...");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{sanitize, Logger, LOG_PATH};
    use crate::filesystem::DataRoot;
    use std::{assert, assert_eq, fs};

    fn fixture() -> (std::path::PathBuf, DataRoot) {
        let root = crate::test_support::marked_root("log");
        let data = DataRoot::open(&root).unwrap();
        (root, data)
    }

    #[test]
    fn records_are_appended_inside_the_virtual_root_and_tailed_in_order() {
        let (root, data) = fixture();
        let logger = Logger::open(&data).unwrap();
        logger.record(&data, "root", "startup").unwrap();
        logger.record(&data, "root", "command: ls /usr").unwrap();
        logger.record(&data, "alice", "command: echo 星澜").unwrap();
        let lines = logger.tail(&data, 2).unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with(" root command: ls /usr"));
        assert!(lines[1].ends_with(" alice command: echo 星澜"));
        assert!(root.join("var/log/elara.log").is_file());
        assert_eq!(
            data.read_file(LOG_PATH, 4096)
                .unwrap()
                .iter()
                .filter(|b| **b == b'\n')
                .count(),
            3
        );
    }

    #[test]
    fn reopening_keeps_existing_entries() {
        let (root, data) = fixture();
        Logger::open(&data)
            .unwrap()
            .record(&data, "root", "first")
            .unwrap();
        let logger = Logger::open(&data).unwrap();
        logger.record(&data, "root", "second").unwrap();
        assert_eq!(
            fs::read_to_string(root.join("var/log/elara.log"))
                .unwrap()
                .lines()
                .count(),
            2
        );
    }

    #[test]
    fn control_characters_cannot_forge_log_lines() {
        assert_eq!(sanitize("a\nfake entry\tb"), "a fake entry b");
        let long = "x".repeat(600);
        assert_eq!(sanitize(&long).len(), 515);
    }
}
