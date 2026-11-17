//! Local, content-addressed Git source transport for the Linux packer.
//!
//! No source tree is checked out on the host. Complete bundles (including a
//! `refs/gizmoth/pinned` ref) and verified LFS objects are wrapped in a gzip tar.
//! The packer reads `gizmoth-git-source.json`, initializes each repository in
//! parent-first order, fetches its bundle, checks out the recorded commit with
//! filters disabled, and replaces LFS pointers with the recorded object bytes.
//! Neither this archive nor its memo belongs in the JSON-only registry.
use flate2::{write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};
use url::Url;

type R<T> = Result<T, String>;
const GIT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_TREE: usize = 64 * 1024 * 1024;
const MAX_CONFIG: usize = 1024 * 1024;
const MAX_BATCH: usize = 128 * 1024 * 1024;
const MAX_POINTER: u64 = 1024;
const MAX_REPOS: usize = 1024;
const MAX_DEPTH: usize = 32;
const PIN: &str = "refs/gizmoth/pinned";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Acquired {
    pub digest: String,
    pub bytes: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Memo {
    version: u32,
    url: String,
    commit: String,
    submodules: bool,
    digest: String,
    bytes: u64,
}

fn recursive() -> bool {
    true
}
fn is_recursive(value: &bool) -> bool {
    *value
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    #[serde(default = "recursive", skip_serializing_if = "is_recursive")]
    submodules: bool,
    repositories: Vec<Repository>,
    lfs: Vec<Lfs>,
}
#[derive(Debug, Serialize, Deserialize)]
struct Repository {
    path: String,
    commit: String,
    bundle: String,
}
#[derive(Debug, Serialize, Deserialize)]
struct Lfs {
    path: String,
    oid: String,
    size: u64,
    object: String,
}

#[derive(Clone, Copy, Default)]
struct Transport {
    // Production cannot enable the local fixture transport.
    #[cfg(test)]
    local: bool,
}
impl Transport {
    fn local(self) -> bool {
        #[cfg(test)]
        {
            self.local
        }
        #[cfg(not(test))]
        {
            false
        }
    }
    fn remote(self, value: &str) -> R<Url> {
        if value.chars().any(char::is_control) {
            return Err("invalid source URL".into());
        }
        let url = Url::parse(value).map_err(|_| "invalid source URL")?;
        let local = self.local() && url.scheme() == "file" && url.host_str().is_none();
        if (!local && (url.scheme() != "https" || url.host_str().is_none()))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(
                "source URLs must be public HTTPS without credentials, query or fragment".into(),
            );
        }
        Ok(url)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn valid_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn check(cancel: &dyn Fn() -> bool) -> R<()> {
    if cancel() {
        Err("cancelled".into())
    } else {
        Ok(())
    }
}
fn io_error(e: io::Error) -> String {
    format!("Git source cache: {e}")
}

/// Own only a newly created, empty directory. Failed/cancelled work stays here.
struct Staging(PathBuf);
impl Staging {
    fn new(parent: &Path) -> R<Self> {
        for _ in 0..10 {
            let path = parent.join(format!(".git-source-{}", uuid::Uuid::new_v4()));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io_error(e)),
            }
        }
        Err("could not allocate Git source staging directory".into())
    }
}
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn command(directory: &Path, transport: Transport) -> Command {
    let mut command = Command::new("git");
    // In particular, discard GIT_CONFIG_COUNT/PARAMETERS, object alternates,
    // GIT_EXEC_PATH, SSH/askpass commands, proxy credentials and trace settings.
    command.env_clear();
    for name in ["PATH", "SYSTEMROOT", "WINDIR", "TEMP", "TMP", "TMPDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    command
        .current_dir(directory)
        .env("HOME", directory)
        .env("XDG_CONFIG_HOME", directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", null)
        .env("GIT_CONFIG_GLOBAL", null)
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("LC_ALL", "C")
        .args([
            "--no-pager",
            "-c",
            "core.hooksPath=",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.attributesFile=",
            "-c",
            "credential.helper=",
            "-c",
            "credential.interactive=false",
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.https.allow=always",
            "-c",
            "http.followRedirects=false",
            "-c",
            "http.sslVerify=true",
            "-c",
            "fetch.recurseSubmodules=false",
            "-c",
            "transfer.fsckObjects=true",
            "-c",
            "init.templateDir=",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
            "-c",
            "core.logAllRefUpdates=false",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if transport.local() {
        command.args(["-c", "protocol.file.allow=always"]);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

#[cfg(unix)]
struct ProcessTree(Option<i32>);
#[cfg(unix)]
impl ProcessTree {
    fn attach(child: &std::process::Child) -> R<Self> {
        Ok(Self(Some(child.id() as i32)))
    }
    fn stop(&mut self) {
        if let Some(group) = self.0.take() {
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
    }
}
#[cfg(windows)]
struct ProcessTree(std::os::windows::io::OwnedHandle);
#[cfg(windows)]
impl ProcessTree {
    fn attach(child: &std::process::Child) -> R<Self> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows::Win32::{Foundation::HANDLE, System::JobObjects::*};
        let create = || -> windows::core::Result<Self> {
            let handle = unsafe { CreateJobObjectW(None, None)? };
            let owned = unsafe { OwnedHandle::from_raw_handle(handle.0) };
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            unsafe {
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as *const _,
                    std::mem::size_of_val(&limits) as u32,
                )?;
                AssignProcessToJobObject(handle, HANDLE(child.as_raw_handle()))?;
            }
            Ok(Self(owned))
        };
        create().map_err(|_| "cannot manage Git process group".into())
    }
    fn stop(&self) {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{Foundation::HANDLE, System::JobObjects::TerminateJobObject};
        unsafe {
            let _ = TerminateJobObject(HANDLE(self.0.as_raw_handle()), 1);
        }
    }
}
impl Drop for ProcessTree {
    fn drop(&mut self) {
        self.stop();
    }
}

// Always drain both pipes. An exceeded bound signals the supervisor immediately;
// it does not leave Git blocked writing the other pipe.
fn drain(
    mut pipe: impl Read,
    limit: usize,
    file: Option<PathBuf>,
    abort: Arc<AtomicBool>,
) -> R<Vec<u8>> {
    let mut output = match file {
        Some(path) => Some(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(io_error)?,
        ),
        None => None,
    };
    let mut bytes = Vec::new();
    let mut total = 0usize;
    let mut buffer = [0u8; 32 * 1024];
    loop {
        let n = pipe.read(&mut buffer).map_err(io_error)?;
        if n == 0 {
            break;
        }
        total = total.saturating_add(n);
        if total > limit {
            abort.store(true, Ordering::Relaxed);
            continue;
        }
        if let Some(file) = &mut output {
            file.write_all(&buffer[..n]).map_err(io_error)?;
        } else {
            bytes.extend_from_slice(&buffer[..n]);
        }
    }
    Ok(bytes)
}

fn run(
    command: &mut Command,
    limit: usize,
    file: Option<&Path>,
    timeout: Duration,
    cancel: &dyn Fn() -> bool,
) -> R<Vec<u8>> {
    check(cancel)?;
    let mut child = command
        .spawn()
        .map_err(|_| "cannot start Git; install Git and retry")?;
    let mut tree = match ProcessTree::attach(&child) {
        Ok(tree) => tree,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };
    let abort = Arc::new(AtomicBool::new(false));
    let out_abort = abort.clone();
    let err_abort = abort.clone();
    let stdout = child.stdout.take().ok_or("missing Git stdout")?;
    let stderr = child.stderr.take().ok_or("missing Git stderr")?;
    let file = file.map(Path::to_path_buf);
    let out = thread::spawn(move || {
        let result = drain(stdout, limit, file, out_abort.clone());
        if result.is_err() {
            out_abort.store(true, Ordering::Relaxed);
        }
        result
    });
    let err = thread::spawn(move || {
        let result = drain(stderr, 256 * 1024, None, err_abort.clone());
        if result.is_err() {
            err_abort.store(true, Ordering::Relaxed);
        }
        result
    });
    let start = Instant::now();
    let status = loop {
        if cancel() {
            break Err("cancelled".to_owned());
        }
        if start.elapsed() > timeout {
            break Err("Git source command timed out".into());
        }
        if abort.load(Ordering::Relaxed) {
            break Err("Git source output exceeded its bound or could not be read".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => break Err("could not wait for Git".into()),
        }
    };
    // Also close pipes inherited by descendants after an unexpected parent exit.
    tree.stop();
    let _ = child.kill();
    let _ = child.wait();
    let stdout = out.join().map_err(|_| "Git stdout reader failed")?;
    let stderr = err.join().map_err(|_| "Git stderr reader failed")?;
    let status = status?;
    let stdout = stdout?;
    let _ = stderr?;
    if abort.load(Ordering::Relaxed) {
        return Err("Git source output exceeded its bound".into());
    }
    // Never echo remote stderr: URLs, redirects and transport helpers may print secrets.
    if !status.success() {
        return Err(
            "Git source command failed (remote inaccessible, invalid metadata or missing commit)"
                .into(),
        );
    }
    check(cancel)?;
    Ok(stdout)
}
fn git(
    dir: &Path,
    transport: Transport,
    args: &[&str],
    limit: usize,
    cancel: &dyn Fn() -> bool,
) -> R<Vec<u8>> {
    run(
        command(dir, transport).args(args),
        limit,
        None,
        GIT_TIMEOUT,
        cancel,
    )
}
fn text(dir: &Path, transport: Transport, args: &[&str], cancel: &dyn Fn() -> bool) -> R<String> {
    String::from_utf8(git(dir, transport, args, MAX_CONFIG, cancel)?)
        .map(|s| s.trim().to_owned())
        .map_err(|_| "Git metadata is not UTF-8".into())
}

fn safe_path(path: &str) -> R<()> {
    if path.is_empty()
        || path.contains(['\\', ':'])
        || path.chars().any(char::is_control)
        || path.split('/').any(|part| {
            part.is_empty() || part == "." || part == ".." || part.eq_ignore_ascii_case(".git")
        })
    {
        return Err("unsafe Git source path".into());
    }
    Ok(())
}
fn joined(parent: &str, path: &str) -> String {
    if parent.is_empty() {
        path.to_owned()
    } else {
        format!("{parent}/{path}")
    }
}
fn submodule_url(parent: &Url, value: &str, transport: Transport) -> R<Url> {
    if value.starts_with("./") || value.starts_with("../") {
        if value.contains(['\\', '?', '#']) || value.chars().any(char::is_control) {
            return Err("unsafe relative submodule URL".into());
        }
        // Git resolves relative submodules as if the repository URL were a directory.
        let base = format!("{}/", parent.as_str().trim_end_matches('/'));
        let url = Url::parse(&base)
            .and_then(|u| u.join(value))
            .map_err(|_| "invalid relative submodule URL")?;
        transport.remote(url.as_str())
    } else {
        transport.remote(value)
    }
}

#[derive(Debug)]
struct Entry {
    mode: String,
    oid: String,
    size: u64,
    path: String,
}
fn entries(bytes: &[u8]) -> R<Vec<Entry>> {
    let mut entries = Vec::new();
    for record in bytes.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let (head, path) = record.split_at(
            record
                .iter()
                .position(|b| *b == b'\t')
                .ok_or("invalid Git tree record")?,
        );
        let path = std::str::from_utf8(&path[1..]).map_err(|_| "source paths must be UTF-8")?;
        safe_path(path)?;
        let fields: Vec<_> = std::str::from_utf8(head)
            .map_err(|_| "invalid Git tree header")?
            .split_whitespace()
            .collect();
        if fields.len() != 4 || !valid_hex(fields[2], 40) {
            return Err("invalid Git tree entry".into());
        }
        let (mode, size) = match (fields[0], fields[1]) {
            ("160000", "commit") => ("160000", 0),
            (mode @ ("100644" | "100755" | "120000"), "blob") => (
                mode,
                fields[3]
                    .parse::<u64>()
                    .map_err(|_| "invalid Git blob size")?,
            ),
            _ => return Err("unsupported Git tree entry".into()),
        };
        entries.push(Entry {
            mode: mode.into(),
            oid: fields[2].into(),
            size,
            path: path.into(),
        });
    }
    Ok(entries)
}

// Use Git's config grammar, but never its include mechanism or URL rewriting.
fn config(
    dir: &Path,
    transport: Transport,
    bytes: &[u8],
    name: &str,
    cancel: &dyn Fn() -> bool,
) -> R<Vec<(String, String)>> {
    if bytes.len() > MAX_CONFIG {
        return Err("Git source config too large".into());
    }
    let file = dir.join(name);
    fs::write(&file, bytes).map_err(io_error)?;
    let output = git(
        dir,
        transport,
        &[
            "config",
            "--no-includes",
            "--null",
            "--list",
            "--file",
            file.to_str().ok_or("invalid staging path")?,
        ],
        MAX_CONFIG,
        cancel,
    )?;
    let mut values = Vec::new();
    for record in output.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let value = std::str::from_utf8(record).map_err(|_| "Git config must be UTF-8")?;
        let (key, value) = value.split_once('\n').ok_or("Git config requires values")?;
        if key.starts_with("include.") || key.starts_with("includeif.") || value.starts_with('!') {
            return Err("Git source config includes and commands are forbidden".into());
        }
        values.push((key.into(), value.into()));
    }
    Ok(values)
}
fn submodule_map(values: Vec<(String, String)>) -> R<BTreeMap<String, String>> {
    let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (key, value) in values {
        if let Some(rest) = key.strip_prefix("submodule.") {
            let (name, field) = rest.rsplit_once('.').ok_or("invalid submodule config")?;
            if name.is_empty() {
                return Err("empty submodule name".into());
            }
            if sections
                .entry(name.into())
                .or_default()
                .insert(field.into(), value)
                .is_some()
            {
                return Err("ambiguous submodule config".into());
            }
        }
    }
    let mut result = BTreeMap::new();
    for fields in sections.values() {
        let path = fields.get("path").ok_or("submodule has no path")?;
        safe_path(path)?;
        let url = fields.get("url").ok_or("submodule has no URL")?;
        if result.insert(path.clone(), url.clone()).is_some() {
            return Err("ambiguous submodule path".into());
        }
    }
    Ok(result)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Pointer {
    oid: String,
    size: u64,
}
fn pointer(bytes: &[u8]) -> R<Option<Pointer>> {
    if !bytes.starts_with(b"version https://git-lfs.github.com/spec/v1") {
        return Ok(None);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "invalid LFS pointer")?;
    let mut lines = text.lines();
    if lines.next() != Some("version https://git-lfs.github.com/spec/v1") {
        return Err("invalid LFS pointer version".into());
    }
    let mut oid = None;
    let mut size = None;
    for line in lines {
        if let Some(value) = line.strip_prefix("oid sha256:") {
            if oid.is_some() || !valid_hex(value, 64) {
                return Err("invalid LFS OID".into());
            }
            oid = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("size ") {
            if size.is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err("invalid LFS size".into());
            }
            size = Some(value.parse::<u64>().map_err(|_| "invalid LFS size")?);
        } else {
            // Extension pointers need additional transformations; silently copying
            // their underlying storage object would yield the wrong file bytes.
            return Err("unsupported LFS pointer field or extension".into());
        }
    }
    Ok(Some(Pointer {
        oid: oid.ok_or("missing LFS OID")?,
        size: size.ok_or("missing LFS size")?,
    }))
}

fn blobs(
    dir: &Path,
    transport: Transport,
    entries: &[Entry],
    cancel: &dyn Fn() -> bool,
) -> R<BTreeMap<String, Vec<u8>>> {
    let mut wanted = BTreeMap::new();
    let mut total = 0u64;
    for entry in entries {
        if entry.mode != "160000"
            && ((entry.mode != "120000" && entry.size <= MAX_POINTER)
                || entry.path == ".gitmodules"
                || entry.path == ".lfsconfig")
        {
            if entry.size > MAX_CONFIG as u64 {
                return Err("Git source config too large".into());
            }
            if !wanted.contains_key(&entry.oid) {
                total += entry.size + 128;
            }
            wanted.insert(entry.oid.clone(), entry.size);
        }
    }
    if total > MAX_BATCH as u64 {
        return Err("Git source metadata too large".into());
    }
    if wanted.is_empty() {
        return Ok(BTreeMap::new());
    }
    let input = dir.join("blob-input");
    fs::write(
        &input,
        wanted
            .keys()
            .map(|oid| format!("{oid}\n"))
            .collect::<String>(),
    )
    .map_err(io_error)?;
    let output = dir.join("blob-output");
    let mut command = command(dir, transport);
    command
        .args(["cat-file", "--batch"])
        .stdin(File::open(input).map_err(io_error)?);
    run(&mut command, MAX_BATCH, Some(&output), GIT_TIMEOUT, cancel)?;
    let mut reader = BufReader::new(File::open(output).map_err(io_error)?);
    let mut result = BTreeMap::new();
    for (oid, size) in wanted {
        check(cancel)?;
        let mut header = String::new();
        reader.read_line(&mut header).map_err(io_error)?;
        if header != format!("{oid} blob {size}\n") {
            return Err("invalid Git blob response".into());
        }
        let mut bytes = vec![0u8; size as usize];
        reader.read_exact(&mut bytes).map_err(io_error)?;
        let mut separator = [0];
        reader.read_exact(&mut separator).map_err(io_error)?;
        if separator != *b"\n" {
            return Err("invalid Git blob delimiter".into());
        }
        result.insert(oid, bytes);
    }
    Ok(result)
}

fn https(value: &str) -> R<Url> {
    let url = Url::parse(value).map_err(|_| "invalid LFS HTTPS URL")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("LFS requires HTTPS without embedded credentials".into());
    }
    Ok(url)
}
fn lfs_endpoint(remote: &Url, values: Vec<(String, String)>) -> R<Url> {
    let urls: Vec<_> = values.iter().filter(|(key, _)| key == "lfs.url").collect();
    if urls.len() > 1 {
        return Err("ambiguous LFS URL".into());
    }
    let mut base = if let Some((_, value)) = urls.first() {
        https(value)?
    } else {
        let mut base = remote.clone();
        let path = base.path().trim_end_matches('/');
        let path = if path.ends_with(".git") {
            path.into()
        } else {
            format!("{path}.git")
        };
        base.set_path(&format!("{path}/info/lfs"));
        base
    };
    // Preserve a validated endpoint's query, if any.
    base.set_path(&format!(
        "{}/objects/batch",
        base.path().trim_end_matches('/')
    ));
    https(base.as_str())
}

struct Http {
    url: Url,
    body: Option<Vec<u8>>,
    headers: reqwest::header::HeaderMap,
    file: Option<PathBuf>,
    expected: Option<Pointer>,
}
enum HttpEvent {
    Progress(u64, u64),
    Done(R<Vec<u8>>),
}

// The synchronous API supervises a private async network runtime, so cancellation
// drops stalled requests promptly, including during DNS, headers and body reads.
// Callback closures never leave the caller's thread and need not be Send/Sync.
fn http(request: Http, progress: &dyn Fn(u64, u64), cancel: &dyn Fn() -> bool) -> R<Vec<u8>> {
    check(cancel)?;
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let (sender, receiver) = mpsc::sync_channel(16);
    let worker = thread::spawn(move || {
        let result = (|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| "cannot start LFS network runtime")?;
            runtime.block_on(async {
                tokio::select! {
                    result = http_inner(request, &sender) => result,
                    _ = async {
                        while !worker_stop.load(Ordering::Relaxed) { tokio::time::sleep(Duration::from_millis(50)).await; }
                    } => Err("cancelled".into()),
                }
            })
        })();
        let _ = sender.send(HttpEvent::Done(result));
    });
    let mut cancelled = false;
    let result = loop {
        if cancel() {
            cancelled = true;
            stop.store(true, Ordering::Relaxed);
        }
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(HttpEvent::Progress(done, total)) => {
                if !cancelled {
                    progress(done, total);
                }
            }
            Ok(HttpEvent::Done(result)) => break result,
            Err(mpsc::RecvTimeoutError::Timeout) => (),
            Err(_) => break Err("LFS network worker failed".into()),
        }
    };
    let _ = worker.join();
    if cancelled {
        Err("cancelled".into())
    } else {
        check(cancel)?;
        result
    }
}
async fn http_inner(request: Http, events: &mpsc::SyncSender<HttpEvent>) -> R<Vec<u8>> {
    let client = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(30 * 60))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("GizMoth/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| "cannot create LFS HTTPS client")?;
    let mut url = request.url;
    let mut headers = request.headers;
    let mut redirects = 0;
    let mut response = loop {
        let mut builder = if let Some(body) = &request.body {
            client.post(url.clone()).body(body.clone())
        } else {
            client.get(url.clone())
        };
        builder = builder.headers(headers.clone());
        let response = builder
            .send()
            .await
            .map_err(|_| "LFS HTTPS request failed")?;
        if response.status().is_redirection() {
            // Do not repeat batch POSTs under ambiguous redirect semantics.
            if request.body.is_some() || redirects >= 5 {
                return Err("unsupported LFS redirect".into());
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or("missing LFS redirect target")?;
            let next = url
                .join(location)
                .map_err(|_| "invalid LFS redirect target")?;
            https(next.as_str())?;
            // Action headers may be credentials with arbitrary names. Never carry
            // any of them across origins, including after a chain of redirects.
            if next.origin() != url.origin() {
                headers.clear();
            }
            url = next;
            redirects += 1;
            continue;
        }
        if !response.status().is_success() {
            return Err(format!("LFS HTTP status {}", response.status().as_u16()));
        }
        break response;
    };
    let mut output = match &request.file {
        Some(path) => Some(
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(path)
                .map_err(io_error)?,
        ),
        None => None,
    };
    let bound = request
        .expected
        .as_ref()
        .map(|p| p.size)
        .unwrap_or(4 * 1024 * 1024);
    if response.content_length().is_some_and(|size| size > bound) {
        return Err("LFS response exceeds expected size".into());
    }
    let mut bytes = Vec::new();
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "could not read LFS response")?
    {
        done = done
            .checked_add(chunk.len() as u64)
            .ok_or("LFS size overflow")?;
        if done > bound {
            return Err("LFS response exceeds expected size".into());
        }
        hasher.update(&chunk);
        if let Some(file) = &mut output {
            file.write_all(&chunk).map_err(io_error)?;
        } else {
            bytes.extend_from_slice(&chunk);
        }
        // Progress is advisory; a slow UI must never block network cancellation.
        let _ = events.try_send(HttpEvent::Progress(done, bound));
    }
    if let Some(expected) = &request.expected {
        verify_lfs(&hex(&hasher.finalize()), done, expected)?;
    }
    if let Some(file) = &mut output {
        file.sync_all().map_err(io_error)?;
    }
    Ok(bytes)
}

fn verify_lfs(digest: &str, bytes: u64, expected: &Pointer) -> R<()> {
    if bytes != expected.size || digest != expected.oid {
        return Err("LFS object SHA-256 or size mismatch".into());
    }
    Ok(())
}

#[derive(Deserialize)]
struct BatchResponse {
    transfer: Option<String>,
    objects: Vec<BatchObject>,
}
#[derive(Deserialize)]
struct BatchObject {
    oid: String,
    size: u64,
    error: Option<serde_json::Value>,
    actions: Option<BTreeMap<String, Action>>,
}
#[derive(Deserialize)]
struct Action {
    href: String,
    #[serde(default)]
    header: BTreeMap<String, String>,
}
fn download_action(response: &[u8], pointer: &Pointer) -> R<(Url, reqwest::header::HeaderMap)> {
    let response: BatchResponse =
        serde_json::from_slice(response).map_err(|_| "invalid LFS batch response")?;
    if response.transfer.as_deref().is_some_and(|t| t != "basic") || response.objects.len() != 1 {
        return Err("unsupported or ambiguous LFS batch response".into());
    }
    let object = &response.objects[0];
    if object.oid != pointer.oid || object.size != pointer.size || object.error.is_some() {
        return Err("LFS batch OID/size mismatch or object unavailable".into());
    }
    let action = object
        .actions
        .as_ref()
        .and_then(|a| a.get("download"))
        .ok_or("LFS object has no download action")?;
    let url = https(&action.href)?;
    let mut headers = reqwest::header::HeaderMap::new();
    for (name, value) in &action.header {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| "invalid LFS action header name")?;
        if [
            "host",
            "connection",
            "content-length",
            "transfer-encoding",
            "upgrade",
            "proxy-authorization",
            "proxy-connection",
            "te",
            "trailer",
        ]
        .contains(&name.as_str())
        {
            return Err("unsafe LFS action header".into());
        }
        let value = reqwest::header::HeaderValue::from_str(value)
            .map_err(|_| "invalid LFS action header value")?;
        if headers.insert(name, value).is_some() {
            return Err("ambiguous LFS action headers".into());
        }
    }
    Ok((url, headers))
}
fn fetch_lfs(
    endpoint: &Url,
    pointer: &Pointer,
    destination: &Path,
    progress: &dyn Fn(u64, u64),
    cancel: &dyn Fn() -> bool,
) -> R<()> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/vnd.git-lfs+json"),
    );
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("application/vnd.git-lfs+json"),
    );
    let body = serde_json::to_vec(&serde_json::json!({
        "operation": "download", "transfers": ["basic"], "hash_algo": "sha256",
        "objects": [{"oid": pointer.oid, "size": pointer.size}]
    }))
    .map_err(|_| "cannot encode LFS batch request")?;
    let response = http(
        Http {
            url: endpoint.clone(),
            body: Some(body),
            headers,
            file: None,
            expected: None,
        },
        &|_, _| {},
        cancel,
    )?;
    let (url, headers) = download_action(&response, pointer)?;
    http(
        Http {
            url,
            body: None,
            headers,
            file: Some(destination.into()),
            expected: Some(pointer.clone()),
        },
        progress,
        cancel,
    )?;
    Ok(())
}

