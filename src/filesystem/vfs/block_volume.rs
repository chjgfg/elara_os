//! Stable inode/page keys: a changed file never shifts unrelated file data.
use super::{
    archive,
    block_store::{Records, BLOCK},
    data::Data,
    model::{Content, Inode},
    Error, Volume, VolumeAccess,
};
use crate::filesystem::Credentials;
use std::collections::BTreeMap;
use std::sync::Arc;
fn split(records: &mut Records, kind: u8, inode: u64, bytes: &[u8]) {
    for (index, chunk) in bytes.chunks(BLOCK).enumerate() {
        records.insert((kind, inode, index as u32), chunk.to_vec());
    }
}
fn join(records: &Records, kind: u8, inode: u64) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    let mut count = 0;
    for (&(_, _, page), value) in records.range((kind, inode, 0)..=(kind, inode, u32::MAX)) {
        if page != count
            || value.is_empty()
            || value.len() > BLOCK
            || bytes.len() + value.len() > 2 * 1024 * 1024
            || bytes.len() % BLOCK != 0
        {
            return Err(Error::InvalidImage);
        }
        bytes.extend(value);
        count += 1;
    }
    if count == 0 {
        return Err(Error::InvalidImage);
    }
    Ok(bytes)
}
impl Volume {
    pub fn block_records(&self, actor: &Credentials) -> Result<Records, Error> {
        let (mut tree, kind) = self.archived_tree(actor)?;
        let nodes = std::mem::take(&mut tree.nodes);
        let mut records = Records::new();
        split(&mut records, 0, 0, &archive::encode(&tree, kind)?);
        for (id, mut node) in nodes {
            let length = node.size();
            if let Content::File(data) = &node.content {
                for page in 0..data.len().div_ceil(BLOCK) {
                    let bytes = data.read(page * BLOCK, BLOCK);
                    if bytes.iter().any(|b| *b != 0) {
                        records.insert((2, id, page as u32), bytes);
                    }
                }
                node.content = Content::File(Data::default());
            }
            split(
                &mut records,
                1,
                id,
                &serde_json::to_vec(&(node, length)).map_err(|_| Error::InvalidImage)?,
            );
        }
        Ok(records)
    }
    pub fn load_block_records(records: &Records, readonly: bool) -> Result<Arc<Self>, Error> {
        let (mut tree, kind) = archive::decode(&join(records, 0, 0)?)?;
        if !tree.nodes.is_empty() {
            return Err(Error::InvalidImage);
        }
        let mut ids = BTreeMap::new();
        for &(kind, inode, page) in records.keys() {
            match kind {
                0 if inode == 0 => {}
                1 if inode != 0 => {
                    if page == 0 {
                        ids.insert(inode, ());
                    }
                }
                2 if inode != 0 => {}
                _ => return Err(Error::InvalidImage),
            }
        }
        if ids.len() > 4096 {
            return Err(Error::InvalidImage);
        }
        for &id in ids.keys() {
            let (mut node, length): (Inode, u64) =
                serde_json::from_slice(&join(records, 1, id)?).map_err(|_| Error::InvalidImage)?;
            let original_size = node.size();
            match &mut node.content {
                Content::File(data) => {
                    if data.len() != 0 || length > 64 * 1024 * 1024 {
                        return Err(Error::InvalidImage);
                    }
                    data.resize(length as usize)?;
                    for (&(_, _, page), bytes) in records.range((2, id, 0)..=(2, id, u32::MAX)) {
                        let offset = page as u64 * BLOCK as u64;
                        if bytes.is_empty()
                            || bytes.len() > BLOCK
                            || offset + bytes.len() as u64 > length
                            || bytes.len() as u64 != (length - offset).min(BLOCK as u64)
                            || bytes.iter().all(|b| *b == 0)
                        {
                            return Err(Error::InvalidImage);
                        }
                        data.write(offset as usize, bytes)?;
                    }
                }
                Content::Pseudo(_) => return Err(Error::InvalidImage),
                _ if original_size != length => return Err(Error::InvalidImage),
                _ => {}
            }
            tree.nodes.insert(id, node);
        }
        for &(kind, inode, _) in records.keys() {
            if kind != 0 && !tree.nodes.contains_key(&inode) {
                return Err(Error::InvalidImage);
            }
            if kind == 2
                && !tree
                    .nodes
                    .get(&inode)
                    .is_some_and(|n| std::matches!(n.content, Content::File(_)))
            {
                return Err(Error::InvalidImage);
            }
        }
        tree.validate()?;
        Self::from_tree_access(tree, VolumeAccess::restored(kind, readonly))
    }
}
