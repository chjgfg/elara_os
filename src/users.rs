//! User accounts stored in the virtual `/etc/passwd`, with salted password
//! hashes in `/etc/shadow`. The permission model is deliberately small:
//! `root` (uid 0) is the only administrator; every other account is a regular
//! user. Accounts live only inside the virtual root and have no relation to
//! host OS accounts.
use crate::filesystem::{DataRoot, FsError};
use crate::sha256;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{format, write};

pub const PASSWD_PATH: &str = "/etc/passwd";
pub const SHADOW_PATH: &str = "/etc/shadow";
pub const ROOT_UID: u32 = 0;
const FIRST_USER_UID: u32 = 1000;
const MAX_PASSWD_BYTES: u64 = 64 * 1024;
const HASH_ROUNDS: u32 = 10_000;
const MIN_PASSWORD_LEN: usize = 4;

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
    InvalidPassword,
    Duplicate(String),
    Unknown(String),
    PermissionDenied,
    WrongPassword,
}

impl fmt::Display for UserError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fs(error) => write!(output, "{error}"),
            Self::Corrupt(line) => write!(output, "Account database line {line} is malformed."),
            Self::InvalidName(name) => write!(
                output,
                "'{name}' is not a valid user name: use 1-32 lowercase letters, digits, '_' or '-', starting with a letter."
            ),
            Self::InvalidPassword => write!(
                output,
                "Passwords need at least {MIN_PASSWORD_LEN} characters and no spaces or control characters."
            ),
            Self::Duplicate(name) => write!(output, "User '{name}' already exists."),
            Self::Unknown(name) => write!(output, "Unknown user '{name}'."),
            Self::PermissionDenied => write!(output, "Only root can manage users."),
            Self::WrongPassword => write!(output, "Authentication failed."),
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
    let valid_rest =
        chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
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
        if !home.starts_with('/')
            || accounts
                .iter()
                .any(|existing: &Account| existing.name == *name || existing.uid == uid)
        {
            return Err(UserError::Corrupt(index + 1));
        }
        accounts.push(Account {
            name: (*name).to_owned(),
            uid,
            home: (*home).to_owned(),
        });
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

/// Create `/etc/passwd` (only `root`) and `/etc/shadow` when they do not exist yet.
/// A fresh root account has no password until root sets one; until then only
/// the initial root session can act as root.
pub fn ensure_passwd(data_root: &DataRoot) -> Result<(), UserError> {
    match data_root.create_file(SHADOW_PATH) {
        Ok(()) | Err(FsError::AlreadyExists) => {}
        Err(error) => return Err(error.into()),
    }
    match data_root.create_file(PASSWD_PATH) {
        Ok(()) => {}
        Err(FsError::AlreadyExists) => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let root = Account {
        name: "root".into(),
        uid: ROOT_UID,
        home: "/root".into(),
    };
    data_root.write_file(PASSWD_PATH, render(&[root]).as_bytes())?;
    Ok(())
}

pub fn lookup<'a>(accounts: &'a [Account], name: &str) -> Result<&'a Account, UserError> {
    accounts
        .iter()
        .find(|account| account.name == name)
        .ok_or_else(|| UserError::Unknown(name.to_owned()))
}

fn validate_password(password: &str) -> Result<(), UserError> {
    if password.chars().count() < MIN_PASSWORD_LEN
        || password
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == ':')
    {
        return Err(UserError::InvalidPassword);
    }
    Ok(())
}

fn hash_password(salt: &str, password: &str) -> String {
    let mut digest = sha256::sha256(format!("{salt}:{password}").as_bytes());
    for _ in 1..HASH_ROUNDS {
        let mut input = Vec::with_capacity(32 + salt.len());
        input.extend_from_slice(&digest);
        input.extend_from_slice(salt.as_bytes());
        digest = sha256::sha256(&input);
    }
    sha256::hex(&digest)
}

fn fresh_salt(name: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let seed = format!("{name}:{nanos}:{}", std::process::id());
    sha256::hex(&sha256::sha256(seed.as_bytes()))[..16].to_owned()
}

/// `name:salt:hash` per line; a missing line means the account has no password.
fn load_shadow(data_root: &DataRoot) -> Result<Vec<(String, String, String)>, UserError> {
    let bytes = data_root.read_file(SHADOW_PATH, MAX_PASSWD_BYTES)?;
    let text = String::from_utf8(bytes).map_err(|_| UserError::Corrupt(0))?;
    let mut entries = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split(':').collect();
        let [name, salt, hash] = fields.as_slice() else {
            return Err(UserError::Corrupt(index + 1));
        };
        entries.push(((*name).to_owned(), (*salt).to_owned(), (*hash).to_owned()));
    }
    Ok(entries)
}

fn save_shadow(
    data_root: &DataRoot,
    entries: &[(String, String, String)],
) -> Result<(), UserError> {
    let text: String = entries
        .iter()
        .map(|(name, salt, hash)| format!("{name}:{salt}:{hash}\n"))
        .collect();
    data_root.write_file(SHADOW_PATH, text.as_bytes())?;
    Ok(())
}

/// Whether the account has a password entry in `/etc/shadow`.
pub fn has_password(data_root: &DataRoot, name: &str) -> Result<bool, UserError> {
    Ok(load_shadow(data_root)?
        .iter()
        .any(|(existing, _, _)| existing == name))
}

/// Set or replace a password. Root may set anyone's; a user may set only its own.
pub fn set_password(
    data_root: &DataRoot,
    actor: &Account,
    name: &str,
    password: &str,
) -> Result<(), UserError> {
    if !actor.is_admin() && actor.name != name {
        return Err(UserError::PermissionDenied);
    }
    lookup(&load(data_root)?, name)?;
    validate_password(password)?;
    let salt = fresh_salt(name);
    let hash = hash_password(&salt, password);
    let mut entries = load_shadow(data_root)?;
    entries.retain(|(existing, _, _)| existing != name);
    entries.push((name.to_owned(), salt, hash));
    save_shadow(data_root, &entries)
}

