//! Shared namespace storage: names are not inode metadata or identity.
use super::{
    ownership_model::{key, MAX_OWNED_OBJECTS},
    FsError,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InodeTable<K: Ord, V> {
    pub(super) names: BTreeMap<String, K>,
    pub(super) inodes: BTreeMap<K, V>,
}
impl<K: Ord, V> Default for InodeTable<K, V> {
    fn default() -> Self {
        Self {
            names: BTreeMap::new(),
            inodes: BTreeMap::new(),
        }
    }
}
impl<K: Ord + Copy, V: Clone + PartialEq> InodeTable<K, V> {
    pub fn len(&self) -> usize {
        self.names.len()
    }
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.names.keys()
    }
    pub fn contains_key(&self, path: &str) -> bool {
        self.names.contains_key(path)
    }
    pub fn inode_mut(&mut self, path: &str) -> Option<&mut V> {
        self.inodes.get_mut(self.names.get(path)?)
    }
    pub fn bind(&mut self, path: String, id: K, inode: V, aliases: bool) -> Result<(), FsError> {
        if key(&path)? != path || self.names.get(&path).is_some_and(|old| *old != id) {
            return Err(FsError::InvalidOwnershipRecord);
        }
        if !self.names.contains_key(&path) {
            if self.names.len() >= MAX_OWNED_OBJECTS {
                return Err(FsError::OwnershipCapacity);
            }
            if self
                .inodes
                .get(&id)
                .is_some_and(|old| !aliases || old != &inode)
            {
                return Err(FsError::InvalidOwnershipRecord);
            }
        }
        self.inodes.insert(id, inode);
        self.names.insert(path, id);
        Ok(())
    }
    pub fn unbind(&mut self, path: &str) -> Option<(K, V)> {
        let id = *self.names.get(path)?;
        let inode = self.inodes.get(&id)?.clone();
        self.names.remove(path);
        if !self.names.values().any(|other| *other == id) {
            self.inodes.remove(&id);
        }
        Some((id, inode))
    }
}
