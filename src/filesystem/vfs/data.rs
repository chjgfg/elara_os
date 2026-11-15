//! Bounded sparse 4 KiB payloads. Tree transactions/snapshots share immutable
//! pages; only touched pages are copied. The byte-sequence serde format remains
//! compatible with ELARAFS1's previous Vec<u8> payload representation.
use super::Error;
use serde::{
    de::{SeqAccess, Visitor},
    ser::SerializeSeq,
    Deserialize, Deserializer, Serialize, Serializer,
};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

pub(super) const PAGE: usize = 4096;
const MAX: usize = 64 * 1024 * 1024;
pub(super) type PagePool = BTreeMap<[u8; 32], Vec<Arc<[u8; PAGE]>>>;
#[derive(Clone, Default)]
pub(super) struct Data {
    length: usize,
    pages: BTreeMap<usize, Arc<[u8; PAGE]>>,
}
impl Data {
    pub fn new(bytes: &[u8]) -> Result<Self, Error> {
        let mut data = Self::default();
        data.write(0, bytes)?;
        Ok(data)
    }
    pub fn len(&self) -> usize {
        self.length
    }
    pub fn allocated(&self) -> usize {
        self.pages.len() * PAGE
    }
    pub(super) fn pages(&self) -> impl Iterator<Item = &Arc<[u8; PAGE]>> {
        self.pages.values()
    }
    pub(super) fn intern_pages(&mut self, pool: &mut PagePool) -> u64 {
        let mut replaced = 0;
        for page in self.pages.values_mut() {
            let digest = crate::sha256::sha256(page.as_slice());
            let bucket = pool.entry(digest).or_default();
            // A digest is only an index; equality of all bytes proves sharing safe.
            if let Some(existing) = bucket
                .iter()
                .find(|existing| existing.as_ref() == page.as_ref())
            {
                if !Arc::ptr_eq(existing, page) {
                    *page = existing.clone();
                    replaced += 1;
                }
            } else {
                bucket.push(page.clone());
            }
        }
        replaced
    }
    pub fn shared_pages(&self) -> usize {
        self.pages
            .values()
            .filter(|p| Arc::strong_count(p) > 1)
            .count()
    }
    pub fn read(&self, offset: usize, maximum: usize) -> Vec<u8> {
        let start = offset.min(self.length);
        let end = start.saturating_add(maximum).min(self.length);
        let mut out = std::vec![0;end-start];
        for (index, page) in self.pages.range(start / PAGE..end.div_ceil(PAGE)) {
            let base = index * PAGE;
            let from = start.max(base);
            let to = end.min(base + PAGE);
            out[from - start..to - start].copy_from_slice(&page[from - base..to - base]);
        }
        out
    }
    /// Returns payload bytes copied to detach shared pages, not logical write size.
    pub fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<u64, Error> {
        let end = offset.checked_add(bytes.len()).ok_or(Error::Overflow)?;
        if end > MAX {
            return Err(Error::Quota);
        }
        if bytes.is_empty() {
            return Ok(0);
        }
        let mut consumed = 0;
        let mut copied = 0;
        while consumed < bytes.len() {
            let at = offset + consumed;
            let index = at / PAGE;
            let within = at % PAGE;
            let count = (PAGE - within).min(bytes.len() - consumed);
            let input = &bytes[consumed..consumed + count];
            if self.pages.contains_key(&index) || input.iter().any(|b| *b != 0) {
                let page = self
                    .pages
                    .entry(index)
                    .or_insert_with(|| Arc::new([0; PAGE]));
                if Arc::strong_count(page) > 1 {
                    copied += PAGE as u64;
                }
                Arc::make_mut(page)[within..within + count].copy_from_slice(input);
                if page.iter().all(|b| *b == 0) {
                    self.pages.remove(&index);
                }
            }
            consumed += count;
        }
        self.length = self.length.max(end);
        Ok(copied)
    }
    pub fn resize(&mut self, length: usize) -> Result<u64, Error> {
        if length > MAX {
            return Err(Error::Quota);
        }
        let mut copied = 0;
        if length < self.length {
            self.pages.retain(|index, _| *index < length.div_ceil(PAGE));
            if !length.is_multiple_of(PAGE) {
                let index = length / PAGE;
                if let Some(page) = self.pages.get_mut(&index) {
                    if Arc::strong_count(page) > 1 {
                        copied += PAGE as u64;
                    }
                    Arc::make_mut(page)[length % PAGE..].fill(0);
                    if page.iter().all(|b| *b == 0) {
                        self.pages.remove(&index);
                    }
                }
            }
        }
        self.length = length;
        Ok(copied)
    }
}
impl Serialize for Data {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.length))?;
        for index in 0..self.length.div_ceil(PAGE) {
            let count = (self.length - index * PAGE).min(PAGE);
            if let Some(page) = self.pages.get(&index) {
                for byte in &page[..count] {
                    seq.serialize_element(byte)?;
                }
            } else {
                for _ in 0..count {
                    seq.serialize_element(&0u8)?;
                }
            }
        }
        seq.end()
    }
}
impl<'de> Deserialize<'de> for Data {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Bytes;
        impl<'de> Visitor<'de> for Bytes {
            type Value = Data;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("bounded file byte sequence")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Data, A::Error> {
                if seq.size_hint().is_some_and(|n| n > MAX) {
                    return Err(serde::de::Error::custom("file exceeds volume limit"));
                }
                let mut data = Data::default();
                let mut page = [0; PAGE];
                let mut used = 0;
                while let Some(byte) = seq.next_element::<u8>()? {
                    if data.length == MAX {
                        return Err(serde::de::Error::custom("file exceeds volume limit"));
                    }
                    page[used] = byte;
                    used += 1;
                    data.length += 1;
                    if used == PAGE {
                        if page.iter().any(|b| *b != 0) {
                            data.pages.insert((data.length - 1) / PAGE, Arc::new(page));
                        }
                        page = [0; PAGE];
                        used = 0;
                    }
                }
                if used != 0 && page.iter().any(|b| *b != 0) {
                    data.pages.insert((data.length - 1) / PAGE, Arc::new(page));
                }
                Ok(data)
            }
        }
        deserializer.deserialize_seq(Bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{assert, assert_eq};
    #[test]
    fn cow_shares_untouched_pages_and_detaches_only_modified_page() {
        let original = Data::new(&[7; PAGE * 3]).unwrap();
        let mut copy = original.clone();
        for index in 0..3 {
            assert!(Arc::ptr_eq(&original.pages[&index], &copy.pages[&index]));
        }
        assert_eq!(copy.write(PAGE + 1, b"x").unwrap(), PAGE as u64);
        assert!(Arc::ptr_eq(&original.pages[&0], &copy.pages[&0]));
        assert!(!Arc::ptr_eq(&original.pages[&1], &copy.pages[&1]));
        assert!(Arc::ptr_eq(&original.pages[&2], &copy.pages[&2]));
        assert_eq!(original.read(PAGE + 1, 1), [7]);
        assert_eq!(copy.read(PAGE + 1, 1), b"x");
        assert_eq!(copy.write(PAGE + 2, b"y").unwrap(), 0);
    }
    #[test]
    fn sparse_resize_zeroes_discarded_tail_and_keeps_old_snapshot() {
        let mut data = Data::new(&[9; PAGE + 10]).unwrap();
        let snapshot = data.clone();
        assert_eq!(data.resize(PAGE + 2).unwrap(), PAGE as u64);
        data.resize(PAGE * 3).unwrap();
        assert_eq!(data.allocated(), PAGE * 2);
        assert_eq!(
            data.read(PAGE, 16),
            [9, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(snapshot.read(PAGE, 10), [9; 10]);
        data.write(0, &[0; PAGE]).unwrap();
        assert_eq!(data.allocated(), PAGE);
        let encoded = serde_json::to_vec(&data).unwrap();
        assert_eq!(
            serde_json::from_slice::<Vec<u8>>(&encoded).unwrap(),
            data.read(0, data.len())
        );
        let restored: Data = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(restored.read(0, restored.len()), data.read(0, data.len()));
        assert_eq!(restored.allocated(), PAGE);
        assert_eq!(Data::new(&[0; PAGE * 4]).unwrap().allocated(), 0);
    }
}

#[cfg(test)]
mod dedup_tests {
    use super::*;
    use std::{assert, assert_eq};
    #[test]
    fn dedup_digest_bucket_still_requires_full_byte_equality() {
        let mut data = Data::new(&[7; PAGE]).unwrap();
        let mut pool = PagePool::new();
        let digest = crate::sha256::sha256(&[7; PAGE]);
        // Simulate a digest collision without needing to break SHA-256.
        pool.insert(digest, std::vec![Arc::new([9; PAGE])]);
        assert_eq!(data.intern_pages(&mut pool), 0);
        assert_eq!(data.read(0, PAGE), [7; PAGE]);
        assert_eq!(pool[&digest].len(), 2);
        let mut equal = Data::new(&[7; PAGE]).unwrap();
        assert!(!Arc::ptr_eq(
            data.pages().next().unwrap(),
            equal.pages().next().unwrap()
        ));
        assert_eq!(equal.intern_pages(&mut pool), 1);
        assert!(Arc::ptr_eq(
            data.pages().next().unwrap(),
            equal.pages().next().unwrap()
        ));
    }
}
