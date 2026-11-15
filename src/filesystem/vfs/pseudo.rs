//! Generated Elara information and virtual byte devices. No host device/process IO.
use super::model::{Content, Inode, Tree};
use super::{Error, Limits, Volume};
use crate::filesystem::{Acl, Credentials, FileOwner, VirtualMode};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::{format, matches};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PseudoKind {
    Proc,
    Sys,
    Dev,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
pub(super) enum Node {
    Uptime,
    Identity,
    Version,
    Limits,
    Null,
    Zero,
    Full,
}
impl Node {
    pub(super) fn is_device(self) -> bool {
        matches!(self, Self::Null | Self::Zero | Self::Full)
    }
    // Generated at open, stable across partial reads on that open description.
    pub(super) fn render(self, actor: &Credentials, elapsed: std::time::Duration) -> Vec<u8> {
        match self {
            Self::Uptime=>format!("mount_uptime_ns: {}\n",elapsed.as_nanos()),
            Self::Identity=>format!("euid: {}\negid: {}\ngroups: {:?}\n",actor.uid,actor.gid,actor.groups),
            Self::Version=>format!("ElaraOS {} virtual runtime\n",env!("CARGO_PKG_VERSION")),
            Self::Limits=>format!("max_file_io_bytes: {}\nmax_open_files: {}\nmax_directory_entries: {}\nmax_walk_entries: {}\n",crate::config::ROOTFS_DEFAULTS.max_file_io_bytes,crate::config::ROOTFS_DEFAULTS.max_open_files,crate::config::ROOTFS_DEFAULTS.max_directory_entries,crate::config::ROOTFS_DEFAULTS.max_walk_entries),
            _=>String::new(),
        }.into_bytes()
    }
}
impl Volume {
    /// A readonly namespace; dev permits only the defined device IO operations.
    /// Snapshots/images of these live providers are deliberately unsupported.
    pub fn pseudo(kind: PseudoKind) -> Result<Arc<Self>, Error> {
        let owner = FileOwner { uid: 0, gid: 0 };
        let mut tree = Tree::new(
            owner,
            Limits {
                bytes: if kind == PseudoKind::Proc {
                    1024 * 1024
                } else {
                    65536
                },
                inodes: if kind == PseudoKind::Proc { 4096 } else { 16 },
            },
        )?;
        tree.node_mut(1)?.mode = VirtualMode::new(0o555).map_err(|_| Error::Denied)?;
        let entries: &[(&str, Node)] = match kind {
            PseudoKind::Proc => &[
                ("uptime", Node::Uptime),
                ("identity", Node::Identity),
                ("version", Node::Version),
            ],
            PseudoKind::Sys => &[("limits", Node::Limits), ("version", Node::Version)],
            PseudoKind::Dev => &[
                ("null", Node::Null),
                ("zero", Node::Zero),
                ("full", Node::Full),
            ],
        };
        let mut names = BTreeMap::new();
        for (name, node) in entries {
            let id = tree.next;
            tree.next += 1;
            tree.nodes.insert(
                id,
                Inode {
                    owner,
                    mode: VirtualMode::new(if node.is_device() { 0o666 } else { 0o444 })
                        .map_err(|_| Error::Denied)?,
                    acl: Acl::default(),
                    default_acl: None,
                    links: 1,
                    created: 1,
                    modified: 1,
                    changed: 1,
                    content: Content::Pseudo(*node),
                },
            );
            names.insert((*name).to_owned(), id);
        }
        for name in if kind == PseudoKind::Proc {
            &["tasks", "jobs", "executions"][..]
        } else {
            &[][..]
        } {
            let id = tree.next;
            tree.next += 1;
            let mut directory = tree.node(1)?.clone();
            directory.content = Content::Directory(BTreeMap::new());
            tree.nodes.insert(id, directory);
            names.insert((*name).to_owned(), id);
        }
        tree.node_mut(1)?.content = Content::Directory(names);
        Self::from_tree_kind(tree, true, Some(kind))
    }
}
