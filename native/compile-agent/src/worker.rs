//! Worker VMs: the read-only erofs system image under a tmpfs overlay, plus the
//! slot's host-backed writable `/work` directory (sources, outputs, ccache,
//! home). Jobs arrive over vsock; each connection is one job or one probe.
use crate::{
    command::{self, Sink},
    log,
    protocol::{
        self, BootConfig, BootMode, Entry, EntryKind, GuestJob, GuestMessage, HostMessage, BUILDER_UID, CACHE_PORT,
        DATA_CHUNK, MANIFEST_CHUNK,
    },
    session, sys,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    os::unix::{
        fs::{symlink, MetadataExt, PermissionsExt},
        net::UnixStream,
    },
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::Duration,
};

const NEWROOT: &str = "/newroot";
const SRC: &str = "/work/src";
const MANIFESTS: &str = "/work/src/.manifests";
const BASE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// A downloaded userland EROFS is the only root disk. The kernel and static
/// agent boot without it in pack mode; build slots attach the published pack.
pub fn prepare(config: &BootConfig) -> io::Result<()> {
    let lower = Path::new("/lower");
    let system_disk = sys::scsi_disk(0)?;
    sys::mount(&system_disk.to_string_lossy(), lower, "erofs", libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV, "")?;
    let overlay = Path::new("/run/overlay");
    sys::mount("tmpfs", overlay, "tmpfs", 0, "mode=0755")?;
    fs::create_dir_all(overlay.join("upper"))?;
    fs::create_dir_all(overlay.join("work"))?;
    let root = Path::new(NEWROOT);
    sys::mount("overlay", root, "overlay", 0, "lowerdir=/lower,upperdir=/run/overlay/upper,workdir=/run/overlay/work")?;
    for path in ["dev", "proc", "sys"] {
        sys::bind(&Path::new("/").join(path), &root.join(path))?;
    }
    for path in ["usr", "bin", "sbin", "lib", "lib64", "opt"] {
        if fs::symlink_metadata(lower.join(path)).is_ok_and(|m| m.is_dir()) {
            sys::bind_readonly(&lower.join(path), &root.join(path))?;
        }
    }
    sys::mount("tmpfs", &root.join("tmp"), "tmpfs", libc::MS_NOSUID | libc::MS_NODEV, "mode=1777")?;
    sys::mount("tmpfs", &root.join("run"), "tmpfs", libc::MS_NOSUID | libc::MS_NODEV, "mode=0755")?;
    let work_share = config.shares.iter().find(|share| share.path == "/work" && share.writable)
        .ok_or_else(|| io::Error::other("The host work directory export is missing"))?;
    mount_share(root, work_share)?;
    crate::work::migrate(root, &config.legacy_work)?;
    for dir in ["home", "src", "src/.manifests", "build", "output", "cache", "bazel", "bazel-repo-cache", "ccache", "tmp"] {
        let path = root.join("work").join(dir);
        fs::create_dir_all(&path)?;
        sys::lchown(&path, BUILDER_UID, BUILDER_UID)?;
    }
    for share in &config.shares { if share.path != "/work" { mount_share(root, share)?; } }
    session::write_resolv(root)?;
    fs::write(root.join("etc/hosts"), "127.0.0.1 localhost gizmoth-build\n::1 localhost\n")?;
    fs::write(root.join("etc/bazel.bazelrc"), bazelrc(config))?;
    fs::write(root.join("etc/ccache.conf"), ccache_conf(config))?;
    Ok(())
}

