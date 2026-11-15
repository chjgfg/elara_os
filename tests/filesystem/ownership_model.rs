use super::*;
use std::{assert, assert_eq, format};

fn info() -> OwnedFileInfo {
    OwnedFileInfo {
        kind: HostFileKind::Regular,
        id: ObjectId(1),
        storage_generation: 1,
        path: "/root/a".into(),
        owner: FileOwner { uid: 0, gid: 0 },
        mode: VirtualMode::new(0o600).unwrap(),
        storage_class: OwnedStorageClass::UserData,
        delete_condition: DeleteCondition::OwnerRequest,
    }
}
#[test]
fn inode_model_aliases_share_metadata_but_projections_are_detached() {
    let mut entries = OwnedEntries::default();
    let a = info();
    entries.insert(a.path.clone(), a.clone()).unwrap();
    let mut b = a.clone();
    b.path = "/root/b".into();
    entries.insert(b.path.clone(), b).unwrap();
    assert_eq!(entries.names.len(), 2);
    assert_eq!(entries.inodes.len(), 1);
    entries.inode_mut("/root/a").unwrap().mode = VirtualMode::new(0o640).unwrap();
    assert_eq!(entries.get("/root/b").unwrap().mode.bits(), 0o640);
    let mut detached = entries.get("/root/a").unwrap();
    detached.owner.uid = 99;
    assert_eq!(entries.get("/root/a").unwrap().owner.uid, 0);
    assert_eq!(entries.remove("/root/a").unwrap().id, a.id);
    assert_eq!(entries.inodes.len(), 1);
    assert_eq!(entries.get("/root/b").unwrap().mode.bits(), 0o640);
    entries.remove("/root/b").unwrap();
    assert!(entries.inodes.is_empty());
    assert!(entries.names.is_empty());
}
#[test]
fn inode_model_conflicting_alias_never_changes_authoritative_metadata() {
    let mut entries = OwnedEntries::default();
    let a = info();
    entries.insert(a.path.clone(), a.clone()).unwrap();
    let mut b = a.clone();
    b.path = "/root/b".into();
    b.owner.uid = 99;
    assert!(entries.insert(b.path.clone(), b).is_err());
    assert_eq!(entries.get("/root/a"), Some(a.clone()));
    assert!(!entries.contains_key("/root/b"));
    let mut replacement = a.clone();
    replacement.id = ObjectId(2);
    assert!(entries.insert(a.path.clone(), replacement).is_err());
    assert_eq!(entries.get("/root/a"), Some(a));
    assert_eq!(entries.inodes.len(), 1);
}
#[test]
fn inode_model_legacy_v1_through_v5_roundtrip_without_reencoding() {
    for (magic, path, mode, class, suffix) in [
        (MAGIC, "/root/a", "600", "user", ""),
        (MAGIC_V2, "/tmp/a", "600", "tmp", " task:1"),
        (MAGIC_V3, "/root/a", "4600", "user", " owner"),
        (MAGIC_V4, "/run/a", "600", "run", " task:1"),
        (MAGIC_V5, "/opt/a", "600", "app", " resource:1"),
    ] {
        let bytes = format!(
            "{magic}
2 2
1 1 0 0 {mode} {class} {}{suffix}
",
            hex(path)
        )
        .into_bytes();
        let decoded = Catalog::decode(&bytes).unwrap();
        assert_eq!(decoded.entries.names.len(), 1);
        assert_eq!(decoded.entries.inodes.len(), 1);
        assert_eq!(decoded.encode().unwrap(), bytes);
    }
}
#[test]
fn inode_model_v6_roundtrip_and_legacy_duplicate_ids_remain_rejected() {
    let mut catalog = Catalog::empty();
    let a = catalog
        .allocate("/root/a", info().owner, info().mode, info().storage_class)
        .unwrap();
    let mut b = a;
    b.path = "/root/b".into();
    catalog.entries.insert(b.path.clone(), b).unwrap();
    let encoded = catalog.encode().unwrap();
    assert!(encoded.starts_with(
        format!(
            "{MAGIC_V6}
"
        )
        .as_bytes()
    ));
    let decoded = Catalog::decode(&encoded).unwrap();
    assert_eq!(decoded.encode().unwrap(), encoded);
    assert_eq!(decoded.entries.names.len(), 2);
    assert_eq!(decoded.entries.inodes.len(), 1);
    let duplicate = format!(
        "{MAGIC}
2 2
1 1 0 0 600 user {}
1 1 0 0 600 user {}
",
        hex("/root/a"),
        hex("/root/b")
    );
    assert!(Catalog::decode(duplicate.as_bytes()).is_err());
}

#[test]
fn inode_model_v6_rejects_dangling_orphan_zero_generation_and_unknown_fields() {
    let mut catalog = Catalog::empty();
    let a = catalog
        .allocate("/root/a", info().owner, info().mode, info().storage_class)
        .unwrap();
    let mut b = a;
    b.path = "/root/b".into();
    catalog.entries.insert(b.path.clone(), b).unwrap();
    let original = serde_json::to_value(&catalog).unwrap();
    let valid: Catalog = serde_json::from_value(original.clone()).unwrap();
    assert_eq!(valid.encode().unwrap(), catalog.encode().unwrap());
    for defect in 0..5 {
        let mut value = original.clone();
        match defect {
            0 => value["entries"]["names"]["/root/b"] = serde_json::json!(99),
            1 => {
                value["entries"]["inodes"]["2"] = value["entries"]["inodes"]["1"].clone();
                value["next_id"] = serde_json::json!(3);
            }
            2 => value["entries"]["inodes"]["1"]["storage_generation"] = serde_json::json!(0),
            3 => value["entries"]["inodes"]["1"]["foreign"] = serde_json::json!(true),
            _ => value["next_id"] = serde_json::json!(1),
        }
        // Check the graph gate itself, not rejection caused by JSON key order.
        match serde_json::from_value::<Catalog>(value) {
            Ok(invalid) => assert!(invalid.encode().is_err()),
            Err(_) => assert_eq!(defect, 3),
        }
    }
}

#[test]
fn inode_model_v7_links_roundtrip_and_refuse_aliases_or_host_syntax() {
    let mut catalog = Catalog::empty();
    let a = catalog
        .allocate(
            "/root/link",
            info().owner,
            info().mode,
            info().storage_class,
        )
        .unwrap();
    catalog.entries.inode_mut(&a.path).unwrap().kind = HostFileKind::Symlink {
        target: "../tmp/data".into(),
    };
    let bytes = catalog.encode().unwrap();
    assert!(bytes.starts_with(format!("{MAGIC_V7}\n").as_bytes()));
    assert_eq!(Catalog::decode(&bytes).unwrap().encode().unwrap(), bytes);
    let mut alias = catalog.entries.get(&a.path).unwrap();
    alias.path = "/root/alias".into();
    catalog.entries.insert(alias.path.clone(), alias).unwrap();
    assert!(catalog.encode().is_err());
    catalog.entries.remove("/root/alias").unwrap();
    catalog.entries.inode_mut(&a.path).unwrap().kind = HostFileKind::Symlink {
        target: "C:/Windows".into(),
    };
    assert!(catalog.encode().is_err());
}
