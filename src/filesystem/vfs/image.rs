//! Bounded checkpoint image: checksummed superblock, deduplicated 4 KiB blocks,
//! and two independently verified copies per block. No ambient IO here.
use super::{Error, Volume};
use crate::{filesystem::Credentials, sha256::sha256};
use std::collections::BTreeMap;
use std::sync::Arc;
const MAGIC: &[u8; 8] = b"ELARAFS1";
const BLOCK: usize = 4096;
const MAX_IMAGE: usize = 1024 * 1024;
const MAX_DECODED: usize = 8 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    Clean,
    RecoveredReadOnly { damaged_copies: u64 },
}
pub struct Loaded {
    pub volume: Arc<Volume>,
    pub recovery: Recovery,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageStats {
    pub logical_bytes: u64,
    pub image_bytes: u64,
    pub logical_blocks: u64,
    pub unique_blocks: u64,
}
fn put(out: &mut Vec<u8>, n: usize) -> Result<(), Error> {
    out.extend_from_slice(&u32::try_from(n).map_err(|_| Error::Overflow)?.to_le_bytes());
    Ok(())
}
fn rle(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let mut n = 1;
        while i + n < bytes.len() && bytes[i + n] == bytes[i] && n < 255 {
            n += 1;
        }
        out.push(n as u8);
        out.push(bytes[i]);
        i += n;
    }
    out
}
fn unpack(bytes: &[u8], length: usize, compressed: bool) -> Result<Vec<u8>, Error> {
    if length > BLOCK {
        return Err(Error::InvalidImage);
    }
    if !compressed {
        return if bytes.len() == length {
            Ok(bytes.to_vec())
        } else {
            Err(Error::InvalidImage)
        };
    }
    if !bytes.len().is_multiple_of(2) {
        return Err(Error::InvalidImage);
    }
    let mut out = Vec::with_capacity(length);
    for pair in bytes.chunks_exact(2) {
        let n = pair[0] as usize;
        if n == 0 || out.len() + n > length {
            return Err(Error::InvalidImage);
        }
        out.resize(out.len() + n, pair[1]);
    }
    if out.len() != length {
        return Err(Error::InvalidImage);
    }
    Ok(out)
}
struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.position.checked_add(n).ok_or(Error::InvalidImage)?;
        let b = self
            .bytes
            .get(self.position..end)
            .ok_or(Error::InvalidImage)?;
        self.position = end;
        Ok(b)
    }
    fn number(&mut self) -> Result<usize, Error> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| Error::InvalidImage)?) as usize)
    }
}
impl Volume {
    pub fn image(&self, actor: &Credentials) -> Result<(Vec<u8>, ImageStats), Error> {
        let payload = self.encode(actor)?;
        if payload.len() > MAX_DECODED {
            return Err(Error::Quota);
        }
        let mut blocks = Vec::<Vec<u8>>::new();
        let mut map = BTreeMap::<Vec<u8>, usize>::new();
        let mut index = Vec::new();
        for chunk in payload.chunks(BLOCK) {
            let id = match map.get(chunk) {
                Some(id) => *id,
                None => {
                    let id = blocks.len();
                    blocks.push(chunk.to_vec());
                    map.insert(chunk.to_vec(), id);
                    id
                }
            };
            index.push(id);
        }
        let mut metadata = Vec::new();
        put(&mut metadata, payload.len())?;
        put(&mut metadata, index.len())?;
        put(&mut metadata, blocks.len())?;
        metadata.extend_from_slice(&sha256(&payload));
        for id in &index {
            put(&mut metadata, *id)?;
        }
        let mut data = Vec::new();
        for block in &blocks {
            let packed = rle(block);
            let compressed = packed.len() < block.len();
            let bytes = if compressed { &packed } else { block };
            put(&mut metadata, block.len())?;
            put(&mut metadata, bytes.len())?;
            metadata.push(u8::from(compressed));
            metadata.extend_from_slice(&sha256(bytes));
            data.extend_from_slice(bytes);
            data.extend_from_slice(bytes);
        }
        let mut image = MAGIC.to_vec();
        put(&mut image, metadata.len())?;
        image.extend_from_slice(&sha256(&metadata));
        image.extend_from_slice(&metadata);
        image.extend_from_slice(&data);
        if image.len() > MAX_IMAGE {
            return Err(Error::Quota);
        }
        let stats = ImageStats {
            logical_bytes: payload.len() as u64,
            image_bytes: image.len() as u64,
            logical_blocks: index.len() as u64,
            unique_blocks: blocks.len() as u64,
        };
        Ok((image, stats))
    }
    pub fn load_image(bytes: &[u8]) -> Result<Loaded, Error> {
        if bytes.len() > MAX_IMAGE {
            return Err(Error::Quota);
        }
        let mut input = Cursor { bytes, position: 0 };
        if input.take(8)? != MAGIC {
            return Err(Error::InvalidImage);
        }
        let meta_len = input.number()?;
        let digest = input.take(32)?;
        let meta_bytes = input.take(meta_len)?;
        if sha256(meta_bytes).as_slice() != digest {
            return Err(Error::InvalidImage);
        }
        let mut meta = Cursor {
            bytes: meta_bytes,
            position: 0,
        };
        let size = meta.number()?;
        let count = meta.number()?;
        let unique = meta.number()?;
        let payload_digest = meta.take(32)?;
        if size == 0
            || size > MAX_DECODED
            || count != size.div_ceil(BLOCK)
            || unique == 0
            || unique > count
        {
            return Err(Error::InvalidImage);
        }
        let mut index = Vec::with_capacity(count);
        for _ in 0..count {
            let id = meta.number()?;
            if id >= unique {
                return Err(Error::InvalidImage);
            }
            index.push(id);
        }
        let mut blocks = Vec::with_capacity(unique);
        let mut damaged = 0;
        for _ in 0..unique {
            let length = meta.number()?;
            let encoded = meta.number()?;
            let flag = meta.take(1)?[0];
            if flag > 1 || encoded > BLOCK || encoded == 0 {
                return Err(Error::InvalidImage);
            }
            let digest = meta.take(32)?;
            let a = input.take(encoded)?;
            let b = input.take(encoded)?;
            let av = sha256(a).as_slice() == digest;
            let bv = sha256(b).as_slice() == digest;
            let valid = match (av, bv) {
                (true, true) => {
                    if a != b {
                        return Err(Error::InvalidImage);
                    }
                    a
                }
                (true, false) => {
                    damaged += 1;
                    a
                }
                (false, true) => {
                    damaged += 1;
                    b
                }
                (false, false) => return Err(Error::InvalidImage),
            };
            blocks.push(unpack(valid, length, flag == 1)?);
        }
        if input.position != bytes.len() || meta.position != meta_bytes.len() {
            return Err(Error::InvalidImage);
        }
        let mut payload = Vec::with_capacity(size);
        for (i, id) in index.into_iter().enumerate() {
            let expected = if i + 1 == count {
                size - i * BLOCK
            } else {
                BLOCK
            };
            if blocks[id].len() != expected {
                return Err(Error::InvalidImage);
            }
            payload.extend_from_slice(&blocks[id]);
        }
        if sha256(&payload).as_slice() != payload_digest {
            return Err(Error::InvalidImage);
        }
        let recovery = if damaged == 0 {
            Recovery::Clean
        } else {
            Recovery::RecoveredReadOnly {
                damaged_copies: damaged,
            }
        };
        Ok(Loaded {
            volume: Self::decode(&payload, damaged != 0)?,
            recovery,
        })
    }
}
