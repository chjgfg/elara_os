//! Shared inode storage and old directory-wire compatibility.
use crate::filesystem::directory_model::DirectoryEntries;
use crate::filesystem::{Acl, DeleteCondition, DirectoryInfo, FileOwner, VirtualMode};
use std::{assert, assert_eq, collections::BTreeMap};
fn directory(path: &str) -> DirectoryInfo {
    DirectoryInfo {
        id: 1,
        path: path.into(),
        owner: FileOwner {
            uid: 1000,
            gid: 1000,
        },
        mode: VirtualMode::new(0o750).unwrap(),
        acl: Acl::default(),
        delete_condition: DeleteCondition::OwnerRequest,
    }
}
#[test]
fn directory_model_wire_is_exact_legacy_projection_and_roundtrips() {
    let info = directory("/tmp/d");
    let mut original = BTreeMap::new();
    original.insert(info.path.clone(), info.clone());
    let golden = serde_json::to_vec(&original).unwrap();
    let mut entries = DirectoryEntries::default();
    entries.insert(info.path.clone(), info).unwrap();
    assert_eq!(serde_json::to_vec(&entries).unwrap(), golden);
    let decoded: DirectoryEntries = serde_json::from_slice(&golden).unwrap();
    assert_eq!(decoded, entries);
    assert_eq!(serde_json::to_vec(&decoded).unwrap(), golden);
}
#[test]
fn directory_model_rejects_alias_without_mutating_existing_namespace() {
    let mut entries = DirectoryEntries::default();
    entries
        .insert("/tmp/d".into(), directory("/tmp/d"))
        .unwrap();
    let before = entries.clone();
    assert!(entries
        .insert("/tmp/alias".into(), directory("/tmp/alias"))
        .is_err());
    assert_eq!(entries, before);
    let mut duplicate = BTreeMap::new();
    duplicate.insert("/tmp/d", directory("/tmp/d"));
    duplicate.insert("/tmp/alias", directory("/tmp/alias"));
    assert!(
        serde_json::from_slice::<DirectoryEntries>(&serde_json::to_vec(&duplicate).unwrap())
            .is_err()
    );
}
#[test]
fn directory_model_policy_is_inode_owned_and_rename_preserves_identity() {
    let mut entries = DirectoryEntries::default();
    entries
        .insert("/tmp/d".into(), directory("/tmp/d"))
        .unwrap();
    let mut projection = entries.get("/tmp/d").unwrap();
    projection.owner.uid = 99;
    assert_eq!(entries.get("/tmp/d").unwrap().owner.uid, 1000);
    entries.inode_mut("/tmp/d").unwrap().mode = VirtualMode::new(0o700).unwrap();
    assert_eq!(entries.get("/tmp/d").unwrap().mode.bits(), 0o700);
    let mut renamed = entries.remove("/tmp/d").unwrap();
    renamed.path = "/tmp/next".into();
    entries
        .insert(renamed.path.clone(), renamed.clone())
        .unwrap();
    assert_eq!(entries.get("/tmp/next"), Some(renamed));
    assert!(!entries.contains_key("/tmp/d"));
    assert_eq!(entries.len(), 1);
}