struct Acquisition<'a> {
    staging: &'a Path,
    transport: Transport,
    manifest: Manifest,
    objects: BTreeMap<String, u64>,
    ancestors: BTreeSet<(String, String)>,
    paths: BTreeSet<String>,
    submodules: bool,
    progress: &'a dyn Fn(u64, u64),
    cancel: &'a dyn Fn() -> bool,
}
impl Acquisition<'_> {
    fn repository(&mut self, remote: &Url, commit: &str, path: &str, depth: usize) -> R<()> {
        check(self.cancel)?;
        if depth > MAX_DEPTH || self.manifest.repositories.len() >= MAX_REPOS {
            return Err("too many Git submodules or excessive recursion".into());
        }
        if !self.paths.insert(path.into()) {
            return Err("ambiguous repository path".into());
        }
        let identity = (
            remote
                .as_str()
                .trim_end_matches('/')
                .trim_end_matches(".git")
                .to_owned(),
            commit.to_owned(),
        );
        if !self.ancestors.insert(identity.clone()) {
            return Err("cyclic Git submodule graph".into());
        }
        let index = self.manifest.repositories.len();
        let repo = self.staging.join(format!("repo-{index}"));
        fs::create_dir(&repo).map_err(io_error)?;
        git(
            &repo,
            self.transport,
            &["init", "--bare", "--object-format=sha1", "."],
            MAX_CONFIG,
            self.cancel,
        )?;
        // No depth/filter: bundle --all is self-contained even when the pinned
        // commit is no longer advertised by a moving branch. Fetching a raw SHA
        // does not reliably auto-follow tags, so request them explicitly for
        // offline describe. A tag/ref collision must not prevent the exact tree.
        if git(
            &repo,
            self.transport,
            &[
                "fetch",
                "--tags",
                "--force",
                "--no-recurse-submodules",
                "--",
                remote.as_str(),
                commit,
            ],
            MAX_CONFIG,
            self.cancel,
        )
        .is_err()
        {
            check(self.cancel)?;
            git(
                &repo,
                self.transport,
                &[
                    "fetch",
                    "--no-tags",
                    "--force",
                    "--no-recurse-submodules",
                    "--",
                    remote.as_str(),
                    commit,
                ],
                MAX_CONFIG,
                self.cancel,
            )?;
        }
        let actual = text(
            &repo,
            self.transport,
            &["rev-parse", "--verify", &format!("{commit}^{{commit}}")],
            self.cancel,
        )?;
        if actual != commit {
            return Err("Git remote did not provide the exact pinned commit".into());
        }
        git(
            &repo,
            self.transport,
            &["update-ref", PIN, commit],
            MAX_CONFIG,
            self.cancel,
        )?;
        let tree = entries(&git(
            &repo,
            self.transport,
            &["ls-tree", "-r", "-l", "-z", "--full-tree", commit],
            MAX_TREE,
            self.cancel,
        )?)?;
        let blobs = blobs(&repo, self.transport, &tree, self.cancel)?;
        let config_bytes = |name: &str| -> R<Option<&[u8]>> {
            match tree.iter().find(|e| e.path == name) {
                Some(entry) if entry.mode == "100644" || entry.mode == "100755" => Ok(Some(
                    blobs
                        .get(&entry.oid)
                        .ok_or("missing config blob")?
                        .as_slice(),
                )),
                Some(_) => Err("Git source config must be a regular pinned file".into()),
                None => Ok(None),
            }
        };
        let mut modules = BTreeMap::new();
        if self.submodules {
            if let Some(bytes) = config_bytes(".gitmodules")? {
                modules = submodule_map(config(
                    &repo,
                    self.transport,
                    bytes,
                    "modules-config",
                    self.cancel,
                )?)?;
            }
            let links: BTreeSet<_> = tree
                .iter()
                .filter(|e| e.mode == "160000")
                .map(|e| e.path.as_str())
                .collect();
            if links != modules.keys().map(String::as_str).collect() {
                return Err("pinned gitlinks and .gitmodules do not match".into());
            }
        }
        let pointers: Vec<_> = tree
            .iter()
            .filter(|e| e.mode == "100644" || e.mode == "100755")
            .filter_map(|e| blobs.get(&e.oid).map(|b| (e, b)))
            .map(|(entry, bytes)| pointer(bytes).map(|p| p.map(|p| (entry, p))))
            .collect::<R<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();
        if !pointers.is_empty() {
            let values = match config_bytes(".lfsconfig")? {
                Some(bytes) => config(&repo, self.transport, bytes, "lfs-config", self.cancel)?,
                None => Vec::new(),
            };
            let endpoint = lfs_endpoint(remote, values)?;
            for (entry, pointer) in pointers {
                let object = format!("objects/{}", pointer.oid);
                if let Some(size) = self.objects.get(&pointer.oid) {
                    if *size != pointer.size {
                        return Err("conflicting LFS sizes for one OID".into());
                    }
                } else {
                    fetch_lfs(
                        &endpoint,
                        &pointer,
                        &self.staging.join(&object),
                        self.progress,
                        self.cancel,
                    )?;
                    self.objects.insert(pointer.oid.clone(), pointer.size);
                }
                self.manifest.lfs.push(Lfs {
                    path: joined(path, &entry.path),
                    oid: pointer.oid,
                    size: pointer.size,
                    object,
                });
            }
        }
        let bundle = format!("repos/{index}.bundle");
        let output = self.staging.join(&bundle);
        git(
            &repo,
            self.transport,
            &[
                "bundle",
                "create",
                output.to_str().ok_or("invalid staging path")?,
                "--all",
            ],
            MAX_CONFIG,
            self.cancel,
        )?;
        self.manifest.repositories.push(Repository {
            path: path.into(),
            commit: commit.into(),
            bundle,
        });
        if self.submodules {
            for entry in tree.iter().filter(|e| e.mode == "160000") {
                let url = submodule_url(remote, &modules[&entry.path], self.transport)?;
                self.repository(&url, &entry.oid, &joined(path, &entry.path), depth + 1)?;
            }
        }
        self.ancestors.remove(&identity);
        Ok(())
    }
}

