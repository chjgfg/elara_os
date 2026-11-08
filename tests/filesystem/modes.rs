use crate::filesystem::{FileAccess, FileBehavior, FileCreation, FileMode, FsError};
use std::{assert, assert_eq, matches};

#[test]
fn readonly_mode_cannot_create_truncate_or_append() {
    for creation in [
        FileCreation::Existing,
        FileCreation::IfMissing,
        FileCreation::Exclusive,
    ] {
        for behavior in [
            FileBehavior::Preserve,
            FileBehavior::Truncate,
            FileBehavior::Append,
        ] {
            let result = FileMode::new(FileAccess::ReadOnly, creation, behavior);
            if creation == FileCreation::Existing && behavior == FileBehavior::Preserve {
                assert_eq!(result.unwrap(), FileMode::READ);
            } else {
                assert!(matches!(result, Err(FsError::InvalidOpenMode)));
            }
        }
    }
}
#[test]
fn writable_modes_preserve_all_typed_choices() {
    for access in [FileAccess::WriteOnly, FileAccess::ReadWrite] {
        for creation in [
            FileCreation::Existing,
            FileCreation::IfMissing,
            FileCreation::Exclusive,
        ] {
            for behavior in [
                FileBehavior::Preserve,
                FileBehavior::Truncate,
                FileBehavior::Append,
            ] {
                let mode = FileMode::new(access, creation, behavior).unwrap();
                assert_eq!(mode.access(), access);
                assert_eq!(mode.creation(), creation);
                assert_eq!(mode.behavior(), behavior);
            }
        }
    }
}
