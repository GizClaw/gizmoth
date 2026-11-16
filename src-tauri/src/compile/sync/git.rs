//! Flatten a linked worktree's Git metadata into the private guest copy.
//! Objects are read from the host; mutable metadata is snapshotted in memory.
//! No host Git files are changed or exposed writable.
use super::{excluded, Entry, EntryKind};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs, io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

pub(super) enum Source {
    File(PathBuf),
    Bytes(Vec<u8>),
}

impl Source {
    pub(super) fn open(&self) -> io::Result<Box<dyn io::Read + '_>> {
        match self {
            Self::File(path) => Ok(Box::new(fs::File::open(path)?)),
            Self::Bytes(bytes) => Ok(Box::new(io::Cursor::new(bytes))),
        }
    }
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|e| format!("Git metadata {}: {e}", path.display()))
}

fn pointer(base: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "Git metadata path must be UTF-8")?;
    let text = text.trim_end_matches(['\n', '\r']);
    if text.is_empty() || text.contains(['\n', '\r', '\0']) {
        return Err("Invalid Git metadata directory".into());
    }
    fs::canonicalize(base.join(text)).map_err(|e| format!("Git metadata directory: {e}"))
}

fn bytes_entry(
    path: String,
    bytes: Vec<u8>,
    entries: &mut BTreeMap<String, Entry>,
    sources: &mut BTreeMap<String, Source>,
) {
    // Manifest comparisons must notice same-size edits even with preserved host
    // timestamps. Keep the synthetic nanosecond timestamp in a valid range.
    let hash: [u8; 32] = Sha256::digest(&bytes).into();
    let mtime = (u64::from_le_bytes(hash[..8].try_into().unwrap()) & 0x3fff_ffff_ffff_ffff) as i64;
    entries.insert(
        path.clone(),
        Entry {
            path: path.clone(),
            kind: EntryKind::File,
            size: bytes.len() as u64,
            mtime,
            mode: 0o600,
            target: None,
        },
    );
    sources.insert(path, Source::Bytes(bytes));
}

fn scan_metadata(base: &Path, excludes: &[String]) -> Result<BTreeMap<String, Entry>, String> {
    let mut entries = BTreeMap::new();
    let mut stack = vec![(base.to_path_buf(), String::new())];
    while let Some((directory, prefix)) = stack.pop() {
        for item in fs::read_dir(&directory).map_err(|e| format!("{}: {e}", directory.display()))? {
            let item = item.map_err(|e| e.to_string())?;
            let name = item
                .file_name()
                .into_string()
                .map_err(|_| format!("{}: file names must be UTF-8", item.path().display()))?;
            if prefix.is_empty()
                && matches!(
                    name.as_str(),
                    "worktrees" | "hooks" | "commondir" | "gitdir" | "locked"
                )
            {
                continue;
            }
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let path = format!(".git/{relative}");
            // Match workspace paths before descending, so excluding a directory
            // also excludes its contents without inspecting them.
            if excluded(&path, &name, excludes) {
                continue;
            }
            let meta = fs::symlink_metadata(item.path()).map_err(|e| e.to_string())?;
            let kind = if meta.file_type().is_symlink() {
                EntryKind::Link
            } else if meta.is_dir() {
                stack.push((item.path(), relative.clone()));
                EntryKind::Dir
            } else if meta.is_file() {
                EntryKind::File
            } else {
                continue;
            };
            let is_file = kind == EntryKind::File;
            entries.insert(
                relative.clone(),
                Entry {
                    path: relative,
                    kind,
                    size: if is_file { meta.len() } else { 0 },
                    mtime: if is_file {
                        meta.mtime() * 1_000_000_000 + meta.mtime_nsec()
                    } else {
                        0
                    },
                    mode: meta.mode() & 0o7777,
                    target: None,
                },
            );
            if entries.len() > super::MAX_ENTRIES {
                return Err(format!(
                    "the workspace has more than {} entries; add exclude patterns",
                    super::MAX_ENTRIES
                ));
            }
        }
    }
    Ok(entries)
}

