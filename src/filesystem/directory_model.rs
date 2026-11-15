//! Directory names use the same inode table as files, but cannot alias.
//! Legacy wire maps are projections only; decoding reconstructs the inode table.
use super::inode_table::InodeTable;
use super::{Acl, DeleteCondition, DirectoryInfo, FileOwner, FsError, VirtualMode};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DirectoryInode {
    pub owner: FileOwner,
    pub mode: VirtualMode,
    pub acl: Acl,
    pub delete_condition: DeleteCondition,
}
impl DirectoryInode {
    fn project(&self, id: u64, path: &str) -> DirectoryInfo {
        DirectoryInfo {
            id,
            path: path.to_owned(),
            owner: self.owner,
            mode: self.mode,
            acl: self.acl.clone(),
            delete_condition: self.delete_condition,
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct DirectoryEntries(InodeTable<u64, DirectoryInode>);
impl DirectoryEntries {
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn get(&self, path: &str) -> Option<DirectoryInfo> {
        let id = *self.0.names.get(path)?;
        Some(self.0.inodes.get(&id)?.project(id, path))
    }
    pub fn inode_mut(&mut self, path: &str) -> Option<&mut DirectoryInode> {
        self.0.inode_mut(path)
    }
    pub fn contains_key(&self, path: &str) -> bool {
        self.0.contains_key(path)
    }
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.0.keys()
    }
    pub fn values(&self) -> impl Iterator<Item = DirectoryInfo> + '_ {
        self.0
            .names
            .iter()
            .map(|(path, id)| self.0.inodes[id].project(*id, path))
    }
    pub fn iter(&self) -> impl Iterator<Item = (&String, DirectoryInfo)> {
        self.0
            .names
            .iter()
            .map(|(path, id)| (path, self.0.inodes[id].project(*id, path)))
    }
    pub fn insert(&mut self, path: String, info: DirectoryInfo) -> Result<(), FsError> {
        if path != info.path || info.id == 0 {
            return Err(FsError::InvalidOwnershipRecord);
        }
        let inode = DirectoryInode {
            owner: info.owner,
            mode: info.mode,
            acl: info.acl,
            delete_condition: info.delete_condition,
        };
        self.0.bind(path, info.id, inode, false)
    }
    pub fn remove(&mut self, path: &str) -> Option<DirectoryInfo> {
        let (id, inode) = self.0.unbind(path)?;
        Some(inode.project(id, path))
    }
}
impl Serialize for DirectoryEntries {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.len()))?;
        for (path, info) in self.iter() {
            map.serialize_entry(path, &info)?;
        }
        map.end()
    }
}
impl<'de> Deserialize<'de> for DirectoryEntries {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let values = BTreeMap::<String, DirectoryInfo>::deserialize(deserializer)?;
        let mut table = Self::default();
        for (path, info) in values {
            table.insert(path, info).map_err(serde::de::Error::custom)?;
        }
        Ok(table)
    }
}
impl<'a> IntoIterator for &'a DirectoryEntries {
    type Item = (&'a String, DirectoryInfo);
    type IntoIter = Box<dyn Iterator<Item = Self::Item> + 'a>;
    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}