/// Verify a password without revealing whether the account has one.
pub fn authenticate(
    data_root: &DataRoot,
    name: &str,
    password: &str,
) -> Result<Account, UserError> {
    let account = lookup(&load(data_root)?, name)?.clone();
    let entries = load_shadow(data_root)?;
    let Some((_, salt, expected)) = entries.iter().find(|(existing, _, _)| existing == name) else {
        return Err(UserError::WrongPassword);
    };
    let actual = hash_password(salt, password);
    let mut difference = actual.len() ^ expected.len();
    for (left, right) in actual.bytes().zip(expected.bytes()) {
        difference |= usize::from(left ^ right);
    }
    if difference != 0 {
        return Err(UserError::WrongPassword);
    }
    Ok(account)
}

pub fn add_user(
    data_root: &DataRoot,
    actor: &Account,
    name: &str,
    password: &str,
) -> Result<Account, UserError> {
    if !actor.is_admin() {
        return Err(UserError::PermissionDenied);
    }
    validate_name(name)?;
    validate_password(password)?;
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
    let account = Account {
        name: name.to_owned(),
        uid,
        home,
    };
    accounts.push(account.clone());
    data_root.write_file(PASSWD_PATH, render(&accounts).as_bytes())?;
    set_password(data_root, actor, name, password)?;
    Ok(account)
}

#[cfg(test)]
mod tests {
    use super::{
        add_user, authenticate, ensure_passwd, load, lookup, parse, set_password, Account,
        UserError, PASSWD_PATH,
    };
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
        fs::write(
            root.join("etc/passwd"),
            "root:0:/root\nalice:1000:/home/alice\n",
        )
        .unwrap();
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
        let alice = add_user(&data, &admin, "alice", "secret1").unwrap();
        let bob = add_user(&data, &admin, "bob", "secret2").unwrap();
        assert_eq!(alice.uid, 1000);
        assert_eq!(bob.uid, 1001);
        assert!(root.join("home/alice").is_dir());
        assert!(root.join("home/bob").is_dir());
        assert!(matches!(
            add_user(&data, &admin, "alice", "secret1"),
            Err(UserError::Duplicate(_))
        ));
        assert!(matches!(
            add_user(&data, &admin, "Alice", "secret1"),
            Err(UserError::InvalidName(_))
        ));
        assert!(matches!(
            add_user(&data, &admin, "../x", "secret1"),
            Err(UserError::InvalidName(_))
        ));
        assert!(matches!(
            add_user(&data, &admin, "carol", "abc"),
            Err(UserError::InvalidPassword)
        ));
        assert!(matches!(
            add_user(&data, &alice, "carol", "secret3"),
            Err(UserError::PermissionDenied)
        ));
        assert_eq!(load(&data).unwrap().len(), 3);
    }

    #[test]
    fn passwords_are_hashed_salted_and_verified() {
        let (root, data) = fixture();
        ensure_passwd(&data).unwrap();
        let admin = lookup(&load(&data).unwrap(), "root").unwrap().clone();
        let alice = add_user(&data, &admin, "alice", "correct horse").unwrap_err();
        assert!(matches!(alice, UserError::InvalidPassword));
        let alice = add_user(&data, &admin, "alice", "correct-horse").unwrap();
        let shadow = fs::read_to_string(root.join("etc/shadow")).unwrap();
        assert!(shadow.starts_with("alice:"));
        assert!(!shadow.contains("correct-horse"));
        assert_eq!(
            authenticate(&data, "alice", "correct-horse").unwrap(),
            alice
        );
        assert!(matches!(
            authenticate(&data, "alice", "wrong"),
            Err(UserError::WrongPassword)
        ));
        assert!(matches!(
            authenticate(&data, "root", "anything"),
            Err(UserError::WrongPassword)
        ));
        set_password(&data, &admin, "root", "root-pass").unwrap();
        assert!(authenticate(&data, "root", "root-pass").is_ok());
        assert!(matches!(
            set_password(&data, &alice, "root", "hijack"),
            Err(UserError::PermissionDenied)
        ));
        set_password(&data, &alice, "alice", "new-pass").unwrap();
        assert!(authenticate(&data, "alice", "new-pass").is_ok());
        assert!(authenticate(&data, "alice", "correct-horse").is_err());
    }

    #[test]
    fn corrupt_passwd_is_rejected_not_guessed() {
        assert!(matches!(parse("root:0\n"), Err(UserError::Corrupt(1))));
        assert!(matches!(
            parse("root:0:/root\nroot:5:/x\n"),
            Err(UserError::Corrupt(2))
        ));
        assert!(matches!(
            parse("root:zero:/root\n"),
            Err(UserError::Corrupt(1))
        ));
        assert!(matches!(
            parse("root:0:relative\n"),
            Err(UserError::Corrupt(1))
        ));
        let accounts = parse("root:0:/root\n\nalice:1000:/home/alice\n").unwrap();
        assert_eq!(
            accounts[1],
            Account {
                name: "alice".into(),
                uid: 1000,
                home: "/home/alice".into()
            }
        );
    }

    #[test]
    fn accounts_live_only_in_the_virtual_root() {
        let (root, data) = fixture();
        ensure_passwd(&data).unwrap();
        assert!(root.join("etc/passwd").is_file());
        assert_eq!(
            data.read_file(PASSWD_PATH, 1024).unwrap(),
            b"root:0:/root\n"
        );
    }
}
