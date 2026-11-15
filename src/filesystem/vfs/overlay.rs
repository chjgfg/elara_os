//! Frozen overlay composition: private merged metadata, shared immutable pages.
//! Sources are snapshotted independently, not a joint cross-volume transaction.
//! Whiteouts affect lower only; upper wins. This is not a live Linux overlayfs.
use super::{
    model::{valid_name, Content, Tree},
    Error, Volume,
};
use crate::filesystem::Credentials;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::{matches, vec};

fn whiteout(tree: &mut Tree, path: &str) -> Result<(), Error> {
    if path.len() > 4096 || !path.starts_with('/') || path == "/" {
        return Err(Error::InvalidPath);
    }
    let parts: Vec<_> = path[1..].split('/').collect();
    if parts
        .iter()
        .any(|s| !valid_name(s) || s.contains(['\\', ':', '\0']))
    {
        return Err(Error::InvalidPath);
    }
    let mut dir = 1;
    for part in &parts[..parts.len() - 1] {
        // Never follow lower symlinks while interpreting a whiteout.
        let Some(child) = tree.node(dir)?.entries()?.get(*part).copied() else {
            return Ok(());
        };
        tree.node(child)?.entries()?;
        dir = child;
    }
    tree.node_mut(dir)?
        .entries_mut()?
        .remove(parts[parts.len() - 1]);
    Ok(())
}
fn collect(tree: &mut Tree) -> Result<(), Error> {
    let mut seen = BTreeSet::new();
    let mut links = BTreeMap::from([(1, 1u64)]);
    let mut queue = vec![1];
    while let Some(id) = queue.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Content::Directory(entries) = &tree.node(id)?.content {
            for target in entries.values() {
                let count = links.entry(*target).or_default();
                *count = count.checked_add(1).ok_or(Error::Overflow)?;
                queue.push(*target);
            }
        }
    }
    tree.nodes.retain(|id, _| seen.contains(id));
    for (id, node) in &mut tree.nodes {
        node.links = *links.get(id).ok_or(Error::InvalidImage)?;
    }
    Ok(())
}
impl Volume {
    /// Result uses upper limits/UID quotas and directory metadata. No source IO
    /// occurs after construction; saves serialize the flattened visible tree.
    pub fn overlay_snapshot(
        actor: &Credentials,
        lower: &Arc<Self>,
        upper: &Arc<Self>,
        whiteouts: &[String],
    ) -> Result<Arc<Self>, Error> {
        if actor.uid != 0 {
            return Err(Error::Denied);
        }
        if whiteouts.len() > 4096 {
            return Err(Error::Quota);
        }
        let lower = lower.snapshot(actor, true)?;
        let upper = upper.snapshot(actor, true)?;
        let mut tree = lower.guard()?.tree.clone();
        let top = upper.guard()?.tree.clone();
        for path in whiteouts {
            whiteout(&mut tree, path)?;
        }
        collect(&mut tree)?;
        tree.limits = top.limits;
        tree.quotas = top.quotas.clone();
        tree.clock = tree.clock.max(top.clock);
        // Map upper directory IDs to existing lower directories when merging;
        // map all other upper IDs to new ones, preserving upper hardlink groups.
        let mut mapping = BTreeMap::from([(1, 1)]);
        let mut directories = vec![(1, 1)];
        while let Some((up, dest)) = directories.pop() {
            for (name, child) in top.node(up)?.entries()? {
                if mapping.contains_key(child) {
                    continue;
                }
                let node = top.node(*child)?;
                let existing = tree.node(dest)?.entries()?.get(name).copied();
                let merge = match existing {
                    Some(id)
                        if matches!(node.content, Content::Directory(_))
                            && matches!(tree.node(id)?.content, Content::Directory(_)) =>
                    {
                        Some(id)
                    }
                    _ => None,
                };
                let id = match merge {
                    Some(id) => id,
                    None => {
                        let id = tree.next;
                        tree.next = id.checked_add(1).ok_or(Error::Overflow)?;
                        // Temporary placeholders allow iterative traversal, not recursion.
                        let mut copy = node.clone();
                        if matches!(copy.content, Content::Directory(_)) {
                            copy.content = Content::Directory(BTreeMap::new());
                        }
                        tree.nodes.insert(id, copy);
                        id
                    }
                };
                mapping.insert(*child, id);
                if matches!(node.content, Content::Directory(_)) {
                    directories.push((*child, id));
                }
            }
        }
        for (up, dest) in &mapping {
            let mut node = top.node(*up)?.clone();
            if let Content::Directory(entries) = &node.content {
                let mut combined = tree.node(*dest)?.entries()?.clone();
                for (name, id) in entries {
                    combined.insert(name.clone(), *mapping.get(id).ok_or(Error::InvalidImage)?);
                }
                node.content = Content::Directory(combined);
            }
            tree.nodes.insert(*dest, node);
        }
        collect(&mut tree)?;
        tree.validate()?;
        Self::from_tree(tree, false)
    }
}
