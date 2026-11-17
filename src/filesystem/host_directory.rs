//! Type projection for pinned host directory snapshots. No link is followed.
use super::{binding::controlled_binding, ownership::payload, ownership_model::key, *};
use cap_std::fs::{Dir, OpenOptions};
use std::{format, sync::Arc};
pub(super) struct HostDirectoryView {
    root: Arc<DataRoot>,
    path: String,
    inode: Option<u64>,
}
impl HostDirectoryView {
    pub(super) fn new(root: &DataRoot, path: &str) -> Result<Self, FsError> {
        let access = root.load_access_catalog()?;
        Ok(Self {
            root: Arc::new(root.clone_file_domain()?),
            path: path.to_owned(),
            inode: root.directory_at_locked(path, &access)?.map(|d| d.id),
        })
    }
    pub(super) fn snapshot(&self, directory: &Dir) -> Result<Vec<DirectoryEntry>, FsError> {
        let guard = self.root.namespace_guard()?;
        self.snapshot_locked(&self.root, directory, &guard)
    }
    pub(super) fn snapshot_locked(&self, root: &DataRoot, directory: &Dir,
        guard: &super::transaction::NamespaceGuard<'_>) -> Result<Vec<DirectoryEntry>, FsError> {
        guard.require_root(root)?;
        let mut entries = super::directory::snapshot(directory)?;
        self.project(root, directory, &mut entries, guard)?;
        Ok(entries)
    }
    pub(super) fn project(
        &self,
        root: &DataRoot,
        directory: &Dir,
        entries: &mut [DirectoryEntry],
        guard: &super::transaction::NamespaceGuard<'_>,
    ) -> Result<(), FsError> {
        guard.require_root(root)?;
        if !Arc::ptr_eq(&root.family, &self.root.family) {
            return Err(FsError::InvalidRoot);
        }
        let path = if let Some(id) = self.inode {
            match root
                .load_access_catalog()?
                .directories
                .values()
                .find(|d| d.id == id)
            {
                Some(info) => info.path,
                // An unlinked directory was empty; the retained handle grants no
                // authority to classify an unrelated replacement at the old path.
                None => return Ok(()),
            }
        } else {
            self.path.clone()
        };
        let Some(store) = root.existing_owner_store()? else {
            return Ok(());
        };
        let catalog = root.load_owner_catalog(&store)?;
        for entry in entries {
            let Some(name) = entry.name.to_str() else {
                continue;
            };
            let Ok(path) = key(&format!("{path}/{name}")) else {
                continue;
            };
            let Some(info) = catalog.entries.get(&path) else {
                continue;
            };
            if info.kind.is_regular() {
                continue;
            }
            if entry.kind != ObjectKind::File {
                return Err(FsError::OwnershipBindingUnconfirmed);
            }
            root.verify_storage(&info)?;
            let options = OpenOptions::new().read(true).write(true).to_owned();
            let public = directory.open_with(&entry.name, &options)?.into_std();
            let private = root.dir.open_with(payload(&info), &options)?.into_std();
            if controlled_binding(&public, &private) != ObjectBinding::SameObject {
                return Err(FsError::OwnershipBindingUnconfirmed);
            }
            entry.kind = ObjectKind::Symlink;
        }
        Ok(())
    }
}