/// QEMU is another downloaded formula. Register only x86-64 ELF binaries;
/// ARM64 programs keep executing natively on the same Linux kernel.
pub(crate) fn translation(root: &Path) -> io::Result<bool> {
    let interpreter = ["/usr/bin/qemu-x86_64-static", "/usr/bin/qemu-x86_64"].into_iter().find(|p| root.join(p.trim_start_matches('/')).is_file());
    let Some(interpreter) = interpreter else { return Ok(false); };
    let dir = root.join("proc/sys/fs/binfmt_misc");
    sys::mount("binfmt_misc", &dir, "binfmt_misc", libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC, "")?;
    let mut magic = [0u8; 20];
    magic[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    magic[18] = 62;
    let mut mask = [0u8; 20];
    mask[..7].fill(0xff); mask[18] = 0xff; mask[19] = 0xff;
    let escaped = |bytes: &[u8]| bytes.iter().map(|b| format!("\\x{b:02x}")).collect::<String>();
    fs::write(dir.join("register"), format!(":gizmoth-x86_64:M::{}:{}:{interpreter}:O\n", escaped(&magic), escaped(&mask)))?;
    log("x86-64 programs use the downloaded QEMU user interpreter");
    Ok(true)
}

/// Mounts a host folder over 9P with the host's explicit access policy: the
/// host has no virtio-fs, so the kernel's v9fs uses the socket (`trans=fd`).
pub fn mount_share(root: &Path, share: &protocol::Share) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let mut attempts = 0;
    let stream = loop {
        match sys::vsock_connect(libc::VMADDR_CID_HOST, share.port) {
            Ok(stream) => break stream,
            // hv_sock may still be coming up this early in boot.
            Err(_) if attempts < 50 => {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(io::Error::new(error.kind(), format!("share port {}: {error}", share.port))),
        }
    };
    let fd = stream.as_raw_fd();
    let options = format!(
        "trans=fd,rfdno={fd},wfdno={fd},version=9p2000.L,msize={},cache={},access=any",
        protocol::SHARE_MSIZE, if share.writable { "mmap" } else { "none" }
    );
    let target = root.join(share.path.trim_start_matches('/'));
    let flags = libc::MS_NOSUID | libc::MS_NODEV | if share.writable { 0 } else { libc::MS_RDONLY };
    sys::mount("gizmoth", &target, "9p", flags, &options)?;
    // The kernel keeps its own reference to the connection.
    drop(stream);
    log(format!("mounted share {} from host port {}", share.path, share.port));
    Ok(())
}

fn cache_url(namespace: &str) -> String {
    format!("http://127.0.0.1:{CACHE_PORT}/{namespace}")
}

fn bazelrc(config: &BootConfig) -> String {
    let mut lines = vec![
        "# Written by GizMoth for this compile VM.".to_string(),
        "startup --output_user_root=/work/bazel".into(),
        "common --repository_cache=/work/bazel-repo-cache".into(),
    ];
    if config.caches.bazel {
        lines.push(format!("build --remote_cache={}", cache_url("bazel")));
        lines.push("build --remote_upload_local_results=true".into());
        lines.push("build --remote_timeout=60".into());
    }
    lines.extend(config.bazelrc.iter().cloned());
    lines.join("\n") + "\n"
}

fn ccache_conf(config: &BootConfig) -> String {
    let mut lines = vec![
        "cache_dir = /work/ccache".to_string(),
        format!(
            "max_size = {}",
            if config.caches.ccache_local_size.is_empty() { "5G" } else { config.caches.ccache_local_size.as_str() }
        ),
    ];
    if config.caches.ccache {
        lines.push(format!("remote_storage = {}", cache_url("ccache")));
    }
    lines.join("\n") + "\n"
}

/// Runs in the forked agent: chroot into the prepared root and serve vsock.
pub fn serve(config: &BootConfig) -> i32 {
    if let Err(error) = sys::chroot(Path::new(NEWROOT)) {
        log(format!("chroot: {error}"));
        return 2;
    }
    let mut config = config.clone();
    config.env.retain(|(name, _)| name != "GIZMOTH_X86_64_TRANSLATION");
    match translation(Path::new("/")) {
        Ok(true) => config.env.push(("GIZMOTH_X86_64_TRANSLATION".into(), "qemu-binfmt-v1".into())),
        Ok(false) => {},
        Err(error) => log(format!("x86 translation unavailable: {error}")),
    }
    if config.caches.bazel || config.caches.ccache || !config.caches.custom.is_empty() {
        std::thread::spawn(cache_proxy);
    }
    let listener = match sys::VsockListener::bind(protocol::CONTROL_PORT) {
        Ok(listener) => listener,
        Err(error) => {
            log(format!("vsock listen: {error}"));
            return 2;
        }
    };
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let config = Arc::new(config);
    std::thread::spawn(move || loop {
        let stream = match listener.accept() {
            Ok(stream) => stream,
            Err(error) => {
                log(format!("vsock accept: {error}"));
                let _ = stop_tx.send(());
                return;
            }
        };
        let (config, stop) = (config.clone(), stop_tx.clone());
        std::thread::spawn(move || {
            if connection(stream, &config) {
                let _ = stop.send(());
            }
        });
    });
    let _ = stop_rx.recv();
    0
}

/// Relays guest TCP connections on 127.0.0.1:7421 to the host cache over vsock.
fn cache_proxy() {
    let listener = match TcpListener::bind(("127.0.0.1", CACHE_PORT as u16)) {
        Ok(listener) => listener,
        Err(error) => {
            log(format!("cache proxy: {error}"));
            return;
        }
    };
    for client in listener.incoming().flatten() {
        std::thread::spawn(move || {
            let Ok(host) = sys::vsock_connect(libc::VMADDR_CID_HOST, CACHE_PORT) else {
                return;
            };
            relay(client, host);
        });
    }
}

fn relay(client: TcpStream, host: UnixStream) {
    let (Ok(mut client_read), Ok(mut host_write)) = (client.try_clone(), host.try_clone()) else {
        return;
    };
    let upstream = std::thread::spawn(move || {
        let _ = io::copy(&mut client_read, &mut host_write);
        let _ = host_write.shutdown(Shutdown::Write);
    });
    let (mut host_read, mut client_write) = (host, client);
    let _ = io::copy(&mut host_read, &mut client_write);
    let _ = client_write.shutdown(Shutdown::Write);
    let _ = upstream.join();
}

fn image_info() -> Option<serde_json::Value> {
    serde_json::from_slice(&fs::read("/etc/gizmoth-image.json").ok()?).ok()
}

/// Returns true when the host asked the VM to power off.
fn connection(mut stream: UnixStream, config: &BootConfig) -> bool {
    let first = match protocol::read_frame(&mut stream) {
        Ok(Some((protocol::JSON, payload))) => protocol::parse::<HostMessage>(&payload),
        _ => return false,
    };
    match first {
        Ok(HostMessage::Hello) => {
            let hello = GuestMessage::Hello {
                version: protocol::AGENT_VERSION,
                mode: BootMode::Worker,
                work: sys::disk_info(Path::new("/work")),
                image: image_info(),
            };
            let _ = protocol::write_json(&mut stream, &hello);
            false
        }
        Ok(HostMessage::Poweroff) => true,
        Ok(HostMessage::Job { job }) => {
            let Ok(writer) = stream.try_clone() else { return false };
            let sink: Sink = Arc::new(Mutex::new(writer));
            if let Err(error) = run_job(stream, &sink, job, config) {
                let _ = command::send_json(&sink, &GuestMessage::Error { message: error });
            }
            false
        }
        _ => false,
    }
}

fn workspace_root(job: &GuestJob) -> Result<PathBuf, String> {
    if !protocol::valid_key(&job.workspace) {
        return Err("invalid workspace key".into());
    }
    Ok(Path::new(SRC).join(&job.workspace))
}

/// The host identity must remain unchanged even when the guest normalizes modes
/// or its filesystem rounds mtimes. A missing guest stamp is an old manifest:
/// files are resent once rather than trusting metadata without a content digest.
#[derive(Clone, Serialize, Deserialize)]
struct SavedEntry {
    #[serde(flatten)]
    host: Entry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    guest: Option<GuestFile>,
}

#[derive(Clone, Serialize, Deserialize)]
struct GuestFile {
    mtime: i64,
    mode: u32,
    sha256: [u8; 32],
}

fn load_manifest(path: &Path) -> BTreeMap<String, SavedEntry> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<SavedEntry>>(&bytes).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| protocol::valid_relative(&entry.host.path))
        .map(|entry| (entry.host.path.clone(), entry))
        .collect()
}