pub(super) fn prepare(
    root: &Path,
    guest_root: &str,
    excludes: &[String],
    entries: &mut BTreeMap<String, Entry>,
) -> Result<BTreeMap<String, Source>, String> {
    let mut sources = BTreeMap::new();
    // An ordinary repository already has a self-contained .git directory.
    if !entries
        .get(".git")
        .is_some_and(|e| e.kind == EntryKind::File)
    {
        return Ok(sources);
    }
    let gitfile = read(&root.join(".git"))?;
    let target = gitfile
        .strip_prefix(b"gitdir: ")
        .ok_or("Invalid workspace .git file")?;
    let directory = pointer(root, target)?;
    let common = match fs::read(directory.join("commondir")) {
        Ok(bytes) => pointer(&directory, &bytes)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => directory.clone(),
        Err(e) => return Err(format!("Git common directory: {e}")),
    };
    if !directory.join("HEAD").is_file() || !common.join("objects").is_dir() {
        return Err("The workspace Git metadata is incomplete".into());
    }
    if common.join("reftable").exists() || directory.join("reftable").exists() {
        return Err("Workspace Git reftable storage is not supported for synchronization".into());
    }
    if common.join("objects/info/alternates").exists() {
        return Err(
            "Workspace Git object alternates must be made self-contained before building".into(),
        );
    }
    // Start with the common object database, refs and configuration, then
    // overlay this worktree's HEAD/index/sharedindex/config. Never copy the
    // other worktrees or pointers back into the host's directory layout.
    entries.remove(".git");
    entries.insert(
        ".git".into(),
        Entry {
            path: ".git".into(),
            kind: EntryKind::Dir,
            size: 0,
            mtime: 0,
            mode: 0o700,
            target: None,
        },
    );
    for base in [&common, &directory] {
        for (relative, mut entry) in scan_metadata(base, excludes)? {
            let first = relative.split('/').next().unwrap();
            if base == &common && common != directory {
                let shared = matches!(
                    first,
                    "objects"
                        | "refs"
                        | "logs"
                        | "config"
                        | "packed-refs"
                        | "shallow"
                        | "info"
                        | "rr-cache"
                );
                let private = [
                    "logs/HEAD",
                    "info/sparse-checkout",
                    "refs/bisect",
                    "refs/worktree",
                    "refs/rewritten",
                    "logs/refs/bisect",
                    "logs/refs/worktree",
                    "logs/refs/rewritten",
                ]
                .iter()
                .any(|prefix| relative == *prefix || relative.starts_with(&format!("{prefix}/")));
                if !shared || private {
                    continue;
                }
            }
            let path = format!(".git/{relative}");
            if entry.kind == EntryKind::Link {
                return Err(format!("Git metadata symlinks are not portable: {path}"));
            }
            let source = base.join(&relative);
            entry.path = path.clone();
            if entry.kind == EntryKind::File && !relative.starts_with("objects/") {
                bytes_entry(path, read(&source)?, entries, &mut sources);
            } else {
                entries.insert(path.clone(), entry);
                if source.is_file() {
                    sources.insert(path, Source::File(source));
                }
            }
        }
    }
    // Both config locations can select core.worktree. The final section wins
    // without interpreting or rewriting the user's original configuration.
    let escaped = guest_root.replace('\\', "\\\\").replace('"', "\\\"");
    for name in [".git/config", ".git/config.worktree"] {
        let Some(Source::Bytes(mut bytes)) = sources.remove(name) else {
            continue;
        };
        bytes.extend_from_slice(
            format!("\n[core]\n\tbare = false\n\tworktree = \"{escaped}\"\n").as_bytes(),
        );
        bytes_entry(name.into(), bytes, entries, &mut sources);
    }
    if entries.len() > super::MAX_ENTRIES {
        return Err(format!(
            "the workspace has more than {} entries; add exclude patterns",
            super::MAX_ENTRIES
        ));
    }
    Ok(sources)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::{
        guest::protocol::{self, HostMessage},
        sync::Workspace,
    };
    use std::{io::Write, process::Command};

    fn git(root: &Path, args: &[&str]) -> Vec<u8> {
        let output = Command::new("git")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
            ])
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn fixture(root: &Path) -> PathBuf {
        let repository = root.join("main");
        fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "--quiet"]);
        fs::write(repository.join("tracked.txt"), "base\n").unwrap();
        git(&repository, &["add", "tracked.txt"]);
        git(&repository, &["commit", "--quiet", "-m", "fixture"]);
        let linked = root.join("linked");
        git(
            &repository,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "build",
                linked.to_str().unwrap(),
            ],
        );
        linked
    }

    fn state(root: &Path) -> Vec<Vec<u8>> {
        [
            vec!["rev-parse", "HEAD"],
            vec!["diff", "--binary", "--no-ext-diff", "--no-textconv"],
            vec![
                "diff",
                "--cached",
                "--binary",
                "--no-ext-diff",
                "--no-textconv",
            ],
        ]
        .iter()
        .map(|args| git(root, args))
        .collect()
    }

    // Decode the actual existing synchronization wire format, rather than
    // copying the synthetic source map directly into the test destination.
    fn materialize(source: &Workspace, root: &Path, guest: &Path) {
        let mut wire = Vec::new();
        let puts = source.entries.values().cloned().collect::<Vec<_>>();
        source.send(&mut wire, root, &[], &puts, &|| false).unwrap();
        let mut reader = wire.as_slice();
        while let Some((kind, payload)) = protocol::read_frame(&mut reader).unwrap() {
            assert_eq!(kind, protocol::JSON);
            match protocol::parse::<HostMessage>(&payload).unwrap() {
                HostMessage::Put { entry } => {
                    let target = guest.join(entry.path);
                    match entry.kind {
                        EntryKind::Dir => fs::create_dir_all(target).unwrap(),
                        EntryKind::File => {
                            let mut file = fs::File::create(target).unwrap();
                            let mut left = entry.size;
                            while left > 0 {
                                let (kind, bytes) =
                                    protocol::read_frame(&mut reader).unwrap().unwrap();
                                assert_eq!(kind, protocol::DATA);
                                assert!(bytes.len() as u64 <= left);
                                file.write_all(&bytes).unwrap();
                                left -= bytes.len() as u64;
                            }
                        }
                        EntryKind::Link => {
                            std::os::unix::fs::symlink(entry.target.unwrap(), target).unwrap()
                        }
                    }
                }
                HostMessage::SyncDone => assert!(reader.is_empty()),
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn linked_worktree_keeps_head_staged_and_unstaged_state_without_host_paths() {
        let temporary = tempfile::tempdir().unwrap();
        let root = fixture(temporary.path());
        fs::write(root.join("tracked.txt"), "staged\n").unwrap();
        fs::write(root.join("new.txt"), "staged new blob\n").unwrap();
        git(&root, &["add", "tracked.txt", "new.txt"]);
        fs::write(root.join("tracked.txt"), "unstaged\n").unwrap();
        git(&root, &["update-index", "--split-index"]);
        git(&root, &["config", "extensions.worktreeConfig", "true"]);
        git(
            &root,
            &[
                "config",
                "--worktree",
                "core.worktree",
                root.to_str().unwrap(),
            ],
        );
        let before = state(&root);
        let gitfile = fs::read(root.join(".git")).unwrap();
        let directory = pointer(&root, gitfile.strip_prefix(b"gitdir: ").unwrap()).unwrap();
        let index = fs::read(directory.join("index")).unwrap();
        let guest = temporary.path().join("guest");
        fs::create_dir(&guest).unwrap();
        let source = Workspace::scan(&root, guest.to_str().unwrap(), &[]).unwrap();
        assert!(!source.entries.contains_key(".git/commondir"));
        assert!(!source.entries.contains_key(".git/gitdir"));
        assert!(!source
            .entries
            .keys()
            .any(|p| p.starts_with(".git/worktrees/")));
        materialize(&source, &root, &guest);
        // Remove the originals entirely from Git discovery. The new metadata
        // remains usable and includes staged blobs absent from HEAD.
        let main = temporary.path().join("main");
        fs::rename(main.join(".git"), main.join("hidden-git")).unwrap();
        assert_eq!(state(&guest), before);
        assert_eq!(
            git(&guest, &["rev-parse", "--show-toplevel"]),
            format!("{}\n", guest.canonicalize().unwrap().display()).as_bytes()
        );
        git(&guest, &["fsck", "--full"]);
        fs::rename(main.join("hidden-git"), main.join(".git")).unwrap();
        assert_eq!(fs::read(root.join(".git")).unwrap(), gitfile);
        assert_eq!(fs::read(directory.join("index")).unwrap(), index);
        assert_eq!(state(&root), before);
    }

    fn metadata_snapshot(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, u32, i64)> {
        let mut snapshot = BTreeMap::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(directory) = stack.pop() {
            for item in fs::read_dir(directory).unwrap() {
                let path = item.unwrap().path();
                let meta = fs::symlink_metadata(&path).unwrap();
                if meta.is_dir() {
                    stack.push(path);
                } else {
                    snapshot.insert(
                        path.strip_prefix(root).unwrap().to_path_buf(),
                        (
                            fs::read(&path).unwrap(),
                            meta.mode(),
                            meta.mtime() * 1_000_000_000 + meta.mtime_nsec(),
                        ),
                    );
                }
            }
        }
        snapshot
    }

    #[test]
    fn reserved_branch_names_survive_without_traversing_root_worktrees() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().unwrap();
        let root = fixture(temporary.path());
        git(&root, &["branch", "-m", "hooks"]);
        git(&root, &["branch", "feature/locked"]);
        let expected = state(&root);
        let common = temporary.path().join("main/.git");
        let ignored = common.join("worktrees/ignored");
        fs::create_dir(&ignored).unwrap();
        fs::write(ignored.join("marker"), "must not scan").unwrap();
        let before = metadata_snapshot(&common);
        let guest = temporary.path().join("guest");
        fs::create_dir(&guest).unwrap();
        let permissions = fs::metadata(&ignored).unwrap().permissions();
        fs::set_permissions(&ignored, fs::Permissions::from_mode(0o0)).unwrap();
        let source = Workspace::scan(&root, guest.to_str().unwrap(), &[]);
        fs::set_permissions(&ignored, permissions).unwrap();
        let source = source.unwrap();
        assert!(source.entries.contains_key(".git/refs/heads/hooks"));
        assert!(source
            .entries
            .contains_key(".git/refs/heads/feature/locked"));
        assert!(!source
            .entries
            .keys()
            .any(|p| p.starts_with(".git/worktrees")));
        materialize(&source, &root, &guest);
        assert_eq!(state(&guest), expected);
        assert_eq!(metadata_snapshot(&common), before);
    }

    #[test]
    fn git_directory_exclusions_prune_descendants_before_scanning() {
        use std::os::unix::fs::PermissionsExt;

        for name in ["objects", "refs"] {
            let temporary = tempfile::tempdir().unwrap();
            let root = fixture(temporary.path());
            let common = temporary.path().join("main/.git");
            let ignored = common.join(name).join("ignored");
            fs::create_dir(&ignored).unwrap();
            fs::write(ignored.join("marker"), "must not scan").unwrap();
            let before = metadata_snapshot(&common);
            let excluded = format!(".git/{name}");
            let permissions = fs::metadata(&ignored).unwrap().permissions();
            fs::set_permissions(&ignored, fs::Permissions::from_mode(0o0)).unwrap();
            let source =
                Workspace::scan(&root, "/work/src/fixture", std::slice::from_ref(&excluded));
            fs::set_permissions(&ignored, permissions).unwrap();
            let source = source.unwrap();
            let prefix = format!("{excluded}/");
            assert!(!source
                .entries
                .keys()
                .any(|p| p == &excluded || p.starts_with(&prefix)));
            assert!(!source
                .sources
                .keys()
                .any(|p| p == &excluded || p.starts_with(&prefix)));
            assert!(source.entries.contains_key(".git/HEAD"));
            assert_eq!(metadata_snapshot(&common), before);
        }
    }

    #[test]
    fn reftable_metadata_is_rejected_without_changing_host_files() {
        for private in [false, true] {
            let temporary = tempfile::tempdir().unwrap();
            let root = fixture(temporary.path());
            let common = temporary.path().join("main/.git");
            let gitfile = fs::read(root.join(".git")).unwrap();
            let directory = pointer(&root, gitfile.strip_prefix(b"gitdir: ").unwrap()).unwrap();
            let tables = if private { &directory } else { &common }.join("reftable");
            fs::create_dir(&tables).unwrap();
            fs::write(tables.join("tables.list"), "fixture.ref\n").unwrap();
            let before = metadata_snapshot(&common);
            let error = Workspace::scan(&root, "/work/src/fixture", &[])
                .err()
                .expect("reftable must be rejected");
            assert!(error.contains("reftable"), "{error}");
            assert_eq!(metadata_snapshot(&common), before);
            assert_eq!(fs::read(root.join(".git")).unwrap(), gitfile);
        }
    }

    #[test]
    fn same_size_same_mtime_head_changes_invalidate_the_manifest() {
        let temporary = tempfile::tempdir().unwrap();
        let root = fixture(temporary.path());
        git(&root, &["branch", "other"]);
        let first = Workspace::scan(&root, "/work/src/fixture", &[]).unwrap();
        let gitfile = fs::read(root.join(".git")).unwrap();
        let directory = pointer(&root, gitfile.strip_prefix(b"gitdir: ").unwrap()).unwrap();
        let head = directory.join("HEAD");
        let modified = fs::metadata(&head).unwrap().modified().unwrap();
        let size = fs::metadata(&head).unwrap().len();
        fs::write(&head, "ref: refs/heads/other\n").unwrap();
        fs::File::open(&head)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert_eq!(fs::metadata(&head).unwrap().len(), size);
        let second = Workspace::scan(&root, "/work/src/fixture", &[]).unwrap();
        let (_, puts) = super::super::diff(&second.entries, &first.entries);
        assert!(puts.iter().any(|e| e.path == ".git/HEAD"));
    }

    #[test]
    fn linked_worktree_does_not_inherit_the_main_index_or_private_config() {
        let temporary = tempfile::tempdir().unwrap();
        let root = fixture(temporary.path());
        git(&root, &["config", "extensions.worktreeConfig", "true"]);
        let main = temporary.path().join("main");
        git(
            &main,
            &["config", "--worktree", "fixture.owner", "main-only"],
        );
        let gitfile = fs::read(root.join(".git")).unwrap();
        let directory = pointer(&root, gitfile.strip_prefix(b"gitdir: ").unwrap()).unwrap();
        fs::remove_file(directory.join("index")).unwrap();
        let expected = state(&root);
        let guest = temporary.path().join("guest");
        fs::create_dir(&guest).unwrap();
        let source = Workspace::scan(&root, guest.to_str().unwrap(), &[]).unwrap();
        assert!(!source.entries.contains_key(".git/index"));
        assert!(!source.entries.contains_key(".git/config.worktree"));
        materialize(&source, &root, &guest);
        assert_eq!(state(&guest), expected);
    }

    #[test]
    fn explicit_git_exclusion_does_not_read_the_gitfile_target() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(
            temporary.path().join(".git"),
            "gitdir: /missing/private/repository\n",
        )
        .unwrap();
        let source =
            Workspace::scan(temporary.path(), "/work/src/fixture", &[".git".into()]).unwrap();
        assert!(source.sources.is_empty());
        assert!(!source.entries.contains_key(".git"));
    }
}
