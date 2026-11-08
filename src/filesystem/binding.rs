//! Object binding without pathname, timestamp or content-hash identity guesses.
//! The caller holds the namespace lock. Fresh descriptors and a positive /
//! conflict / released-lock control distinguish the actual OS lock object.
use super::{DataRoot, FsError};
use cap_std::fs::OpenOptions;
use std::fs::{File, TryLockError};
use std::io;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectBinding {
    SameObject,
    DifferentObject,
    Unconfirmed(io::ErrorKind),
}
fn kind(error: TryLockError) -> io::ErrorKind {
    match error {
        TryLockError::WouldBlock => io::ErrorKind::WouldBlock,
        TryLockError::Error(error) => error.kind(),
    }
}
fn controlled_binding(first: &File, second: &File) -> ObjectBinding {
    // A pre-existing lock cannot be mistaken for our own conflict.
    if let Err(error) = second.try_lock() {
        return ObjectBinding::Unconfirmed(kind(error));
    }
    if let Err(error) = second.unlock() {
        return ObjectBinding::Unconfirmed(error.kind());
    }
    if let Err(error) = first.try_lock() {
        return ObjectBinding::Unconfirmed(kind(error));
    }
    let observed = second.try_lock();
    match observed {
        Ok(()) => {
            let second_unlock = second.unlock();
            let first_unlock = first.unlock();
            if let Err(error) = second_unlock.and(first_unlock) {
                ObjectBinding::Unconfirmed(error.kind())
            } else {
                ObjectBinding::DifferentObject
            }
        }
        Err(TryLockError::WouldBlock) => {
            if let Err(error) = first.unlock() {
                return ObjectBinding::Unconfirmed(error.kind());
            }
            // The conflict must disappear after releasing OUR lock.
            if let Err(error) = second.try_lock() {
                return ObjectBinding::Unconfirmed(kind(error));
            }
            match second.unlock() {
                Ok(()) => ObjectBinding::SameObject,
                Err(error) => ObjectBinding::Unconfirmed(error.kind()),
            }
        }
        Err(error) => {
            let observed = kind(error);
            match first.unlock() {
                Ok(()) => ObjectBinding::Unconfirmed(observed),
                Err(error) => ObjectBinding::Unconfirmed(error.kind()),
            }
        }
    }
}
impl DataRoot {
    /// Binding probe for regular files. Unsupported/readonly/busy hosts are
    /// explicit unconfirmed results, never an equality or permission grant.
    /// This is a short cooperative lock probe, not an OS-wide sandbox.
    pub fn compare_file_objects(&self, first: &str, second: &str) -> Result<ObjectBinding, FsError> {
        let guard = self.namespace_guard()?;
        let first = self.resolve(first)?;
        let second = self.resolve(second)?;
        self.compare_relative_objects(&first, &second, &guard)
    }
    pub(super) fn compare_relative_objects(
        &self, first: &std::path::Path, second: &std::path::Path,
        guard: &super::transaction::NamespaceGuard<'_>,
    ) -> Result<ObjectBinding, FsError> {
        guard.require_root(self)?;
        self.inspect_relative(first)?;
        self.inspect_relative(second)?;
        if !self.dir.metadata(first)?.is_file() || !self.dir.metadata(second)?.is_file() {
            return Err(FsError::NotFile);
        }
        let options = OpenOptions::new().read(true).write(true).to_owned();
        let first = match self.dir.open_with(first, &options) {
            Ok(file) => file.into_std(),
            Err(error) => return Ok(ObjectBinding::Unconfirmed(error.kind())),
        };
        let second = match self.dir.open_with(second, &options) {
            Ok(file) => file.into_std(),
            Err(error) => return Ok(ObjectBinding::Unconfirmed(error.kind())),
        };
        Ok(controlled_binding(&first, &second))
    }
}
