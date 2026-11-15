//! File-system tests live outside production sources. Registered as unit
//! modules so test-only guard removal remains unavailable in production.
use std::path::PathBuf;

fn fixture() -> PathBuf {
    crate::test_support::marked_root("fs")
}

mod access;
mod binding;
mod boundary;
mod default_acl;
mod diagnostics;
mod directory;
mod execution;
mod handles;
mod host_boundary;
mod io;
mod log_store;
mod metadata;
mod modes;
mod namespace;
mod objects;
mod open;
mod owned_open;
mod ownership;
mod policy;
mod privilege;
mod recovery;
mod special_permissions;
mod sticky;
mod transaction;
mod traversal;

mod vfs;

mod system_providers;

mod resources;

mod block_store;

mod host_locks;

mod host_links;

mod directory_model;

mod host_symlinks;

mod host_rename;
