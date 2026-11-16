//! Restore a pinned SDK from an offline source container. Payloads and Git
//! scratch files stay on the assembly disk; only bounded metadata is buffered.
use crate::archive;
use flate2::read::GzDecoder;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

const MANIFEST: &str = "gizmoth-git-source.json";
const POINTER: &str = "version https://git-lfs.github.com/spec/v1";
const LFS_FILTER: &str = "usr/libexec/gizmoth/git-lfs-filter";
const METADATA_LIMIT: u64 = 16 * 1024 * 1024;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::SeqCst) {
        Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
    } else {
        Ok(())
    }
}

/// Require one spelling for each path. In particular, reject `.git` anywhere
/// that could replace repository metadata, and do not normalize aliases away.
fn relative(value: &str, root: bool) -> io::Result<PathBuf> {
    if root && value.is_empty() {
        return Ok(PathBuf::new());
    }
    if value.is_empty()
        || value.contains(['\\', '\0', '\n', '\r'])
        || value
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == ".." || p.eq_ignore_ascii_case(".git"))
        || !Path::new(value)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(invalid(format!("unsafe Git source path {value:?}")));
    }
    Ok(PathBuf::from(value))
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    repositories: Vec<Repository>,
    lfs: Vec<Lfs>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Repository {
    path: String,
    commit: String,
    bundle: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Lfs {
    path: String,
    oid: String,
    size: u64,
    object: String,
}

impl Manifest {
    fn validate(&mut self) -> io::Result<()> {
        if self.version != 1 {
            return Err(invalid("unsupported Git source manifest version"));
        }
        let mut paths = BTreeSet::new();
        let mut bundles = BTreeSet::new();
        for repo in &self.repositories {
            relative(&repo.path, true)?;
            let bundle = relative(&repo.bundle, false)?;
            if !hex(&repo.commit, 40)
                || !bundle.starts_with("repos")
                || bundle.components().count() != 2
                || bundle.extension().is_none_or(|e| e != "bundle")
            {
                return Err(invalid("invalid Git commit or bundle path"));
            }
            if !paths.insert(repo.path.clone()) || !bundles.insert(repo.bundle.clone()) {
                return Err(invalid("duplicate repository or bundle"));
            }
        }
        if !paths.contains("") {
            return Err(invalid("Git source has no root repository"));
        }
        // Parents precede their children regardless of manifest order.
        self.repositories.sort_by_key(|r| {
            (
                !r.path.is_empty(),
                r.path.split('/').count(),
                r.path.clone(),
            )
        });
        let mut files = BTreeSet::new();
        let mut objects = BTreeMap::new();
        for item in &self.lfs {
            let path = relative(&item.path, false)?;
            if !hex(&item.oid, 64) || item.object != format!("objects/{}", item.oid) {
                return Err(invalid("invalid LFS object path or digest"));
            }
            if !files.insert(path) {
                return Err(invalid("duplicate LFS path"));
            }
            if objects
                .insert(&item.oid, item.size)
                .is_some_and(|size| size != item.size)
            {
                return Err(invalid("conflicting LFS object sizes"));
            }
        }
        for path in &files {
            if path.ancestors().skip(1).any(|p| files.contains(p))
                || self
                    .repositories
                    .iter()
                    .any(|r| !r.path.is_empty() && Path::new(&r.path).starts_with(path))
            {
                return Err(invalid("overlapping LFS paths or repository roots"));
            }
        }
        Ok(())
    }
}

/// Check every ancestor with lstat, never through a symlink. This is used for
/// source files, submodule destinations, and LFS replacement paths alike.
fn contained(root: &Path, path: &Path) -> io::Result<PathBuf> {
    if !fs::symlink_metadata(root)?.is_dir() {
        return Err(invalid("Git source root is not a directory"));
    }
    let mut result = root.to_path_buf();
    let count = path.components().count();
    for (index, component) in path.components().enumerate() {
        if !matches!(component, Component::Normal(_)) {
            return Err(invalid("unsafe relative path"));
        }
        result.push(component);
        let meta = fs::symlink_metadata(&result)?;
        if meta.file_type().is_symlink() || (index + 1 < count && !meta.is_dir()) {
            return Err(invalid(format!(
                "unsafe Git source ancestor {}",
                result.display()
            )));
        }
    }
    Ok(result)
}

fn regular(root: &Path, path: &Path) -> io::Result<PathBuf> {
    let path = contained(root, path)?;
    let meta = fs::symlink_metadata(&path)?;
    if !meta.is_file() || meta.nlink() != 1 {
        return Err(invalid("Git source file is not a private regular file"));
    }
    Ok(path)
}

fn digest_file(path: &Path, cancel: &AtomicBool) -> io::Result<(String, u64)> {
    digest_copy(&mut File::open(path)?, &mut io::sink(), cancel)
}

/// Hash exactly the bytes written to a private staging file. Source containers
/// are read across the host share only once; validation and extraction then
/// consume the same verified bytes from the assembly disk.
fn digest_copy(
    input: &mut impl Read,
    output: &mut impl Write,
    cancel: &AtomicBool,
) -> io::Result<(String, u64)> {
    let mut digest = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0; 128 * 1024];
    loop {
        check_cancel(cancel)?;
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        size = size
            .checked_add(n as u64)
            .ok_or_else(|| invalid("source size overflow"))?;
        output.write_all(&buffer[..n])?;
        digest.update(&buffer[..n]);
    }
    Ok((format!("{:x}", digest.finalize()), size))
}