struct CheckedReader<'a, T> {
    reader: T,
    cancel: &'a dyn Fn() -> bool,
}
impl<T: Read> Read for CheckedReader<'_, T> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if (self.cancel)() {
            return Err(io::Error::other("cancelled"));
        }
        self.reader.read(bytes)
    }
}
fn sha256(path: &Path, cancel: &dyn Fn() -> bool) -> R<(String, u64)> {
    let mut reader = CheckedReader {
        reader: File::open(path).map_err(io_error)?,
        cancel,
    };
    let mut hasher = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0; 256 * 1024];
    loop {
        let n = reader.read(&mut buffer).map_err(io_error)?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        hasher.update(&buffer[..n]);
    }
    Ok((hex(&hasher.finalize()), bytes))
}
fn regular(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && !m.is_symlink())
}
fn publish(from: &Path, to: &Path) -> R<()> {
    // Windows rename cannot replace a file. A gap is safe: readers reacquire
    // when a memo/asset is absent; they never see partial bytes under its name.
    #[cfg(windows)]
    if fs::symlink_metadata(to).is_ok() {
        fs::remove_file(to).map_err(io_error)?;
    }
    fs::rename(from, to).map_err(io_error)
}
fn append<W: Write>(tar: &mut tar::Builder<W>, name: &str, size: u64, reader: impl Read) -> R<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(size);
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_cksum();
    tar.append_data(&mut header, name, reader).map_err(io_error)
}

