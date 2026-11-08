//! File-system tests live outside production sources. Registered as unit
//! modules so test-only guard removal remains unavailable in production.
use std::path::PathBuf;

fn fixture() -> PathBuf {
    crate::test_support::marked_root("fs")
}

mod boundary;
mod binding;
mod diagnostics;
mod directory;
mod handles;
mod io;
mod log_store;
mod metadata;
mod modes;
mod namespace;
mod objects;
mod ownership;
mod open;
mod policy;
mod recovery;
mod transaction;
mod traversal;
