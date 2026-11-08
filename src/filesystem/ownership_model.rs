//! Versioned text ownership records. IDs belong to this virtual filesystem,
//! never to a guessed host inode, pathname, mtime, or account directory prefix.
use super::FsError;
use crate::virtual_path::VirtualPath;
use std::collections::BTreeMap;
use std::{format, write};
use std::fmt::Write as _;

pub(super) const MAX_OWNED_OBJECTS: usize = 4096;
pub(super) const MAX_CATALOG_BYTES: usize = 400 * 1024;
const MAGIC: &str = "ELARA_OWNERSHIP_CATALOG_V1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId(pub(super) u64);
impl ObjectId { pub fn value(self) -> u64 { self.0 } }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VirtualMode(u16);
impl VirtualMode {
    pub fn new(bits: u16) -> Result<Self, FsError> {
        if bits & !0o777 != 0 { return Err(FsError::InvalidOwnershipRecord); }
        Ok(Self(bits))
    }
    pub fn bits(self) -> u16 { self.0 }
    pub fn permits(self, owner: FileOwner, uid: u32, read: bool, write: bool) -> bool {
        if uid == 0 { return true; }
        // Initial virtual groups are private primary groups, gid == account uid.
        let shift = if uid == owner.uid { 6 } else if uid == owner.gid { 3 } else { 0 };
        let granted = ((self.0 >> shift) & 7) as u8;
        (!read || granted & 4 != 0) && (!write || granted & 2 != 0)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileOwner { pub uid: u32, pub gid: u32 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteCondition { OwnerRequest }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnedStorageClass { UserData, Temporary }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedFileInfo {
    pub id: ObjectId,
    pub storage_generation: u64,
    pub path: String,
    pub owner: FileOwner,
    pub mode: VirtualMode,
    pub storage_class: OwnedStorageClass,
    pub delete_condition: DeleteCondition,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnershipState {
    LegacyUnknown,
    Managed(OwnedFileInfo),
    Unconfirmed { id: ObjectId, reason: std::io::ErrorKind },
}

pub(super) fn key(path: &str) -> Result<String, FsError> {
    let path = VirtualPath::strict(path).map_err(|_| FsError::InvalidPath)?.to_string();
    if std::cfg!(windows) { Ok(path.to_ascii_lowercase()) } else { Ok(path) }
}
fn hex(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len() * 2);
    for byte in text.as_bytes() { write!(encoded, "{byte:02x}").expect("string formatting"); }
    encoded
}
fn unhex(text: &str) -> Result<String, FsError> {
    if text.len() % 2 != 0 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(FsError::InvalidOwnershipRecord);
    }
    let mut bytes = Vec::with_capacity(text.len()/2);
    for chunk in text.as_bytes().chunks_exact(2) {
        let digit = |b: u8| match b { b'0'..=b'9' => Some(b-b'0'), b'a'..=b'f' => Some(b-b'a'+10), _ => None };
        let high = digit(chunk[0]).ok_or(FsError::InvalidOwnershipRecord)?;
        let low = digit(chunk[1]).ok_or(FsError::InvalidOwnershipRecord)?;
        bytes.push((high << 4) | low);
    }
    String::from_utf8(bytes).map_err(|_| FsError::InvalidOwnershipRecord)
}
#[derive(Clone)]
pub(super) struct Catalog {
    pub revision: u64,
    pub next_id: u64,
    pub entries: BTreeMap<String, OwnedFileInfo>,
}
impl Catalog {
    pub fn empty() -> Self { Self { revision: 1, next_id: 1, entries: BTreeMap::new() } }
    pub fn allocate(&mut self, path: &str, owner: FileOwner, mode: VirtualMode, storage_class: OwnedStorageClass) -> Result<OwnedFileInfo, FsError> {
        if self.entries.len() >= MAX_OWNED_OBJECTS { return Err(FsError::OwnershipCapacity); }
        let path = key(path)?;
        if self.entries.contains_key(&path) { return Err(FsError::AlreadyExists); }
        let id = self.next_id;
        let next_id = id.checked_add(1).ok_or(FsError::OwnershipIdentityExhausted)?;
        let revision = self.revision.checked_add(1).ok_or(FsError::OwnershipIdentityExhausted)?;
        self.next_id = next_id;
        self.revision = revision;
        let info = OwnedFileInfo { id: ObjectId(id), storage_generation: 1, path: path.clone(), owner, mode, storage_class, delete_condition: DeleteCondition::OwnerRequest };
        self.entries.insert(path, info.clone());
        Ok(info)
    }
    pub fn encode(&self) -> Result<Vec<u8>, FsError> {
        let mut text = format!("{MAGIC}\n{} {}\n", self.revision, self.next_id);
        for info in self.entries.values() {
            write!(text, "{} {} {} {} {:03o} {} {}\n", info.id.0, info.storage_generation, info.owner.uid, info.owner.gid, info.mode.0,
                match info.storage_class { OwnedStorageClass::UserData => "user", OwnedStorageClass::Temporary => "tmp" }, hex(&info.path))
                .map_err(|_| FsError::InvalidOwnershipRecord)?;
            if text.len() > MAX_CATALOG_BYTES { return Err(FsError::OwnershipCapacity); }
        }
        Ok(text.into_bytes())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, FsError> {
        if bytes.len() > MAX_CATALOG_BYTES || !bytes.ends_with(b"\n") { return Err(FsError::InvalidOwnershipRecord); }
        let text = std::str::from_utf8(bytes).map_err(|_| FsError::InvalidOwnershipRecord)?;
        let mut lines = text.lines();
        if lines.next() != Some(MAGIC) { return Err(FsError::InvalidOwnershipRecord); }
        let fields: Vec<_> = lines.next().ok_or(FsError::InvalidOwnershipRecord)?.split(' ').collect();
        if fields.len() != 2 { return Err(FsError::InvalidOwnershipRecord); }
        let revision = fields[0].parse::<u64>().map_err(|_| FsError::InvalidOwnershipRecord)?;
        let next_id = fields[1].parse::<u64>().map_err(|_| FsError::InvalidOwnershipRecord)?;
        if revision == 0 || next_id == 0 { return Err(FsError::InvalidOwnershipRecord); }
        let mut entries = BTreeMap::new();
        let mut ids = std::collections::BTreeSet::new();
        for line in lines {
            if entries.len() >= MAX_OWNED_OBJECTS { return Err(FsError::OwnershipCapacity); }
            let fields: Vec<_> = line.split(' ').collect();
            if fields.len() != 7 { return Err(FsError::InvalidOwnershipRecord); }
            let id = fields[0].parse::<u64>().map_err(|_| FsError::InvalidOwnershipRecord)?;
            let generation = fields[1].parse::<u64>().map_err(|_| FsError::InvalidOwnershipRecord)?;
            if generation == 0 { return Err(FsError::InvalidOwnershipRecord); }
            let uid = fields[2].parse::<u32>().map_err(|_| FsError::InvalidOwnershipRecord)?;
            let gid = fields[3].parse::<u32>().map_err(|_| FsError::InvalidOwnershipRecord)?;
            let mode = VirtualMode::new(u16::from_str_radix(fields[4], 8).map_err(|_| FsError::InvalidOwnershipRecord)?)?;
            let storage_class = match fields[5] { "user" => OwnedStorageClass::UserData, "tmp" => OwnedStorageClass::Temporary, _ => return Err(FsError::InvalidOwnershipRecord) };
            let path = unhex(fields[6])?;
            if id == 0 || id >= next_id || key(&path)? != path || !ids.insert(id) || entries.contains_key(&path) {
                return Err(FsError::InvalidOwnershipRecord);
            }
            entries.insert(path.clone(), OwnedFileInfo { id: ObjectId(id), storage_generation: generation, path, owner: FileOwner { uid, gid }, mode, storage_class,
                delete_condition: DeleteCondition::OwnerRequest });
        }
        let catalog = Self { revision, next_id, entries };
        if catalog.encode()?.as_slice() != bytes { return Err(FsError::InvalidOwnershipRecord); }
        Ok(catalog)
    }
}
