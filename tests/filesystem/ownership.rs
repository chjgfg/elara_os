//! Explicit ownership, stable virtual IDs, modes and metadata/data transactions.
use super::fixture;
use crate::filesystem::{DataRoot, FsError, OwnershipState, VirtualMode};
use crate::users::Account;
use std::{assert, assert_eq, fs, matches};
fn alice() -> Account { Account { name: "alice".into(), uid: 1000, home: "/home/alice".into() } }
fn bob() -> Account { Account { name: "bob".into(), uid: 1001, home: "/home/bob".into() } }
fn root_user() -> Account { Account { name: "root".into(), uid: 0, home: "/root".into() } }

#[test]
fn create_owned_data_and_reopen_keep_id_owner_mode_and_native_bytes() {
    let path = fixture(); let root = DataRoot::open(&path).unwrap();
    let receipt = root.create_owned_file(&alice(), "/tmp/owned", b"hello", VirtualMode::new(0o600).unwrap()).unwrap();
    let OwnershipState::Managed(first) = root.owned_file_info("/tmp/owned").unwrap() else { panic!("not managed"); };
    assert_eq!(first.id, receipt.object);
    assert_eq!(first.owner.uid, 1000); assert_eq!(first.owner.gid, 1000);
    assert_eq!(first.mode.bits(), 0o600);
    let reopened = DataRoot::open(&path).unwrap();
    assert_eq!(reopened.owned_file_info("/tmp/owned").unwrap(), OwnershipState::Managed(first));
    assert_eq!(reopened.read_owned_file(&alice(), "/tmp/owned", 128).unwrap(), b"hello");
    assert_eq!(fs::read(path.join("tmp/owned")).unwrap(), b"hello");
}

#[test]
fn another_user_cannot_read_write_rename_remove_or_change_the_mode() {
    let path = fixture(); let root = DataRoot::open(&path).unwrap();
    root.create_owned_file(&alice(), "/tmp/private", b"keep", VirtualMode::new(0o600).unwrap()).unwrap();
    assert!(matches!(root.read_owned_file(&bob(), "/tmp/private", 128), Err(FsError::OwnershipDenied)));
    assert!(matches!(root.append_owned_file(&bob(), "/tmp/private", b"bad"), Err(FsError::OwnershipDenied)));
    assert!(matches!(root.replace_owned_file(&bob(), "/tmp/private", b"bad"), Err(FsError::OwnershipDenied)));
    assert!(matches!(root.rename_owned_file(&bob(), "/tmp/private", "/tmp/stolen"), Err(FsError::OwnershipDenied)));
    assert!(matches!(root.remove_owned_file(&bob(), "/tmp/private"), Err(FsError::OwnershipDenied)));
    assert!(matches!(root.chmod_owned_file(&bob(), "/tmp/private", VirtualMode::new(0o666).unwrap()), Err(FsError::OwnershipDenied)));
    assert_eq!(fs::read(path.join("tmp/private")).unwrap(), b"keep");
}

#[test]
fn mode_change_controls_new_access_without_reassigning_owner_or_id() {
    let path = fixture(); let root = DataRoot::open(&path).unwrap();
    root.create_owned_file(&alice(), "/tmp/shared", b"same", VirtualMode::new(0o600).unwrap()).unwrap();
    let before = root.owned_file_info("/tmp/shared").unwrap();
    root.chmod_owned_file(&alice(), "/tmp/shared", VirtualMode::new(0o644).unwrap()).unwrap();
    assert_eq!(root.read_owned_file(&bob(), "/tmp/shared", 128).unwrap(), b"same");
    assert!(matches!(root.append_owned_file(&bob(), "/tmp/shared", b"bad"), Err(FsError::OwnershipDenied)));
    let OwnershipState::Managed(before) = before else { panic!() };
    let OwnershipState::Managed(after) = root.owned_file_info("/tmp/shared").unwrap() else { panic!() };
    assert_eq!(before.id, after.id); assert_eq!(before.owner, after.owner);
}

#[test]
fn replace_and_rename_keep_object_id_and_remove_invalidates_the_name() {
    let path = fixture(); let root = DataRoot::open(&path).unwrap();
    root.create_owned_file(&alice(), "/tmp/source", b"old", VirtualMode::new(0o600).unwrap()).unwrap();
    let OwnershipState::Managed(before) = root.owned_file_info("/tmp/source").unwrap() else { panic!() };
    root.replace_owned_file(&alice(), "/tmp/source", b"new").unwrap();
    root.rename_owned_file(&alice(), "/tmp/source", "/tmp/moved").unwrap();
    let OwnershipState::Managed(after) = root.owned_file_info("/tmp/moved").unwrap() else { panic!() };
    assert_eq!(before.id, after.id); assert_eq!(before.owner, after.owner);
    assert_eq!(after.storage_generation, before.storage_generation + 1);
    assert!(!path.join("tmp/source").exists());
    assert_eq!(fs::read(path.join("tmp/moved")).unwrap(), b"new");
    root.remove_owned_file(&alice(), "/tmp/moved").unwrap();
    assert!(!path.join("tmp/moved").exists());
    assert_eq!(root.owned_file_info("/tmp/moved").unwrap(), OwnershipState::LegacyUnknown);
}

