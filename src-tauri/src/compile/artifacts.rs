//! Build outputs collected from compile VMs. Each artifact is an immutable
//! directory `<working directory>/artifacts/<id>/files/` plus `artifact.json`,
//! shared by every agent through its absolute path or ID.
use super::guest::protocol;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, RwLock,
    },
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub id: String,
    pub job_id: String,
    pub client: String,
    pub title: String,
    pub workspace: String,
    pub command: Vec<String>,
    pub created_at: u64,
    #[serde(default)]
    pub pinned: bool,
    pub files: Vec<ArtifactFile>,
    pub bytes: u64,
}

pub struct Store {
    /// Moves with the working directory; no build runs while it changes.
    root: RwLock<PathBuf>,
    index: Mutex<Vec<Artifact>>,
    identity: String,
    /// Updated under the index lock, independently of job/log notifications.
    revision: AtomicU64,
}

pub struct Staging {
    pub id: String,
    dir: PathBuf,
    files: Vec<ArtifactFile>,
    bytes: u64,
}

impl Staging {
    pub fn write(
        &mut self,
        relative: &str,
        size: u64,
        mode: u32,
        data: &mut dyn Read,
    ) -> Result<(), String> {
        if !protocol::valid_relative(relative) {
            return Err(format!("invalid artifact path {relative}"));
        }
        let target = self.dir.join("files").join(relative);
        fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
        let mut file = File::create(&target).map_err(|e| e.to_string())?;
        let mut hasher = Sha256::new();
        let (mut buffer, mut left) = (vec![0u8; 256 * 1024], size);
        while left > 0 {
            let want = left.min(buffer.len() as u64) as usize;
            let n = data.read(&mut buffer[..want]).map_err(|e| e.to_string())?;
            if n == 0 {
                return Err(format!("{relative}: truncated"));
            }
            hasher.update(&buffer[..n]);
            file.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
            left -= n as u64;
        }
        file.sync_all().map_err(|e| e.to_string())?;
        // Artifacts are read-only records; keep the executable bit if present.
        let mode = if mode & 0o111 != 0 { 0o555 } else { 0o444 };
        fs::set_permissions(&target, fs::Permissions::from_mode(mode))
            .map_err(|e| e.to_string())?;
        self.files.push(ArtifactFile {
            path: relative.into(),
            size,
            sha256: hasher
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        });
        self.bytes += size;
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if self.dir.exists() {
            make_writable(&self.dir);
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

fn make_writable(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o755));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            make_writable(&path);
        } else {
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o644));
        }
    }
}

pub fn valid_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok()
}

