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
    sync::{Mutex, RwLock},
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
        }
    }

    pub fn root(&self) -> PathBuf {
        self.root.read().unwrap().clone()
    }

    /// Points the store at the moved artifacts directory; IDs stay the same.
    pub fn relocate(&self, root: PathBuf) {
        let _ = fs::create_dir(&root);
        *self.root.write().unwrap() = root;
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
        self.index.lock().unwrap().insert(0, artifact.clone());
        Ok(artifact)
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
        artifact.pinned = pinned;
        let bytes = serde_json::to_vec_pretty(&*artifact).map_err(|e| e.to_string())?;
        let path = self.root().join(id).join("artifact.json");
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, bytes)
            .and_then(|_| fs::rename(&temporary, &path))
            .map_err(|e| e.to_string())?;
        Ok(artifact.clone())
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        if !valid_id(id) {
            return Err("Invalid artifact ID".into());
        }
        let mut index = self.index.lock().unwrap();
        let before = index.len();
        index.retain(|a| a.id != id);
        if index.len() == before {
            return Err("Artifact not found".into());
        }
        drop(index);
        let dir = self.root().join(id);
        make_writable(&dir);
        fs::remove_dir_all(&dir).map_err(|e| e.to_string())
    }

    /// Deletes the oldest unpinned artifacts beyond `keep` or `max_bytes`.
    pub fn retain(&self, keep: usize, max_bytes: u64) -> Vec<String> {
        let candidates: Vec<String> = {
            let index = self.index.lock().unwrap();
            let mut total: u64 = index.iter().map(|a| a.bytes).sum();
            let mut count = index.iter().filter(|a| !a.pinned).count();
            let mut doomed = Vec::new();
            for artifact in index.iter().rev().filter(|a| !a.pinned) {
                if count <= keep && (max_bytes == 0 || total <= max_bytes) {
                    break;
                }
                doomed.push(artifact.id.clone());
                count -= 1;
                total = total.saturating_sub(artifact.bytes);
            }
            doomed
        };
        for id in &candidates {
            let _ = self.delete(id);
        }
        candidates
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
