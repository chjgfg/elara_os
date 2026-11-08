//! Managed logs: bounded append plus a length/hash commit journal; atomic
//! generation changes only at rotation. The public view is an internal hard
//! link, never a virtual-link feature. Buffered appends are NOT fsync promises.
use super::backend::linked;
use super::transaction::{read_magic, NamespaceGuard, LOG_GENERATION_TX_MAGIC, TX_OWNER};
use super::{CleanupStatus, DataRoot, DirectoryDurability, FsError};
use crate::config::{LOG_ARCHIVES, LOG_SEGMENT_BYTES, LOG_TAIL_BYTES, PRIVATE_LOG_DIRECTORY};
use cap_std::fs::{Dir, OpenOptions};
use std::format;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const STORE_MAGIC: &[u8] = b"ELARA_LOG_STORE_V2\n";
const MANIFEST_MAGIC: &str = "ELARA_LOG_MANIFEST_V2";
const GENERATION_MAGIC: &str = "ELARA_LOG_GENERATION_V2";
const VIEW: &str = "var/log/elara.log";
const MAINTENANCE_MAGIC: &[u8] = b"ELARA_LOG_MAINTENANCE_V2\n";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogPolicy {
    segment_bytes: usize,
    archives: usize,
}
impl Default for LogPolicy {
    fn default() -> Self {
        Self {
            segment_bytes: LOG_SEGMENT_BYTES,
            archives: LOG_ARCHIVES,
        }
    }
}
impl LogPolicy {
    pub fn new(segment_bytes: usize, archives: usize) -> Result<Self, FsError> {
        if !(64..=LOG_SEGMENT_BYTES).contains(&segment_bytes) || archives > LOG_ARCHIVES {
            return Err(FsError::InvalidLogPolicy);
        }
        Ok(Self {
            segment_bytes,
            archives,
        })
    }
    pub fn segment_bytes(self) -> usize {
        self.segment_bytes
    }
    pub fn archives(self) -> usize {
        self.archives
    }
    pub fn peak_body_bytes(self) -> usize {
        self.segment_bytes * (self.archives + 3)
    }
    pub fn journal_bytes(self) -> usize {
        self.segment_bytes * 4 + 256
    }
    /// Bound for names owned by this log protocol, not physical allocation or
    /// unrelated/unknown files. Shared /tmp transaction staging is separate.
    pub fn peak_named_bytes(self) -> usize {
        self.peak_body_bytes() + (self.archives + 2) * self.journal_bytes() + 4096
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogMode {
    Managed,
    LegacyReadOnly,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MirrorStatus {
    Current,
    Deferred(io::ErrorKind),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogDataDurability {
    HostBuffered,
    Synced,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogWriteProgress {
    Known(usize),
    Unconfirmed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogCommit {
    pub generation: u64,
    pub rotated: bool,
    pub data_durability: LogDataDurability,
    pub directory_durability: DirectoryDurability,
    pub cleanup: CleanupStatus,
    pub mirror: MirrorStatus,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogBudget {
    Managed {
        peak_body_bytes: usize,
        peak_named_bytes: usize,
    },
    Unmanaged,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogStats {
    pub budget: LogBudget,
    pub mode: LogMode,
    pub generation: u64,
    pub archives: usize,
    pub active_bytes: usize,
    pub retained_body_bytes: usize,
    pub peak_body_bound: usize,
    pub peak_named_bytes_bound: usize,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LogFault {
    None,
    #[cfg(test)]
    AfterGeneration,
    #[cfg(test)]
    AfterManifest,
    #[cfg(test)]
    BeforeCleanup,
    #[cfg(test)]
    PartialAppend,
    #[cfg(test)]
    PartialGeneration,
}
struct Manifest {
    policy: LogPolicy,
    active: u64,
    archives: Vec<u64>,
    pending: bool,
}
impl Manifest {
    fn encode(&self) -> Vec<u8> {
        format!(
            "{MANIFEST_MAGIC}\n{}\n{}\n{}\n{}\n{}\n",
            self.policy.segment_bytes,
            self.policy.archives,
            self.active,
            self.archives
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(","),
            u8::from(self.pending)
        )
        .into_bytes()
    }
    fn decode(bytes: &[u8], policy: LogPolicy) -> Result<Self, FsError> {
        let text = std::str::from_utf8(bytes).map_err(|_| FsError::CorruptLogStore)?;
        let lines: Vec<_> = text.lines().collect();
        if lines.len() != 6 || lines[0] != MANIFEST_MAGIC {
            return Err(FsError::CorruptLogStore);
        }
        let number = |text: &str| text.parse::<u64>().map_err(|_| FsError::CorruptLogStore);
        if number(lines[1])? != policy.segment_bytes as u64
            || number(lines[2])? != policy.archives as u64
        {
            return Err(FsError::InvalidLogPolicy);
        }
        let active = number(lines[3])?;
        let mut archives = Vec::new();
        if !lines[4].is_empty() {
            for part in lines[4].split(',') {
                let id = number(part)?;
                if id == 0 || id >= active || archives.contains(&id) {
                    return Err(FsError::CorruptLogStore);
                }
                archives.push(id);
            }
        }
        if active == 0 || archives.len() > policy.archives || !["0", "1"].contains(&lines[5]) {
            return Err(FsError::CorruptLogStore);
        }
        Ok(Self {
            policy,
            active,
            archives,
            pending: lines[5] == "1",
        })
    }
    fn contains(&self, id: u64) -> bool {
        self.active == id || self.archives.contains(&id)
    }
}
fn store_path() -> PathBuf {
    Path::new("var/log").join(PRIVATE_LOG_DIRECTORY)
}
fn generation(id: u64) -> String {
    format!("generation-{id}")
}
fn digest(bytes: &[u8]) -> String {
    crate::sha256::sha256(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn proof(bytes: &[u8]) -> Vec<u8> {
    format!("{} {}\n", bytes.len(), digest(bytes)).into_bytes()
}
fn journal_header(id: u64) -> Vec<u8> {
    format!("{GENERATION_MAGIC}\n{id}\n").into_bytes()
}
fn read_bounded(dir: &Dir, name: &str, maximum: usize) -> Result<Vec<u8>, FsError> {
    let metadata = dir.symlink_metadata(name)?;
    if linked(&metadata) || !metadata.is_file() {
        return Err(FsError::CorruptLogStore);
    }
    let mut bytes = Vec::new();
    dir.open(name)?
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(FsError::CorruptLogStore);
    }
    Ok(bytes)
}
fn owned_generation(store: &Dir, id: u64) -> Result<Dir, FsError> {
    let name = generation(id);
    let metadata = store.symlink_metadata(&name)?;
    if linked(&metadata) || !metadata.is_dir() {
        return Err(FsError::CorruptLogStore);
    }
    let dir = store.open_dir(name)?;
    let journal = read_bounded(&dir, "journal", LOG_SEGMENT_BYTES * 4 + 256)?;
    if !journal.starts_with(&journal_header(id)) {
        return Err(FsError::CorruptLogStore);
    }
    Ok(dir)
}
struct Committed {
    bytes: Vec<u8>,
    payload_length: usize,
    journal_length: usize,
    sealed_journal: usize,
}
fn committed(store: &Dir, id: u64, policy: LogPolicy) -> Result<Committed, FsError> {
    committed_checked(store, id, policy, |bytes, expected| {
        digest(bytes) == expected
    })
}
fn committed_checked<F>(
    store: &Dir,
    id: u64,
    policy: LogPolicy,
    verify: F,
) -> Result<Committed, FsError>
where
    F: FnOnce(&[u8], &str) -> bool,
{
    let dir = owned_generation(store, id)?;
    let journal = read_bounded(&dir, "journal", policy.journal_bytes())?;
    let header = journal_header(id);
    let sealed_journal = journal
        .iter()
        .rposition(|b| *b == b'\n')
        .map(|at| at + 1)
        .ok_or(FsError::CorruptLogStore)?;
    let text = std::str::from_utf8(&journal[header.len()..sealed_journal])
        .map_err(|_| FsError::CorruptLogStore)?;
    let mut last = None;
    let mut previous = 0;
    for line in text.lines() {
        let (length, hash) = line.split_once(' ').ok_or(FsError::CorruptLogStore)?;
        let length = length
            .parse::<usize>()
            .map_err(|_| FsError::CorruptLogStore)?;
        if length < previous
            || length > policy.segment_bytes
            || hash.len() != 64
            || !hash.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(FsError::CorruptLogStore);
        }
        previous = length;
        last = Some((length, hash));
    }
    let (length, hash) = last.ok_or(FsError::CorruptLogStore)?;
    let mut bytes = read_bounded(&dir, "payload", policy.segment_bytes)?;
    let payload_length = bytes.len();
    if length > bytes.len() || !verify(&bytes[..length], hash) {
        return Err(FsError::CorruptLogStore);
    }
    bytes.truncate(length);
    if (!bytes.is_empty() && !bytes.ends_with(b"\n")) || std::str::from_utf8(&bytes).is_err() {
        return Err(FsError::CorruptLogStore);
    }
    Ok(Committed {
        bytes,
        payload_length,
        journal_length: journal.len(),
        sealed_journal,
    })
}
fn repair_active(store: &Dir, id: u64, state: &Committed) -> Result<(), FsError> {
    let dir = owned_generation(store, id)?;
    // Only trim beyond a verified committed prefix; hash mismatches are preserved.
    if state.payload_length != state.bytes.len() {
        dir.open_with("payload", OpenOptions::new().write(true))?
            .set_len(state.bytes.len() as u64)?;
    }
    if state.journal_length != state.sealed_journal {
        dir.open_with("journal", OpenOptions::new().write(true))?
            .set_len(state.sealed_journal as u64)?;
    }
    Ok(())
}
fn error_kind(error: FsError) -> io::ErrorKind {
    match error {
        FsError::Io(error)
        | FsError::BusyPath(error)
        | FsError::AtomicPublishUnavailable(error)
        | FsError::AtomicReplaceUnavailable(error) => error.kind(),
        _ => io::ErrorKind::Other,
    }
}
fn merge_cleanup(first: CleanupStatus, second: CleanupStatus) -> CleanupStatus {
    if first != CleanupStatus::Complete {
        first
    } else {
        second
    }
}
impl DataRoot {
    #[cfg(test)]
    pub(super) fn log_active_without_hash_guard(
        &self,
        policy: LogPolicy,
    ) -> Result<Vec<u8>, FsError> {
        let _guard = self.namespace_guard()?;
        let store = self.existing_log_store()?.ok_or(FsError::UnownedLogStore)?;
        let manifest = self.manifest(&store, policy)?;
        Ok(committed_checked(&store, manifest.active, policy, |_, _| true)?.bytes)
    }
    fn existing_log_store(&self) -> Result<Option<Dir>, FsError> {
        let store = if let Some(store) = self.log_store_cache.get() {
            store.try_clone()?
        } else {
            let path = store_path();
            match self.dir.symlink_metadata(&path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.into()),
                Ok(metadata) if linked(&metadata) || !metadata.is_dir() => {
                    return Err(FsError::UnownedLogStore)
                }
                Ok(_) => {}
            }
            self.inspect_relative(&path)?;
            self.dir.open_dir(path)?
        };
        let marker = store
            .symlink_metadata("store.owner")
            .map_err(|_| FsError::UnownedLogStore)?;
        if linked(&marker) || !marker.is_file() || !read_magic(&store, "store.owner", STORE_MAGIC)?
        {
            return Err(FsError::UnownedLogStore);
        }
        let _ = self.log_store_cache.set(store.try_clone()?);
        Ok(Some(store))
    }
    fn manifest(&self, store: &Dir, policy: LogPolicy) -> Result<Manifest, FsError> {
        Manifest::decode(&read_bounded(store, "manifest", 1024)?, policy)
    }
    fn create_log_generation(
        &self,
        id: u64,
        bytes: &[u8],
        guard: &NamespaceGuard<'_>,
        _fault: LogFault,
    ) -> Result<(), FsError> {
        let target = store_path().join(generation(id));
        match self.dir.symlink_metadata(&target) {
            Ok(_) => return Err(FsError::AlreadyExists),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let (staging, name) = self.create_protocol_stage(guard)?;
        let dir = staging.open_dir(&name)?;
        let mut owner = dir.open_with(TX_OWNER, OpenOptions::new().write(true).create_new(true))?;
        owner.write_all(LOG_GENERATION_TX_MAGIC)?;
        owner.sync_all()?;
        drop(owner);
        let mut journal =
            dir.open_with("journal", OpenOptions::new().write(true).create_new(true))?;
        journal.write_all(&journal_header(id))?;
        journal.write_all(&proof(b""))?;
        journal.sync_all()?;
        let mut payload =
            dir.open_with("payload", OpenOptions::new().write(true).create_new(true))?;
        #[cfg(test)]
        if _fault == LogFault::PartialGeneration {
            payload.write_all(&bytes[..bytes.len() / 2])?;
            return Err(io::Error::other("injected partial log generation staging").into());
        }
        payload.write_all(bytes)?;
        payload.sync_all()?;
        if !bytes.is_empty() {
            journal.write_all(&proof(bytes))?;
            journal.sync_all()?;
        }
        drop(payload);
        drop(journal);
        drop(dir);
        self.inspect_relative(&store_path())?;
        staging
            .rename(&name, &self.dir, target)
            .map_err(FsError::AtomicRenameUnavailable)?;
        Ok(())
    }
    fn prune_log_generations(&self, store: &Dir, manifest: &Manifest) -> CleanupStatus {
        let result = (|| -> Result<(), FsError> {
            let mut names = Vec::new();
            for entry in store.entries()? {
                if names.len() >= 256 {
                    return Err(FsError::DirectorySnapshotTooLarge);
                }
                names.push(entry?.file_name());
            }
            for name in names {
                let Some(name) = name.to_str() else {
                    continue;
                };
                let Some(id) = name
                    .strip_prefix("generation-")
                    .and_then(|n| n.parse::<u64>().ok())
                else {
                    continue;
                };
                if id == 0 || name != generation(id) || manifest.contains(id) {
                    continue;
                }
                let Ok(dir) = owned_generation(store, id) else {
                    continue;
                };
                let mut entries = Vec::new();
                let mut unknown = false;
                for entry in dir.entries()? {
                    let entry = entry?;
                    let name = entry.file_name();
                    let meta = dir.symlink_metadata(&name)?;
                    if (!["journal", "payload"].iter().any(|part| name == *part)
                        && name != TX_OWNER)
                        || (name == TX_OWNER
                            && !read_magic(&dir, TX_OWNER, LOG_GENERATION_TX_MAGIC)?)
                        || linked(&meta)
                        || !meta.is_file()
                    {
                        unknown = true;
                        break;
                    }
                    entries.push(name);
                }
                if unknown {
                    continue;
                }
                for entry in entries {
                    dir.remove_file(entry)?;
                }
                drop(dir);
                store.remove_dir(name)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => CleanupStatus::Complete,
            Err(error) => CleanupStatus::Deferred(error_kind(error)),
        }
    }
    fn log_maintenance_pending(&self, store: &Dir) -> Result<bool, FsError> {
        match store.symlink_metadata("maintenance") {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
            Ok(metadata) if linked(&metadata) || !metadata.is_file() => {
                Err(FsError::CorruptLogStore)
            }
            Ok(_) => {
                if read_magic(store, "maintenance", MAINTENANCE_MAGIC)? {
                    Ok(true)
                } else {
                    Err(FsError::CorruptLogStore)
                }
            }
        }
    }
    fn mark_log_maintenance(&self, store: &Dir) -> Result<(), FsError> {
        if !self.log_maintenance_pending(store)? {
            let mut file = store.open_with(
                "maintenance",
                OpenOptions::new().write(true).create_new(true),
            )?;
            file.write_all(MAINTENANCE_MAGIC)?;
            file.sync_all()?;
        }
        Ok(())
    }
    fn finish_log_rotation(
        &self,
        store: &Dir,
        manifest: &mut Manifest,
        guard: &NamespaceGuard<'_>,
    ) -> Result<CleanupStatus, FsError> {
        self.mark_log_maintenance(store)?;
        let source = store_path()
            .join(generation(manifest.active))
            .join("payload");
        let view = self.publish_protocol_link_locked(&source, Path::new(VIEW), guard)?;
        let cleanup = merge_cleanup(view.cleanup, self.prune_log_generations(store, manifest));
        if cleanup != CleanupStatus::Complete {
            return Ok(cleanup);
        }
        if manifest.pending {
            manifest.pending = false;
            self.publish_protocol_locked(
                &store_path().join("manifest"),
                &manifest.encode(),
                false,
                guard,
            )?;
        }
        // This also resolves deferred payload/manifest/link staging under the
        // same lock. Keep the maintenance marker if any owned cleanup fails.
        if let Err(error) = self.recover_transactions_locked(guard) {
            return Ok(CleanupStatus::Deferred(error_kind(error)));
        }
        store.remove_file("maintenance")?;
        Ok(CleanupStatus::Complete)
    }
    pub fn initialize_log(&self, policy: LogPolicy) -> Result<LogMode, FsError> {
        let guard = self.namespace_guard()?;
        self.recover_transactions_locked(&guard)?;
        let store = match self.existing_log_store()? {
            Some(store) => store,
            None => {
                match self.dir.symlink_metadata(VIEW) {
                    Ok(metadata) if linked(&metadata) => {
                        return Err(FsError::Link(self.root.join(VIEW)))
                    }
                    Ok(metadata) if !metadata.is_file() => return Err(FsError::NotFile),
                    Ok(_) => return Ok(LogMode::LegacyReadOnly),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                self.inspect_relative(Path::new("var/log"))?;
                self.dir.create_dir(store_path())?;
                let store = self.dir.open_dir(store_path())?;
                let mut owner = store.open_with(
                    "store.owner",
                    OpenOptions::new().write(true).create_new(true),
                )?;
                owner.write_all(STORE_MAGIC)?;
                owner.sync_all()?;
                drop(owner);
                let _ = self.log_store_cache.set(store.try_clone()?);
                store
            }
        };
        let mut manifest = match store.symlink_metadata("manifest") {
            Ok(_) => self.manifest(&store, policy)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let manifest = Manifest {
                    policy,
                    active: 1,
                    archives: Vec::new(),
                    pending: true,
                };
                self.prune_log_generations(
                    &store,
                    &Manifest {
                        policy,
                        active: u64::MAX,
                        archives: Vec::new(),
                        pending: true,
                    },
                );
                self.create_log_generation(1, b"", &guard, LogFault::None)?;
                self.publish_protocol_locked(
                    &store_path().join("manifest"),
                    &manifest.encode(),
                    true,
                    &guard,
                )?;
                manifest
            }
            Err(error) => return Err(error.into()),
        };
        let state = committed(&store, manifest.active, policy)?;
        repair_active(&store, manifest.active, &state)?;
        for id in &manifest.archives {
            committed(&store, *id, policy)?;
        }
        if let CleanupStatus::Deferred(error) =
            self.finish_log_rotation(&store, &mut manifest, &guard)?
        {
            return Err(FsError::LogCleanupUnconfirmed {
                generation: manifest.active,
                error,
            });
        }
        Ok(LogMode::Managed)
    }
    pub fn append_log(&self, policy: LogPolicy, record: &[u8]) -> Result<LogCommit, FsError> {
        self.append_log_with_fault(policy, record, LogFault::None)
    }
    pub(super) fn append_log_with_fault(
        &self,
        policy: LogPolicy,
        record: &[u8],
        _fault: LogFault,
    ) -> Result<LogCommit, FsError> {
        if record.len() > policy.segment_bytes {
            return Err(FsError::LogRecordTooLarge);
        }
        if record.is_empty()
            || !record.ends_with(b"\n")
            || record[..record.len() - 1].contains(&b'\n')
            || std::str::from_utf8(record).is_err()
        {
            return Err(FsError::CorruptLogStore);
        }
        let guard = self.namespace_guard()?;
        let store = self
            .existing_log_store()?
            .ok_or(FsError::LegacyLogReadOnly)?;
        let mut manifest = self.manifest(&store, policy)?;
        if manifest.pending || self.log_maintenance_pending(&store)? {
            self.recover_transactions_locked(&guard)?;
            if let CleanupStatus::Deferred(error) =
                self.finish_log_rotation(&store, &mut manifest, &guard)?
            {
                return Err(FsError::LogMaintenanceBlocked {
                    generation: manifest.active,
                    error,
                });
            }
        }
        let state = committed(&store, manifest.active, policy)?;
        repair_active(&store, manifest.active, &state)?;
        let mut next = state.bytes.clone();
        next.extend_from_slice(record);
        let receipt_line = proof(&next);
        let rotated = next.len() > policy.segment_bytes
            || state.sealed_journal + receipt_line.len() > policy.journal_bytes();
        #[cfg(test)]
        let rotated = rotated
            || [
                LogFault::AfterGeneration,
                LogFault::AfterManifest,
                LogFault::BeforeCleanup,
                LogFault::PartialGeneration,
            ]
            .contains(&_fault);
        if !rotated {
            let dir = owned_generation(&store, manifest.active)?;
            let mut payload = dir.open_with("payload", OpenOptions::new().append(true))?;
            #[cfg(test)]
            if _fault == LogFault::PartialAppend {
                let count = payload.write(&record[..record.len() / 2])?;
                return Err(FsError::LogAppendUnconfirmed {
                    progress: LogWriteProgress::Known(count),
                    error: io::ErrorKind::WriteZero,
                });
            }
            let count = payload
                .write(record)
                .map_err(|error| FsError::LogAppendUnconfirmed {
                    progress: LogWriteProgress::Unconfirmed,
                    error: error.kind(),
                })?;
            if count != record.len() {
                return Err(FsError::LogAppendUnconfirmed {
                    progress: LogWriteProgress::Known(count),
                    error: io::ErrorKind::WriteZero,
                });
            }
            let mut journal = dir
                .open_with("journal", OpenOptions::new().append(true))
                .map_err(|error| FsError::LogAppendUnconfirmed {
                    progress: LogWriteProgress::Known(count),
                    error: error.kind(),
                })?;
            let committed =
                journal
                    .write(&receipt_line)
                    .map_err(|error| FsError::LogAppendUnconfirmed {
                        progress: LogWriteProgress::Known(count),
                        error: error.kind(),
                    })?;
            if committed != receipt_line.len() {
                return Err(FsError::LogAppendUnconfirmed {
                    progress: LogWriteProgress::Known(count),
                    error: io::ErrorKind::WriteZero,
                });
            }
            return Ok(LogCommit {
                generation: manifest.active,
                rotated: false,
                data_durability: LogDataDurability::HostBuffered,
                directory_durability: DirectoryDurability::Unconfirmed(io::ErrorKind::Unsupported),
                cleanup: CleanupStatus::Complete,
                mirror: MirrorStatus::Current,
            });
        }
        if let CleanupStatus::Deferred(error) = self.prune_log_generations(&store, &manifest) {
            return Err(FsError::LogMaintenanceBlocked {
                generation: manifest.active,
                error,
            });
        }
        let old = owned_generation(&store, manifest.active)?;
        old.open_with("payload", OpenOptions::new().write(true))?
            .sync_all()?;
        old.open_with("journal", OpenOptions::new().write(true))?
            .sync_all()?;
        drop(old);
        manifest.archives.insert(0, manifest.active);
        manifest.archives.truncate(policy.archives);
        manifest.active = manifest
            .active
            .checked_add(1)
            .ok_or(FsError::CorruptLogStore)?;
        manifest.pending = true;
        self.mark_log_maintenance(&store)?;
        self.create_log_generation(manifest.active, record, &guard, _fault)?;
        #[cfg(test)]
        if _fault == LogFault::AfterGeneration {
            return Err(io::Error::other("injected after log generation").into());
        }
        let published = self.publish_protocol_locked(
            &store_path().join("manifest"),
            &manifest.encode(),
            false,
            &guard,
        )?;
        #[cfg(test)]
        if _fault == LogFault::AfterManifest {
            return Ok(LogCommit {
                generation: manifest.active,
                rotated: true,
                data_durability: LogDataDurability::Synced,
                directory_durability: published.directory_durability,
                cleanup: CleanupStatus::Deferred(io::ErrorKind::Other),
                mirror: MirrorStatus::Deferred(io::ErrorKind::Other),
            });
        }
        #[cfg(test)]
        if _fault == LogFault::BeforeCleanup {
            return Ok(LogCommit {
                generation: manifest.active,
                rotated: true,
                data_durability: LogDataDurability::Synced,
                directory_durability: published.directory_durability,
                cleanup: CleanupStatus::Deferred(io::ErrorKind::Other),
                mirror: MirrorStatus::Deferred(io::ErrorKind::Other),
            });
        }
        let followup = self.finish_log_rotation(&store, &mut manifest, &guard);
        let (cleanup, mirror) = match followup {
            Ok(cleanup) => (
                merge_cleanup(published.cleanup, cleanup),
                MirrorStatus::Current,
            ),
            Err(error) => {
                let kind = error_kind(error);
                (CleanupStatus::Deferred(kind), MirrorStatus::Deferred(kind))
            }
        };
        Ok(LogCommit {
            generation: manifest.active,
            rotated: true,
            data_durability: LogDataDurability::Synced,
            directory_durability: published.directory_durability,
            cleanup,
            mirror,
        })
    }
    pub fn log_tail(&self, policy: LogPolicy, count: usize) -> Result<Vec<String>, FsError> {
        if count == 0 || count > 1000 {
            return Err(FsError::LogTailIncomplete);
        }
        let guard = self.namespace_guard()?;
        let Some(store) = self.existing_log_store()? else {
            drop(guard);
            self.inspect_relative(Path::new(VIEW))?;
            let mut file = self.dir.open(VIEW)?;
            let length = file.metadata()?.len();
            let start = length.saturating_sub(LOG_TAIL_BYTES as u64);
            file.seek(SeekFrom::Start(start))?;
            let mut bytes = Vec::new();
            file.take(LOG_TAIL_BYTES as u64).read_to_end(&mut bytes)?;
            let bytes = if start != 0 {
                let at = bytes
                    .iter()
                    .position(|b| *b == b'\n')
                    .ok_or(FsError::LogTailIncomplete)?;
                &bytes[at + 1..]
            } else {
                &bytes
            };
            let text = std::str::from_utf8(bytes).map_err(|_| FsError::CorruptLogStore)?;
            let lines: Vec<_> = text.lines().collect();
            if start != 0 && lines.len() < count {
                return Err(FsError::LogTailIncomplete);
            }
            return Ok(lines[lines.len().saturating_sub(count)..]
                .iter()
                .map(|s| (*s).to_owned())
                .collect());
        };
        let manifest = self.manifest(&store, policy)?;
        let mut lines = Vec::new();
        for id in std::iter::once(manifest.active).chain(manifest.archives.iter().copied()) {
            let state = committed(&store, id, policy)?;
            let text = std::str::from_utf8(&state.bytes).map_err(|_| FsError::CorruptLogStore)?;
            for line in text.lines().rev() {
                if lines.len() == count {
                    break;
                }
                lines.push(line.to_owned());
            }
            if lines.len() == count {
                break;
            }
        }
        lines.reverse();
        Ok(lines)
    }
    pub fn log_stats(&self, policy: LogPolicy) -> Result<LogStats, FsError> {
        let _guard = self.namespace_guard()?;
        let Some(store) = self.existing_log_store()? else {
            self.inspect_relative(Path::new(VIEW))?;
            let bytes = usize::try_from(self.dir.metadata(VIEW)?.len())
                .map_err(|_| FsError::InvalidOffset)?;
            return Ok(LogStats {
                budget: LogBudget::Unmanaged,
                mode: LogMode::LegacyReadOnly,
                generation: 0,
                archives: 0,
                active_bytes: bytes,
                retained_body_bytes: bytes,
                peak_body_bound: 0,
                peak_named_bytes_bound: 0,
            });
        };
        let manifest = self.manifest(&store, policy)?;
        let active_bytes = committed(&store, manifest.active, policy)?.bytes.len();
        let mut retained_body_bytes = active_bytes;
        for id in &manifest.archives {
            retained_body_bytes += committed(&store, *id, policy)?.bytes.len();
        }
        Ok(LogStats {
            budget: LogBudget::Managed {
                peak_body_bytes: policy.peak_body_bytes(),
                peak_named_bytes: policy.peak_named_bytes(),
            },
            mode: LogMode::Managed,
            generation: manifest.active,
            archives: manifest.archives.len(),
            active_bytes,
            retained_body_bytes,
            peak_body_bound: policy.peak_body_bytes(),
            peak_named_bytes_bound: policy.peak_named_bytes(),
        })
    }
}