impl Store {
    pub fn new(root: PathBuf) -> Store {
        // Not recursive: a working directory on a missing disk stays missing.
        let _ = fs::create_dir(&root);
        let mut index = Vec::new();
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name == ".staging" {
                    let _ = fs::remove_dir_all(entry.path());
                    continue;
                }
                if !valid_id(&name) {
                    continue;
                }
                match fs::read(entry.path().join("artifact.json"))
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Artifact>(&b).ok())
                {
                    Some(artifact) if artifact.id == name => index.push(artifact),
                    _ => {}
                }
            }
        }
        index.sort_by_key(|a| std::cmp::Reverse(a.created_at));
        Store {
            root: RwLock::new(root),
            index: Mutex::new(index),
            identity: uuid::Uuid::new_v4().to_string(),
            revision: AtomicU64::new(0),
        }
    }

    pub fn root(&self) -> PathBuf {
        self.root.read().unwrap().clone()
    }

    /// Points the store at the moved artifacts directory; IDs stay the same.
    pub fn relocate(&self, root: PathBuf) {
        let _index = self.index.lock().unwrap();
        let _ = fs::create_dir(&root);
        *self.root.write().unwrap() = root;
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    pub fn begin(&self) -> Result<Staging, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let dir = self.root().join(".staging").join(&id);
        fs::create_dir_all(dir.join("files")).map_err(|e| e.to_string())?;
        Ok(Staging {
            id,
            dir,
            files: Vec::new(),
            bytes: 0,
        })
    }

    pub fn commit(&self, mut staging: Staging, mut artifact: Artifact) -> Result<Artifact, String> {
        artifact.id = staging.id.clone();
        artifact.files = std::mem::take(&mut staging.files);
        artifact.bytes = staging.bytes;
        let bytes = serde_json::to_vec_pretty(&artifact).map_err(|e| e.to_string())?;
        fs::write(staging.dir.join("artifact.json"), bytes).map_err(|e| e.to_string())?;
        let target = self.root().join(&artifact.id);
        fs::rename(&staging.dir, &target).map_err(|e| e.to_string())?;
        let mut index = self.index.lock().unwrap();
        index.insert(0, artifact.clone());
        self.revision.fetch_add(1, Ordering::Relaxed);
        Ok(artifact)
    }

    /// A coherent list and summary. The token changes for artifact metadata and
    /// paths, and a new store identity prevents reuse after a service restart.
    pub fn snapshot(&self) -> (Vec<Artifact>, serde_json::Value) {
        let index = self.index.lock().unwrap();
        let summary = serde_json::json!({
            "count": index.len(),
            "bytes": index.iter().map(|artifact| artifact.bytes).sum::<u64>(),
            "dir": self.root().display().to_string(),
            "revision": format!("{}:{}", self.identity, self.revision.load(Ordering::Relaxed)),
        });
        (index.clone(), summary)
    }

    pub fn list(&self) -> Vec<Artifact> {
        self.index.lock().unwrap().clone()
    }

    pub fn get(&self, id: &str) -> Option<Artifact> {
        self.index
            .lock()
            .unwrap()
            .iter()
            .find(|a| a.id == id)
            .cloned()
    }

    pub fn dir(&self, id: &str) -> PathBuf {
        self.root().join(id).join("files")
    }

    /// Resolves one file of an artifact. Without `file`, the artifact must
    /// contain exactly one file with the given suffix.
    pub fn file(&self, id: &str, file: Option<&str>, suffix: &str) -> Result<PathBuf, String> {
        let artifact = self
            .get(id)
            .ok_or_else(|| format!("Artifact {id} not found"))?;
        let chosen = match file {
            Some(path) => artifact
                .files
                .iter()
                .find(|f| f.path == path)
                .ok_or_else(|| format!("{path} is not part of artifact {id}"))?,
            None => {
                let matching: Vec<_> = artifact
                    .files
                    .iter()
                    .filter(|f| f.path.ends_with(suffix))
                    .collect();
                match matching.as_slice() {
                    [one] => *one,
                    [] => return Err(format!("Artifact {id} has no {suffix} file")),
                    _ => {
                        return Err(format!(
                            "Artifact {id} has several {suffix} files; pass file"
                        ))
                    }
                }
            }
        };
        Ok(self.dir(id).join(&chosen.path))
    }

    pub fn set_pinned(&self, id: &str, pinned: bool) -> Result<Artifact, String> {
        let mut index = self.index.lock().unwrap();
        let artifact = index
            .iter_mut()
            .find(|a| a.id == id)
            .ok_or("Artifact not found")?;
        if artifact.pinned == pinned {
            return Ok(artifact.clone());
        }
        let mut next = artifact.clone();
        next.pinned = pinned;
        let bytes = serde_json::to_vec_pretty(&next).map_err(|e| e.to_string())?;
        let path = self.root().join(id).join("artifact.json");
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, bytes)
            .and_then(|_| fs::rename(&temporary, &path))
            .map_err(|e| e.to_string())?;
        *artifact = next;
        self.revision.fetch_add(1, Ordering::Relaxed);
        Ok(artifact.clone())
    }

    /// Remove from disk before committing the index change. Callers hold the
    /// index lock through both steps, including the current pin check.
    fn delete_at(&self, index: &mut Vec<Artifact>, position: usize) -> Result<Artifact, String> {
        let dir = self.root().join(&index[position].id);
        make_writable(&dir);
        match fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        let artifact = index.remove(position);
        self.revision.fetch_add(1, Ordering::Relaxed);
        Ok(artifact)
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        if !valid_id(id) {
            return Err("Invalid artifact ID".into());
        }
        let mut index = self.index.lock().unwrap();
        let position = index
            .iter()
            .position(|a| a.id == id)
            .ok_or("Artifact not found")?;
        self.delete_at(&mut index, position).map(|_| ())
    }

    /// Clears committed, unpinned artifacts. Pin changes and commits use the
    /// same lock, so a pin is checked again when cleanup actually takes place.
    /// Keep failed removals indexed and leave in-progress staging untouched.
    pub fn clear_unpinned(&self) -> Result<usize, String> {
        let mut index = self.index.lock().unwrap();
        let mut removed = 0;
        let mut position = 0;
        while position < index.len() {
            if index[position].pinned {
                position += 1;
                continue;
            }
            self.delete_at(&mut index, position).map_err(|error| {
                format!("Could not clear artifact {}: {error}", index[position].id)
            })?;
            removed += 1;
        }
        Ok(removed)
    }

    /// Deletes the oldest unpinned artifacts beyond `keep` or `max_bytes`.
    /// Pin checks and removals are atomic with respect to other index changes.
    /// Pinned bytes count toward quota; failed deletions do not free quota.
    pub fn retain(&self, keep: usize, max_bytes: u64) -> Vec<String> {
        let mut index = self.index.lock().unwrap();
        let mut total: u128 = index.iter().map(|a| a.bytes as u128).sum();
        let mut count = index.iter().filter(|a| !a.pinned).count();
        let mut removed = Vec::new();
        let mut position = index.len();
        while position > 0 {
            if count <= keep && (max_bytes == 0 || total <= max_bytes as u128) {
                break;
            }
            position -= 1;
            if index[position].pinned {
                continue;
            }
            if let Ok(artifact) = self.delete_at(&mut index, position) {
                count -= 1;
                total -= artifact.bytes as u128;
                removed.push(artifact.id);
            }
        }
        removed
    }

    /// JSON with absolute paths for clients.
    pub fn view(&self, artifact: &Artifact) -> serde_json::Value {
        let dir = self.dir(&artifact.id);
        let mut value = serde_json::to_value(artifact).unwrap_or_default();
        value["dir"] = dir.display().to_string().into();
        if let Some(files) = value["files"].as_array_mut() {
            for file in files {
                let path = dir.join(file["path"].as_str().unwrap_or_default());
                file["absolutePath"] = path.display().to_string().into();
            }
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> Artifact {
        Artifact {
            id: String::new(),
            job_id: "j".into(),
            client: "agent".into(),
            title: "Build".into(),
            workspace: "/w".into(),
            command: vec!["bazel".into()],
            created_at: 0,
            pinned: false,
            files: vec![],
            bytes: 0,
        }
    }

    #[test]
    fn cleanup_preserves_current_pins_and_in_progress_collection() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("artifacts"));
        let mut committed = Vec::new();
        for _ in 0..3 {
            let mut staging = store.begin().unwrap();
            staging
                .write("app.bin", 3, 0o644, &mut b"abc".as_slice())
                .unwrap();
            committed.push(store.commit(staging, meta()).unwrap());
        }
        // Pin after a caller has listed the artifacts: cleanup must use the
        // current state, not the earlier list used by the confirmation dialog.
        store.set_pinned(&committed[0].id, true).unwrap();
        let mut collecting = store.begin().unwrap();
        collecting
            .write("next.bin", 1, 0o644, &mut b"x".as_slice())
            .unwrap();

        assert_eq!(store.clear_unpinned().unwrap(), 2);
        assert_eq!(store.list().len(), 1);
        assert!(store.get(&committed[0].id).unwrap().pinned);
        assert_eq!(
            fs::read(store.dir(&committed[0].id).join("app.bin")).unwrap(),
            b"abc"
        );
        for artifact in &committed[1..] {
            assert!(store.get(&artifact.id).is_none());
            assert!(!store.dir(&artifact.id).exists());
        }
        // Do not reload the store before committing: startup intentionally
        // discards staging left over from a previous application process.
        collecting
            .write("last.bin", 1, 0o644, &mut b"y".as_slice())
            .unwrap();
        let next = store.commit(collecting, meta()).unwrap();
        assert_eq!(next.files.len(), 2);
        assert_eq!(store.clear_unpinned().unwrap(), 1);
        assert_eq!(store.clear_unpinned().unwrap(), 0);
        assert_eq!(Store::new(dir.path().join("artifacts")).list().len(), 1);
        // Explicit single-artifact deletion keeps its existing semantics.
        store.delete(&committed[0].id).unwrap();
        assert!(store.list().is_empty());
    }

    #[test]
    fn cleanup_removes_missing_records_and_still_preserves_pins() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("artifacts"));
        let pinned = store.commit(store.begin().unwrap(), meta()).unwrap();
        store.set_pinned(&pinned.id, true).unwrap();
        let mut staging = store.begin().unwrap();
        staging
            .write("app.bin", 3, 0o644, &mut b"abc".as_slice())
            .unwrap();
        let missing = store.commit(staging, meta()).unwrap();
        fs::remove_dir_all(store.root().join(&missing.id)).unwrap();
        assert_eq!(store.snapshot().1["bytes"], 3);
        assert_eq!(store.clear_unpinned().unwrap(), 1);
        assert!(store.get(&missing.id).is_none());
        assert!(store.get(&pinned.id).unwrap().pinned);
        assert_eq!(store.snapshot().1["bytes"], 0);
        assert_eq!(store.clear_unpinned().unwrap(), 0);

        // Single-artifact deletion is equally idempotent when its directory
        // was already removed externally, including a pinned record.
        fs::remove_dir_all(store.root().join(&pinned.id)).unwrap();
        store.delete(&pinned.id).unwrap();
        assert!(store.list().is_empty());
    }

    #[test]
    fn retention_releases_missing_bytes_before_considering_newer_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("artifacts"));
        let mut artifacts = Vec::new();
        for _ in 0..2 {
            let mut staging = store.begin().unwrap();
            staging
                .write("app.bin", 3, 0o644, &mut b"abc".as_slice())
                .unwrap();
            artifacts.push(store.commit(staging, meta()).unwrap());
        }
        fs::remove_dir_all(store.root().join(&artifacts[0].id)).unwrap();
        assert_eq!(store.retain(2, 3), vec![artifacts[0].id.clone()]);
        assert!(store.get(&artifacts[0].id).is_none());
        assert!(store.get(&artifacts[1].id).is_some());
        assert_eq!(store.snapshot().1["bytes"], 3);
        assert_eq!(
            fs::read(store.dir(&artifacts[1].id).join("app.bin")).unwrap(),
            b"abc"
        );
    }

    #[test]
    fn summary_revision_tracks_artifact_changes_and_store_identity() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("artifacts");
        let store = Store::new(root.clone());
        let empty = store.snapshot().1;
        let mut staging = store.begin().unwrap();
        staging
            .write("app.bin", 3, 0o644, &mut b"abc".as_slice())
            .unwrap();
        assert_eq!(store.snapshot().1, empty);
        let artifact = store.commit(staging, meta()).unwrap();
        let (list, committed) = store.snapshot();
        assert_eq!(list.len(), 1);
        assert_eq!(committed["count"], 1);
        assert_eq!(committed["bytes"], 3);
        assert_ne!(committed["revision"], empty["revision"]);
        store.set_pinned(&artifact.id, true).unwrap();
        let pinned = store.snapshot().1;
        assert_eq!(pinned["count"], committed["count"]);
        assert_eq!(pinned["bytes"], committed["bytes"]);
        assert_ne!(pinned["revision"], committed["revision"]);
        store.set_pinned(&artifact.id, true).unwrap();
        assert_eq!(store.snapshot().1, pinned);
        store.list();
        assert_eq!(store.snapshot().1, pinned);
        store.set_pinned(&artifact.id, false).unwrap();
        let unpinned = store.snapshot().1;
        assert_ne!(unpinned["revision"], pinned["revision"]);

        let reloaded = Store::new(root.clone());
        let restarted = reloaded.snapshot().1;
        assert_eq!(restarted["count"], unpinned["count"]);
        assert_eq!(restarted["bytes"], unpinned["bytes"]);
        assert_ne!(restarted["revision"], unpinned["revision"]);

        let moved = dir.path().join("moved");
        fs::rename(&root, &moved).unwrap();
        store.relocate(moved.clone());
        let relocated = store.snapshot().1;
        assert_eq!(relocated["dir"], moved.display().to_string());
        assert_ne!(relocated["revision"], unpinned["revision"]);
        store.delete(&artifact.id).unwrap();
        let deleted = store.snapshot().1;
        assert_eq!(deleted["count"], 0);
        assert_eq!(deleted["bytes"], 0);
        assert_ne!(deleted["revision"], relocated["revision"]);
    }

    #[test]
    fn cleanup_keeps_a_failed_removal_indexed() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("artifacts"));
        let artifact = store.commit(store.begin().unwrap(), meta()).unwrap();
        let path = store.root().join(&artifact.id);
        fs::remove_dir_all(&path).unwrap();
        fs::write(&path, b"not a directory").unwrap();
        let before = store.snapshot().1;
        assert!(store.clear_unpinned().is_err());
        assert_eq!(store.snapshot().1, before);
        assert!(store.get(&artifact.id).is_some());
        assert_eq!(fs::read(path).unwrap(), b"not a directory");
    }

    #[test]
    fn failed_pin_does_not_change_memory_or_persisted_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("artifacts");
        let store = Store::new(root.clone());
        let artifact = store.commit(store.begin().unwrap(), meta()).unwrap();
        // Force the temporary metadata write to fail without removing the
        // committed metadata or relying on the test process's permissions.
        fs::create_dir(root.join(&artifact.id).join("artifact.json.tmp")).unwrap();
        let before = store.snapshot().1;
        assert!(store.set_pinned(&artifact.id, true).is_err());
        assert_eq!(store.snapshot().1, before);
        assert!(!store.get(&artifact.id).unwrap().pinned);
        assert!(!Store::new(root).get(&artifact.id).unwrap().pinned);
    }

    #[test]
    fn explicit_delete_keeps_a_failed_removal_indexed() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("artifacts"));
        let artifact = store.commit(store.begin().unwrap(), meta()).unwrap();
        let path = store.root().join(&artifact.id);
        fs::remove_dir_all(&path).unwrap();
        fs::write(&path, b"not a directory").unwrap();
        assert!(store.delete(&artifact.id).is_err());
        assert!(store.get(&artifact.id).is_some());
        assert_eq!(fs::read(path).unwrap(), b"not a directory");
    }

    #[test]
    fn retention_counts_pinned_bytes_and_only_reports_successful_deletions() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("artifacts"));
        let mut artifacts = Vec::new();
        for bytes in [b"old".as_slice(), b"next", b"pinned!!"] {
            let mut staging = store.begin().unwrap();
            staging
                .write("app.bin", bytes.len() as u64, 0o644, &mut &*bytes)
                .unwrap();
            artifacts.push(store.commit(staging, meta()).unwrap());
        }
        store.set_pinned(&artifacts[2].id, true).unwrap();
        let broken = store.root().join(&artifacts[0].id);
        fs::remove_dir_all(&broken).unwrap();
        fs::write(&broken, b"not a directory").unwrap();
        // Both unpinned artifacts are within keep, but the pin's eight bytes
        // leave only three bytes of quota. The oldest cannot be deleted, so
        // retention must continue to the next and only report that success.
        assert_eq!(store.retain(2, 11), vec![artifacts[1].id.clone()]);
        assert!(store.get(&artifacts[0].id).is_some());
        assert!(store.get(&artifacts[1].id).is_none());
        assert!(store.get(&artifacts[2].id).unwrap().pinned);
        assert_eq!(fs::read(broken).unwrap(), b"not a directory");
        assert!(store.retain(0, 1).is_empty());
        assert_eq!(store.list().len(), 2);
    }

    #[test]
    fn concurrent_retention_cannot_remove_a_successfully_pinned_artifact() {
        use std::sync::{Arc, Barrier};
        // Race each pin against a real deletion. Either retention wins and pin
        // returns not found, or pin succeeds and its artifact remains intact.
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(dir.path().join("artifacts")));
        for _ in 0..32 {
            let artifact = store.commit(store.begin().unwrap(), meta()).unwrap();
            let barrier = Arc::new(Barrier::new(2));
            let deleting = store.clone();
            let ready = barrier.clone();
            let retention = std::thread::spawn(move || {
                ready.wait();
                deleting.retain(0, 0)
            });
            barrier.wait();
            let pinned = store.set_pinned(&artifact.id, true);
            let removed = retention.join().unwrap();
            if pinned.is_ok() {
                assert!(!removed.contains(&artifact.id));
                assert!(store.get(&artifact.id).unwrap().pinned);
                assert!(store
                    .root()
                    .join(&artifact.id)
                    .join("artifact.json")
                    .is_file());
            } else {
                assert!(removed.contains(&artifact.id));
                assert!(store.get(&artifact.id).is_none());
            }
        }
    }

    #[test]
    fn stores_resolves_pins_and_prunes() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("a"));
        let mut ids = Vec::new();
        for index in 0..3u64 {
            let mut staging = store.begin().unwrap();
            staging
                .write("out/app.update.tar.zlib", 3, 0o644, &mut b"abc".as_slice())
                .unwrap();
            staging
                .write("out/app.map", 1, 0o644, &mut b"m".as_slice())
                .unwrap();
            assert!(staging
                .write("../x", 1, 0o644, &mut b"m".as_slice())
                .is_err());
            let artifact = store
                .commit(
                    staging,
                    Artifact {
                        created_at: index,
                        ..meta()
                    },
                )
                .unwrap();
            ids.push(artifact.id);
        }
        let first = store.get(&ids[0]).unwrap();
        assert_eq!(first.bytes, 4);
        assert_eq!(
            first.files[0].sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let path = store.file(&ids[0], None, ".update.tar.zlib").unwrap();
        assert_eq!(fs::read(path).unwrap(), b"abc");
        assert!(store.file(&ids[0], Some("nope"), "").is_err());
        store.set_pinned(&ids[0], true).unwrap();
        let removed = store.retain(1, 0);
        assert_eq!(removed, vec![ids[1].clone()]);
        assert_eq!(store.list().len(), 2);
        // Reloading reads the same index from disk.
        let reloaded = Store::new(dir.path().join("a"));
        assert!(reloaded.get(&ids[0]).unwrap().pinned);
        assert!(
            reloaded.view(&reloaded.get(&ids[2]).unwrap())["files"][0]["absolutePath"]
                .as_str()
                .unwrap()
                .ends_with("out/app.update.tar.zlib")
        );
    }
}
