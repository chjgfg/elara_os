//! User accounts stored in the virtual `/etc/passwd`.
//! The permission model is deliberately small: `root` (uid 0) is the only
//! administrator; every other account is a regular user. Accounts live only
//! inside the virtual root and have no relation to host OS accounts.
use crate::filesystem::{DataRoot, FsError};
use std::fmt;
use std::{format, write};

pub const PASSWD_PATH: &str = "/etc/passwd";
pub const ROOT_UID: u32 = 0;
const FIRST_USER_UID: u32 = 1000;
const MAX_PASSWD_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub uid: u32,
    pub home: String,
}

impl Account {
    pub fn is_admin(&self) -> bool {
        self.uid == ROOT_UID
    }
}

#[derive(Debug)]
pub enum UserError {
    Fs(FsError),
    Corrupt(usize),
    InvalidName(String),
    Duplicate(String),
    Unknown(String),
    PermissionDenied,
}

impl fmt::Display for UserError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fs(error) => write!(output, "{error}"),
            Self::Corrupt(line) => write!(output, "{PASSWD_PATH} line {line} is malformed."),
            Self::InvalidName(name) => write!(
                output,
                "'{name}' is not a valid user name: use 1-32 lowercase letters, digits, '_' or '-', starting with a letter."
            ),
            Self::Duplicate(name) => write!(output, "User '{name}' already exists."),
            Self::Unknown(name) => write!(output, "Unknown user '{name}'."),
            Self::PermissionDenied => write!(output, "Only root can manage users."),
        }
    }
}

impl From<FsError> for UserError {
    fn from(error: FsError) -> Self {
        Self::Fs(error)
    }
}

fn validate_name(name: &str) -> Result<(), UserError> {
    let mut chars = name.chars();
    let valid_start = chars.next().is_some_and(|c| c.is_ascii_lowercase());
    let valid_rest = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if name.len() > 32 || !valid_start || !valid_rest {
        return Err(UserError::InvalidName(name.to_owned()));
    }
    Ok(())
}

pub fn parse(text: &str) -> Result<Vec<Account>, UserError> {
    let mut accounts = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split(':').collect();
        let [name, uid, home] = fields.as_slice() else {
            return Err(UserError::Corrupt(index + 1));
        };
        validate_name(name).map_err(|_| UserError::Corrupt(index + 1))?;
        let uid = uid.parse().map_err(|_| UserError::Corrupt(index + 1))?;
        if !home.starts_with('/') || accounts.iter().any(|existing: &Account| existing.name == *name || existing.uid == uid) {
            return Err(UserError::Corrupt(index + 1));
        }
        accounts.push(Account { name: (*name).to_owned(), uid, home: (*home).to_owned() });
    }
    Ok(accounts)
}

fn render(accounts: &[Account]) -> String {
    accounts
        .iter()
        .map(|account| format!("{}:{}:{}\n", account.name, account.uid, account.home))
        .collect()
}

pub fn load(data_root: &DataRoot) -> Result<Vec<Account>, UserError> {
    let bytes = data_root.read_file(PASSWD_PATH, MAX_PASSWD_BYTES)?;
    let text = String::from_utf8(bytes).map_err(|_| UserError::Corrupt(0))?;
    parse(&text)
}

/// Create `/etc/passwd` with only `root` when it does not exist yet.
pub fn ensure_passwd(data_root: &DataRoot) -> Result<(), UserError> {
    match data_root.create_file(PASSWD_PATH) {
        Ok(()) => {}
        Err(FsError::AlreadyExists) => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let root = Account { name: "root".into(), uid: ROOT_UID, home: "/root".into() };
    data_root.write_file(PASSWD_PATH, render(&[root]).as_bytes())?;
    Ok(())
}

pub fn lookup<'a>(accounts: &'a [Account], name: &str) -> Result<&'a Account, UserError> {
    accounts
        .iter()
        .find(|account| account.name == name)
        .ok_or_else(|| UserError::Unknown(name.to_owned()))
}