/// Acquire public HTTPS Git sources without a host worktree. `commit` must be a
/// full SHA-1 commit ID; branch/tag expressions are deliberately not accepted.
/// Returns `dir/<digest>`'s SHA-256 and actual compressed length. A verified memo
/// hit works completely offline. Progress reports LFS transfer bytes, followed
/// by the completed archive length; unknown Git transfer totals are `(0, 0)`.
pub fn fetch(
    dir: &Path,
    url: &str,
    commit: &str,
    submodules: bool,
    progress: &dyn Fn(u64, u64),
    cancel: &dyn Fn() -> bool,
) -> R<Acquired> {
    fetch_with(
        dir,
        url,
        commit,
        submodules,
        progress,
        cancel,
        Transport::default(),
    )
}
fn fetch_with(
    dir: &Path,
    url: &str,
    commit: &str,
    submodules: bool,
    progress: &dyn Fn(u64, u64),
    cancel: &dyn Fn() -> bool,
    transport: Transport,
) -> R<Acquired> {
    check(cancel)?;
    let remote = transport.remote(url)?;
    let commit = commit.to_ascii_lowercase();
    if !valid_hex(&commit, 40) {
        return Err("Git sources require a full 40-hex commit".into());
    }
    let url = remote.as_str();
    fs::create_dir_all(dir).map_err(io_error)?;
    let memos = dir.join(".git-sources");
    fs::create_dir_all(&memos).map_err(io_error)?;
    if fs::symlink_metadata(&memos).map_err(io_error)?.is_symlink() {
        return Err("Git source memo directory must not be a symlink".into());
    }
    let spec = serde_json::to_vec(&(url, &commit, submodules))
        .map_err(|_| "cannot encode Git source spec")?;
    let key = hex(&Sha256::digest(&spec));
    let memo_path = memos.join(format!("{key}.json"));
    if regular(&memo_path) && fs::metadata(&memo_path).is_ok_and(|m| m.len() <= 16 * 1024) {
        if let Ok(bytes) = fs::read(&memo_path) {
            if let Ok(memo) = serde_json::from_slice::<Memo>(&bytes) {
                if (memo.version == 2 || (memo.version == 1 && submodules))
                    && memo.url == url
                    && memo.commit == commit
                    && memo.submodules == submodules
                    && valid_hex(&memo.digest, 64)
                {
                    let artifact = dir.join(&memo.digest);
                    if regular(&artifact)
                        && sha256(&artifact, cancel).is_ok_and(|(digest, bytes)| {
                            digest == memo.digest && bytes == memo.bytes
                        })
                    {
                        check(cancel)?;
                        progress(memo.bytes, memo.bytes);
                        return Ok(Acquired {
                            digest: memo.digest,
                            bytes: memo.bytes,
                        });
                    }
                }
            }
        }
    }
    check(cancel)?;
    let staging = Staging::new(dir)?;
    fs::create_dir(staging.0.join("repos")).map_err(io_error)?;
    fs::create_dir(staging.0.join("objects")).map_err(io_error)?;
    progress(0, 0);
    let mut acquisition = Acquisition {
        staging: &staging.0,
        transport,
        manifest: Manifest {
            version: 1,
            submodules,
            repositories: Vec::new(),
            lfs: Vec::new(),
        },
        objects: BTreeMap::new(),
        ancestors: BTreeSet::new(),
        paths: BTreeSet::new(),
        submodules,
        progress,
        cancel,
    };
    acquisition.repository(&remote, &commit, "", 0)?;
    let archive = staging.0.join("source.tar.gz");
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&archive)
        .map_err(io_error)?;
    let mut tar = tar::Builder::new(GzEncoder::new(file, Compression::default()));
    let manifest = serde_json::to_vec(&acquisition.manifest)
        .map_err(|_| "cannot encode Git source manifest")?;
    append(
        &mut tar,
        "gizmoth-git-source.json",
        manifest.len() as u64,
        manifest.as_slice(),
    )?;
    for name in acquisition
        .manifest
        .repositories
        .iter()
        .map(|r| r.bundle.clone())
        .chain(
            acquisition
                .objects
                .keys()
                .map(|oid| format!("objects/{oid}")),
        )
    {
        check(cancel)?;
        let file = File::open(staging.0.join(&name)).map_err(io_error)?;
        let size = file.metadata().map_err(io_error)?.len();
        append(
            &mut tar,
            &name,
            size,
            CheckedReader {
                reader: file,
                cancel,
            },
        )?;
    }
    let encoder = tar.into_inner().map_err(io_error)?;
    let file = encoder.finish().map_err(io_error)?;
    file.sync_all().map_err(io_error)?;
    drop(file);
    let (digest, bytes) = sha256(&archive, cancel)?;
    let memo = Memo {
        // Old nonrecursive containers did not retain the acquisition policy.
        version: 2,
        url: url.into(),
        commit,
        submodules,
        digest: digest.clone(),
        bytes,
    };
    let memo_file = staging.0.join("memo.json");
    let mut output = File::create(&memo_file).map_err(io_error)?;
    output
        .write_all(&serde_json::to_vec(&memo).map_err(|_| "cannot encode Git source memo")?)
        .map_err(io_error)?;
    output.sync_all().map_err(io_error)?;
    drop(output);
    check(cancel)?;
    // Atomic publication. Concurrent acquisitions may replace the memo only
    // with another complete archive for the same immutable source specification.
    let destination = dir.join(&digest);
    if regular(&destination)
        && sha256(&destination, cancel).is_ok_and(|(d, n)| d == digest && n == bytes)
    {
        fs::remove_file(&archive).map_err(io_error)?;
    } else {
        check(cancel)?;
        publish(&archive, &destination)?;
    }
    // Cancellation after the asset rename can leave an unreferenced *complete*
    // content-addressed asset, never a partial artifact or a memo pointing at it.
    check(cancel)?;
    publish(&memo_file, &memo_path)?;
    progress(bytes, bytes);
    Ok(Acquired { digest, bytes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const LOCAL: Transport = Transport { local: true };
    fn never() -> bool {
        false
    }
    fn quiet(_: u64, _: u64) {}

    // Every source fixture is bare, including trees with names APFS cannot
    // represent side by side. No test relies on a host source checkout either.
    struct Fixture {
        dir: PathBuf,
    }
    impl Fixture {
        fn new(root: &Path, name: &str) -> Self {
            let dir = root.join(name);
            fs::create_dir(&dir).unwrap();
            git(
                &dir,
                LOCAL,
                &["init", "--bare", "--object-format=sha1", "."],
                MAX_CONFIG,
                &never,
            )
            .unwrap();
            Self { dir }
        }
        fn url(&self) -> String {
            Url::from_directory_path(&self.dir)
                .unwrap()
                .to_string()
                .trim_end_matches('/')
                .into()
        }
        fn input(&self, args: &[&str], bytes: &[u8]) -> String {
            let input = self.dir.join("fixture-input");
            fs::write(&input, bytes).unwrap();
            let mut command = command(&self.dir, LOCAL);
            command
                .args(args)
                .stdin(File::open(input).unwrap())
                .env("GIT_AUTHOR_NAME", "Fixture")
                .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
                .env("GIT_COMMITTER_NAME", "Fixture")
                .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
                .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
                .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z");
            String::from_utf8(run(&mut command, MAX_CONFIG, None, GIT_TIMEOUT, &never).unwrap())
                .unwrap()
                .trim()
                .into()
        }
        fn blob(&self, bytes: &[u8]) -> String {
            self.input(&["hash-object", "-w", "--stdin"], bytes)
        }
        fn tree(&self, entries: &[(&str, &str, &str)]) -> String {
            let mut bytes = Vec::new();
            for (mode, name, oid) in entries {
                let kind = match *mode {
                    "160000" => "commit",
                    "040000" => "tree",
                    _ => "blob",
                };
                bytes.extend_from_slice(format!("{mode} {kind} {oid}\t{name}\0").as_bytes());
            }
            self.input(&["mktree", "-z", "--missing"], &bytes)
        }
        fn commit(&self, tree: &str, parent: Option<&str>) -> String {
            let mut args = vec![
                "-c",
                "commit.gpgsign=false",
                "commit-tree",
                tree,
                "-m",
                "Fixture",
            ];
            if let Some(parent) = parent {
                args.extend(["-p", parent]);
            }
            let commit = self.input(&args, b"");
            git(
                &self.dir,
                LOCAL,
                &["update-ref", "refs/heads/main", &commit],
                MAX_CONFIG,
                &never,
            )
            .unwrap();
            commit
        }
        fn file_commit(&self, content: &[u8], parent: Option<&str>) -> String {
            let blob = self.blob(content);
            self.commit(&self.tree(&[("100644", "file.txt", &blob)]), parent)
        }
    }
    fn acquire(assets: &Path, source: &Fixture, commit: &str, modules: bool) -> Acquired {
        fetch_with(
            assets,
            &source.url(),
            commit,
            modules,
            &quiet,
            &never,
            LOCAL,
        )
        .unwrap()
    }
    fn archive(assets: &Path, acquired: &Acquired) -> (Manifest, BTreeMap<String, Vec<u8>>) {
        let file = File::open(assets.join(&acquired.digest)).unwrap();
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
        let mut files = BTreeMap::new();
        for entry in tar.entries().unwrap() {
            let mut entry = entry.unwrap();
            assert!(entry.header().entry_type().is_file());
            let name = entry.path().unwrap().to_str().unwrap().to_owned();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            assert!(files.insert(name, bytes).is_none());
        }
        let manifest = serde_json::from_slice(&files["gizmoth-git-source.json"]).unwrap();
        (manifest, files)
    }
    fn restore(
        root: &Path,
        name: &str,
        repository: &Repository,
        files: &BTreeMap<String, Vec<u8>>,
    ) -> Fixture {
        let restored = Fixture::new(root, name);
        let bundle = restored.dir.join("source.bundle");
        fs::write(&bundle, &files[&repository.bundle]).unwrap();
        git(
            &restored.dir,
            LOCAL,
            &["bundle", "verify", bundle.to_str().unwrap()],
            MAX_CONFIG,
            &never,
        )
        .unwrap();
        git(
            &restored.dir,
            LOCAL,
            &["fetch", "--", bundle.to_str().unwrap(), "+refs/*:refs/*"],
            MAX_CONFIG,
            &never,
        )
        .unwrap();
        assert_eq!(
            text(&restored.dir, LOCAL, &["rev-parse", PIN], &never).unwrap(),
            repository.commit
        );
        git(
            &restored.dir,
            LOCAL,
            &["fsck", "--full", "--no-reflogs"],
            MAX_CONFIG,
            &never,
        )
        .unwrap();
        restored
    }
    fn assert_no_publication(assets: &Path) {
        if !assets.exists() {
            return;
        }
        for entry in fs::read_dir(assets).unwrap() {
            let entry = entry.unwrap();
            assert_eq!(entry.file_name(), ".git-sources");
            assert_eq!(fs::read_dir(entry.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn exact_commit_full_history_tags_case_distinct_names_and_offline_bundle() {
        let root = tempfile::tempdir().unwrap();
        let source = Fixture::new(root.path(), "source.git");
        let upper = source.blob(b"UPPER");
        let lower = source.blob(b"lower");
        let tree = source.tree(&[
            ("100644", "Header.h", &upper),
            ("100644", "header.h", &lower),
        ]);
        let first = source.commit(&tree, None);
        git(
            &source.dir,
            LOCAL,
            &["update-ref", "refs/tags/v1.0", &first],
            MAX_CONFIG,
            &never,
        )
        .unwrap();
        let pinned = source.commit(&tree, Some(&first));
        let newer = source.file_commit(b"moving branch", Some(&pinned));
        assert_ne!(pinned, newer);
        let assets = root.path().join("assets");
        let acquired = acquire(&assets, &source, &pinned, true);
        assert_eq!(
            sha256(&assets.join(&acquired.digest), &never).unwrap(),
            (acquired.digest.clone(), acquired.bytes)
        );
        let (manifest, files) = archive(&assets, &acquired);
        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.repositories.len(), 1);
        assert_eq!(manifest.repositories[0].path, "");
        assert_eq!(manifest.repositories[0].commit, pinned);
        assert!(manifest.lfs.is_empty());
        assert_eq!(files.len(), 2);
        fs::rename(&source.dir, root.path().join("offline.git")).unwrap();
        let restored = restore(
            root.path(),
            "restored.git",
            &manifest.repositories[0],
            &files,
        );
        for (name, expected) in [("Header.h", "UPPER"), ("header.h", "lower")] {
            assert_eq!(
                text(
                    &restored.dir,
                    LOCAL,
                    &["show", &format!("{pinned}:{name}")],
                    &never
                )
                .unwrap(),
                expected
            );
        }
        assert_eq!(
            text(
                &restored.dir,
                LOCAL,
                &["rev-parse", &format!("{pinned}^")],
                &never
            )
            .unwrap(),
            first
        );
        assert!(text(
            &restored.dir,
            LOCAL,
            &["describe", "--tags", &pinned],
            &never
        )
        .unwrap()
        .starts_with("v1.0-1-g"));
        let cached = acquire(&assets, &source, &pinned, true);
        assert_eq!(cached.digest, acquired.digest);
        assert_eq!(cached.bytes, acquired.bytes);
        assert!(!restored.dir.join("Header.h").exists());
    }

    #[test]
    fn nested_submodules_use_gitlinks_and_config_from_pinned_trees() {
        let root = tempfile::tempdir().unwrap();
        let leaf = Fixture::new(root.path(), "leaf.git");
        let leaf_pin = leaf.file_commit(b"pinned leaf", None);
        let _leaf_moved = leaf.file_commit(b"moved leaf", Some(&leaf_pin));
        let child = Fixture::new(root.path(), "child.git");
        let child_modules = child.blob(b"[submodule \"leaf\"]\npath = nested\nurl = ../leaf.git\n");
        let child_tree = child.tree(&[
            ("100644", ".gitmodules", &child_modules),
            ("160000", "nested", &leaf_pin),
        ]);
        let child_pin = child.commit(&child_tree, None);
        let _child_moved = child.file_commit(b"child changed its tree", Some(&child_pin));
        let source = Fixture::new(root.path(), "source.git");
        let modules = source
            .blob(b"[submodule \"child.with.dots\"]\npath = deps/child\nurl = ../child.git\n");
        let deps = source.tree(&[("160000", "child", &child_pin)]);
        let tree = source.tree(&[
            ("100644", ".gitmodules", &modules),
            ("040000", "deps", &deps),
        ]);
        let pinned = source.commit(&tree, None);
        let _moved = source.file_commit(b"root removed all submodules", Some(&pinned));
        let assets = root.path().join("assets");
        let acquired = acquire(&assets, &source, &pinned, true);
        let (manifest, files) = archive(&assets, &acquired);
        assert_eq!(
            manifest
                .repositories
                .iter()
                .map(|r| r.path.as_str())
                .collect::<Vec<_>>(),
            ["", "deps/child", "deps/child/nested"]
        );
        assert_eq!(
            manifest
                .repositories
                .iter()
                .map(|r| r.commit.as_str())
                .collect::<Vec<_>>(),
            [&pinned, &child_pin, &leaf_pin]
        );
        for (i, repo) in manifest.repositories.iter().enumerate() {
            restore(root.path(), &format!("restored-{i}.git"), repo, &files);
        }
        assert!(manifest.submodules);
        // Recursive containers retain their original bytes and legacy meaning.
        assert!(!files["gizmoth-git-source.json"]
            .windows(b"submodules".len())
            .any(|bytes| bytes == b"submodules"));
        let without = acquire(&assets, &source, &pinned, false);
        let (manifest, _) = archive(&assets, &without);
        assert_eq!(manifest.repositories.len(), 1);
        assert!(!manifest.submodules);
        assert_ne!(acquired.digest, without.digest);
        // A legacy memo may point at a container lacking the false policy.
        let key = hex(&Sha256::digest(
            serde_json::to_vec(&(source.url().as_str(), &pinned, false)).unwrap(),
        ));
        let memo_path = assets.join(".git-sources").join(format!("{key}.json"));
        let mut memo: Memo = serde_json::from_slice(&fs::read(&memo_path).unwrap()).unwrap();
        memo.version = 1;
        memo.digest = acquired.digest;
        memo.bytes = acquired.bytes;
        fs::write(&memo_path, serde_json::to_vec(&memo).unwrap()).unwrap();
        let repaired = acquire(&assets, &source, &pinned, false);
        assert_eq!(repaired.digest, without.digest);
        let memo: Memo = serde_json::from_slice(&fs::read(memo_path).unwrap()).unwrap();
        assert_eq!(memo.version, 2);
    }

    #[test]
    fn missing_commit_cancelled_and_corrupt_memo_never_publish_partial_data() {
        let root = tempfile::tempdir().unwrap();
        let source = Fixture::new(root.path(), "source.git");
        let commit = source.file_commit(b"one", None);
        let missing = root.path().join("missing-assets");
        assert!(fetch_with(
            &missing,
            &source.url(),
            &"1".repeat(40),
            true,
            &quiet,
            &never,
            LOCAL
        )
        .is_err());
        assert_no_publication(&missing);
        let cancelled = root.path().join("cancelled-assets");
        let count = Cell::new(0);
        let cancel = || {
            count.set(count.get() + 1);
            count.get() > 7
        };
        assert!(fetch_with(
            &cancelled,
            &source.url(),
            &commit,
            true,
            &quiet,
            &cancel,
            LOCAL
        )
        .unwrap_err()
        .contains("cancelled"));
        assert_no_publication(&cancelled);
        let assets = root.path().join("assets");
        let first = acquire(&assets, &source, &commit, true);
        let memo = fs::read_dir(assets.join(".git-sources"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::write(&memo, b"not json").unwrap();
        let recovered = acquire(&assets, &source, &commit, true);
        assert_eq!(
            sha256(&assets.join(&recovered.digest), &never).unwrap().0,
            recovered.digest
        );
        fs::write(assets.join(&recovered.digest), b"corrupted archive").unwrap();
        // Corrupt artifacts must trigger an actual reacquisition, never a hit.
        fs::rename(&source.dir, root.path().join("offline.git")).unwrap();
        assert!(fetch_with(&assets, &source.url(), &commit, true, &quiet, &never, LOCAL).is_err());
        assert!(!fs::read_dir(&assets).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".git-source-")));
        fs::rename(root.path().join("offline.git"), &source.dir).unwrap();
        let repaired = acquire(&assets, &source, &commit, true);
        assert_eq!(
            sha256(&assets.join(&repaired.digest), &never).unwrap(),
            (repaired.digest, repaired.bytes)
        );
        assert!(first.bytes > 0);
        // An already-cancelled operation must not even serve a valid cache hit.
        assert!(fetch_with(
            &assets,
            &source.url(),
            &commit,
            true,
            &quiet,
            &|| true,
            LOCAL
        )
        .is_err());
    }

    #[test]
    fn cancellation_during_packing_and_after_complete_asset_publication() {
        let root = tempfile::tempdir().unwrap();
        let source = Fixture::new(root.path(), "source.git");
        let commit = source.file_commit(b"source bytes", None);
        for after_publication in [false, true] {
            let assets = root.path().join(if after_publication {
                "published"
            } else {
                "packing"
            });
            let cancel = || {
                fs::read_dir(&assets).is_ok_and(|entries| {
                    entries.flatten().any(|entry| {
                        if after_publication {
                            valid_hex(&entry.file_name().to_string_lossy(), 64)
                        } else {
                            entry.path().join("source.tar.gz").is_file()
                        }
                    })
                })
            };
            assert!(fetch_with(
                &assets,
                &source.url(),
                &commit,
                true,
                &quiet,
                &cancel,
                LOCAL
            )
            .unwrap_err()
            .contains("cancelled"));
            if after_publication {
                let files: Vec<_> = fs::read_dir(&assets)
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .filter(|p| p.is_file())
                    .collect();
                assert_eq!(files.len(), 1);
                let (digest, bytes) = sha256(&files[0], &never).unwrap();
                assert_eq!(files[0].file_name().unwrap().to_str().unwrap(), digest);
                let acquired = Acquired { digest, bytes };
                assert_eq!(archive(&assets, &acquired).0.repositories[0].commit, commit);
                assert_eq!(
                    fs::read_dir(assets.join(".git-sources")).unwrap().count(),
                    0
                );
            } else {
                assert_no_publication(&assets);
            }
        }
    }

    #[test]
    fn rejects_unsafe_urls_paths_missing_and_ambiguous_submodule_config() {
        let root = tempfile::tempdir().unwrap();
        let source = Fixture::new(root.path(), "source.git");
        let pin = source.file_commit(b"valid", None);
        for url in [
            "file:///tmp/repo",
            "/tmp/repo",
            "git://example.com/r",
            "ssh://example.com/r",
            "https://user:secret@example.com/r",
            "https://example.com/r?token=secret",
            "ext::command",
        ] {
            assert!(
                fetch(&root.path().join("unused"), url, &pin, true, &quiet, &never).is_err(),
                "{url}"
            );
        }
        assert!(!root.path().join("unused").exists());
        for path in [
            "",
            "../escape",
            "a/../b",
            "a//b",
            "/root",
            "a\\b",
            "a/.git/b",
            "A/.GIT/b",
            "C:drive",
            "x\ny",
        ] {
            assert!(safe_path(path).is_err(), "{path}");
        }
        let parent = Url::parse("https://example.com/team/project.git").unwrap();
        assert_eq!(
            submodule_url(&parent, "../library.git", Transport::default())
                .unwrap()
                .as_str(),
            "https://example.com/team/library.git"
        );
        assert!(submodule_url(&parent, "//evil.example/repo", Transport::default()).is_err());
        for (i, config_text) in [
            "[submodule \"x\"]\npath = ../escape\nurl = https://example.com/r\n",
            "[submodule \"x\"]\npath = child\nurl = ext::evil\n",
            "[submodule \"x\"]\npath = child\nurl = https://example.com/a\nurl = https://example.com/b\n",
            "[submodule \"x\"]\npath = child\nurl = https://example.com/a\nupdate = !evil\n",
            "[submodule \"x\"]\npath = child\n",
            "[include]\npath = /tmp/config\n",
            "",
        ].iter().enumerate() {
            let module = source.blob(config_text.as_bytes());
            let tree = source.tree(&[("100644", ".gitmodules", &module), ("160000", "child", &pin)]);
            let commit = source.commit(&tree, None);
            let assets = root.path().join(format!("invalid-{i}"));
            assert!(fetch_with(&assets, &source.url(), &commit, true, &quiet, &never, LOCAL).is_err());
            assert_no_publication(&assets);
        }
        let duplicate = vec![
            ("submodule.x.path".into(), "child".into()),
            ("submodule.x.url".into(), "https://example.com/a".into()),
            ("submodule.y.path".into(), "child".into()),
            ("submodule.y.url".into(), "https://example.com/b".into()),
        ];
        assert!(submodule_map(duplicate).is_err());
    }

    #[test]
    fn rejects_unsafe_tree_paths_and_recursion_cycles_before_fetch() {
        let root = tempfile::tempdir().unwrap();
        let source = Fixture::new(root.path(), "source.git");
        let commit = source.file_commit(b"test", None);
        for path in ["../escape", "x/.git/config", "bad\\name"] {
            let record = format!("100644 blob {} 4\t{path}\0", "1".repeat(40));
            assert!(entries(record.as_bytes()).is_err());
        }
        let staging = Staging::new(root.path()).unwrap();
        let mut acquisition = Acquisition {
            staging: &staging.0,
            transport: LOCAL,
            manifest: Manifest {
                version: 1,
                submodules: true,
                repositories: Vec::new(),
                lfs: Vec::new(),
            },
            objects: BTreeMap::new(),
            ancestors: BTreeSet::new(),
            paths: BTreeSet::new(),
            submodules: true,
            progress: &quiet,
            cancel: &never,
        };
        let remote = LOCAL.remote(&source.url()).unwrap();
        acquisition.ancestors.insert((
            remote
                .as_str()
                .trim_end_matches('/')
                .trim_end_matches(".git")
                .into(),
            commit.clone(),
        ));
        assert!(acquisition
            .repository(&remote, &commit, "", 0)
            .unwrap_err()
            .contains("cyclic"));
        assert!(acquisition
            .repository(&remote, &commit, "deep", MAX_DEPTH + 1)
            .unwrap_err()
            .contains("recursion"));
        assert_eq!(fs::read_dir(&staging.0).unwrap().count(), 0);
    }

    #[test]
    fn lfs_pointer_batch_and_content_digest_size_validation() {
        let bytes = b"real LFS object bytes\0\xff";
        let digest = hex(&Sha256::digest(bytes));
        let expected = Pointer {
            oid: digest.clone(),
            size: bytes.len() as u64,
        };
        let encoded = format!(
            "version https://git-lfs.github.com/spec/v1\noid sha256:{digest}\nsize {}\n",
            bytes.len()
        );
        assert_eq!(pointer(encoded.as_bytes()).unwrap(), Some(expected.clone()));
        assert_eq!(pointer(b"ordinary small source file").unwrap(), None);
        verify_lfs(&digest, bytes.len() as u64, &expected).unwrap();
        assert!(verify_lfs(
            &hex(&Sha256::digest(b"wrong contents")),
            expected.size,
            &expected
        )
        .is_err());
        assert!(verify_lfs(&digest, expected.size + 1, &expected).is_err());
        assert!(pointer(encoded.replace("size ", "ext-0-foo ").as_bytes()).is_err());
        assert!(pointer(format!("{encoded}size 1\n").as_bytes()).is_err());
        assert!(pointer(encoded.replace(&digest, "bad").as_bytes()).is_err());
        let batch = |oid: &str, size: u64, href: &str, headers: serde_json::Value| {
            serde_json::to_vec(&serde_json::json!({
            "transfer":"basic", "objects":[{"oid":oid,"size":size,"actions":{"download":{"href":href,"header":headers}}}]
        })).unwrap()
        };
        let response = batch(
            &digest,
            expected.size,
            "https://objects.example/file?signature=private",
            serde_json::json!({"Authorization":"Bearer secret", "X-Custom-Token":"required"}),
        );
        let (url, headers) = download_action(&response, &expected).unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(headers["x-custom-token"], "required");
        assert!(download_action(
            &batch(
                &"0".repeat(64),
                expected.size,
                "https://objects.example/file",
                serde_json::json!({})
            ),
            &expected
        )
        .is_err());
        assert!(download_action(
            &batch(
                &digest,
                0,
                "https://objects.example/file",
                serde_json::json!({})
            ),
            &expected
        )
        .is_err());
        assert!(download_action(
            &batch(
                &digest,
                expected.size,
                "http://objects.example/file",
                serde_json::json!({})
            ),
            &expected
        )
        .is_err());
        assert!(download_action(
            &batch(
                &digest,
                expected.size,
                "https://objects.example/file",
                serde_json::json!({"Host":"evil"})
            ),
            &expected
        )
        .is_err());
        let remote = Url::parse("https://example.com/team/project.git").unwrap();
        assert_eq!(
            lfs_endpoint(&remote, Vec::new()).unwrap().as_str(),
            "https://example.com/team/project.git/info/lfs/objects/batch"
        );
        assert!(lfs_endpoint(
            &remote,
            vec![("lfs.url".into(), "file:///etc/passwd".into())]
        )
        .is_err());
        let root = tempfile::tempdir().unwrap();
        assert!(config(
            root.path(),
            LOCAL,
            b"[lfs]\nurl = !evil\n",
            "unsafe-config",
            &never
        )
        .is_err());
    }

    #[test]
    fn git_output_bounds_deadlines_and_stalled_transport_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let source = Fixture::new(root.path(), "source.git");
        let mut list = command(&source.dir, LOCAL);
        list.args(["config", "--list"]);
        assert!(run(&mut list, 1, None, GIT_TIMEOUT, &never)
            .unwrap_err()
            .contains("bound"));
        #[cfg(unix)]
        {
            // A descendant filling stderr must not deadlock the stdout reader.
            let mut noisy = command(&source.dir, LOCAL);
            noisy.args([
                "-c",
                "alias.gizmoth-test-noisy=!yes bounded-stderr >&2",
                "gizmoth-test-noisy",
            ]);
            assert!(
                run(&mut noisy, MAX_CONFIG, None, Duration::from_secs(5), &never)
                    .unwrap_err()
                    .contains("bound")
            );
        }
        for cancel_request in [false, true] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let ready = Arc::new(AtomicBool::new(false));
            let server_ready = ready.clone();
            let server = thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut buffer = [0; 8192];
                assert!(socket.read(&mut buffer).unwrap() > 0);
                server_ready.store(true, Ordering::Relaxed);
                match socket.read(&mut buffer) {
                    Ok(0) => (),
                    Err(e) if e.kind() == io::ErrorKind::ConnectionReset => (),
                    result => panic!("transport survived process cleanup: {result:?}"),
                }
            });
            let mut command = command(&source.dir, LOCAL);
            command.args([
                "-c",
                "protocol.http.allow=always",
                "ls-remote",
                &format!("http://{address}/stall"),
            ]);
            let cancel = || cancel_request && ready.load(Ordering::Relaxed);
            let timeout = if cancel_request {
                Duration::from_secs(5)
            } else {
                Duration::from_secs(1)
            };
            let start = Instant::now();
            let error = run(&mut command, MAX_CONFIG, None, timeout, &cancel).unwrap_err();
            assert!(
                error.contains(if cancel_request {
                    "cancelled"
                } else {
                    "timed out"
                }),
                "{error}"
            );
            assert!(start.elapsed() < Duration::from_secs(5));
            server.join().unwrap();
        }
    }

    #[test]
    fn lfs_http_cancellation_interrupts_a_stalled_tls_handshake() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let ready = Arc::new(AtomicBool::new(false));
        let server_ready = ready.clone();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = [0; 16384];
            assert!(socket.read(&mut bytes).unwrap() > 0);
            server_ready.store(true, Ordering::Relaxed);
            match socket.read(&mut bytes) {
                Ok(0) => (),
                Err(e) if e.kind() == io::ErrorKind::ConnectionReset => (),
                result => panic!("TLS request survived cancellation: {result:?}"),
            }
        });
        let start = Instant::now();
        let request = Http {
            url: Url::parse(&format!("https://{address}/stall")).unwrap(),
            body: None,
            headers: reqwest::header::HeaderMap::new(),
            file: None,
            expected: None,
        };
        assert!(http(request, &quiet, &|| ready.load(Ordering::Relaxed))
            .unwrap_err()
            .contains("cancelled"));
        assert!(start.elapsed() < Duration::from_secs(5));
        server.join().unwrap();
    }
}