/// The shared extractor deliberately supports links and replacement archive
/// entries. A source container needs neither: preflight its tar namespace so
/// aliases, duplicate entries and hardlinks cannot hide a manifest or object.
fn preflight(source: &Path, cancel: &AtomicBool) -> io::Result<BTreeSet<PathBuf>> {
    let mut archive = tar::Archive::new(GzDecoder::new(File::open(source)?));
    let mut entries = BTreeMap::new();
    for entry in archive.entries()? {
        check_cancel(cancel)?;
        let entry = entry?;
        let name = entry.path()?;
        let name = name
            .to_str()
            .ok_or_else(|| invalid("non-UTF8 source container path"))?;
        // Conventional tar directory entries may have a trailing slash.
        let name = if entry.header().entry_type().is_dir() {
            name.strip_suffix('/').unwrap_or(name)
        } else {
            name
        };
        let path = relative(name, false)?;
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            return Err(invalid(
                "links and special files are forbidden in Git source containers",
            ));
        }
        if entries.insert(path.clone(), kind.is_dir()).is_some() {
            return Err(invalid("duplicate source container entry"));
        }
        if path == Path::new(MANIFEST) && entry.size() > METADATA_LIMIT {
            return Err(invalid("Git source manifest is too large"));
        }
    }
    for path in entries.keys() {
        if path
            .ancestors()
            .skip(1)
            .any(|parent| entries.get(parent) == Some(&false))
        {
            return Err(invalid("source container file is an ancestor"));
        }
    }
    Ok(entries
        .into_iter()
        .filter_map(|(p, directory)| (!directory).then_some(p))
        .collect())
}

struct Scratch(PathBuf);
impl Scratch {
    fn new(parent: &Path) -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..100 {
            let path = parent.join(format!(
                ".gizmoth-git-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(invalid("cannot allocate private Git scratch directory"))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
use std::os::unix::fs::DirBuilderExt;

/// `cwd` and argv use chroot paths; `stdout` is an agent-side disk file. The
/// adapter must run with an empty inherited environment and stream stderr to
/// the session. Keeping this boundary small also permits host fixture tests.
pub struct GitCommand {
    pub cwd: PathBuf,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub stdout: Option<PathBuf>,
}

fn guest(root: &Path, path: &Path) -> io::Result<String> {
    Ok(format!(
        "/{}",
        path.strip_prefix(root)
            .map_err(|_| invalid("Git path outside chroot"))?
            .to_str()
            .ok_or_else(|| invalid("non-UTF8 Git path"))?
    ))
}

struct Git<'a, F> {
    root: &'a Path,
    scratch: &'a Path,
    cancel: &'a AtomicBool,
    path: &'a str,
    run: F,
}
impl<F: FnMut(GitCommand) -> Result<(), String>> Git<'_, F> {
    fn command(&mut self, cwd: &Path, args: &[&str], stdout: Option<&Path>) -> Result<(), String> {
        check_cancel(self.cancel).map_err(|e| e.to_string())?;
        let scratch = guest(self.root, self.scratch).map_err(|e| e.to_string())?;
        let mut argv: Vec<String> = [
            "git",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.attributesFile=/dev/null",
            "-c",
            "core.excludesFile=/dev/null",
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.symlinks=true",
            "-c",
            "core.protectHFS=true",
            "-c",
            "core.protectNTFS=true",
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.file.allow=always",
            "-c",
            "submodule.recurse=false",
            "-c",
            "fetch.recurseSubmodules=false",
            "-c",
            "fetch.fsckObjects=true",
            "-c",
            "transfer.fsckObjects=true",
            "-c",
            "gc.auto=0",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        argv.extend(args.iter().map(|s| s.to_string()));
        (self.run)(GitCommand {
            cwd: PathBuf::from(guest(self.root, cwd).map_err(|e| e.to_string())?),
            argv,
            env: vec![
                ("PATH".into(), self.path.into()),
                ("HOME".into(), scratch.clone()),
                ("XDG_CONFIG_HOME".into(), scratch.clone()),
                ("TMPDIR".into(), scratch),
                ("LANG".into(), "C".into()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
                ("GIT_ATTR_NOSYSTEM".into(), "1".into()),
                ("GIT_TERMINAL_PROMPT".into(), "0".into()),
                ("GIT_LFS_SKIP_SMUDGE".into(), "1".into()),
                ("GIT_ALLOW_PROTOCOL".into(), "file".into()),
                ("GIT_NO_REPLACE_OBJECTS".into(), "1".into()),
            ],
            stdout: stdout.map(Path::to_path_buf),
        })
    }
}

fn read_small(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("Git metadata is too large"));
    }
    Ok(bytes)
}

fn pointer(bytes: &[u8]) -> io::Result<Option<(String, u64)>> {
    if !bytes.starts_with(POINTER.as_bytes()) {
        return Ok(None);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("invalid LFS pointer"))?;
    let mut lines = text.lines();
    if lines.next() != Some(POINTER) {
        return Err(invalid("invalid LFS pointer version"));
    }
    let mut oid = None;
    let mut size = None;
    for line in lines {
        if let Some(value) = line.strip_prefix("oid sha256:") {
            if !hex(value, 64) || oid.replace(value.to_string()).is_some() {
                return Err(invalid("invalid LFS pointer digest"));
            }
        } else if let Some(value) = line.strip_prefix("size ") {
            if size
                .replace(
                    value
                        .parse::<u64>()
                        .map_err(|_| invalid("invalid LFS pointer size"))?,
                )
                .is_some()
            {
                return Err(invalid("duplicate LFS size"));
            }
        } else {
            // LFS extensions would require transforms unavailable in this offline installer.
            return Err(invalid("unsupported LFS pointer field"));
        }
    }
    Ok(Some((
        oid.ok_or_else(|| invalid("missing LFS pointer digest"))?,
        size.ok_or_else(|| invalid("missing LFS pointer size"))?,
    )))
}

/// Git's clean filter runs against read-only SDKs too. Hash stdin directly:
/// unlike git-lfs clean, this needs no writable .git/lfs/tmp or payload spool.
/// Existing pointer blobs pass through so index/worktree comparisons agree.
pub fn lfs_clean(mut input: impl Read, mut output: impl Write) -> io::Result<()> {
    let mut prefix = Vec::new();
    input.by_ref().take(1025).read_to_end(&mut prefix)?;
    if prefix.len() <= 1024 && pointer(&prefix)?.is_some() {
        return output.write_all(&prefix);
    }
    let mut digest = Sha256::new();
    digest.update(&prefix);
    let mut size = prefix.len() as u64;
    let mut buffer = [0; 128 * 1024];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
        size = size
            .checked_add(n as u64)
            .ok_or_else(|| invalid("LFS clean size overflow"))?;
    }
    writeln!(
        output,
        "{POINTER}\noid sha256:{:x}\nsize {size}",
        digest.finalize()
    )
}

fn install_lfs_filter(root: &Path) -> io::Result<PathBuf> {
    let relative = Path::new(LFS_FILTER);
    let mut directory = root.to_path_buf();
    for component in relative
        .parent()
        .ok_or_else(|| invalid("missing filter parent"))?
        .components()
    {
        directory.push(component);
        match fs::symlink_metadata(&directory) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(invalid("unsafe LFS filter directory")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir(&directory)?,
            Err(e) => return Err(e),
        }
    }
    let destination = root.join(relative);
    match fs::symlink_metadata(&destination) {
        Ok(_) => {
            let path = regular(root, relative)?;
            if fs::metadata(&path)?.permissions().mode() & 0o111 == 0 {
                return Err(invalid("LFS filter is not executable"));
            }
            return Ok(path);
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let temporary = Scratch::new(&directory)?;
    let binary = temporary.0.join("filter");
    fs::copy(std::env::current_exe()?, &binary)?;
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))?;
    fs::rename(binary, &destination)?;
    Ok(destination)
}

fn materialize(
    container: &Path,
    checkout: &Path,
    scratch: &Path,
    item: &Lfs,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let target = regular(checkout, &relative(&item.path, false)?)?;
    let mode = fs::metadata(&target)?.permissions().mode() & 0o777;
    let original = read_small(&target, 1024)?;
    if pointer(&original)? != Some((item.oid.clone(), item.size)) {
        return Err(invalid("LFS manifest does not match checked-out pointer"));
    }
    let object = regular(container, &relative(&item.object, false)?)?;
    if fs::metadata(&object)?.len() != item.size {
        return Err(invalid("LFS object size mismatch"));
    }
    let temporary = scratch.join("lfs-output");
    let mut input = File::open(object)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temporary)?;
    let mut digest = Sha256::new();
    let mut count = 0u64;
    let mut buffer = [0; 128 * 1024];
    loop {
        check_cancel(cancel)?;
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        count = count
            .checked_add(n as u64)
            .ok_or_else(|| invalid("LFS object size overflow"))?;
        if count > item.size {
            return Err(invalid("LFS object size mismatch"));
        }
        digest.update(&buffer[..n]);
        output.write_all(&buffer[..n])?;
    }
    if count != item.size || format!("{:x}", digest.finalize()) != item.oid {
        return Err(invalid("LFS object digest or size mismatch"));
    }
    output.set_permissions(fs::Permissions::from_mode(mode))?;
    drop(output);
    // Rename replaces a private regular file, never follows its symlink or
    // mutates a shared inode. Recheck immediately before publishing the object.
    regular(checkout, Path::new(&item.path))?;
    fs::rename(temporary, target)
}

