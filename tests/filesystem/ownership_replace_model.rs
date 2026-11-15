use super::*;
use std::{assert, assert_eq, matches};
#[test]
fn host_rename_intent_exact_graph_and_v3_roundtrip_reject_unrelated_changes() {
    let mut old = Catalog::empty();
    let owner = FileOwner { uid: 0, gid: 0 };
    let mode = VirtualMode::new(0o600).unwrap();
    let source = old
        .allocate("/root/source", owner, mode, OwnedStorageClass::UserData)
        .unwrap();
    let victim = old
        .allocate("/root/dest", owner, mode, OwnedStorageClass::UserData)
        .unwrap();
    old.allocate("/root/other", owner, mode, OwnedStorageClass::UserData)
        .unwrap();
    let mut alias = victim.clone();
    alias.path = "/root/alias".into();
    old.entries.insert(alias.path.clone(), alias).unwrap();
    let mut new = old.clone();
    new.entries.remove(&source.path);
    new.entries.remove(&victim.path);
    let mut moved = source;
    moved.path = victim.path;
    new.entries.insert(moved.path.clone(), moved).unwrap();
    new.revision += 1;
    let intent = Intent {
        old,
        new,
        stage: "-".into(),
        access: None,
    };
    assert!(matches!(
        classify(&intent).unwrap(),
        Mutation::Replace { .. }
    ));
    let bytes = intent.encode().unwrap();
    assert!(bytes.starts_with(b"ELARA_OWNERSHIP_INTENT_V3\n"));
    assert_eq!(Intent::decode(&bytes).unwrap().encode().unwrap(), bytes);
    for defect in 0..6 {
        let mut bad = intent.clone();
        match defect {
            0 => {
                bad.new.entries.inode_mut("/root/dest").unwrap().mode =
                    VirtualMode::new(0o777).unwrap()
            }
            1 => {
                bad.new.entries.remove("/root/alias");
            }
            2 => {
                bad.new
                    .entries
                    .inode_mut("/root/other")
                    .unwrap()
                    .storage_generation += 1
            }
            3 => bad.new.next_id += 1,
            4 => bad.stage = "txn-1-2".into(),
            _ => bad.new.revision += 1,
        }
        assert!(classify(&bad).is_err());
        assert!(bad.encode().is_err());
    }
}