#[test]
fn existing_legacy_files_are_not_automatically_adopted_or_overwritten() {
    let path = fixture(); fs::write(path.join("tmp/legacy"), b"unknown owner").unwrap();
    let root = DataRoot::open(&path).unwrap();
    assert_eq!(root.owned_file_info("/tmp/legacy").unwrap(), OwnershipState::LegacyUnknown);
    assert!(matches!(root.create_owned_file(&alice(), "/tmp/legacy", b"bad", VirtualMode::new(0o600).unwrap()), Err(FsError::AlreadyExists)));
    assert!(matches!(root.replace_owned_file(&root_user(), "/tmp/legacy", b"bad"), Err(FsError::LegacyOwnershipUnknown)));
    assert_eq!(fs::read(path.join("tmp/legacy")).unwrap(), b"unknown owner");
}

#[test]
fn readonly_system_paths_and_implicit_lifecycle_changes_are_refused() {
    let path = fixture(); let root = DataRoot::open(&path).unwrap();
    assert!(matches!(root.create_owned_file(&root_user(), "/usr/bad", b"bad", VirtualMode::new(0o600).unwrap()), Err(FsError::ReadOnlySystemPath)));
    root.create_owned_file(&root_user(), "/root/persistent", b"keep", VirtualMode::new(0o600).unwrap()).unwrap();
    assert!(matches!(root.rename_owned_file(&root_user(), "/root/persistent", "/tmp/bad"), Err(FsError::OwnershipClassMismatch)));
    assert_eq!(fs::read(path.join("root/persistent")).unwrap(), b"keep");
}

#[test]
fn host_replacement_of_a_name_is_not_inherited_as_a_managed_object() {
    let path = fixture(); let root = DataRoot::open(&path).unwrap();
    root.create_owned_file(&alice(), "/tmp/owned", b"same", VirtualMode::new(0o600).unwrap()).unwrap();
    fs::remove_file(path.join("tmp/owned")).unwrap();
    fs::write(path.join("tmp/owned"), b"same").unwrap();
    assert!(matches!(root.owned_file_info("/tmp/owned").unwrap(), OwnershipState::Unconfirmed { .. }));
    assert!(matches!(root.read_owned_file(&alice(), "/tmp/owned", 128), Err(FsError::OwnershipBindingUnconfirmed)));
    assert!(matches!(root.remove_owned_file(&alice(), "/tmp/owned"), Err(FsError::OwnershipBindingUnconfirmed)));
    assert_eq!(fs::read(path.join("tmp/owned")).unwrap(), b"same");
}

#[test]
fn creation_faults_recover_before_any_account_can_observe_anonymous_data() {
    use crate::filesystem::ownership::OwnershipFault;
    for fault in [OwnershipFault::AfterStorage, OwnershipFault::AfterPublicName, OwnershipFault::AfterCatalog] {
        let path = fixture(); let root = DataRoot::open(&path).unwrap();
        let result = root.create_owned_file_with_fault(&alice(), "/tmp/recover", b"committed-or-recoverable", VirtualMode::new(0o600).unwrap(), fault);
        assert!(matches!(result, Err(FsError::OwnershipCommitPending { .. })));
        // An unrelated account's first access must resolve the intent then deny
        // the read, rather than granting access through the old empty catalog.
        assert!(matches!(root.read_owned_file(&bob(), "/tmp/recover", 128), Err(FsError::OwnershipDenied)));
        assert_eq!(root.read_owned_file(&alice(), "/tmp/recover", 128).unwrap(), b"committed-or-recoverable");
        let OwnershipState::Managed(info) = root.owned_file_info("/tmp/recover").unwrap() else { panic!() };
        assert_eq!(info.owner.uid, alice().uid);
        let reopened = DataRoot::open(&path).unwrap();
        reopened.recover_transactions().unwrap();
        reopened.recover_transactions().unwrap();
        assert_eq!(reopened.owned_file_info("/tmp/recover").unwrap(), OwnershipState::Managed(info));
        assert!(!path.join("var/.elara-objects/pending").exists());
    }
}

#[test]
fn replacement_and_partially_completed_retirement_are_idempotently_recovered() {
    use crate::filesystem::ownership::OwnershipFault;
    for fault in [OwnershipFault::AfterStorage, OwnershipFault::AfterPublicName, OwnershipFault::AfterCatalog, OwnershipFault::AfterOldPayloadRemoval] {
        let path = fixture(); let root = DataRoot::open(&path).unwrap();
        root.create_owned_file(&alice(), "/tmp/recover", b"old", VirtualMode::new(0o600).unwrap()).unwrap();
        let OwnershipState::Managed(before) = root.owned_file_info("/tmp/recover").unwrap() else { panic!() };
        let result = root.replace_owned_file_with_fault(&alice(), "/tmp/recover", b"new-complete", fault);
        assert!(matches!(result, Err(FsError::OwnershipCommitPending { .. })));
        root.recover_transactions().unwrap();
        root.recover_transactions().unwrap();
        assert_eq!(root.read_owned_file(&alice(), "/tmp/recover", 128).unwrap(), b"new-complete");
        let OwnershipState::Managed(after) = root.owned_file_info("/tmp/recover").unwrap() else { panic!() };
        assert_eq!(before.id, after.id); assert_eq!(before.owner, after.owner);
        assert_eq!(after.storage_generation, before.storage_generation + 1);
        assert!(!path.join("var/.elara-objects/pending").exists());
    }
}

