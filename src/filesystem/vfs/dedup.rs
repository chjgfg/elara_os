//! Explicit within-volume page interning; no background work or global pool.
use super::{
    data::{PagePool, PAGE},
    model::{Content, Tree},
    Error, Volume,
};
use crate::filesystem::Credentials;
use std::collections::BTreeSet;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DedupStats {
    pub page_references: u64,
    pub unique_pages_before: u64,
    pub unique_pages_after: u64,
    pub redirected_references: u64,
    /// Reduction in unique payload bytes referenced by this volume, not RSS.
    /// Snapshots/FDs in other volumes can keep replaced allocations alive.
    pub referenced_bytes_reduced: u64,
}
fn usage(tree: &Tree) -> (u64, u64) {
    let mut refs = 0;
    let mut unique = BTreeSet::new();
    for node in tree.nodes.values() {
        if let Content::File(data) = &node.content {
            for page in data.pages() {
                refs += 1;
                unique.insert(Arc::as_ptr(page));
            }
        }
    }
    (refs, unique.len() as u64)
}
impl Volume {
    pub fn deduplicate(&self, actor: &Credentials) -> Result<DedupStats, Error> {
        if actor.uid != 0 {
            return Err(Error::Denied);
        }
        if self.pseudo.is_some() {
            return Err(Error::Unsupported);
        }
        let mut state = self.guard()?;
        if state.access.is_readonly() {
            return Err(Error::ReadOnly);
        }
        let (page_references, unique_pages_before) = usage(&state.tree);
        let mut pool = PagePool::new();
        let mut redirected_references = 0;
        for node in state.tree.nodes.values_mut() {
            if let Content::File(data) = &mut node.content {
                redirected_references += data.intern_pages(&mut pool);
            }
        }
        // Drop temporary references before reporting persistent sharing.
        drop(pool);
        let (_, unique_pages_after) = usage(&state.tree);
        Ok(DedupStats {
            page_references,
            unique_pages_before,
            unique_pages_after,
            redirected_references,
            referenced_bytes_reduced: (unique_pages_before - unique_pages_after) * PAGE as u64,
        })
    }
}
