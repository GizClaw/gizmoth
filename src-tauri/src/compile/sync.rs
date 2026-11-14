//! Incremental workspace synchronization. The guest reports the host state it
//! last applied; only changed paths travel over vsock, and paths that vanished
//! on the host are deleted in the guest copy.
use super::guest::protocol::{self, Entry, EntryKind, HostMessage, DATA_CHUNK};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::Path,
};

pub const MAX_ENTRIES: usize = 500_000;

/// Names never synchronized: Bazel's convenience symlinks at the root point to
/// host output directories, and Finder metadata is noise.
fn excluded(relative: &str, name: &str, excludes: &[String]) -> bool {
    if name == ".DS_Store" || (!relative.contains('/') && name.starts_with("bazel-")) {
        return true;
    }
    excludes.iter().any(|pattern| matches(pattern, relative))
}

/// `*`, `?` and `**` matching of a workspace-relative path.
pub fn matches(pattern: &str, path: &str) -> bool {
    fn go(pattern: &[&str], path: &[&str]) -> bool {
        match pattern.split_first() {
            None => path.is_empty(),
            Some((&"**", rest)) => (0..=path.len()).any(|skip| go(rest, &path[skip..])),
            Some((first, rest)) => path.split_first().is_some_and(|(name, tail)| {
                protocol::component_matches(first, name) && go(rest, tail)
            }),
        }
    }
    let pattern: Vec<&str> = pattern.trim_matches('/').split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    // A pattern without a slash matches the name at any depth, like .gitignore.
    if pattern.len() == 1 && pattern[0] != "**" {
        return path
            .last()
            .is_some_and(|name| protocol::component_matches(pattern[0], name));
    }
    go(&pattern, &path)
}

pub fn scan(root: &Path, excludes: &[String]) -> Result<BTreeMap<String, Entry>, String> {
    let mut entries = BTreeMap::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let listing = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for item in listing {
            let item = item.map_err(|e| e.to_string())?;
            let Some(name) = item.file_name().to_str().map(str::to_owned) else {
                return Err(format!(
                    "{}: file names must be UTF-8",
                    item.path().display()
                ));
            };
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if excluded(&relative, &name, excludes) {
                continue;
            }
            let meta = fs::symlink_metadata(item.path()).map_err(|e| e.to_string())?;
            let mtime = meta.mtime() * 1_000_000_000 + meta.mtime_nsec();
            let entry = if meta.file_type().is_symlink() {
                let target = fs::read_link(item.path()).map_err(|e| e.to_string())?;
                Entry {
                    path: relative.clone(),
                    kind: EntryKind::Link,
                    size: 0,
                    mtime,
                    mode: 0o777,
                    target: Some(target.to_string_lossy().into_owned()),
                }
            } else if meta.is_dir() {
                stack.push((item.path(), relative.clone()));
                Entry {
                    path: relative.clone(),
                    kind: EntryKind::Dir,
                    size: 0,
                    mtime: 0,
                    mode: meta.mode() & 0o7777,
                    target: None,
                }
            } else if meta.is_file() {
                Entry {
                    path: relative.clone(),
                    kind: EntryKind::File,
                    size: meta.len(),
                    mtime,
                    mode: meta.mode() & 0o7777,
                    target: None,
                }
            } else {
                continue; // sockets, FIFOs and devices are not workspace content
            };
            entries.insert(relative, entry);
            if entries.len() > MAX_ENTRIES {
                return Err(format!(
                    "the workspace has more than {MAX_ENTRIES} entries; add exclude patterns"
                ));
            }
        }
    }
    Ok(entries)
}

/// Paths to delete (children of deleted directories omitted) and entries to put,
/// parents before children.
pub fn diff(
    host: &BTreeMap<String, Entry>,
    guest: &BTreeMap<String, Entry>,
) -> (Vec<String>, Vec<Entry>) {
    let mut deletes: Vec<String> = Vec::new();
    for (path, entry) in guest {
        let gone = match host.get(path) {
            None => true,
            Some(current) => current.kind != entry.kind,
        };
        if gone
            && !deletes
                .iter()
                .any(|parent| path.starts_with(&format!("{parent}/")))
        {
            deletes.push(path.clone());
        }
    }
    let puts = host
        .values()
        .filter(|entry| {
            guest
                .get(&entry.path)
                .is_none_or(|old| !old.same_content(entry))
        })
        .cloned()
        .collect();
    (deletes, puts)
}

