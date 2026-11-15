//! Persist frozen snapshots without changing ordinary legacy Tree bytes.
//! The prefix is covered by checkpoint/block checksums and by AEAD when sealed.
use super::{model::Tree, Error};
use std::matches;
const FROZEN_MAGIC: &[u8] = b"ELARASN1\n";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VolumeAccess {
    #[default]
    ReadWrite,
    FrozenSnapshot,
    RecoveryReadOnly,
    FrozenRecoveredReadOnly,
    /// Immutable provider namespace; defined virtual device IO remains allowed.
    VirtualProvider,
}
impl VolumeAccess {
    pub(super) fn is_readonly(self) -> bool {
        self != Self::ReadWrite
    }
    pub(super) fn archive_kind(self) -> ArchiveKind {
        if matches!(self, Self::FrozenSnapshot | Self::FrozenRecoveredReadOnly) {
            ArchiveKind::Frozen
        } else {
            ArchiveKind::Ordinary
        }
    }
    pub(super) fn restored(kind: ArchiveKind, recovered: bool) -> Self {
        match (kind, recovered) {
            (ArchiveKind::Ordinary, false) => Self::ReadWrite,
            (ArchiveKind::Ordinary, true) => Self::RecoveryReadOnly,
            (ArchiveKind::Frozen, false) => Self::FrozenSnapshot,
            (ArchiveKind::Frozen, true) => Self::FrozenRecoveredReadOnly,
        }
    }
}
#[derive(Clone, Copy)]
pub(super) enum ArchiveKind {
    Ordinary,
    Frozen,
}
pub(super) fn encode(tree: &Tree, kind: ArchiveKind) -> Result<Vec<u8>, Error> {
    let mut bytes = match kind {
        ArchiveKind::Ordinary => Vec::new(),
        ArchiveKind::Frozen => FROZEN_MAGIC.to_vec(),
    };
    serde_json::to_writer(&mut bytes, tree).map_err(|_| Error::InvalidImage)?;
    Ok(bytes)
}
pub(super) fn decode(bytes: &[u8]) -> Result<(Tree, ArchiveKind), Error> {
    let (payload, kind) = match bytes.strip_prefix(FROZEN_MAGIC) {
        Some(payload) => (payload, ArchiveKind::Frozen),
        None => (bytes, ArchiveKind::Ordinary),
    };
    // Unknown/malformed version prefixes cannot parse as legacy JSON.
    let tree = serde_json::from_slice(payload).map_err(|_| Error::InvalidImage)?;
    Ok((tree, kind))
}
