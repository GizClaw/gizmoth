//! Host-backed work directories and non-destructive legacy disk migration.
use super::{guest::protocol, storage, vm};
use std::{
    fs,
    io::{self, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub struct Directory {
    pub path: PathBuf,
    pub fingerprint: String,
    pub legacy_clone: Option<PathBuf>,
    destination: PathBuf,
    marker: PathBuf,
    staging: bool,
}

fn completion(path: &Path) -> io::Result<Option<String>> {
    let path = path.join(protocol::WORK_MIGRATION_MARKER);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 256 {
        return Err(io::Error::other(
            "Invalid legacy migration completion marker",
        ));
    }
    let mut value = String::new();
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?
        .read_to_string(&mut value)?;
    Ok(Some(value))
}

fn publish_marker(path: &Path, fingerprint: &str) -> io::Result<()> {
    let temporary = path.with_extension("json.new");
    fs::write(
        &temporary,
        serde_json::to_vec(&serde_json::json!({"version":1,"source":fingerprint}))?,
    )?;
    fs::rename(temporary, path)
}

impl Directory {
    pub fn prepare(slot: &Path) -> Result<Self, String> {
        fs::create_dir_all(slot).map_err(|e| e.to_string())?;
        let destination = slot.join("work");
        let marker = slot.join("work.migrated.json");
        let legacy = slot.join("work.img");
        let mut directory = Self {
            path: destination.clone(),
            fingerprint: String::new(),
            legacy_clone: None,
            destination,
            marker,
            staging: false,
        };
        let metadata = match fs::symlink_metadata(&legacy) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        let needs_migration = !directory.marker.exists()
            && metadata
                .as_ref()
                .is_some_and(|m| m.is_file() && vm::allocated(&legacy) > 0);
        if !needs_migration {
            fs::create_dir_all(&directory.path).map_err(|e| e.to_string())?;
            return Ok(directory);
        }
        let metadata = metadata.unwrap();
        if metadata.file_type().is_symlink() {
            return Err("Legacy work disk is not a regular file".into());
        }
        let fingerprint = format!(
            "{}:{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec()
        );
        if directory.destination.exists() {
            if completion(&directory.destination)
                .map_err(|e| e.to_string())?
                .as_deref()
                == Some(fingerprint.as_str())
            {
                publish_marker(&directory.marker, &fingerprint).map_err(|e| e.to_string())?;
                return Ok(directory);
            }
            if fs::read_dir(&directory.destination)
                .map_err(|e| e.to_string())?
                .next()
                .is_some()
            {
                return Err("The host work directory already contains data; the original work disk was retained and will not be overwritten".into());
            }
        }
        let staging = slot.join("work.migrating");
        if staging.exists() {
            remove_owned_tree(&staging).map_err(|e| e.to_string())?;
        }
        fs::create_dir(&staging).map_err(|e| e.to_string())?;
        let clone = slot.join("work.migration.img");
        if clone.exists() {
            fs::remove_file(&clone).map_err(|e| e.to_string())?;
        }
        // On APFS std::fs::copy uses a clone when possible. The clone, not the
        // original, is mounted writable so ext4 can replay an unfinished journal.
        if let Err(error) = fs::copy(&legacy, &clone) {
            let _ = fs::remove_file(&clone);
            let _ = remove_owned_tree(&staging);
            return Err(error.to_string());
        }
        directory.path = staging;
        directory.fingerprint = fingerprint;
        directory.legacy_clone = Some(clone);
        directory.staging = true;
        Ok(directory)
    }

    pub fn publish(&mut self) -> Result<(), String> {
        if !self.staging {
            return Ok(());
        }
        if completion(&self.path)
            .map_err(|e| e.to_string())?
            .as_deref()
            != Some(self.fingerprint.as_str())
        {
            return Err(
                "The legacy work migration did not complete; original disk retained".into(),
            );
        }
        if self.destination.exists() {
            fs::remove_dir(&self.destination).map_err(|e| e.to_string())?;
        }
        fs::rename(&self.path, &self.destination).map_err(|e| e.to_string())?;
        self.staging = false;
        self.path = self.destination.clone();
        publish_marker(&self.marker, &self.fingerprint).map_err(|e| e.to_string())
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        if self.staging {
            let _ = remove_owned_tree(&self.path);
        }
        if let Some(clone) = &self.legacy_clone {
            let _ = fs::remove_file(clone);
        }
    }
}

/// Detached slot data lives beside `slots`, outside the migration item set.
/// Keep the capability open so cleanup remains valid if the root is renamed.
pub struct Detached {
    root: cap_std::fs::Dir,
    name: String,
    path: PathBuf,
}

impl Detached {
    pub fn remove(self) -> Result<(), String> {
        remove_tree(&self.root, Path::new(&self.name)).map_err(|error| {
            format!(
                "Work directory detached, but cleanup failed at {}: {error}",
                self.path.display()
            )
        })
    }
}

/// Reap the VM first, then detach under the storage fence. Recursive deletion
/// must happen after releasing that fence; no links are followed during either.
pub fn detach(slot: &Path) -> Result<Option<Detached>, String> {
    use cap_fs_ext::DirExt;
    use cap_std::{ambient_authority, fs::Dir};
    let parent = slot.parent().ok_or("The slot has no parent directory")?;
    let root_path = parent.parent().ok_or("The slot has no working directory")?;
    let name = slot.file_name().ok_or("The slot has no directory name")?;
    let root = Dir::open_ambient_dir(root_path, ambient_authority()).map_err(|e| e.to_string())?;
    let parent =
        match root.open_dir_nofollow(parent.file_name().ok_or("Missing slots directory name")?) {
            Ok(parent) => parent,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
    let trash = format!(".slot-trash-{}", uuid::Uuid::new_v4());
    match parent.rename(name, &root, &trash) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Could not clear the work directory: {error}")),
    }
    Ok(Some(Detached {
        path: root_path.join(&trash),
        root,
        name: trash,
    }))
}

/// Recover detached data after a crash, before any VM can be started.
pub fn recover_trash(path: &Path) -> Result<(), String> {
    use cap_std::{ambient_authority, fs::Dir};
    let root = match Dir::open_ambient_dir(path, ambient_authority()) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    for entry in root.entries().map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        if name
            .to_str()
            .and_then(|name| name.strip_prefix(".slot-trash-"))
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
        {
            remove_tree(&root, Path::new(&name)).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
fn clear(slot: &Path) -> Result<(), String> {
    if let Some(detached) = detach(slot)? {
        detached.remove()?;
    }
    Ok(())
}

fn remove_owned_tree(path: &Path) -> io::Result<()> {
    use cap_std::{ambient_authority, fs::Dir};
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Missing tree parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("Missing tree name"))?;
    let parent = Dir::open_ambient_dir(parent, ambient_authority())?;
    remove_tree(&parent, Path::new(name))
}

fn remove_tree(parent: &cap_std::fs::Dir, name: &Path) -> io::Result<()> {
    use cap_fs_ext::DirExt;
    use cap_std::fs::{Permissions, PermissionsExt};
    let metadata = parent.symlink_metadata(name)?;
    if !metadata.is_dir() {
        return parent.remove_file(name);
    }
    // Build systems can leave read-only directories. Change only the directory
    // entry itself, then traverse through a no-follow directory capability.
    parent.set_symlink_permissions(
        name,
        Permissions::from_mode(metadata.permissions().mode() | 0o700),
    )?;
    let dir = parent.open_dir_nofollow(name)?;
    for entry in dir.entries()? {
        remove_tree(&dir, Path::new(&entry?.file_name()))?;
    }
    parent.remove_dir(name)
}

pub fn allocated(slot: &Path) -> u64 {
    storage::allocated(&slot.join("work")).saturating_add(vm::allocated(&slot.join("work.img")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fresh_work_uses_a_directory_and_never_creates_a_disk() {
        let root = tempfile::tempdir().unwrap();
        let work = Directory::prepare(root.path()).unwrap();
        assert!(work.path.is_dir());
        assert!(work.legacy_clone.is_none());
        assert!(work.fingerprint.is_empty());
        assert!(!root.path().join("work.img").exists());
    }
    #[test]
    fn failed_migration_retains_original_and_does_not_publish_partial_data() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("work.img");
        fs::write(&original, b"original disk bytes").unwrap();
        let staging;
        {
            let mut work = Directory::prepare(root.path()).unwrap();
            staging = work.path.clone();
            fs::create_dir(work.path.join("readonly")).unwrap();
            fs::write(work.path.join("readonly/partial"), b"partial").unwrap();
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                work.path.join("readonly"),
                fs::Permissions::from_mode(0o000),
            )
            .unwrap();
            assert!(work.publish().is_err());
        }
        assert_eq!(fs::read(original).unwrap(), b"original disk bytes");
        assert!(!staging.exists());
        assert!(!root.path().join("work.migrated.json").exists());
    }
    #[test]
    fn clearing_readonly_work_never_follows_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), b"untouched").unwrap();
        let slot = root.path().join("slots/test");
        fs::create_dir_all(slot.join("work/readonly/nested")).unwrap();
        fs::write(slot.join("work/readonly/nested/file"), b"remove").unwrap();
        symlink(&outside, slot.join("work/outside-link")).unwrap();
        fs::set_permissions(
            slot.join("work/readonly/nested"),
            fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        fs::set_permissions(
            slot.join("work/readonly"),
            fs::Permissions::from_mode(0o555),
        )
        .unwrap();
        clear(&slot).unwrap();
        assert!(!slot.exists());
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"untouched");
        assert_eq!(fs::read_dir(root.path().join("slots")).unwrap().count(), 0);
        clear(&slot).unwrap();
    }
    #[test]
    fn detached_cleanup_is_outside_migration_and_preserves_replacement_work() {
        let root = tempfile::tempdir().unwrap();
        let slot = root.path().join("slots/s12345678");
        fs::create_dir_all(slot.join("work")).unwrap();
        fs::write(slot.join("work/old"), b"retired").unwrap();
        let detached = detach(&slot).unwrap().unwrap();
        assert_eq!(detached.path.parent(), Some(root.path()));
        assert!(!super::storage::ITEMS
            .iter()
            .any(|item| detached.path == root.path().join(item)));
        fs::create_dir_all(slot.join("work")).unwrap();
        fs::write(slot.join("work/new"), b"keep").unwrap();
        detached.remove().unwrap();
        assert_eq!(fs::read(slot.join("work/new")).unwrap(), b"keep");
    }

    #[test]
    fn abandoned_slot_cleanup_never_follows_links() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("keep"), b"outside").unwrap();
        symlink(
            outside.path(),
            root.path()
                .join(format!(".slot-trash-{}", uuid::Uuid::new_v4())),
        )
        .unwrap();
        recover_trash(root.path()).unwrap();
        assert_eq!(fs::read(outside.path().join("keep")).unwrap(), b"outside");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn migration_refuses_existing_data_and_symlink_completion_markers() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("work.img"), b"original").unwrap();
        fs::create_dir(root.path().join("work")).unwrap();
        fs::write(root.path().join("work/existing"), b"keep").unwrap();
        assert!(Directory::prepare(root.path()).is_err());
        assert_eq!(
            fs::read(root.path().join("work/existing")).unwrap(),
            b"keep"
        );
        fs::remove_file(root.path().join("work/existing")).unwrap();
        let mut work = Directory::prepare(root.path()).unwrap();
        fs::write(root.path().join("forged-marker"), &work.fingerprint).unwrap();
        symlink(
            root.path().join("forged-marker"),
            work.path.join(protocol::WORK_MIGRATION_MARKER),
        )
        .unwrap();
        assert!(work.publish().is_err());
        assert_eq!(fs::read(root.path().join("work.img")).unwrap(), b"original");
    }
    #[test]
    fn completed_migration_publishes_and_is_recovered_without_recopying() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("work.img"), b"original").unwrap();
        let mut work = Directory::prepare(root.path()).unwrap();
        fs::write(work.path.join("kept"), b"keep").unwrap();
        fs::write(
            work.path.join(protocol::WORK_MIGRATION_MARKER),
            &work.fingerprint,
        )
        .unwrap();
        work.publish().unwrap();
        assert_eq!(fs::read(root.path().join("work/kept")).unwrap(), b"keep");
        assert!(root.path().join("work.img").is_file());
        drop(work);
        let again = Directory::prepare(root.path()).unwrap();
        assert!(again.legacy_clone.is_none());
        assert_eq!(again.path, root.path().join("work"));
    }
}