/// Streams deletions and changed entries, then `SyncDone`.
pub fn send<W: Write>(
    stream: &mut W,
    root: &Path,
    deletes: &[String],
    puts: &[Entry],
    cancel: &dyn Fn() -> bool,
) -> Result<(), String> {
    for chunk in deletes.chunks(2000) {
        protocol::write_json(
            stream,
            &HostMessage::Delete {
                paths: chunk.to_vec(),
            },
        )
        .map_err(|e| e.to_string())?;
    }
    let mut buffer = vec![0u8; DATA_CHUNK];
    for entry in puts {
        if cancel() {
            return Err("cancelled".into());
        }
        protocol::write_json(
            stream,
            &HostMessage::Put {
                entry: entry.clone(),
            },
        )
        .map_err(|e| e.to_string())?;
        if entry.kind != EntryKind::File {
            continue;
        }
        let path = root.join(&entry.path);
        let mut file = File::open(&path).map_err(|e| format!("{}: {e}", entry.path))?;
        let mut left = entry.size;
        while left > 0 {
            let want = left.min(buffer.len() as u64) as usize;
            file.read_exact(&mut buffer[..want]).map_err(|_| {
                format!(
                    "{} changed during synchronization; run the build again",
                    entry.path
                )
            })?;
            protocol::write_frame(stream, protocol::DATA, &buffer[..want])
                .map_err(|e| e.to_string())?;
            left -= want as u64;
        }
    }
    protocol::write_json(stream, &HostMessage::SyncDone).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, size: u64, mtime: i64) -> Entry {
        Entry {
            path: path.into(),
            kind: EntryKind::File,
            size,
            mtime,
            mode: 0o644,
            target: None,
        }
    }
    fn dir(path: &str) -> Entry {
        Entry {
            path: path.into(),
            kind: EntryKind::Dir,
            size: 0,
            mtime: 0,
            mode: 0o755,
            target: None,
        }
    }

    #[test]
    fn diff_sends_changes_and_prunes_deleted_trees() {
        let guest: BTreeMap<_, _> = [
            dir("a"),
            file("a/x", 1, 1),
            dir("old"),
            file("old/y", 1, 1),
            file("keep", 3, 3),
            file("swap", 1, 1),
        ]
        .into_iter()
        .map(|e| (e.path.clone(), e))
        .collect();
        let host: BTreeMap<_, _> = [
            dir("a"),
            file("a/x", 2, 5),
            file("keep", 3, 3),
            dir("swap"),
            file("new", 1, 1),
        ]
        .into_iter()
        .map(|e| (e.path.clone(), e))
        .collect();
        let (deletes, puts) = diff(&host, &guest);
        assert_eq!(deletes, ["old", "swap"]);
        let paths: Vec<_> = puts.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["a/x", "new", "swap"]);
    }

    #[test]
    fn scan_skips_bazel_links_and_exclusions() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("src/sub")).unwrap();
        fs::write(root.path().join("src/sub/main.c"), "int main(){}").unwrap();
        fs::write(root.path().join("src/sub/skip.o"), "x").unwrap();
        fs::write(root.path().join(".DS_Store"), "x").unwrap();
        std::os::unix::fs::symlink("/tmp", root.path().join("bazel-out")).unwrap();
        std::os::unix::fs::symlink("main.c", root.path().join("src/sub/link")).unwrap();
        let entries = scan(root.path(), &["*.o".into()]).unwrap();
        let paths: Vec<_> = entries.keys().map(String::as_str).collect();
        assert_eq!(paths, ["src", "src/sub", "src/sub/link", "src/sub/main.c"]);
        assert_eq!(entries["src/sub/link"].target.as_deref(), Some("main.c"));
        let mut out = Vec::new();
        let (deletes, puts) = diff(&entries, &BTreeMap::new());
        send(&mut out, root.path(), &deletes, &puts, &|| false).unwrap();
        assert!(!out.is_empty());
        assert!(matches("out/**/*.bin", "out/a/b/fw.bin"));
        assert!(matches("out/**/*.bin", "out/fw.bin"));
        assert!(!matches("out/*.bin", "out/a/fw.bin"));
    }
}
