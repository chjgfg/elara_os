//! Open-mode construction is separate from IO and handle bookkeeping.
use super::FsError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileAccess {
    ReadOnly,
    WriteOnly,
    ReadWrite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileCreation {
    Existing,
    IfMissing,
    Exclusive,
}

/// Truncate and append are mutually exclusive in the type itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileBehavior {
    Preserve,
    Truncate,
    Append,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileMode {
    access: FileAccess,
    creation: FileCreation,
    behavior: FileBehavior,
}

impl FileMode {
    pub const READ: Self = Self {
        access: FileAccess::ReadOnly,
        creation: FileCreation::Existing,
        behavior: FileBehavior::Preserve,
    };
    pub const CREATE: Self = Self {
        access: FileAccess::ReadWrite,
        creation: FileCreation::Exclusive,
        behavior: FileBehavior::Preserve,
    };

    pub fn new(
        access: FileAccess,
        creation: FileCreation,
        behavior: FileBehavior,
    ) -> Result<Self, FsError> {
        if access == FileAccess::ReadOnly
            && (creation != FileCreation::Existing || behavior != FileBehavior::Preserve)
        {
            return Err(FsError::InvalidOpenMode);
        }
        Ok(Self {
            access,
            creation,
            behavior,
        })
    }

    pub fn access(self) -> FileAccess {
        self.access
    }
    pub fn creation(self) -> FileCreation {
        self.creation
    }
    pub fn behavior(self) -> FileBehavior {
        self.behavior
    }
    pub(super) fn readable(self) -> bool {
        self.access != FileAccess::WriteOnly
    }
    pub(super) fn writable(self) -> bool {
        self.access != FileAccess::ReadOnly
    }
}
