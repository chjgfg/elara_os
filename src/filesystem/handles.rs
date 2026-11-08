//! Bounded per-owner handle tables. A token contains table identity, slot and
//! generation, so reuse, close-all and tokens from other tables cannot alias.
use super::{DataRoot, FileInfo, FileMode, FsError, OpenFile};
use crate::config::ROOTFS_DEFAULTS;
use std::io::SeekFrom;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TABLE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileHandle {
    table: u64,
    slot: usize,
    generation: u64,
}
impl FileHandle {
    pub fn slot(self) -> usize {
        self.slot
    }
    pub fn generation(self) -> u64 {
        self.generation
    }
}

enum SlotState {
    Free,
    Open(OpenFile),
    Retired,
}
struct Slot {
    generation: u64,
    state: SlotState,
}

pub struct HandleTable {
    id: u64,
    limit: usize,
    slots: Vec<Slot>,
}
impl HandleTable {
    pub fn new() -> Result<Self, FsError> {
        Self::with_limit(ROOTFS_DEFAULTS.max_open_files)
    }

    pub fn with_limit(limit: usize) -> Result<Self, FsError> {
        if limit == 0 || limit > ROOTFS_DEFAULTS.max_open_files {
            return Err(FsError::InvalidHandleLimit);
        }
        let id = NEXT_TABLE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| FsError::HandleSpaceExhausted)?;
        Ok(Self {
            id,
            limit,
            slots: Vec::new(),
        })
    }

    pub fn open(
        &mut self,
        data: &DataRoot,
        path: &str,
        mode: FileMode,
    ) -> Result<FileHandle, FsError> {
        // Reserve capacity before create/truncate can change disk contents.
        self.reserve_slot()?;
        let file = data.open_file(path, mode)?;
        self.insert(file)
    }

    pub fn open_as(
        &mut self, data: &DataRoot, user: &crate::users::Account, path: &str, mode: FileMode,
    ) -> Result<FileHandle, FsError> {
        self.reserve_slot()?;
        let file = data.open_file_as(user, path, mode)?;
        self.insert(file)
    }

    pub fn insert(&mut self, file: OpenFile) -> Result<FileHandle, FsError> {
        self.reserve_slot()?;
        let index = self
            .slots
            .iter()
            .position(|slot| std::matches!(slot.state, SlotState::Free));
        let slot = match index {
            Some(index) => index,
            None => {
                self.slots.push(Slot {
                    generation: 1,
                    state: SlotState::Free,
                });
                self.slots.len() - 1
            }
        };
        self.slots[slot].state = SlotState::Open(file);
        Ok(FileHandle {
            table: self.id,
            slot,
            generation: self.slots[slot].generation,
        })
    }

    fn reserve_slot(&mut self) -> Result<(), FsError> {
        if self
            .slots
            .iter()
            .any(|slot| std::matches!(slot.state, SlotState::Free))
        {
            return Ok(());
        }
        if self.slots.len() >= self.limit {
            return Err(FsError::HandleTableFull);
        }
        self.slots
            .try_reserve(1)
            .map_err(|_| FsError::AllocationFailed)?;
        Ok(())
    }

    fn file(&self, handle: FileHandle) -> Result<&OpenFile, FsError> {
        if handle.table != self.id {
            return Err(FsError::InvalidHandle);
        }
        let slot = self.slots.get(handle.slot).ok_or(FsError::InvalidHandle)?;
        if slot.generation != handle.generation {
            return Err(FsError::InvalidHandle);
        }
        match &slot.state {
            SlotState::Open(file) => Ok(file),
            _ => Err(FsError::InvalidHandle),
        }
    }

    /// Resolve a textual token only in this table. Never reconstruct table identity
    /// from untrusted input; the full generation still has to match.
    pub fn resolve_token(&self, slot: usize, generation: u64) -> Result<FileHandle, FsError> {
        let handle = FileHandle {
            table: self.id,
            slot,
            generation,
        };
        self.file(handle)?;
        Ok(handle)
    }

    pub fn duplicate(&mut self, handle: FileHandle) -> Result<FileHandle, FsError> {
        let file = self.file(handle)?.clone();
        self.insert(file)
    }
    pub fn duplicate_to(
        &self,
        handle: FileHandle,
        target: &mut Self,
    ) -> Result<FileHandle, FsError> {
        target.insert(self.file(handle)?.clone())
    }
    pub fn close(&mut self, handle: FileHandle) -> Result<(), FsError> {
        self.file(handle)?;
        release(&mut self.slots[handle.slot]);
        Ok(())
    }
    pub fn close_all(&mut self) -> usize {
        let mut closed = 0;
        for slot in &mut self.slots {
            if std::matches!(slot.state, SlotState::Open(_)) {
                release(slot);
                closed += 1;
            }
        }
        closed
    }
    pub fn len(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| std::matches!(slot.state, SlotState::Open(_)))
            .count()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn limit(&self) -> usize {
        self.limit
    }
    pub fn info(&self, handle: FileHandle) -> Result<FileInfo, FsError> {
        self.file(handle)?.info()
    }
    pub fn read(&self, handle: FileHandle, maximum: usize) -> Result<Vec<u8>, FsError> {
        self.file(handle)?.read(maximum)
    }
    pub fn write(&self, handle: FileHandle, bytes: &[u8]) -> Result<usize, FsError> {
        self.file(handle)?.write(bytes)
    }
    pub fn read_at(
        &self,
        handle: FileHandle,
        offset: u64,
        maximum: usize,
    ) -> Result<Vec<u8>, FsError> {
        self.file(handle)?.read_at(offset, maximum)
    }
    pub fn write_at(
        &self,
        handle: FileHandle,
        offset: u64,
        bytes: &[u8],
    ) -> Result<usize, FsError> {
        self.file(handle)?.write_at(offset, bytes)
    }
    pub fn seek(&self, handle: FileHandle, position: SeekFrom) -> Result<u64, FsError> {
        self.file(handle)?.seek(position)
    }
    pub fn truncate(&self, handle: FileHandle, length: u64) -> Result<(), FsError> {
        self.file(handle)?.truncate(length)
    }
    pub fn sync_data(&self, handle: FileHandle) -> Result<(), FsError> {
        self.file(handle)?.sync_data()
    }
    pub fn sync_all(&self, handle: FileHandle) -> Result<(), FsError> {
        self.file(handle)?.sync_all()
    }
}

fn release(slot: &mut Slot) {
    match slot.generation.checked_add(1) {
        Some(generation) => {
            slot.generation = generation;
            slot.state = SlotState::Free;
        }
        None => {
            slot.state = SlotState::Retired;
        }
    }
}

#[cfg(test)]
impl HandleTable {
    pub(crate) fn force_last_generation(&mut self, handle: FileHandle) -> FileHandle {
        self.slots[handle.slot].generation = u64::MAX;
        FileHandle {
            generation: u64::MAX,
            ..handle
        }
    }
}