pub fn add_user(data_root: &DataRoot, actor: &Account, name: &str) -> Result<Account, UserError> {
    if !actor.is_admin() {
        return Err(UserError::PermissionDenied);
    }
    validate_name(name)?;
    let mut accounts = load(data_root)?;
    if accounts.iter().any(|account| account.name == name) {
        return Err(UserError::Duplicate(name.to_owned()));
    }
    let uid = accounts
        .iter()
        .map(|account| account.uid)
        .filter(|uid| *uid >= FIRST_USER_UID)
        .max()
        .map_or(FIRST_USER_UID, |uid| uid + 1);
    let home = format!("/home/{name}");
    match data_root.create_directory(&home) {
        Ok(()) | Err(FsError::AlreadyExists) => {}
        Err(error) => return Err(error.into()),
    }
    let account = Account { name: name.to_owned(), uid, home };
    accounts.push(account.clone());
    data_root.write_file(PASSWD_PATH, render(&accounts).as_bytes())?;
    Ok(account)
}

#[cfg(test)]
mod tests {
    use super::{add_user, ensure_passwd, load, lookup, parse, Account, UserError, PASSWD_PATH};
    use crate::filesystem::DataRoot;
    use std::{assert, assert_eq, fs, matches};

    fn fixture() -> (std::path::PathBuf, DataRoot) {
        let root = crate::test_support::marked_root("users");
        let data = DataRoot::open(&root).unwrap();
        (root, data)
    }

    #[test]
    fn fresh_system_has_only_root_and_initialization_is_idempotent() {
        let (root, data) = fixture();
        ensure_passwd(&data).unwrap();
        fs::write(root.join("etc/passwd"), "root:0:/root\nalice:1000:/home/alice\n").unwrap();
        ensure_passwd(&data).unwrap();
        let accounts = load(&data).unwrap();
        assert_eq!(accounts.len(), 2);
        assert!(lookup(&accounts, "root").unwrap().is_admin());
        assert!(!lookup(&accounts, "alice").unwrap().is_admin());
    }

    #[test]
    fn root_adds_users_with_increasing_uids_and_home_directories() {
        let (root, data) = fixture();
        ensure_passwd(&data).unwrap();
        let admin = lookup(&load(&data).unwrap(), "root").unwrap().clone();
        let alice = add_user(&data, &admin, "alice").unwrap();
        let bob = add_user(&data, &admin, "bob").unwrap();
        assert_eq!(alice.uid, 1000);
        assert_eq!(bob.uid, 1001);
        assert!(root.join("home/alice").is_dir());
        assert!(root.join("home/bob").is_dir());
        assert!(matches!(add_user(&data, &admin, "alice"), Err(UserError::Duplicate(_))));
        assert!(matches!(add_user(&data, &admin, "Alice"), Err(UserError::InvalidName(_))));
        assert!(matches!(add_user(&data, &admin, "../x"), Err(UserError::InvalidName(_))));
        assert!(matches!(add_user(&data, &alice, "carol"), Err(UserError::PermissionDenied)));
        assert_eq!(load(&data).unwrap().len(), 3);
    }

    #[test]
    fn corrupt_passwd_is_rejected_not_guessed() {
        assert!(matches!(parse("root:0\n"), Err(UserError::Corrupt(1))));
        assert!(matches!(parse("root:0:/root\nroot:5:/x\n"), Err(UserError::Corrupt(2))));
        assert!(matches!(parse("root:zero:/root\n"), Err(UserError::Corrupt(1))));
        assert!(matches!(parse("root:0:relative\n"), Err(UserError::Corrupt(1))));
        let accounts = parse("root:0:/root\n\nalice:1000:/home/alice\n").unwrap();
        assert_eq!(accounts[1], Account { name: "alice".into(), uid: 1000, home: "/home/alice".into() });
    }

    #[test]
    fn accounts_live_only_in_the_virtual_root() {
        let (root, data) = fixture();
        ensure_passwd(&data).unwrap();
        assert!(root.join("etc/passwd").is_file());
        assert_eq!(data.read_file(PASSWD_PATH, 1024).unwrap(), b"root:0:/root\n");
    }
}