#[test]
fn raw_file_apis_cannot_bypass_an_explicit_managed_owner() {
    let path = fixture(); let root = DataRoot::open(&path).unwrap();
    root.create_owned_file(&alice(), "/tmp/managed", b"keep", VirtualMode::new(0o600).unwrap()).unwrap();
    assert!(matches!(root.read_file("/tmp/managed", 128), Err(FsError::OwnershipContextRequired)));
    assert!(matches!(root.append_file("/tmp/managed", b"bad"), Err(FsError::OwnershipContextRequired)));
    assert!(matches!(root.write_file("/tmp/managed", b"bad"), Err(FsError::OwnershipContextRequired)));
    assert!(matches!(root.remove_file("/tmp/managed"), Err(FsError::OwnershipContextRequired)));
    assert!(matches!(root.open_file("/tmp/managed", crate::filesystem::FileMode::READ), Err(FsError::OwnershipContextRequired)));
    assert_eq!(fs::read(path.join("tmp/managed")).unwrap(), b"keep");
}

#[test]
fn counter_exhaustion_and_invalid_mode_do_not_partially_allocate() {
    use crate::filesystem::ownership_model::Catalog;
    use crate::filesystem::{FileOwner, OwnedStorageClass};
    let mut catalog = Catalog::empty(); catalog.next_id = u64::MAX;
    let before = catalog.encode().unwrap();
    assert!(matches!(catalog.allocate("/tmp/refused", FileOwner { uid: 1000, gid: 1000 }, VirtualMode::new(0o600).unwrap(), OwnedStorageClass::Temporary), Err(FsError::OwnershipIdentityExhausted)));
    assert_eq!(catalog.encode().unwrap(), before);
    assert!(VirtualMode::new(0o1000).is_err());
}

#[test]
fn removing_only_the_object_mode_guard_allows_the_forbidden_read() {
    let path = fixture(); let root = DataRoot::open(&path).unwrap();
    root.create_owned_file(&alice(), "/tmp/guarded", b"private", VirtualMode::new(0o600).unwrap()).unwrap();
    assert!(matches!(root.read_owned_file(&bob(), "/tmp/guarded", 128), Err(FsError::OwnershipDenied)));
    assert_eq!(root.read_without_object_mode_guard(&bob(), "/tmp/guarded", 128).unwrap(), b"private");
    assert_eq!(fs::read(path.join("tmp/guarded")).unwrap(), b"private");
}

#[test]
fn private_store_unknown_contents_and_host_aliases_are_not_permission_backdoors() {
    let path = fixture(); let root = DataRoot::open(&path).unwrap();
    root.create_owned_file(&alice(), "/tmp/private", b"secret", VirtualMode::new(0o600).unwrap()).unwrap();
    fs::hard_link(path.join("tmp/private"), path.join("tmp/alias")).unwrap();
    assert!(matches!(root.read_file_as(&bob(), "/tmp/alias", 128), Err(FsError::OwnershipBindingUnconfirmed)));
    assert_eq!(fs::read(path.join("tmp/private")).unwrap(), b"secret");
    assert!(matches!(root.read_file("/var/.elara-objects/catalog", 128), Err(FsError::InvalidPath)));
    let other = fixture();
    fs::create_dir(other.join("var/.elara-objects")).unwrap();
    fs::write(other.join("var/.elara-objects/unknown"), b"preserve").unwrap();
    let root = DataRoot::open(&other).unwrap();
    assert!(matches!(root.create_owned_file(&alice(), "/tmp/refused", b"bad", VirtualMode::new(0o600).unwrap()), Err(FsError::UnownedOwnershipStore)));
    assert_eq!(fs::read(other.join("var/.elara-objects/unknown")).unwrap(), b"preserve");
    assert!(!other.join("tmp/refused").exists());
}

#[test]
fn canonical_text_catalog_roundtrips_and_rejects_duplicate_ids() {
    use crate::filesystem::ownership_model::Catalog;
    use crate::filesystem::{FileOwner, OwnedStorageClass};
    let mut catalog = Catalog::empty();
    catalog.allocate("/tmp/星澜", FileOwner { uid: 1000, gid: 1000 }, VirtualMode::new(0o600).unwrap(), OwnedStorageClass::Temporary).unwrap();
    let bytes = catalog.encode().unwrap();
    assert_eq!(Catalog::decode(&bytes).unwrap().encode().unwrap(), bytes);
    let mut duplicate = bytes.clone();
    let entry = bytes.split(|b| *b == b'\n').nth(2).unwrap();
    duplicate.extend_from_slice(entry); duplicate.push(b'\n');
    assert!(Catalog::decode(&duplicate).is_err());
}