fn save_manifest(path: &Path, manifest: &BTreeMap<String, SavedEntry>) -> io::Result<()> {
    let temporary = path.with_extension("json.tmp");
    let entries: Vec<&SavedEntry> = manifest.values().collect();
    fs::write(&temporary, serde_json::to_vec(&entries)?)?;
    fs::rename(temporary, path)
}

fn mtime(meta: &fs::Metadata) -> i64 {
    meta.mtime() * 1_000_000_000 + meta.mtime_nsec()
}

fn file_digest(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let (mut digest, mut buffer) = (Sha256::new(), vec![0; 64 * 1024]);
    loop {
        match file.read(&mut buffer) {
            Ok(0) => return Ok(digest.finalize().into()),
            Ok(size) => digest.update(&buffer[..size]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

fn matches_guest(root: &Path, saved: &SavedEntry) -> io::Result<bool> {
    let entry = &saved.host;
    // A replaced parent symlink must not make a recorded path look unchanged
    // by resolving to a matching file somewhere outside this workspace tree.
    let mut path = root.to_path_buf();
    let mut parts = entry.path.split('/').peekable();
    while let Some(part) = parts.next() {
        path.push(part);
        if parts.peek().is_some() && !fs::symlink_metadata(&path)?.is_dir() {
            return Ok(false);
        }
    }
    let meta = fs::symlink_metadata(&path)?;
    Ok(match entry.kind {
        EntryKind::File => match &saved.guest {
            Some(guest) => meta.is_file()
                && meta.len() == entry.size
                && mtime(&meta) == guest.mtime
                && meta.mode() & 0o777 == guest.mode
                && file_digest(&path)? == guest.sha256,
            None => false,
        },
        EntryKind::Dir => meta.is_dir() && meta.mode() & 0o7777 == ((entry.mode & 0o7777) | 0o700),
        EntryKind::Link => meta.file_type().is_symlink()
            && entry.target.as_deref().is_some_and(|target| fs::read_link(&path).is_ok_and(|actual| actual.as_os_str() == std::ffi::OsStr::new(target))),
    })
}

fn advertised_manifest(root: &Path, manifest: &BTreeMap<String, SavedEntry>) -> Vec<Entry> {
    let mut invalid_dirs = HashSet::new();
    manifest.values().map(|saved| {
        let entry = &saved.host;
        let invalid_parent = entry.path.match_indices('/').any(|(index, _)| invalid_dirs.contains(&entry.path[..index]));
        if !invalid_parent && matches_guest(root, saved).unwrap_or(false) {
            return entry.clone();
        }
        // The host deletes a mismatched directory recursively. Its children
        // must be resent too, even if their own content is still intact.
        if entry.kind == EntryKind::Dir { invalid_dirs.insert(entry.path.clone()); }
        // Keep the path so the host can still delete it if it vanished there. A
        // target-less link cannot match any scanned host entry, forcing resend
        // even for directories (whose metadata is ignored by diff).
        Entry { path: entry.path.clone(), kind: EntryKind::Link, size: 0, mtime: 0, mode: 0, target: None }
    }).collect()
}

fn remove_any(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn read_json(stream: &mut UnixStream) -> Result<HostMessage, String> {
    match protocol::read_frame(stream) {
        Ok(Some((protocol::JSON, payload))) => protocol::parse(&payload).map_err(|e| e.to_string()),
        Ok(Some(_)) => Err("unexpected data frame".into()),
        Ok(None) => Err("host closed the connection".into()),
        Err(error) => Err(error.to_string()),
    }
}

/// Applies the host's changes on top of the recorded manifest.
fn sync(stream: &mut UnixStream, sink: &Sink, job: &GuestJob, root: &Path) -> Result<(), String> {
    let path = Path::new(MANIFESTS).join(format!("{}.json", job.workspace));
    sync_workspace(stream, sink, root, &path)
}

fn sync_workspace(stream: &mut UnixStream, sink: &Sink, root: &Path, manifest_path: &Path) -> Result<(), String> {
    let _ = command::send_json(sink, &GuestMessage::Phase { phase: "syncing".into() });
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    sys::lchown(root, BUILDER_UID, BUILDER_UID).map_err(|e| e.to_string())?;
    let mut manifest = load_manifest(manifest_path);
    let entries = advertised_manifest(root, &manifest);
    let envelope = GuestMessage::Manifest { entries: vec![], done: false };
    let chunks = protocol::json_chunks(&entries, MANIFEST_CHUNK, &envelope).map_err(|e| e.to_string())?;
    for (index, chunk) in chunks.iter().enumerate() {
        let done = index + 1 == chunks.len();
        command::send_json(sink, &GuestMessage::Manifest { entries: chunk.to_vec(), done }).map_err(|e| e.to_string())?;
    }
    let (mut files, mut bytes) = (0u64, 0u64);
    let mut buffer_name = 0u64;
    loop {
        match read_json(stream)? {
            HostMessage::Delete { paths } => {
                for path in paths {
                    if !protocol::valid_relative(&path) {
                        return Err(format!("invalid path {path}"));
                    }
                    remove_any(&root.join(&path)).map_err(|e| format!("delete {path}: {e}"))?;
                    let prefix = format!("{path}/");
                    manifest.retain(|key, _| key != &path && !key.starts_with(&prefix));
                }
            }
            HostMessage::Put { entry } => {
                if !protocol::valid_relative(&entry.path) {
                    return Err(format!("invalid path {}", entry.path));
                }
                let target = root.join(&entry.path);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let mut sha256 = None;
                match entry.kind {
                    EntryKind::Dir => {
                        if fs::symlink_metadata(&target).is_ok_and(|m| !m.is_dir()) {
                            remove_any(&target).map_err(|e| e.to_string())?;
                        }
                        fs::create_dir_all(&target).map_err(|e| e.to_string())?;
                        fs::set_permissions(&target, fs::Permissions::from_mode((entry.mode & 0o7777) | 0o700))
                            .map_err(|e| e.to_string())?;
                    }
                    EntryKind::Link => {
                        let link = entry.target.clone().ok_or("symlink without a target")?;
                        remove_any(&target).map_err(|e| e.to_string())?;
                        symlink(link, &target).map_err(|e| e.to_string())?;
                    }
                    EntryKind::File => {
                        buffer_name += 1;
                        let temporary = target.with_file_name(format!(".gzm-sync-{}-{buffer_name}", std::process::id()));
                        let mut file = File::create(&temporary).map_err(|e| e.to_string())?;
                        let mut digest = Sha256::new();
                        let mut left = entry.size;
                        while left > 0 {
                            match protocol::read_frame(stream).map_err(|e| e.to_string())? {
                                Some((protocol::DATA, data)) if data.len() as u64 <= left => {
                                    file.write_all(&data).map_err(|e| e.to_string())?;
                                    digest.update(&data);
                                    left -= data.len() as u64;
                                }
                                _ => {
                                    let _ = fs::remove_file(&temporary);
                                    return Err(format!("incomplete data for {}", entry.path));
                                }
                            }
                        }
                        drop(file);
                        fs::set_permissions(&temporary, fs::Permissions::from_mode((entry.mode & 0o777) | 0o600))
                            .map_err(|e| e.to_string())?;
                        sys::set_mtime(&temporary, entry.mtime).map_err(|e| e.to_string())?;
                        if fs::symlink_metadata(&target).is_ok_and(|m| m.is_dir()) {
                            remove_any(&target).map_err(|e| e.to_string())?;
                        }
                        fs::rename(&temporary, &target).map_err(|e| e.to_string())?;
                        sha256 = Some(digest.finalize().into());
                        files += 1;
                        bytes += entry.size;
                    }
                }
                sys::lchown(&target, BUILDER_UID, BUILDER_UID).map_err(|e| e.to_string())?;
                let guest = if let Some(sha256) = sha256 {
                    let meta = fs::symlink_metadata(&target).map_err(|e| e.to_string())?;
                    Some(GuestFile { mtime: mtime(&meta), mode: meta.mode() & 0o777, sha256 })
                } else {
                    None
                };
                manifest.insert(entry.path.clone(), SavedEntry { host: entry, guest });
            }
            HostMessage::SyncDone => break,
            HostMessage::Cancel => return Err("cancelled".into()),
            other => return Err(format!("unexpected {other:?} during sync")),
        }
    }
    save_manifest(manifest_path, &manifest).map_err(|e| format!("save manifest: {e}"))?;
    command::send_json(sink, &GuestMessage::Synced { files, bytes }).map_err(|e| e.to_string())
}

fn job_env(job: &GuestJob, config: &BootConfig, root: &Path) -> Vec<(String, String)> {
    let base = if config.caches.ccache || Path::new("/usr/lib/ccache").is_dir() {
        format!("/usr/lib/ccache:{BASE_PATH}")
    } else {
        BASE_PATH.to_string()
    };
    // Package toolchains come first; ccache wrappers and the system follow.
    let path = config.path.iter().map(String::as_str).chain([base.as_str()]).collect::<Vec<_>>().join(":");
    let mut env: Vec<(String, String)> = vec![
        ("PATH".into(), path),
        ("HOME".into(), "/work/home".into()),
        ("USER".into(), "builder".into()),
        ("LOGNAME".into(), "builder".into()),
        ("LANG".into(), "C.UTF-8".into()),
        ("TERM".into(), "dumb".into()),
        ("TMPDIR".into(), "/work/tmp".into()),
        ("CCACHE_CONFIGPATH".into(), "/etc/ccache.conf".into()),
        ("GIZMOTH_SLOT".into(), config.slot.clone()),
        ("GIZMOTH_JOB_ID".into(), job.id.clone()),
        ("GIZMOTH_WORKSPACE".into(), root.display().to_string()),
        ("GIZMOTH_SOURCE".into(), "/src".into()),
        ("GIZMOTH_BUILD_DIR".into(), root.display().to_string()),
        ("GIZMOTH_OUTPUT".into(), "/work/output".into()),
        ("QEMU_LD_PREFIX".into(), "/opt/gizmoth/x86_64".into()),
    ];
    for (name, variable) in &config.caches.custom {
        env.push((variable.clone(), cache_url(name)));
    }
    env.extend(config.env.iter().cloned());
    env.extend(job.env.iter().cloned());
    env
}

fn run_job(mut stream: UnixStream, sink: &Sink, job: GuestJob, config: &BootConfig) -> Result<(), String> {
    let root = workspace_root(&job)?;
    sync(&mut stream, sink, &job, &root)?;
    let cwd = if job.cwd.is_empty() {
        root.clone()
    } else if protocol::valid_relative(&job.cwd) {
        root.join(&job.cwd)
    } else {
        return Err("invalid working directory".into());
    };
    // After synchronization every host message goes through this reader.
    let cancel = Arc::new(AtomicBool::new(false));
    let (messages_tx, messages) = mpsc::channel::<HostMessage>();
    {
        let (cancel, mut reader) = (cancel.clone(), stream.try_clone().map_err(|e| e.to_string())?);
        std::thread::spawn(move || loop {
            match protocol::read_frame(&mut reader) {
                Ok(Some((protocol::JSON, payload))) => match protocol::parse::<HostMessage>(&payload) {
                    Ok(HostMessage::Cancel) => cancel.store(true, Ordering::SeqCst),
                    Ok(message) => {
                        if messages_tx.send(message).is_err() {
                            return;
                        }
                    }
                    Err(_) => {}
                },
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => {
                    cancel.store(true, Ordering::SeqCst);
                    return;
                }
            }
        });
    }
    let _ = command::send_json(sink, &GuestMessage::Phase { phase: "running".into() });
    let spec = command::Spec {
        argv: job.argv.clone(),
        cwd,
        env: job_env(&job, config, &root),
        root: None,
        user: Some(BUILDER_UID),
    };
    let outcome = match command::run(spec, sink, &cancel, job.timeout_secs.map(Duration::from_secs)) {
        Ok(outcome) => outcome,
        Err(error) => {
            command::note(sink, &format!("could not start {}: {error}", job.argv[0]));
            command::Outcome { code: Some(127), ..Default::default() }
        }
    };
    command::send_json(
        sink,
        &GuestMessage::Exit {
            code: outcome.code,
            signal: outcome.signal,
            timed_out: outcome.timed_out,
            cancelled: outcome.cancelled,
        },
    )
    .map_err(|e| e.to_string())?;
    while let Ok(message) = messages.recv() {
        match message {
            HostMessage::Collect { patterns, max_files, max_bytes } => {
                let _ = command::send_json(sink, &GuestMessage::Phase { phase: "collecting".into() });
                collect(sink, &root, &patterns, max_files, max_bytes)?;
            }
            HostMessage::Finish => break,
            _ => {}
        }
    }
    Ok(())
}

fn expand(dir: &Path, relative: &str, parts: &[&str], depth: usize, out: &mut Vec<(String, PathBuf)>) {
    if depth > 48 {
        return;
    }
    let Some((first, rest)) = parts.split_first() else {
        if fs::metadata(dir).is_ok_and(|m| m.is_file()) {
            out.push((relative.to_string(), dir.to_path_buf()));
        }
        return;
    };
    let join = |name: &str| if relative.is_empty() { name.to_string() } else { format!("{relative}/{name}") };
    if *first == "**" {
        expand(dir, relative, rest, depth + 1, out);
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    expand(&path, &join(&name), parts, depth + 1, out);
                }
            }
        }
    } else if !first.contains(['*', '?']) {
        let path = dir.join(first);
        if fs::metadata(&path).is_ok() {
            expand(&path, &join(first), rest, depth + 1, out);
        }
    } else if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if protocol::component_matches(first, &name) {
                expand(&entry.path(), &join(&name), rest, depth + 1, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    struct Workspace {
        path: PathBuf,
        root: PathBuf,
        manifest: PathBuf,
    }

    impl Workspace {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!("gzm-sync-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            fs::create_dir(&path).unwrap();
            Self { root: path.join("src"), manifest: path.join("manifest.json"), path }
        }

        // The agent runs as root in Linux. Exercise its actual socket protocol
        // and ownership changes in that environment, without mounting /work.
        fn exchange(&self, send: impl FnOnce(&mut UnixStream)) -> (Vec<Entry>, Result<(), String>) {
            let (mut host, mut guest) = UnixStream::pair().unwrap();
            let sink = Arc::new(Mutex::new(guest.try_clone().unwrap()));
            let (root, manifest) = (self.root.clone(), self.manifest.clone());
            let task = std::thread::spawn(move || sync_workspace(&mut guest, &sink, &root, &manifest));
            let mut advertised = Vec::new();
            loop {
                let (kind, bytes) = protocol::read_frame(&mut host).unwrap().unwrap();
                assert_eq!(kind, protocol::JSON);
                match protocol::parse::<GuestMessage>(&bytes).unwrap() {
                    GuestMessage::Phase { .. } => {}
                    GuestMessage::Manifest { entries, done } => {
                        advertised.extend(entries);
                        if done { break; }
                    }
                    other => panic!("unexpected {other:?}"),
                }
            }
            send(&mut host);
            host.shutdown(Shutdown::Write).unwrap();
            (advertised, task.join().unwrap())
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) { let _ = remove_any(&self.path); }
    }

    fn source(path: &str, kind: EntryKind) -> Entry {
        Entry { path: path.into(), kind, size: 4, mtime: 1_700_000_000_123_456_789, mode: 0o444, target: None }
    }

    fn put(stream: &mut UnixStream, entry: &Entry, bytes: &[u8]) {
        protocol::write_json(stream, &HostMessage::Put { entry: entry.clone() }).unwrap();
        if entry.kind == EntryKind::File {
            // Deliberately different DATA and verification read boundaries.
            for chunk in bytes.chunks(3) { protocol::write_frame(stream, protocol::DATA, chunk).unwrap(); }
        }
    }

    fn done(stream: &mut UnixStream) { protocol::write_json(stream, &HostMessage::SyncDone).unwrap(); }

    #[test]
    fn sync_revalidates_edited_deleted_and_retyped_entries() {
        let workspace = Workspace::new();
        let file = source("a-file", EntryKind::File);
        let dir = source("b-dir", EntryKind::Dir);
        let link = Entry { target: Some("a-file".into()), ..source("c-link", EntryKind::Link) };
        let child = source("b-dir/child", EntryKind::File);
        let originals = vec![file.clone(), dir.clone(), child.clone(), link.clone()];
        let (advertised, result) = workspace.exchange(|stream| {
            for entry in &originals { put(stream, entry, b"orig"); }
            done(stream);
        });
        result.unwrap();
        assert!(advertised.is_empty());
        let (advertised, result) = workspace.exchange(done);
        result.unwrap();
        assert_eq!(advertised, originals);

        let saved = load_manifest(&workspace.manifest);
        fs::write(workspace.root.join("a-file"), b"edit").unwrap();
        sys::set_mtime(&workspace.root.join("a-file"), saved["a-file"].guest.as_ref().unwrap().mtime).unwrap();
        fs::set_permissions(workspace.root.join("b-dir"), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches_guest(&workspace.root, &saved["b-dir/child"]).unwrap());
        remove_any(&workspace.root.join("c-link")).unwrap();
        symlink("./a-file", workspace.root.join("c-link")).unwrap();
        let (advertised, result) = workspace.exchange(done);
        result.unwrap();
        assert_eq!(advertised.len(), 4);
        for (current, original) in advertised.iter().zip(&originals) {
            assert_eq!(current.path, original.path);
            assert!(!current.same_content(original), "{} was falsely advertised unchanged", original.path);
        }
        remove_any(&workspace.root.join("a-file")).unwrap();
        remove_any(&workspace.root.join("b-dir")).unwrap();
        symlink("elsewhere", workspace.root.join("b-dir")).unwrap();
        let (advertised, result) = workspace.exchange(|stream| {
            protocol::write_json(stream, &HostMessage::Delete { paths: vec!["a-file".into(), "b-dir".into()] }).unwrap();
            for entry in &originals { put(stream, entry, b"orig"); }
            done(stream);
        });
        result.unwrap();
        assert!(!advertised[0].same_content(&file));
        let (advertised, result) = workspace.exchange(done);
        result.unwrap();
        assert_eq!(advertised, originals);
    }

    #[test]
    fn interrupted_sync_cannot_reuse_previously_saved_content_identity() {
        let workspace = Workspace::new();
        let file = source("source", EntryKind::File);
        workspace.exchange(|stream| { put(stream, &file, b"orig"); done(stream); }).1.unwrap();
        let changed = Entry { mtime: file.mtime + 1000, ..file.clone() };
        assert!(workspace.exchange(|stream| put(stream, &changed, b"next")).1.is_err());
        assert_eq!(fs::read(workspace.root.join("source")).unwrap(), b"next");
        assert_eq!(load_manifest(&workspace.manifest)["source"].host, file);
        let (advertised, result) = workspace.exchange(|stream| { put(stream, &file, b"orig"); done(stream); });
        result.unwrap();
        assert!(!advertised[0].same_content(&file));
        assert_eq!(workspace.exchange(done).0, [file.clone()]);

        assert!(workspace.exchange(|stream| {
            protocol::write_json(stream, &HostMessage::Delete { paths: vec!["source".into()] }).unwrap();
        }).1.is_err());
        let (advertised, result) = workspace.exchange(|stream| { put(stream, &file, b"orig"); done(stream); });
        result.unwrap();
        assert!(!advertised[0].same_content(&file));
        assert_eq!(fs::read(workspace.root.join("source")).unwrap(), b"orig");
    }

    #[test]
    fn legacy_manifest_resends_files_and_keeps_host_identity_separate_from_guest_stamp() {
        let workspace = Workspace::new();
        fs::create_dir_all(&workspace.root).unwrap();
        let file = source("source", EntryKind::File);
        fs::write(workspace.root.join("source"), b"orig").unwrap();
        sys::set_mtime(&workspace.root.join("source"), file.mtime).unwrap();
        fs::write(&workspace.manifest, serde_json::to_vec(&vec![file.clone()]).unwrap()).unwrap();
        let (advertised, result) = workspace.exchange(|stream| { put(stream, &file, b"orig"); done(stream); });
        result.unwrap();
        assert!(!advertised[0].same_content(&file));
        let mut saved = load_manifest(&workspace.manifest);
        // Model a filesystem restoring a different timestamp precision. Its
        // observed stamp is verified, while the original host identity is sent.
        sys::set_mtime(&workspace.root.join("source"), file.mtime + 99).unwrap();
        saved.get_mut("source").unwrap().guest.as_mut().unwrap().mtime = mtime(&fs::metadata(workspace.root.join("source")).unwrap());
        save_manifest(&workspace.manifest, &saved).unwrap();
        let (advertised, result) = workspace.exchange(done);
        result.unwrap();
        assert_eq!(advertised, [file]);
    }
}

fn collect(sink: &Sink, root: &Path, patterns: &[String], max_files: u32, max_bytes: u64) -> Result<(), String> {
    let (mut seen, mut missing, mut files, mut bytes) = (HashSet::new(), Vec::new(), 0u32, 0u64);
    for pattern in patterns {
        if !protocol::valid_pattern(pattern) {
            missing.push(pattern.clone());
            continue;
        }
        let parts: Vec<&str> = pattern.split('/').collect();
        let mut matches = Vec::new();
        expand(root, "", &parts, 0, &mut matches);
        if matches.is_empty() {
            missing.push(pattern.clone());
        }
        for (relative, path) in matches {
            if !seen.insert(relative.clone()) {
                continue;
            }
            let meta = fs::metadata(&path).map_err(|e| e.to_string())?;
            if files + 1 > max_files || bytes + meta.len() > max_bytes {
                return Err(format!("artifacts exceed the limit of {max_files} files / {max_bytes} bytes"));
            }
            let mut file = File::open(&path).map_err(|e| format!("{relative}: {e}"))?;
            command::send_json(sink, &GuestMessage::File { path: relative.clone(), size: meta.len(), mode: meta.mode() & 0o777 })
                .map_err(|e| e.to_string())?;
            let (mut left, mut buffer) = (meta.len(), vec![0u8; DATA_CHUNK]);
            while left > 0 {
                let want = left.min(buffer.len() as u64) as usize;
                file.read_exact(&mut buffer[..want]).map_err(|e| format!("{relative}: {e}"))?;
                command::send(sink, protocol::DATA, &buffer[..want]).map_err(|e| e.to_string())?;
                left -= want as u64;
            }
            files += 1;
            bytes += meta.len();
        }
    }
    command::send_json(sink, &GuestMessage::Collected { files, bytes, missing }).map_err(|e| e.to_string())
}
