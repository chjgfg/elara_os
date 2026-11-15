//! Fixed-size image slots, bounded write-back cache, and two checksummed COW roots.
//! This module receives a capability, never an ambient path. Drop never publishes.
use crate::sha256::sha256;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::{format, vec};

pub const BLOCK: usize = 4096;
const MAGIC: &[u8; 8] = b"ELARABK1";
const MAX_ENTRIES: usize = 32768;
const ENTRY: usize = 51;
const HEADER: usize = 64;
pub type Key = (u8, u64, u32);
pub type Records = BTreeMap<Key, Vec<u8>>;

/// A caller must serialize access for the capability's entire lifetime.
pub trait Device: Read + Write + Seek {
    fn sync(&mut self) -> io::Result<()>;
    fn resize(&mut self, bytes: u64) -> io::Result<()>;
}
impl Device for std::fs::File {
    fn sync(&mut self) -> io::Result<()> {
        self.sync_all()
    }
    fn resize(&mut self, bytes: u64) -> io::Result<()> {
        self.set_len(bytes)
    }
}
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Invalid,
    NoSpace,
    ReadOnly,
    Bounds,
}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("block store: {self:?}"))
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    Clean,
    RecoveredReadOnly { selected_generation: u64 },
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub hits: u64,
    pub misses: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub writebacks: u64,
    pub cached_pages: usize,
    pub dirty_pages: usize,
    pub allocated_slots: u32,
    pub free_slots: u32,
    pub largest_free_run: u32,
    pub generation: u64,
}
struct Page {
    bytes: Vec<u8>,
    dirty: bool,
    touched: u64,
}
struct Cache<D> {
    device: D,
    pages: BTreeMap<u32, Page>,
    capacity: usize,
    tick: u64,
    stats: Stats,
}
impl<D: Device> Cache<D> {
    fn new(device: D, capacity: usize) -> Result<Self, Error> {
        if !(1..=512).contains(&capacity) {
            return Err(Error::Bounds);
        }
        Ok(Self {
            device,
            pages: BTreeMap::new(),
            capacity,
            tick: 0,
            stats: Stats::default(),
        })
    }
    fn raw_write(&mut self, slot: u32, bytes: &[u8]) -> Result<(), Error> {
        self.device
            .seek(SeekFrom::Start(u64::from(slot) * BLOCK as u64))?;
        self.device.write_all(bytes)?;
        self.stats.bytes_written = self.stats.bytes_written.saturating_add(bytes.len() as u64);
        self.stats.writebacks = self.stats.writebacks.saturating_add(1);
        Ok(())
    }
    fn writeback(&mut self, slot: u32) -> Result<(), Error> {
        if self.pages.get(&slot).is_some_and(|p| p.dirty) {
            let bytes = self.pages[&slot].bytes.clone();
            self.raw_write(slot, &bytes)?;
            self.pages.get_mut(&slot).ok_or(Error::Invalid)?.dirty = false;
        }
        Ok(())
    }
    fn put(&mut self, slot: u32, bytes: Vec<u8>, dirty: bool) -> Result<(), Error> {
        if !self.pages.contains_key(&slot) && self.pages.len() == self.capacity {
            let victim = *self
                .pages
                .iter()
                .min_by_key(|(_, p)| p.touched)
                .ok_or(Error::Invalid)?
                .0;
            self.writeback(victim)?;
            self.pages.remove(&victim);
        }
        self.tick = self.tick.checked_add(1).ok_or(Error::Bounds)?;
        self.pages.insert(
            slot,
            Page {
                bytes,
                dirty,
                touched: self.tick,
            },
        );
        Ok(())
    }
    fn read(&mut self, slot: u32) -> Result<Vec<u8>, Error> {
        self.tick = self.tick.checked_add(1).ok_or(Error::Bounds)?;
        if let Some(page) = self.pages.get_mut(&slot) {
            self.stats.hits = self.stats.hits.saturating_add(1);
            page.touched = self.tick;
            return Ok(page.bytes.clone());
        }
        self.stats.misses = self.stats.misses.saturating_add(1);
        let mut bytes = vec![0; BLOCK];
        self.device
            .seek(SeekFrom::Start(u64::from(slot) * BLOCK as u64))?;
        self.device.read_exact(&mut bytes)?;
        self.stats.bytes_read = self.stats.bytes_read.saturating_add(BLOCK as u64);
        self.put(slot, bytes.clone(), false)?;
        Ok(bytes)
    }
    fn flush(&mut self) -> Result<(), Error> {
        let dirty: Vec<_> = self
            .pages
            .iter()
            .filter(|(_, p)| p.dirty)
            .map(|(s, _)| *s)
            .collect();
        for slot in dirty {
            self.writeback(slot)?;
        }
        self.device.sync()?;
        Ok(())
    }
}
#[derive(Clone)]
struct Location {
    slot: u32,
    length: u16,
    digest: [u8; 32],
}
#[derive(Clone)]
struct Root {
    generation: u64,
    manifest: Vec<u32>,
    entries: BTreeMap<Key, Location>,
}
pub struct Store<D> {
    cache: Cache<D>,
    total: u32,
    roots: [Option<Root>; 2],
    selected: usize,
    recovery: Recovery,
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32, Error> {
    Ok(u32::from_le_bytes(
        bytes
            .get(at..at + 4)
            .ok_or(Error::Invalid)?
            .try_into()
            .map_err(|_| Error::Invalid)?,
    ))
}
fn u64_at(bytes: &[u8], at: usize) -> Result<u64, Error> {
    Ok(u64::from_le_bytes(
        bytes
            .get(at..at + 8)
            .ok_or(Error::Invalid)?
            .try_into()
            .map_err(|_| Error::Invalid)?,
    ))
}
impl<D: Device> Store<D> {
    /// Validate capacity before the caller creates an owned image object.
    pub fn validate_initial(
        records: &Records,
        total: u32,
        cache_pages: usize,
    ) -> Result<(), Error> {
        if !(8..=65536).contains(&total)
            || !(1..=512).contains(&cache_pages)
            || records.len() > MAX_ENTRIES
            || records.values().any(|b| b.is_empty() || b.len() > BLOCK)
        {
            return Err(Error::Bounds);
        }
        let pages = (records.len() * ENTRY).div_ceil(BLOCK);
        if HEADER + pages * 4 > BLOCK - 32 || records.len() + pages + 2 > total as usize {
            return Err(Error::NoSpace);
        }
        Ok(())
    }
    /// Requires a brand-new empty object; never formats existing bytes.
    pub fn format(mut device: D, total: u32, cache_pages: usize) -> Result<Self, Error> {
        if !(8..=65536).contains(&total) || device.seek(SeekFrom::End(0))? != 0 {
            return Err(Error::Bounds);
        }
        if !(1..=512).contains(&cache_pages) {
            return Err(Error::Bounds);
        }
        device.resize(u64::from(total) * BLOCK as u64)?;
        let mut store = Self {
            cache: Cache::new(device, cache_pages)?,
            total,
            roots: [None, None],
            selected: 1,
            recovery: Recovery::Clean,
        };
        store.commit(&Records::new())?;
        Ok(store)
    }
    pub fn open(mut device: D, cache_pages: usize) -> Result<Self, Error> {
        let length = device.seek(SeekFrom::End(0))?;
        if !length.is_multiple_of(BLOCK as u64)
            || !(8 * BLOCK as u64..=65536 * BLOCK as u64).contains(&length)
        {
            return Err(Error::Invalid);
        }
        let total = (length / BLOCK as u64) as u32;
        let mut store = Self {
            cache: Cache::new(device, cache_pages)?,
            total,
            roots: [None, None],
            selected: 0,
            recovery: Recovery::Clean,
        };
        let mut damaged = false;
        for index in 0..2 {
            let bytes = store.cache.read(index as u32)?;
            if bytes.iter().all(|b| *b == 0) {
                continue;
            }
            match store.decode_root(&bytes) {
                Ok(root) => store.roots[index] = Some(root),
                Err(Error::Invalid) => damaged = true,
                Err(error) => return Err(error),
            }
        }
        store.selected = match (&store.roots[0], &store.roots[1]) {
            (Some(a), Some(b)) if a.generation != b.generation => {
                usize::from(b.generation > a.generation)
            }
            (Some(_), None) => 0,
            (None, Some(_)) => 1,
            _ => return Err(Error::Invalid),
        };
        let generation = store.current()?.generation;
        // An empty second header is only normal before the first root rotation.
        if store.roots.iter().any(Option::is_none) && generation != 1 {
            damaged = true;
        }
        if damaged {
            store.recovery = Recovery::RecoveredReadOnly {
                selected_generation: generation,
            };
        }
        Ok(store)
    }
    fn current(&self) -> Result<&Root, Error> {
        self.roots[self.selected].as_ref().ok_or(Error::Invalid)
    }
    pub fn recovery(&self) -> Recovery {
        self.recovery
    }
    pub fn generation(&self) -> Result<u64, Error> {
        Ok(self.current()?.generation)
    }
    fn decode_root(&mut self, bytes: &[u8]) -> Result<Root, Error> {
        if bytes.len() != BLOCK
            || &bytes[..8] != MAGIC
            || sha256(&bytes[..BLOCK - 32]) != bytes[BLOCK - 32..]
        {
            return Err(Error::Invalid);
        }
        let generation = u64_at(bytes, 8)?;
        let count = u32_at(bytes, 20)? as usize;
        let size = u32_at(bytes, 24)? as usize;
        let pages = u32_at(bytes, 28)? as usize;
        if generation == 0
            || u32_at(bytes, 16)? != self.total
            || count > MAX_ENTRIES
            || size != count * ENTRY
            || pages != size.div_ceil(BLOCK)
            || HEADER + pages * 4 > BLOCK - 32
        {
            return Err(Error::Invalid);
        }
        let mut used = BTreeSet::new();
        let mut manifest = Vec::new();
        let mut contents = Vec::new();
        for i in 0..pages {
            let slot = u32_at(bytes, HEADER + i * 4)?;
            if slot < 2 || slot >= self.total || !used.insert(slot) {
                return Err(Error::Invalid);
            }
            manifest.push(slot);
            contents.extend(self.cache.read(slot)?);
        }
        contents.truncate(size);
        if sha256(&contents) != bytes[32..64] {
            return Err(Error::Invalid);
        }
        let mut entries = BTreeMap::new();
        for record in contents.chunks_exact(ENTRY) {
            let key = (record[0], u64_at(record, 1)?, u32_at(record, 9)?);
            let slot = u32_at(record, 13)?;
            let length = u16::from_le_bytes([record[17], record[18]]);
            let digest: [u8; 32] = record[19..51].try_into().map_err(|_| Error::Invalid)?;
            if slot < 2
                || slot >= self.total
                || length == 0
                || usize::from(length) > BLOCK
                || !used.insert(slot)
            {
                return Err(Error::Invalid);
            }
            let data = self.cache.read(slot)?;
            if sha256(&data[..usize::from(length)]) != digest
                || data[usize::from(length)..].iter().any(|b| *b != 0)
            {
                return Err(Error::Invalid);
            }
            if entries
                .insert(
                    key,
                    Location {
                        slot,
                        length,
                        digest,
                    },
                )
                .is_some()
            {
                return Err(Error::Invalid);
            }
        }
        Ok(Root {
            generation,
            manifest,
            entries,
        })
    }
    pub fn read(&mut self, key: Key) -> Result<Option<Vec<u8>>, Error> {
        let Some(location) = self.current()?.entries.get(&key).cloned() else {
            return Ok(None);
        };
        let mut bytes = self.cache.read(location.slot)?;
        bytes.truncate(usize::from(location.length));
        if sha256(&bytes) != location.digest {
            return Err(Error::Invalid);
        }
        Ok(Some(bytes))
    }
    pub fn records(&mut self) -> Result<Records, Error> {
        let keys: Vec<_> = self.current()?.entries.keys().copied().collect();
        let mut records = Records::new();
        for key in keys {
            records.insert(key, self.read(key)?.ok_or(Error::Invalid)?);
        }
        Ok(records)
    }
    fn used(&self) -> BTreeSet<u32> {
        let mut used = BTreeSet::from([0, 1]);
        for root in self.roots.iter().flatten() {
            used.extend(root.manifest.iter().copied());
            used.extend(root.entries.values().map(|location| location.slot));
        }
        used
    }
    pub fn stats(&self) -> Stats {
        let used = self.used();
        let mut stats = self.cache.stats.clone();
        stats.cached_pages = self.cache.pages.len();
        stats.dirty_pages = self.cache.pages.values().filter(|p| p.dirty).count();
        stats.allocated_slots = used.len() as u32;
        stats.free_slots = self.total - stats.allocated_slots;
        stats.generation = self.current().map_or(0, |r| r.generation);
        let mut run = 0;
        for slot in 2..self.total {
            if used.contains(&slot) {
                run = 0;
            } else {
                run += 1;
                stats.largest_free_run = stats.largest_free_run.max(run);
            }
        }
        stats
    }
    /// Explicitly republishes the surviving verified root; never guesses lost data.
    pub fn repair(&mut self) -> Result<u64, Error> {
        let records = self.records()?;
        let previous = self.recovery;
        self.recovery = Recovery::Clean;
        match self.commit(&records) {
            Ok(generation) => Ok(generation),
            Err(error) => {
                if self.recovery == Recovery::Clean {
                    self.recovery = previous;
                }
                Err(error)
            }
        }
    }
    pub fn commit(&mut self, records: &Records) -> Result<u64, Error> {
        if self.recovery != Recovery::Clean {
            return Err(Error::ReadOnly);
        }
        if records.len() > MAX_ENTRIES || records.values().any(|b| b.is_empty() || b.len() > BLOCK)
        {
            return Err(Error::Bounds);
        }
        let used = self.used();
        let free: Vec<_> = (2..self.total).filter(|s| !used.contains(s)).collect();
        let generation = self
            .current()
            .map_or(Ok(1), |r| r.generation.checked_add(1).ok_or(Error::Bounds))?;
        let mut entries = BTreeMap::new();
        let mut changed = Vec::new();
        for (&key, bytes) in records {
            let old = self
                .current()
                .ok()
                .and_then(|r| r.entries.get(&key))
                .cloned();
            if let Some(old) = old {
                if old.length as usize == bytes.len()
                    && old.digest == sha256(bytes)
                    && self.read(key)?.as_ref() == Some(bytes)
                {
                    entries.insert(key, old);
                    continue;
                }
            }
            changed.push((key, bytes));
        }
        let manifest_pages = (records.len() * ENTRY).div_ceil(BLOCK);
        if HEADER + manifest_pages * 4 > BLOCK - 32 || changed.len() + manifest_pages > free.len() {
            return Err(Error::NoSpace);
        }
        for (i, (key, bytes)) in changed.iter().enumerate() {
            let slot = free[i];
            entries.insert(
                *key,
                Location {
                    slot,
                    length: bytes.len() as u16,
                    digest: sha256(bytes),
                },
            );
        }
        let mut metadata = Vec::with_capacity(entries.len() * ENTRY);
        for (key, loc) in &entries {
            metadata.push(key.0);
            metadata.extend(key.1.to_le_bytes());
            metadata.extend(key.2.to_le_bytes());
            metadata.extend(loc.slot.to_le_bytes());
            metadata.extend(loc.length.to_le_bytes());
            metadata.extend(loc.digest);
        }
        let manifest: Vec<_> = free[changed.len()..changed.len() + manifest_pages].to_vec();
        let mut header = vec![0; BLOCK];
        header[..8].copy_from_slice(MAGIC);
        header[8..16].copy_from_slice(&generation.to_le_bytes());
        header[16..20].copy_from_slice(&self.total.to_le_bytes());
        header[20..24].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        header[24..28].copy_from_slice(&(metadata.len() as u32).to_le_bytes());
        header[28..32].copy_from_slice(&(manifest_pages as u32).to_le_bytes());
        header[32..64].copy_from_slice(&sha256(&metadata));
        for (i, slot) in manifest.iter().enumerate() {
            header[HEADER + i * 4..HEADER + i * 4 + 4].copy_from_slice(&slot.to_le_bytes());
        }
        let digest = sha256(&header[..BLOCK - 32]);
        header[BLOCK - 32..].copy_from_slice(&digest);
        let next = 1 - self.selected;
        let result = (|| {
            for (i, (_, bytes)) in changed.iter().enumerate() {
                let mut page = vec![0; BLOCK];
                page[..bytes.len()].copy_from_slice(bytes);
                self.cache.put(free[i], page, true)?;
            }
            for (slot, chunk) in manifest.iter().zip(metadata.chunks(BLOCK)) {
                let mut page = vec![0; BLOCK];
                page[..chunk.len()].copy_from_slice(chunk);
                self.cache.put(*slot, page, true)?;
            }
            self.cache.flush()?; // all data/manifest durable before publishing the new root
            self.cache.raw_write(next as u32, &header)?;
            self.cache.device.sync()?;
            self.cache.pages.remove(&(next as u32));
            Ok::<_, Error>(())
        })();
        if let Err(error) = result {
            // Publication may have reached storage; never retry using the old free map.
            self.recovery = Recovery::RecoveredReadOnly {
                selected_generation: generation - 1,
            };
            return Err(error);
        }
        self.roots[next] = Some(Root {
            generation,
            manifest,
            entries,
        });
        self.selected = next;
        Ok(generation)
    }
}