/// Install atomically into an absent or empty destination. The caller supplies
/// the assembly root (with Git already installed), never the bootstrap root.
pub fn checkout(
    source: &Path,
    source_digest: &str,
    root: &Path,
    destination: &Path,
    cancel: &AtomicBool,
    path: &str,
    run: impl FnMut(GitCommand) -> Result<(), String>,
) -> Result<(), String> {
    checkout_inner(source, source_digest, root, destination, cancel, path, run)
        .map_err(|e| format!("offline Git checkout: {e}"))
}

fn checkout_inner(
    source: &Path,
    source_digest: &str,
    root: &Path,
    destination: &Path,
    cancel: &AtomicBool,
    path: &str,
    run: impl FnMut(GitCommand) -> Result<(), String>,
) -> Result<(), String> {
    let io = |e: io::Error| e.to_string();
    if !hex(source_digest, 64) {
        return Err("source container SHA256 mismatch".into());
    }
    let dest = destination
        .strip_prefix(root)
        .map_err(|_| "checkout destination outside assembly root")?;
    if dest.as_os_str().is_empty() {
        return Err("cannot checkout over assembly root".into());
    }
    if !dest.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err("unsafe checkout destination".into());
    }
    // Remove a conventional `to: "."` suffix before filesystem operations.
    let destination = root.join(dest.components().collect::<PathBuf>());
    let destination = destination.as_path();
    let dest = destination
        .strip_prefix(root)
        .map_err(|_| "checkout destination outside assembly root")?;
    let parent = dest.parent().ok_or("missing checkout destination parent")?;
    let mut current = root.to_path_buf();
    if !fs::symlink_metadata(root).map_err(io)?.is_dir() {
        return Err("assembly root is not a directory".into());
    }
    for part in parent.components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err("checkout destination ancestor is not a directory".into()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(io)?
            }
            Err(e) => return Err(io(e)),
        }
    }
    contained(root, parent).map_err(io)?;
    match fs::symlink_metadata(destination) {
        Ok(meta) if meta.is_dir() && fs::read_dir(destination).map_err(io)?.next().is_none() => {}
        Ok(_) => return Err("checkout destination is not an empty directory".into()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io(e)),
    }
    let scratch_parent = contained(root, Path::new("srv")).map_err(io)?;
    if !fs::symlink_metadata(&scratch_parent).map_err(io)?.is_dir() {
        return Err("scratch parent is not a directory".into());
    }
    let scratch = Scratch::new(&scratch_parent).map_err(io)?;
    let staged = scratch.0.join("source.tar.gz");
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&staged)
        .map_err(io)?;
    let (digest, _) =
        digest_copy(&mut File::open(source).map_err(io)?, &mut output, cancel).map_err(io)?;
    drop(output);
    if digest != source_digest {
        return Err("source container SHA256 mismatch".into());
    }
    let container = scratch.0.join("source");
    fs::create_dir(&container).map_err(io)?;
    let files = preflight(&staged, cancel).map_err(io)?;
    archive::extract(&staged, &container, "tar.gz", 0).map_err(io)?;
    check_cancel(cancel).map_err(io)?;
    let manifest_file = regular(&container, Path::new(MANIFEST)).map_err(io)?;
    let mut manifest: Manifest =
        serde_json::from_slice(&read_small(&manifest_file, METADATA_LIMIT).map_err(io)?)
            .map_err(|e| e.to_string())?;
    manifest.validate().map_err(io)?;
    let expected: BTreeSet<PathBuf> = std::iter::once(PathBuf::from(MANIFEST))
        .chain(
            manifest
                .repositories
                .iter()
                .map(|r| PathBuf::from(&r.bundle)),
        )
        .chain(manifest.lfs.iter().map(|l| PathBuf::from(&l.object)))
        .collect();
    if files != expected {
        return Err("source container has missing or unreferenced files".into());
    }
    for path in &expected {
        regular(&container, path).map_err(io)?;
    }
    let work = scratch.0.join("checkout");
    fs::create_dir(&work).map_err(io)?;
    let template = scratch.0.join("empty-template");
    fs::create_dir(&template).map_err(io)?;
    let metadata = scratch.0.join("metadata");
    let mut git = Git {
        root,
        scratch: &scratch.0,
        cancel,
        path,
        run,
    };
    let mut gitlinks = BTreeMap::<String, String>::new();
    let mut pointers = BTreeMap::<String, (String, u64)>::new();
    for repo in &manifest.repositories {
        let path = relative(&repo.path, true).map_err(io)?;
        let directory = contained(&work, &path).map_err(io)?;
        if !repo.path.is_empty() && gitlinks.remove(&repo.path) != Some(repo.commit.clone()) {
            return Err(format!(
                "repository {} does not match a parent gitlink",
                repo.path
            ));
        }
        if !fs::symlink_metadata(&directory).map_err(io)?.is_dir()
            || fs::read_dir(&directory).map_err(io)?.next().is_some()
        {
            return Err("repository checkout path is not an empty directory".into());
        }
        let bundle = guest(
            root,
            &regular(&container, Path::new(&repo.bundle)).map_err(io)?,
        )
        .map_err(io)?;
        let template = guest(root, &template).map_err(io)?;
        git.command(
            &directory,
            &["init", "--quiet", "--template", &template, "."],
            None,
        )?;
        git.command(&directory, &["bundle", "verify", &bundle], None)?;
        git.command(
            &directory,
            &["bundle", "list-heads", &bundle],
            Some(&metadata),
        )?;
        let heads = String::from_utf8(read_small(&metadata, METADATA_LIMIT).map_err(io)?)
            .map_err(|_| "invalid bundle heads")?;
        let mut has_head = false;
        for head in heads.lines() {
            let (oid, name) = head.split_once(' ').ok_or("invalid bundle head")?;
            if !hex(oid, 40)
                || (name != "HEAD" && !name.starts_with("refs/"))
                || name.starts_with("refs/replace/")
            {
                return Err("invalid or replacement bundle reference".into());
            }
            has_head |= name == "HEAD";
        }
        let mut fetch = vec![
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-recurse-submodules",
            &bundle,
            "+refs/*:refs/*",
        ];
        if has_head {
            fetch.push("HEAD");
        }
        git.command(&directory, &fetch, None)?;
        git.command(
            &directory,
            &["cat-file", "-e", &format!("{}^{{commit}}", repo.commit)],
            None,
        )?;
        git.command(
            &directory,
            &["fsck", "--full", "--strict", "--no-reflogs"],
            None,
        )?;
        git.command(
            &directory,
            &[
                "checkout",
                "--quiet",
                "--detach",
                "--force",
                &repo.commit,
                "--",
            ],
            None,
        )?;
        git.command(
            &directory,
            &["rev-parse", "--verify", "HEAD"],
            Some(&metadata),
        )?;
        if read_small(&metadata, 128).map_err(io)? != format!("{}\n", repo.commit).as_bytes() {
            return Err("checked-out Git commit mismatch".into());
        }
        git.command(&directory, &["ls-files", "--stage", "-z"], Some(&metadata))?;
        let mut entries = BufReader::new(File::open(&metadata).map_err(io)?);
        let mut entry = Vec::new();
        loop {
            check_cancel(cancel).map_err(io)?;
            entry.clear();
            // A single index record is bounded, while total SDK file count is not.
            let n = entries
                .by_ref()
                .take(64 * 1024)
                .read_until(0, &mut entry)
                .map_err(io)?;
            if n == 0 {
                break;
            }
            if entry.pop() != Some(0) {
                return Err("oversized or truncated Git index entry".into());
            }
            let separator = entry
                .iter()
                .position(|b| *b == b'\t')
                .ok_or("invalid Git index entry")?;
            let (header, name) = (&entry[..separator], &entry[separator + 1..]);
            let header = std::str::from_utf8(header).map_err(|_| "invalid Git index header")?;
            let fields: Vec<_> = header.split(' ').collect();
            if fields.len() != 3 || !hex(fields[1], 40) || fields[2] != "0" {
                return Err("invalid Git index stage".into());
            }
            let name = std::str::from_utf8(name).map_err(|_| "non-UTF8 Git path")?;
            let relative = relative(name, false).map_err(io)?;
            let full = path.join(&relative);
            let full_name = full.to_str().ok_or("invalid Git path")?.to_string();
            match fields[0] {
                "160000" => {
                    if gitlinks.insert(full_name, fields[1].to_string()).is_some() {
                        return Err("duplicate Git submodule".into());
                    }
                }
                "100644" | "100755" => {
                    let file = regular(&directory, &relative).map_err(io)?;
                    if fs::metadata(&file).map_err(io)?.len() <= 1024 {
                        if let Some(pointer) =
                            pointer(&read_small(&file, 1024).map_err(io)?).map_err(io)?
                        {
                            pointers.insert(full_name, pointer);
                        }
                    }
                }
                "120000" => {}
                _ => return Err("unsupported Git index mode".into()),
            }
        }
    }
    if !gitlinks.is_empty() {
        return Err(format!(
            "source container is missing submodules: {:?}",
            gitlinks.keys().collect::<Vec<_>>()
        ));
    }
    for item in &manifest.lfs {
        if pointers.remove(&item.path) != Some((item.oid.clone(), item.size)) {
            return Err(format!(
                "LFS manifest does not match tracked pointer {}",
                item.path
            ));
        }
        materialize(&container, &work, &scratch.0, item, cancel).map_err(io)?;
    }
    if !pointers.is_empty() {
        return Err(format!(
            "source container is missing LFS objects for {:?}",
            pointers.keys().collect::<Vec<_>>()
        ));
    }
    // Retain standard LFS objects without duplicating SDK bytes. The clean
    // filter is our streaming static executable at a stable rootfs path; it
    // needs no writable repository or network. Smudge only passes pointers
    // through. `git lfs checkout` can explicitly restore from the offline cache.
    if !manifest.lfs.is_empty() {
        let filter = guest(root, &install_lfs_filter(root).map_err(io)?).map_err(io)?;
        let quoted = format!("'{}'", filter.replace('\'', "'\\''"));
        let mut repositories = BTreeSet::new();
        for item in &manifest.lfs {
            let repo = manifest
                .repositories
                .iter()
                .filter(|r| r.path.is_empty() || Path::new(&item.path).starts_with(&r.path))
                .max_by_key(|r| r.path.len())
                .ok_or("LFS file has no repository")?;
            let directory = work.join(&repo.path);
            let object = directory
                .join(".git/lfs/objects")
                .join(&item.oid[..2])
                .join(&item.oid[2..4])
                .join(&item.oid);
            fs::create_dir_all(object.parent().ok_or("missing LFS cache parent")?).map_err(io)?;
            match fs::hard_link(work.join(&item.path), &object) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    let meta = fs::symlink_metadata(&object).map_err(io)?;
                    if !meta.is_file()
                        || digest_file(&object, cancel).map_err(io)?
                            != (item.oid.clone(), item.size)
                    {
                        return Err("invalid existing LFS cache object".into());
                    }
                }
                Err(e) => return Err(io(e)),
            }
            if repositories.insert(repo.path.clone()) {
                git.command(
                    &directory,
                    &[
                        "config",
                        "--local",
                        "filter.lfs.clean",
                        &format!("{quoted} --git-lfs-clean"),
                    ],
                    None,
                )?;
                git.command(
                    &directory,
                    &["config", "--local", "filter.lfs.smudge", "cat"],
                    None,
                )?;
                git.command(
                    &directory,
                    &["config", "--local", "filter.lfs.required", "true"],
                    None,
                )?;
            }
        }
    }
    // Init records local submodule configuration without fetching. Each child
    // retains its own .git directory and its parent retains the original gitlink.
    for repo in &manifest.repositories {
        git.command(&work.join(&repo.path), &["submodule", "init"], None)?;
    }
    for repo in manifest.repositories.iter().rev() {
        // Reindex the verified materialized bytes through the clean filter so
        // Git records their actual size/stat information. Reject any change to
        // the committed tree; refreshing stat data alone is insufficient when
        // a pointer's on-disk size changes during LFS materialization.
        git.command(
            &work.join(&repo.path),
            &["add", "--renormalize", "--", "."],
            None,
        )?;
        git.command(
            &work.join(&repo.path),
            &["diff", "--cached", "--quiet", "--exit-code", "HEAD", "--"],
            None,
        )?;
        git.command(
            &work.join(&repo.path),
            &[
                "diff",
                "--quiet",
                "--exit-code",
                "--ignore-submodules=none",
                "HEAD",
                "--",
            ],
            None,
        )?;
    }
    check_cancel(cancel).map_err(io)?;
    contained(root, parent).map_err(io)?;
    match fs::remove_dir(destination) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io(e)),
    }
    fs::rename(&work, destination).map_err(io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};
    use serde_json::{json, Value};
    use std::process::{Command, Stdio};

    const PATH: &str = "/usr/local/bin:/usr/bin:/bin";
    const DATA: &[u8] = b"complete SDK LFS executable contents";

    fn temporary() -> Scratch {
        Scratch::new(&std::env::temp_dir()).unwrap()
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_LFS_SKIP_SMUDGE", "1")
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn init(dir: &Path) {
        fs::create_dir(dir).unwrap();
        git(
            dir,
            &["init", "--quiet", "--template=", "--initial-branch=main"],
        );
    }

    fn commit(dir: &Path) -> String {
        git(dir, &["add", "."]);
        git(dir, &["commit", "--quiet", "-m", "fixture"]);
        git(dir, &["tag", "fixture-tag"]);
        git(dir, &["rev-parse", "HEAD"])
    }

    struct Fixture {
        _temporary: Scratch,
        tree: PathBuf,
        source: PathBuf,
        bundles: Vec<PathBuf>,
        manifest: Value,
    }

    impl Fixture {
        fn new(lfs: bool) -> Self {
            let temporary = temporary();
            let tree = temporary.0.join("rootfs");
            fs::create_dir(&tree).unwrap();
            fs::create_dir(tree.join("srv")).unwrap();
            let leaf = temporary.0.join("leaf");
            init(&leaf);
            fs::write(leaf.join("file.txt"), "nested leaf").unwrap();
            let leaf_commit = commit(&leaf);
            let child = temporary.0.join("child");
            init(&child);
            git(
                &child,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "add",
                    "--quiet",
                    leaf.to_str().unwrap(),
                    "deps/leaf",
                ],
            );
            let oid = format!("{:x}", Sha256::digest(DATA));
            let pointer = format!("{POINTER}\noid sha256:{oid}\nsize {}\n", DATA.len());
            if lfs {
                fs::write(
                    child.join(".gitattributes"),
                    "*.blob filter=lfs diff=lfs merge=lfs -text\n",
                )
                .unwrap();
                fs::write(child.join("data.blob"), &pointer).unwrap();
            }
            let child_commit = commit(&child);
            let repo = temporary.0.join("repo");
            init(&repo);
            git(
                &repo,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "add",
                    "--quiet",
                    child.to_str().unwrap(),
                    "modules/child",
                ],
            );
            fs::write(repo.join("README"), "pinned SDK\n").unwrap();
            if lfs {
                fs::write(
                    repo.join(".gitattributes"),
                    "*.blob filter=lfs diff=lfs merge=lfs -text\n",
                )
                .unwrap();
                fs::write(repo.join("sdk.blob"), &pointer).unwrap();
                fs::set_permissions(repo.join("sdk.blob"), fs::Permissions::from_mode(0o755))
                    .unwrap();
            }
            let root_commit = commit(&repo);
            let mut bundles = Vec::new();
            for (index, (directory, revision)) in [
                (&repo, &root_commit),
                (&child, &child_commit),
                (&leaf, &leaf_commit),
            ]
            .into_iter()
            .enumerate()
            {
                git(directory, &["update-ref", "refs/gizmoth/pinned", revision]);
                let bundle = temporary.0.join(format!("{index}.bundle"));
                git(
                    directory,
                    &[
                        "bundle",
                        "create",
                        bundle.to_str().unwrap(),
                        "refs/gizmoth/pinned",
                        "--tags",
                    ],
                );
                bundles.push(bundle);
            }
            let manifest = json!({"version": 1, "repositories": [
                // Deliberately list deepest-first to exercise ordering.
                {"path":"modules/child/deps/leaf", "commit":leaf_commit, "bundle":"repos/2.bundle"},
                {"path":"modules/child", "commit":child_commit, "bundle":"repos/1.bundle"},
                {"path":"", "commit":root_commit, "bundle":"repos/0.bundle"}
            ], "lfs": if lfs { vec![
                json!({"path":"sdk.blob", "oid":oid, "size":DATA.len(), "object":format!("objects/{oid}")}),
                json!({"path":"modules/child/data.blob", "oid":oid, "size":DATA.len(), "object":format!("objects/{oid}")})
            ] } else { vec![] }});
            // Host fixture adapter uses a tiny shell helper. The production
            // streaming implementation is exercised directly in separate tests.
            let helper = tree.join(LFS_FILTER);
            fs::create_dir_all(helper.parent().unwrap()).unwrap();
            fs::write(&helper, format!("#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{POINTER}' 'oid sha256:{oid}' 'size {}'\n", DATA.len())).unwrap();
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
            let source = temporary.0.join("source.tar.gz");
            Self {
                _temporary: temporary,
                tree,
                source,
                bundles,
                manifest,
            }
        }

        fn archive(&self, missing_bundle: bool, corrupt_object: bool) -> String {
            let gzip = GzEncoder::new(File::create(&self.source).unwrap(), Compression::fast());
            let mut tar = tar::Builder::new(gzip);
            let manifest = serde_json::to_vec(&self.manifest).unwrap();
            append(&mut tar, MANIFEST, &manifest);
            for (index, bundle) in self.bundles.iter().enumerate() {
                let name = format!("repos/{index}.bundle");
                if (!missing_bundle || index != 1)
                    && self.manifest["repositories"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|r| r["bundle"] == name)
                {
                    tar.append_path_with_name(bundle, name).unwrap();
                }
            }
            if !self.manifest["lfs"].as_array().unwrap().is_empty() {
                let name = format!("objects/{:x}", Sha256::digest(DATA));
                let mut data = DATA.to_vec();
                if corrupt_object {
                    data[0] ^= 0xff;
                }
                append(&mut tar, &name, &data);
            }
            tar.into_inner().unwrap().finish().unwrap();
            digest_file(&self.source, &AtomicBool::new(false))
                .unwrap()
                .0
        }

        fn run(&self, invocation: GitCommand) -> Result<(), String> {
            let disk = |path: &Path| self.tree.join(path.strip_prefix("/").unwrap());
            let mut command = Command::new(&invocation.argv[0]);
            for arg in &invocation.argv[1..] {
                if arg.starts_with('/') {
                    command.arg(disk(Path::new(arg)));
                } else if arg.starts_with("'/usr/libexec/") {
                    command.arg(arg.replacen(
                        "'/usr/libexec/",
                        &format!("'{}/usr/libexec/", self.tree.display()),
                        1,
                    ));
                } else {
                    command.arg(arg);
                }
            }
            command.current_dir(disk(&invocation.cwd)).env_clear();
            for (key, value) in invocation.env {
                if key == "PATH" {
                    command.env(key, std::env::var("PATH").unwrap());
                } else if ["HOME", "XDG_CONFIG_HOME", "TMPDIR"].contains(&key.as_str()) {
                    command.env(key, disk(Path::new(&value)));
                } else {
                    command.env(key, value);
                }
            }
            if let Some(output) = invocation.stdout {
                command.stdout(File::create(output).map_err(|e| e.to_string())?);
            } else {
                command.stdout(Stdio::null());
            }
            let output = command.output().map_err(|e| e.to_string())?;
            if output.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "{} {:?}: {}",
                    invocation.cwd.display(),
                    invocation.argv,
                    String::from_utf8_lossy(&output.stderr)
                ))
            }
        }

        fn install(&self, digest: &str, cancel: &AtomicBool) -> Result<(), String> {
            checkout(
                &self.source,
                digest,
                &self.tree,
                &self.tree.join("sdk"),
                cancel,
                PATH,
                |command| self.run(command),
            )
        }

        fn assert_clean_scratch(&self) {
            assert_eq!(fs::read_dir(self.tree.join("srv")).unwrap().count(), 0);
        }
    }

    fn append(tar: &mut tar::Builder<impl Write>, path: &str, bytes: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        tar.append_data(&mut header, path, bytes).unwrap();
    }

    #[test]
    fn staging_hashes_exact_bytes_and_propagates_partial_write_failure() {
        let data: Vec<u8> = (0..(384 * 1024 + 17)).map(|n| (n % 251) as u8).collect();
        let mut output = Vec::new();
        let actual =
            digest_copy(&mut data.as_slice(), &mut output, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            actual,
            (format!("{:x}", Sha256::digest(&data)), data.len() as u64)
        );
        assert_eq!(output, data);
        struct FailsAfter(usize);
        impl Write for FailsAfter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.0 == 0 {
                    return Err(io::Error::other("staging disk full"));
                }
                let n = bytes.len().min(self.0);
                self.0 -= n;
                Ok(n)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let error = digest_copy(
            &mut data.as_slice(),
            &mut FailsAfter(19),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "staging disk full");
    }

    #[test]
    fn restores_exact_nested_repositories_tags_and_git_status() {
        let fixture = Fixture::new(false);
        let digest = fixture.archive(false, false);
        fixture.install(&digest, &AtomicBool::new(false)).unwrap();
        let sdk = fixture.tree.join("sdk");
        for entry in fixture.manifest["repositories"].as_array().unwrap() {
            let repo = sdk.join(entry["path"].as_str().unwrap());
            assert!(repo.join(".git/HEAD").is_file());
            assert_eq!(
                git(&repo, &["rev-parse", "HEAD"]),
                entry["commit"].as_str().unwrap()
            );
            assert_eq!(
                git(&repo, &["describe", "--tags", "--exact-match"]),
                "fixture-tag"
            );
            assert!(git(
                &repo,
                &["status", "--porcelain", "--ignore-submodules=none"]
            )
            .is_empty());
            git(
                &repo,
                &[
                    "diff",
                    "--quiet",
                    "--exit-code",
                    "--ignore-submodules=none",
                    "HEAD",
                    "--",
                ],
            );
        }
        assert!(git(&sdk, &["submodule", "status", "--recursive"])
            .lines()
            .all(|line| !line.starts_with(['-', '+', 'U'])));
        fixture.assert_clean_scratch();
    }

    #[test]
    fn materializes_lfs_preserves_modes_caches_objects_and_keeps_git_clean() {
        let fixture = Fixture::new(true);
        let digest = fixture.archive(false, false);
        fixture.install(&digest, &AtomicBool::new(false)).unwrap();
        let sdk = fixture.tree.join("sdk");
        for (name, executable) in [("sdk.blob", true), ("modules/child/data.blob", false)] {
            let file = sdk.join(name);
            assert_eq!(fs::read(&file).unwrap(), DATA);
            assert_eq!(
                fs::metadata(file).unwrap().permissions().mode() & 0o111 != 0,
                executable
            );
        }
        for repo in [&sdk, &sdk.join("modules/child")] {
            assert!(git(repo, &["status", "--porcelain", "--ignore-submodules=none"]).is_empty());
            git(
                repo,
                &[
                    "diff",
                    "--quiet",
                    "--exit-code",
                    "--ignore-submodules=none",
                    "HEAD",
                    "--",
                ],
            );
            let oid = format!("{:x}", Sha256::digest(DATA));
            let cached = repo
                .join(".git/lfs/objects")
                .join(&oid[..2])
                .join(&oid[2..4])
                .join(&oid);
            assert_eq!(fs::read(cached).unwrap(), DATA);
        }
        // Qualification binds /opt read-only. Confirm clean/status do not
        // need writable repository metadata or an LFS temporary directory.
        fn writable(path: &Path, enable: bool) {
            let meta = fs::symlink_metadata(path).unwrap();
            if meta.file_type().is_symlink() {
                return;
            }
            if enable {
                fs::set_permissions(
                    path,
                    fs::Permissions::from_mode(
                        meta.permissions().mode() | if meta.is_dir() { 0o700 } else { 0o200 },
                    ),
                )
                .unwrap();
            }
            if meta.is_dir() {
                for entry in fs::read_dir(path).unwrap() {
                    writable(&entry.unwrap().path(), enable);
                }
            }
            if !enable {
                fs::set_permissions(
                    path,
                    fs::Permissions::from_mode(meta.permissions().mode() & !0o222),
                )
                .unwrap();
            }
        }
        struct Restore(PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                writable(&self.0, true);
            }
        }
        let _restore = Restore(sdk.clone());
        writable(&sdk, false);
        for repo in [&sdk, &sdk.join("modules/child")] {
            assert!(git(repo, &["status", "--porcelain", "--ignore-submodules=none"]).is_empty());
            git(
                repo,
                &[
                    "diff",
                    "--quiet",
                    "--exit-code",
                    "--ignore-submodules=none",
                    "HEAD",
                    "--",
                ],
            );
            assert!(!repo.join(".git/lfs/tmp").exists());
        }
        fixture.assert_clean_scratch();
    }

    #[test]
    fn rejects_missing_bundles_submodules_lfs_and_wrong_commits() {
        for scenario in 0..8 {
            let mut fixture = Fixture::new(true);
            match scenario {
                1 => {
                    fixture.manifest["repositories"]
                        .as_array_mut()
                        .unwrap()
                        .remove(0);
                }
                2 => {
                    fixture.manifest["lfs"].as_array_mut().unwrap().remove(0);
                }
                3 => {
                    fixture.manifest["repositories"][1]["commit"] = json!("0".repeat(40));
                }
                4 => {
                    fixture.manifest["repositories"][2]["commit"] =
                        json!(fixture.manifest["repositories"][0]["commit"]);
                }
                6 => {
                    fixture.manifest["lfs"][0]["size"] = json!(DATA.len() + 1);
                }
                7 => {
                    fixture.manifest["repositories"]
                        .as_array_mut()
                        .unwrap()
                        .remove(2);
                }
                _ => {}
            }
            let digest = fixture.archive(scenario == 0, scenario == 5);
            assert!(
                fixture.install(&digest, &AtomicBool::new(false)).is_err(),
                "scenario {scenario}"
            );
            assert!(!fixture.tree.join("sdk").exists());
            fixture.assert_clean_scratch();
        }
    }

    #[test]
    fn rejects_manifest_aliases_duplicates_and_metadata_paths() {
        let fixture = Fixture::new(true);
        for path in [
            "../outside",
            "/outside",
            "a/../b",
            "a//b",
            "./a",
            "a/",
            "a\\b",
            "a/.git/config",
            "a/.GIT/config",
            "a\0b",
        ] {
            assert!(relative(path, false).is_err(), "{path:?}");
        }
        for edit in 0..6 {
            let mut value = fixture.manifest.clone();
            match edit {
                0 => value["repositories"][2]["path"] = json!("not-root"),
                1 => value["repositories"][0] = value["repositories"][1].clone(),
                2 => value["lfs"][0]["path"] = json!("modules"),
                3 => value["lfs"][1] = value["lfs"][0].clone(),
                4 => value["lfs"][0]["object"] = json!("objects/../repos/0.bundle"),
                5 => value["version"] = json!(2),
                _ => unreachable!(),
            }
            let mut manifest: Manifest = serde_json::from_value(value).unwrap();
            assert!(manifest.validate().is_err(), "edit {edit}");
        }
    }

    #[test]
    fn refuses_links_duplicates_and_overlapping_container_entries() {
        let temporary = temporary();
        for scenario in 0..4 {
            let source = temporary.0.join(format!("{scenario}.tar.gz"));
            let mut tar = tar::Builder::new(GzEncoder::new(
                File::create(&source).unwrap(),
                Compression::fast(),
            ));
            if scenario < 2 {
                let mut header = tar::Header::new_gnu();
                header.set_mode(0o777);
                header.set_size(0);
                header.set_entry_type(if scenario == 0 {
                    tar::EntryType::Symlink
                } else {
                    tar::EntryType::Link
                });
                header.set_link_name("objects/file").unwrap();
                header.set_cksum();
                tar.append_data(&mut header, MANIFEST, io::empty()).unwrap();
            } else {
                append(&mut tar, "objects/file", b"one");
                append(
                    &mut tar,
                    if scenario == 2 {
                        "objects/file"
                    } else {
                        "objects/file/child"
                    },
                    b"two",
                );
            }
            tar.into_inner().unwrap().finish().unwrap();
            assert!(preflight(&source, &AtomicBool::new(false)).is_err());
        }
    }

    #[test]
    fn rejects_symlink_ancestors_lfs_symlinks_and_digest_mismatch() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new(true);
        let digest = fixture.archive(false, false);
        assert!(fixture
            .install(&"0".repeat(64), &AtomicBool::new(false))
            .is_err());
        fs::create_dir(fixture.tree.join("outside")).unwrap();
        symlink("outside", fixture.tree.join("sdk")).unwrap();
        assert!(fixture.install(&digest, &AtomicBool::new(false)).is_err());
        assert_eq!(
            fs::read_dir(fixture.tree.join("outside")).unwrap().count(),
            0
        );
        assert!(regular(&fixture.tree, Path::new("sdk/no-file")).is_err());
        let object = fixture.tree.join("outside/object");
        fs::write(&object, DATA).unwrap();
        symlink("outside/object", fixture.tree.join("pointer")).unwrap();
        assert!(regular(&fixture.tree, Path::new("pointer")).is_err());
        fixture.assert_clean_scratch();
    }

    #[test]
    fn cleans_scratch_on_command_failure_and_cancellation() {
        let fixture = Fixture::new(false);
        let digest = fixture.archive(false, false);
        let cancel = AtomicBool::new(false);
        assert!(checkout(
            &fixture.source,
            &digest,
            &fixture.tree,
            &fixture.tree.join("sdk"),
            &cancel,
            PATH,
            |_| Err("injected Git failure".into())
        )
        .is_err());
        fixture.assert_clean_scratch();
        assert!(checkout(
            &fixture.source,
            &digest,
            &fixture.tree,
            &fixture.tree.join("sdk"),
            &cancel,
            PATH,
            |command| {
                let outcome = fixture.run(command);
                cancel.store(true, Ordering::SeqCst);
                outcome
            }
        )
        .is_err());
        fixture.assert_clean_scratch();
        assert!(!fixture.tree.join("sdk").exists());
    }

    #[test]
    fn accepts_empty_dot_destination_and_rejects_corrupt_bundle() {
        let fixture = Fixture::new(false);
        let digest = fixture.archive(false, false);
        fs::create_dir(fixture.tree.join("sdk")).unwrap();
        checkout(
            &fixture.source,
            &digest,
            &fixture.tree,
            &fixture.tree.join("sdk/."),
            &AtomicBool::new(false),
            PATH,
            |command| fixture.run(command),
        )
        .unwrap();
        fixture.assert_clean_scratch();
        let fixture = Fixture::new(false);
        let mut bundle = fs::read(&fixture.bundles[0]).unwrap();
        let last = bundle.len() - 1;
        bundle[last] ^= 0xff;
        fs::write(&fixture.bundles[0], bundle).unwrap();
        let digest = fixture.archive(false, false);
        assert!(fixture.install(&digest, &AtomicBool::new(false)).is_err());
        fixture.assert_clean_scratch();
        assert!(!fixture.tree.join("sdk").exists());
    }

    #[test]
    fn streaming_clean_matches_pointer_and_preserves_existing_pointer() {
        for bytes in [Vec::new(), DATA.to_vec(), vec![0xff; 1024 * 1024 + 1]] {
            let mut output = Vec::new();
            lfs_clean(bytes.as_slice(), &mut output).unwrap();
            assert_eq!(
                pointer(&output).unwrap(),
                Some((format!("{:x}", Sha256::digest(&bytes)), bytes.len() as u64))
            );
            let mut again = Vec::new();
            lfs_clean(output.as_slice(), &mut again).unwrap();
            assert_eq!(again, output);
        }
        assert!(pointer(format!("{POINTER}\noid sha256:{}\n", "a".repeat(64)).as_bytes()).is_err());
        assert!(pointer(
            format!(
                "{POINTER}\noid sha256:{}\nsize 1\next-0-test value\n",
                "a".repeat(64)
            )
            .as_bytes()
        )
        .is_err());
    }
}
