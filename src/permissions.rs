//! Access rules for the virtual filesystem.
//! Every path check goes through `check`, so the rules live in one place:
//! root (uid 0) may do anything; a regular user owns its home directory,
//! may read most of the system, and is kept out of `/root`, `/etc` writes
//! and the system log.
use crate::users::Account;
use crate::virtual_path::VirtualPath;
use std::fmt;
use std::write;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Denied {
    pub path: String,
    pub access: Access,
}

impl fmt::Display for Denied {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        let verb = match self.access {
            Access::Read => "read",
            Access::Write => "write",
        };
        write!(output, "Permission denied: cannot {verb} {}.", self.path)
    }
}

impl std::error::Error for Denied {}

/// Decide whether `user` may perform `access` on `path`.
pub fn check(user: &Account, path: &VirtualPath, access: Access) -> Result<(), Denied> {
    if user.is_admin() {
        return Ok(());
    }
    let text = path.to_string();
    let deny = || Denied {
        path: text.clone(),
        access,
    };
    if within(&text, "/root") {
        return Err(deny());
    }
    if within(&text, "/var/log") || within(&text, crate::users::SHADOW_PATH) {
        return Err(deny());
    }
    match access {
        Access::Read => Ok(()),
        Access::Write => {
            if within(&text, &user.home) || within(&text, "/tmp") {
                Ok(())
            } else {
                Err(deny())
            }
        }
    }
}

fn within(path: &str, prefix: &str) -> bool {
    let prefix_matches = if std::cfg!(windows) {
        path.get(..prefix.len())
            .is_some_and(|part| part.eq_ignore_ascii_case(prefix))
    } else {
        path.starts_with(prefix)
    };
    prefix_matches
        && (path.len() == prefix.len() || path.as_bytes().get(prefix.len()) == Some(&b'/'))
}

#[cfg(test)]
mod tests {
    use super::{check, Access};
    use crate::users::Account;
    use crate::virtual_path::VirtualPath;
    use std::{assert, assert_eq};

    fn root() -> Account {
        Account {
            name: "root".into(),
            uid: 0,
            home: "/root".into(),
        }
    }

    fn alice() -> Account {
        Account {
            name: "alice".into(),
            uid: 1000,
            home: "/home/alice".into(),
        }
    }

    fn path(text: &str) -> VirtualPath {
        VirtualPath::strict(text).unwrap()
    }

    #[test]
    fn root_may_do_anything() {
        for target in [
            "/",
            "/root",
            "/etc/passwd",
            "/var/log/elara.log",
            "/home/alice",
        ] {
            assert!(
                check(&root(), &path(target), Access::Write).is_ok(),
                "{target}"
            );
        }
    }

    #[test]
    fn regular_users_own_their_home_and_tmp_only() {
        let user = alice();
        assert!(check(&user, &path("/home/alice"), Access::Write).is_ok());
        assert!(check(&user, &path("/home/alice/notes.txt"), Access::Write).is_ok());
        assert!(check(&user, &path("/tmp/scratch"), Access::Write).is_ok());
        for target in [
            "/",
            "/etc",
            "/etc/passwd",
            "/home",
            "/home/bob",
            "/usr/bin",
            "/home/alice-other",
        ] {
            assert_eq!(
                check(&user, &path(target), Access::Write)
                    .unwrap_err()
                    .access,
                Access::Write,
                "{target}"
            );
        }
    }

    #[test]
    fn password_hashes_are_not_readable_by_regular_users() {
        assert!(check(&root(), &path("/etc/shadow"), Access::Read).is_ok());
        assert!(check(&alice(), &path("/etc/shadow"), Access::Read).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_case_aliases_cannot_bypass_permissions() {
        for target in ["/ROOT", "/ROOT/secret", "/VAR/LOG/elara.log", "/ETC/SHADOW"] {
            assert!(
                check(&alice(), &path(target), Access::Read).is_err(),
                "{target}"
            );
        }
        assert!(check(&alice(), &path("/HOME/ALICE/mine.txt"), Access::Write).is_ok());
        assert!(check(&alice(), &path("/HOME/ALICE-OTHER/mine.txt"), Access::Write).is_err());
    }

    #[test]
    fn regular_users_may_read_the_system_but_not_root_or_logs() {
        let user = alice();
        for target in ["/", "/etc/passwd", "/usr", "/home/bob"] {
            assert!(
                check(&user, &path(target), Access::Read).is_ok(),
                "{target}"
            );
        }
        for target in ["/root", "/root/secret", "/var/log", "/var/log/elara.log"] {
            assert!(
                check(&user, &path(target), Access::Read).is_err(),
                "{target}"
            );
        }
        assert_eq!(
            check(&user, &path("/root"), Access::Read)
                .unwrap_err()
                .to_string(),
            "Permission denied: cannot read /root."
        );
    }
}
