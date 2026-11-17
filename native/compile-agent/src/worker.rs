//! Worker VMs: the read-only erofs system image under a tmpfs overlay, plus the
//! slot's host-backed writable `/work` directory (sources, outputs, ccache,
//! home). Jobs arrive over vsock; each connection is one job or one probe.
use crate::{
    command::{self, Sink},
    log,
    protocol::{
        self, BootConfig, BootMode, Entry, EntryKind, GuestJob, GuestMessage, HostMessage,
        BUILDER_UID, CACHE_PORT, DATA_CHUNK, MANIFEST_CHUNK,
    },
    session, sys,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    ffi::{CStr, CString, OsStr},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
            net::UnixStream,
        },
    },
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
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
    sys::mount(
        &system_disk.to_string_lossy(),
        lower,
        "erofs",
        libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV,
        "",
    )?;
    let overlay = Path::new("/run/overlay");
    sys::mount("tmpfs", overlay, "tmpfs", 0, "mode=0755")?;
    fs::create_dir_all(overlay.join("upper"))?;
    fs::create_dir_all(overlay.join("work"))?;
    let root = Path::new(NEWROOT);
    sys::mount(
        "overlay",
        root,
        "overlay",
        0,
        "lowerdir=/lower,upperdir=/run/overlay/upper,workdir=/run/overlay/work",
    )?;
    for path in ["dev", "proc", "sys"] {
        sys::bind(&Path::new("/").join(path), &root.join(path))?;
    }
    for path in ["usr", "bin", "sbin", "lib", "lib64", "opt"] {
        if fs::symlink_metadata(lower.join(path)).is_ok_and(|m| m.is_dir()) {
            sys::bind_readonly(&lower.join(path), &root.join(path))?;
        }
    }
    sys::mount(
        "tmpfs",
        &root.join("tmp"),
        "tmpfs",
        libc::MS_NOSUID | libc::MS_NODEV,
        "mode=1777",
    )?;
    sys::mount(
        "tmpfs",
        &root.join("run"),
        "tmpfs",
        libc::MS_NOSUID | libc::MS_NODEV,
        "mode=0755",
    )?;
    let work_share = config
        .shares
        .iter()
        .find(|share| share.path == "/work" && share.writable)
        .ok_or_else(|| io::Error::other("The host work directory export is missing"))?;
    mount_share(root, work_share)?;
    crate::work::migrate(root, &config.legacy_work)?;
    for dir in [
        "home",
        "src",
        "src/.manifests",
        "build",
        "output",
        "cache",
        "bazel",
        "bazel-repo-cache",
        "tmp",
    ] {
        let path = root.join("work").join(dir);
        fs::create_dir_all(&path)?;
        sys::lchown(&path, BUILDER_UID, BUILDER_UID)?;
    }
    for share in &config.shares {
        if share.path != "/work" {
            mount_share(root, share)?;
        }
    }
    session::write_resolv(root)?;
    fs::write(
        root.join("etc/hosts"),
        "127.0.0.1 localhost gizmoth-build\n::1 localhost\n",
    )?;
    fs::write(root.join("etc/bazel.bazelrc"), bazelrc(config))?;
    fs::write(root.join("etc/ccache.conf"), ccache_conf(config))?;
    Ok(())
}

/// QEMU is another downloaded formula. Register only x86-64 ELF binaries;
/// ARM64 programs keep executing natively on the same Linux kernel.
pub(crate) fn translation(root: &Path) -> io::Result<bool> {
    let interpreter = ["/usr/bin/qemu-x86_64-static", "/usr/bin/qemu-x86_64"]
        .into_iter()
        .find(|p| root.join(p.trim_start_matches('/')).is_file());
    let Some(interpreter) = interpreter else {
        return Ok(false);
    };
    let dir = root.join("proc/sys/fs/binfmt_misc");
    sys::mount(
        "binfmt_misc",
        &dir,
        "binfmt_misc",
        libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
        "",
    )?;
    let mut magic = [0u8; 20];
    magic[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    magic[18] = 62;
    let mut mask = [0u8; 20];
    mask[..7].fill(0xff);
    mask[18] = 0xff;
    mask[19] = 0xff;
    let escaped = |bytes: &[u8]| {
        bytes
            .iter()
            .map(|b| format!("\\x{b:02x}"))
            .collect::<String>()
    };
    fs::write(
        dir.join("register"),
        format!(
            ":gizmoth-x86_64:M::{}:{}:{interpreter}:O\n",
            escaped(&magic),
            escaped(&mask)
        ),
    )?;
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
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("share port {}: {error}", share.port),
                ))
            }
        }
    };
    let fd = stream.as_raw_fd();
    let options = format!(
        "trans=fd,rfdno={fd},wfdno={fd},version=9p2000.L,msize={},cache={},access=any",
        protocol::SHARE_MSIZE,
        if share.writable { "mmap" } else { "none" }
    );
    let target = root.join(share.path.trim_start_matches('/'));
    let flags = libc::MS_NOSUID | libc::MS_NODEV | if share.writable { 0 } else { libc::MS_RDONLY };
    sys::mount("gizmoth", &target, "9p", flags, &options)?;
    // The kernel keeps its own reference to the connection.
    drop(stream);
    log(format!(
        "mounted share {} from host port {}",
        share.path, share.port
    ));
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
        // Only transient metadata and temporary files may use the VM's tmpfs.
        "cache_dir = /tmp/ccache".to_string(),
        "temporary_dir = /tmp/ccache/tmp".into(),
        "stats = false".into(),
        "remote_only = true".into(),
    ];
    if config.caches.ccache {
        lines.push(format!("remote_storage = {}", cache_url("ccache")));
    } else {
        lines.push("disable = true".into());
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
    config
        .env
        .retain(|(name, _)| name != "GIZMOTH_X86_64_TRANSLATION");
    match translation(Path::new("/")) {
        Ok(true) => config
            .env
            .push(("GIZMOTH_X86_64_TRANSLATION".into(), "qemu-binfmt-v1".into())),
        Ok(false) => {}
        Err(error) => log(format!("x86 translation unavailable: {error}")),
    }
    if config.caches.bazel || config.caches.ccache {
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
            let Ok(writer) = stream.try_clone() else {
                return false;
            };
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
    let parent = SyncDir::open(path.parent().unwrap_or(Path::new(".")), false)?;
    let name = sync_name(
        path.file_name()
            .ok_or_else(|| io::Error::other("missing manifest name"))?,
    )?;
    let mut temporary = parent.temporary()?;
    let entries: Vec<&SavedEntry> = manifest.values().collect();
    {
        let mut writer = io::BufWriter::with_capacity(64 * 1024, &mut temporary.file);
        serde_json::to_writer(&mut writer, &entries)?;
        writer.flush()?;
    }
    temporary.persist(&name)
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
            Some(guest) => {
                meta.is_file()
                    && meta.len() == entry.size
                    && mtime(&meta) == guest.mtime
                    && meta.mode() & 0o777 == guest.mode
                    && file_digest(&path)? == guest.sha256
            }
            None => false,
        },
        EntryKind::Dir => meta.is_dir() && meta.mode() & 0o7777 == ((entry.mode & 0o1777) | 0o700),
        EntryKind::Link => {
            meta.file_type().is_symlink()
                && entry.target.as_deref().is_some_and(|target| {
                    fs::read_link(&path)
                        .is_ok_and(|actual| actual.as_os_str() == std::ffi::OsStr::new(target))
                })
        }
    })
}

fn advertised_manifest(root: &Path, manifest: &BTreeMap<String, SavedEntry>) -> Vec<Entry> {
    let mut invalid_dirs = HashSet::new();
    manifest
        .values()
        .map(|saved| {
            let entry = &saved.host;
            let invalid_parent = entry
                .path
                .match_indices('/')
                .any(|(index, _)| invalid_dirs.contains(&entry.path[..index]));
            if !invalid_parent && matches_guest(root, saved).unwrap_or(false) {
                return entry.clone();
            }
            // The host deletes a mismatched directory recursively. Its children
            // must be resent too, even if their own content is still intact.
            if entry.kind == EntryKind::Dir {
                invalid_dirs.insert(entry.path.clone());
            }
            // Keep the path so the host can still delete it if it vanished there. A
            // target-less link cannot match any scanned host entry, forcing resend
            // even for directories (whose metadata is ignored by diff).
            Entry {
                path: entry.path.clone(),
                kind: EntryKind::Link,
                size: 0,
                mtime: 0,
                mode: 0,
                target: None,
            }
        })
        .collect()
}

#[cfg(test)]
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

fn sync_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"))
}

fn sync_check(result: libc::c_int) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// All sync mutations are relative to held directory descriptors. A builder can
/// replace a checked ancestor while a file is arriving; no later syscall may
/// resolve that ancestor again, or follow a symlink at any directory component.
struct SyncDir(File);

impl SyncDir {
    fn open(path: &Path, create: bool) -> io::Result<Self> {
        let mut dir = Self(
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(if path.is_absolute() { "/" } else { "." })?,
        );
        for part in path.components() {
            match part {
                Component::Normal(name) => dir = dir.child(&sync_name(name)?, create)?,
                Component::RootDir | Component::CurDir => {}
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid sync directory",
                    ))
                }
            }
        }
        Ok(dir)
    }

    fn child(&self, name: &CStr, create: bool) -> io::Result<Self> {
        let open = || unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        let mut fd = open();
        if fd < 0 && create && io::Error::last_os_error().kind() == io::ErrorKind::NotFound {
            let result = unsafe { libc::mkdirat(self.0.as_raw_fd(), name.as_ptr(), 0o755) };
            if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists {
                return Err(io::Error::last_os_error());
            }
            fd = open();
        }
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(unsafe { File::from_raw_fd(fd) }))
    }

    fn parent(&self, path: &str, create: bool) -> io::Result<(Self, CString)> {
        let mut parts = path.rsplitn(2, '/');
        let name = sync_name(OsStr::new(parts.next().unwrap()))?;
        let mut dir = Self(self.0.try_clone()?);
        if let Some(parent) = parts.next() {
            for part in parent.split('/') {
                dir = dir.child(&sync_name(OsStr::new(part))?, create)?;
            }
        }
        Ok((dir, name))
    }

    fn is_dir(&self, name: &CStr) -> io::Result<bool> {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let result = unsafe {
            libc::fstatat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::NotFound {
                Ok(false)
            } else {
                Err(error)
            };
        }
        Ok(unsafe { stat.assume_init() }.st_mode & libc::S_IFMT == libc::S_IFDIR)
    }

    fn remove(&self, name: &CStr) -> io::Result<()> {
        match self.child(name, false) {
            Ok(dir) => {
                // fdopendir takes ownership. Keep a separate descriptor for the
                // recursive unlinkat calls, even if a builder renames this dir.
                let fd = dir.0.try_clone()?.into_raw_fd();
                let entries = unsafe { libc::fdopendir(fd) };
                if entries.is_null() {
                    let error = io::Error::last_os_error();
                    unsafe { libc::close(fd) };
                    return Err(error);
                }
                struct Entries(*mut libc::DIR);
                impl Drop for Entries {
                    fn drop(&mut self) {
                        unsafe { libc::closedir(self.0) };
                    }
                }
                let entries = Entries(entries);
                loop {
                    let entry = unsafe { libc::readdir(entries.0) };
                    if entry.is_null() {
                        break;
                    }
                    let child = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
                    if child.to_bytes() != b"." && child.to_bytes() != b".." {
                        dir.remove(child)?;
                    }
                }
                // This also reports ENOTEMPTY if enumeration failed or a builder
                // added another entry. No recursive operation follows symlinks.
                sync_check(unsafe {
                    libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR)
                })
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) if matches!(error.raw_os_error(), Some(libc::ENOTDIR | libc::ELOOP)) => {
                sync_check(unsafe { libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), 0) })
            }
            Err(error) => Err(error),
        }
    }

    fn temporary(&self) -> io::Result<SyncTemporary<'_>> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        loop {
            let name = CString::new(format!(
                ".gzm-sync-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ))
            .unwrap();
            let fd = unsafe {
                libc::openat(
                    self.0.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600 as libc::c_uint,
                )
            };
            if fd >= 0 {
                return Ok(SyncTemporary {
                    parent: self,
                    name,
                    file: unsafe { File::from_raw_fd(fd) },
                    installed: false,
                });
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
    }
}

struct SyncTemporary<'a> {
    parent: &'a SyncDir,
    name: CString,
    file: File,
    installed: bool,
}

impl SyncTemporary<'_> {
    fn persist(mut self, name: &CStr) -> io::Result<()> {
        sync_check(unsafe {
            libc::renameat(
                self.parent.0.as_raw_fd(),
                self.name.as_ptr(),
                self.parent.0.as_raw_fd(),
                name.as_ptr(),
            )
        })?;
        self.installed = true;
        Ok(())
    }
}

impl Drop for SyncTemporary<'_> {
    fn drop(&mut self) {
        if !self.installed {
            unsafe { libc::unlinkat(self.parent.0.as_raw_fd(), self.name.as_ptr(), 0) };
        }
    }
}

fn sync_chown(file: &File) -> io::Result<()> {
    sync_check(unsafe { libc::fchown(file.as_raw_fd(), BUILDER_UID, BUILDER_UID) })
}

fn forget_manifest_tree(manifest: &mut BTreeMap<String, SavedEntry>, path: &str) {
    manifest.remove(path);
    let prefix = format!("{path}/");
    let descendants: Vec<_> = manifest
        .range(prefix.clone()..)
        .take_while(|(key, _)| key.starts_with(&prefix))
        .map(|(key, _)| key.clone())
        .collect();
    for key in descendants {
        manifest.remove(&key);
    }
}

/// Applies the host's changes on top of the recorded manifest.
fn sync(stream: &mut UnixStream, sink: &Sink, job: &GuestJob, root: &Path) -> Result<(), String> {
    let path = Path::new(MANIFESTS).join(format!("{}.json", job.workspace));
    sync_workspace(stream, sink, root, &path)
}

fn sync_workspace(
    stream: &mut UnixStream,
    sink: &Sink,
    root: &Path,
    manifest_path: &Path,
) -> Result<(), String> {
    let _ = command::send_json(
        sink,
        &GuestMessage::Phase {
            phase: "syncing".into(),
        },
    );
    let directory = SyncDir::open(root, true).map_err(|e| e.to_string())?;
    sync_chown(&directory.0).map_err(|e| e.to_string())?;
    let mut manifest = load_manifest(manifest_path);
    let entries = advertised_manifest(root, &manifest);
    let envelope = GuestMessage::Manifest {
        entries: vec![],
        done: false,
    };
    let chunks =
        protocol::json_chunks(&entries, MANIFEST_CHUNK, &envelope).map_err(|e| e.to_string())?;
    for (index, chunk) in chunks.iter().enumerate() {
        let done = index + 1 == chunks.len();
        command::send_json(
            sink,
            &GuestMessage::Manifest {
                entries: chunk.to_vec(),
                done,
            },
        )
        .map_err(|e| e.to_string())?;
    }
    let (mut files, mut bytes) = (0u64, 0u64);
    loop {
        match read_json(stream)? {
            HostMessage::Delete { paths } => {
                for path in paths {
                    if !protocol::valid_relative(&path) {
                        return Err(format!("invalid path {path}"));
                    }
                    let result = match directory.parent(&path, false) {
                        Ok((parent, name)) => parent.remove(&name),
                        // A missing or retyped parent makes this leaf absent in
                        // the held, no-follow directory tree. Never resolve links
                        // to delete a namesake outside the workspace.
                        Err(error)
                            if error.kind() == io::ErrorKind::NotFound
                                || matches!(
                                    error.raw_os_error(),
                                    Some(libc::ENOTDIR | libc::ELOOP)
                                ) =>
                        {
                            Ok(())
                        }
                        Err(error) => Err(error),
                    };
                    result.map_err(|e| format!("delete {path}: {e}"))?;
                    forget_manifest_tree(&mut manifest, &path);
                }
            }
            HostMessage::Put { entry } => {
                if !protocol::valid_relative(&entry.path) {
                    return Err(format!("invalid path {}", entry.path));
                }
                let (parent, name) = directory
                    .parent(&entry.path, true)
                    .map_err(|e| format!("put {}: {e}", entry.path))?;
                let guest = match entry.kind {
                    EntryKind::Dir => {
                        let dir = match parent.child(&name, true) {
                            Ok(dir) => dir,
                            Err(error)
                                if matches!(
                                    error.raw_os_error(),
                                    Some(libc::ENOTDIR | libc::ELOOP)
                                ) =>
                            {
                                parent.remove(&name).map_err(|e| e.to_string())?;
                                parent.child(&name, true).map_err(|e| e.to_string())?
                            }
                            Err(error) => return Err(error.to_string()),
                        };
                        dir.0
                            .set_permissions(fs::Permissions::from_mode(
                                (entry.mode & 0o1777) | 0o700,
                            ))
                            .map_err(|e| e.to_string())?;
                        sync_chown(&dir.0).map_err(|e| e.to_string())?;
                        None
                    }
                    EntryKind::Link => {
                        let link = CString::new(
                            entry.target.as_deref().ok_or("symlink without a target")?,
                        )
                        .map_err(|e| e.to_string())?;
                        parent.remove(&name).map_err(|e| e.to_string())?;
                        sync_check(unsafe {
                            libc::symlinkat(link.as_ptr(), parent.0.as_raw_fd(), name.as_ptr())
                        })
                        .map_err(|e| e.to_string())?;
                        sync_check(unsafe {
                            libc::fchownat(
                                parent.0.as_raw_fd(),
                                name.as_ptr(),
                                BUILDER_UID,
                                BUILDER_UID,
                                libc::AT_SYMLINK_NOFOLLOW,
                            )
                        })
                        .map_err(|e| e.to_string())?;
                        None
                    }
                    EntryKind::File => {
                        let mut temporary = parent.temporary().map_err(|e| e.to_string())?;
                        let mut digest = Sha256::new();
                        let mut left = entry.size;
                        while left > 0 {
                            match protocol::read_frame(stream).map_err(|e| e.to_string())? {
                                Some((protocol::DATA, data)) if data.len() as u64 <= left => {
                                    temporary.file.write_all(&data).map_err(|e| e.to_string())?;
                                    digest.update(&data);
                                    left -= data.len() as u64;
                                }
                                _ => return Err(format!("incomplete data for {}", entry.path)),
                            }
                        }
                        temporary
                            .file
                            .set_permissions(fs::Permissions::from_mode(
                                (entry.mode & 0o777) | 0o600,
                            ))
                            .map_err(|e| e.to_string())?;
                        let times = [
                            libc::timespec {
                                tv_sec: 0,
                                tv_nsec: libc::UTIME_OMIT,
                            },
                            libc::timespec {
                                tv_sec: entry.mtime.div_euclid(1_000_000_000),
                                tv_nsec: entry.mtime.rem_euclid(1_000_000_000),
                            },
                        ];
                        sync_check(unsafe {
                            libc::futimens(temporary.file.as_raw_fd(), times.as_ptr())
                        })
                        .map_err(|e| e.to_string())?;
                        sync_chown(&temporary.file).map_err(|e| e.to_string())?;
                        let meta = temporary.file.metadata().map_err(|e| e.to_string())?;
                        if parent.is_dir(&name).map_err(|e| e.to_string())? {
                            parent.remove(&name).map_err(|e| e.to_string())?;
                        }
                        temporary.persist(&name).map_err(|e| e.to_string())?;
                        files += 1;
                        bytes += entry.size;
                        Some(GuestFile {
                            mtime: mtime(&meta),
                            mode: meta.mode() & 0o777,
                            sha256: digest.finalize().into(),
                        })
                    }
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
    // The managed bin comes first; ccache wrappers and the system follow.
    let path = config
        .path
        .iter()
        .map(String::as_str)
        .chain([base.as_str()])
        .collect::<Vec<_>>()
        .join(":");
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
    env.extend(config.env.iter().cloned());
    env.extend(job.env.iter().cloned());
    env
}

fn run_job(
    mut stream: UnixStream,
    sink: &Sink,
    job: GuestJob,
    config: &BootConfig,
) -> Result<(), String> {
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
        let (cancel, mut reader) = (
            cancel.clone(),
            stream.try_clone().map_err(|e| e.to_string())?,
        );
        std::thread::spawn(move || loop {
            match protocol::read_frame(&mut reader) {
                Ok(Some((protocol::JSON, payload))) => {
                    match protocol::parse::<HostMessage>(&payload) {
                        Ok(HostMessage::Cancel) => cancel.store(true, Ordering::SeqCst),
                        Ok(message) => {
                            if messages_tx.send(message).is_err() {
                                return;
                            }
                        }
                        Err(_) => {}
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => {
                    cancel.store(true, Ordering::SeqCst);
                    return;
                }
            }
        });
    }
    let _ = command::send_json(
        sink,
        &GuestMessage::Phase {
            phase: "running".into(),
        },
    );
    let spec = command::Spec {
        argv: job.argv.clone(),
        cwd,
        env: job_env(&job, config, &root),
        root: None,
        user: Some(BUILDER_UID),
    };
    let outcome = match command::run(
        spec,
        sink,
        &cancel,
        job.timeout_secs.map(Duration::from_secs),
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            command::note(sink, &format!("could not start {}: {error}", job.argv[0]));
            command::Outcome {
                code: Some(127),
                ..Default::default()
            }
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
            HostMessage::Collect {
                patterns,
                max_files,
                max_bytes,
            } => {
                let _ = command::send_json(
                    sink,
                    &GuestMessage::Phase {
                        phase: "collecting".into(),
                    },
                );
                collect(sink, &root, &patterns, max_files, max_bytes, &cancel)?;
            }
            HostMessage::Finish => break,
            _ => {}
        }
    }
    Ok(())
}

// Count directory entries and pattern transitions, including unmatched work.
// File/byte limits alone cannot bound a search through empty trees or aliases.
const MAX_ARTIFACT_SEARCH_WORK: usize = 1_000_000;
const MAX_ARTIFACT_DEPTH: usize = 48;

struct ArtifactSearch<'a> {
    cancel: &'a AtomicBool,
    remaining: usize,
    ancestors: HashSet<(u64, u64)>,
}

impl ArtifactSearch<'_> {
    fn step(&mut self) -> Result<(), String> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or("artifact search exceeds the traversal limit; use narrower patterns")?;
        Ok(())
    }

    fn expand(
        &mut self,
        path: &Path,
        relative: &str,
        parts: &[&str],
        depth: usize,
        visit: &mut dyn FnMut(&str, &Path) -> Result<(), String>,
    ) -> Result<bool, String> {
        self.step()?;
        if depth > MAX_ARTIFACT_DEPTH {
            return Ok(false);
        }
        let Ok(meta) = fs::metadata(path) else {
            return Ok(false);
        };
        if parts.is_empty() {
            if meta.is_file() {
                visit(relative, path)?;
                return Ok(true);
            }
            return Ok(false);
        }
        if !meta.is_dir() {
            return Ok(false);
        }
        // Follow Bazel output symlinks, including those into other host exports.
        // Only active ancestors are cycles: separate aliases remain valid paths.
        let identity = (meta.dev(), meta.ino());
        if !self.ancestors.insert(identity) {
            return Ok(false);
        }
        let result = self.expand_dir(path, relative, parts, depth, visit);
        self.ancestors.remove(&identity);
        result
    }

    fn expand_dir(
        &mut self,
        dir: &Path,
        relative: &str,
        parts: &[&str],
        depth: usize,
        visit: &mut dyn FnMut(&str, &Path) -> Result<(), String>,
    ) -> Result<bool, String> {
        self.step()?;
        if depth > MAX_ARTIFACT_DEPTH {
            return Ok(false);
        }
        let Some((first, rest)) = parts.split_first() else {
            return Ok(false); // this path is a directory, not an artifact
        };
        let join = |name: &str| {
            if relative.is_empty() {
                name.to_string()
            } else {
                format!("{relative}/{name}")
            }
        };
        let recursive = *first == "**";
        // Consuming ** without descending stays in the same directory and must
        // not add that directory to the ancestor set a second time.
        let mut matched = recursive && self.expand_dir(dir, relative, rest, depth + 1, visit)?;
        if !recursive && !first.contains(['*', '?']) {
            return self.expand(&dir.join(first), &join(first), rest, depth + 1, visit);
        }
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries {
                self.step()?;
                let Ok(entry) = entry else { continue };
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                if recursive {
                    if fs::metadata(&path).is_ok_and(|meta| meta.is_dir()) {
                        matched |= self.expand(&path, &join(&name), parts, depth + 1, visit)?;
                    }
                } else if protocol::component_matches(first, &name) {
                    matched |= self.expand(&path, &join(&name), rest, depth + 1, visit)?;
                }
            }
        }
        Ok(matched)
    }
}

fn collect(
    sink: &Sink,
    root: &Path,
    patterns: &[String],
    max_files: u32,
    max_bytes: u64,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut search = ArtifactSearch {
        cancel,
        remaining: MAX_ARTIFACT_SEARCH_WORK,
        ancestors: HashSet::new(),
    };
    let (mut seen, mut missing, mut files, mut bytes) = (HashSet::new(), Vec::new(), 0u32, 0u64);
    let mut buffer = vec![0u8; DATA_CHUNK];
    for pattern in patterns {
        search.step()?;
        if !protocol::valid_pattern(pattern) {
            missing.push(pattern.clone());
            continue;
        }
        let parts: Vec<&str> = pattern.split('/').collect();
        let matched = search.expand(root, "", &parts, 0, &mut |relative, path| {
            if seen.contains(relative) {
                return Ok(());
            }
            let mut file = File::open(path).map_err(|e| format!("{relative}: {e}"))?;
            let meta = file.metadata().map_err(|e| format!("{relative}: {e}"))?;
            if !meta.is_file() {
                return Err(format!("{relative}: artifact is no longer a regular file"));
            }
            if files >= max_files || meta.len() > max_bytes.saturating_sub(bytes) {
                return Err(format!(
                    "artifacts exceed the limit of {max_files} files / {max_bytes} bytes"
                ));
            }
            if cancel.load(Ordering::SeqCst) {
                return Err("cancelled".into());
            }
            command::send_json(
                sink,
                &GuestMessage::File {
                    path: relative.to_string(),
                    size: meta.len(),
                    mode: meta.mode() & 0o777,
                },
            )
            .map_err(|e| e.to_string())?;
            // Once File is announced, send its complete body before observing
            // cancellation: the host cannot read a JSON error inside that body.
            let mut left = meta.len();
            while left > 0 {
                let want = left.min(buffer.len() as u64) as usize;
                file.read_exact(&mut buffer[..want])
                    .map_err(|e| format!("{relative}: {e}"))?;
                command::send(sink, protocol::DATA, &buffer[..want]).map_err(|e| e.to_string())?;
                left -= want as u64;
            }
            seen.insert(relative.to_string());
            files += 1;
            bytes += meta.len();
            Ok(())
        })?;
        if !matched {
            missing.push(pattern.clone());
        }
    }
    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled".into());
    }
    command::send_json(
        sink,
        &GuestMessage::Collected {
            files,
            bytes,
            missing,
        },
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Workspace {
        path: PathBuf,
        root: PathBuf,
        manifest: PathBuf,
    }

    impl Workspace {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "gzm-sync-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self {
                root: path.join("src"),
                manifest: path.join("manifest.json"),
                path,
            }
        }

        // The agent runs as root in Linux. Exercise its actual socket protocol
        // and ownership changes in that environment, without mounting /work.
        fn exchange(&self, send: impl FnOnce(&mut UnixStream)) -> (Vec<Entry>, Result<(), String>) {
            let (mut host, mut guest) = UnixStream::pair().unwrap();
            let sink = Arc::new(Mutex::new(guest.try_clone().unwrap()));
            let (root, manifest) = (self.root.clone(), self.manifest.clone());
            let task =
                std::thread::spawn(move || sync_workspace(&mut guest, &sink, &root, &manifest));
            let mut advertised = Vec::new();
            loop {
                let (kind, bytes) = protocol::read_frame(&mut host).unwrap().unwrap();
                assert_eq!(kind, protocol::JSON);
                match protocol::parse::<GuestMessage>(&bytes).unwrap() {
                    GuestMessage::Phase { .. } => {}
                    GuestMessage::Manifest { entries, done } => {
                        advertised.extend(entries);
                        if done {
                            break;
                        }
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
        fn drop(&mut self) {
            let _ = remove_any(&self.path);
        }
    }

    fn source(path: &str, kind: EntryKind) -> Entry {
        Entry {
            path: path.into(),
            kind,
            size: 4,
            mtime: 1_700_000_000_123_456_789,
            mode: 0o444,
            target: None,
        }
    }

    fn put(stream: &mut UnixStream, entry: &Entry, bytes: &[u8]) {
        protocol::write_json(
            stream,
            &HostMessage::Put {
                entry: entry.clone(),
            },
        )
        .unwrap();
        if entry.kind == EntryKind::File {
            // Deliberately different DATA and verification read boundaries.
            for chunk in bytes.chunks(3) {
                protocol::write_frame(stream, protocol::DATA, chunk).unwrap();
            }
        }
    }

    fn done(stream: &mut UnixStream) {
        protocol::write_json(stream, &HostMessage::SyncDone).unwrap();
    }

    fn collect_artifacts(
        root: &Path,
        patterns: &[&str],
        max_files: u32,
        max_bytes: u64,
        cancel: Arc<AtomicBool>,
    ) -> (Vec<GuestMessage>, Result<(), String>) {
        let (mut host, guest) = UnixStream::pair().unwrap();
        let sink = Arc::new(Mutex::new(guest));
        let root = root.to_path_buf();
        let patterns: Vec<_> = patterns.iter().map(|pattern| pattern.to_string()).collect();
        let task = std::thread::spawn(move || {
            collect(&sink, &root, &patterns, max_files, max_bytes, &cancel)
        });
        let (mut messages, mut left) = (Vec::new(), 0);
        while let Some((kind, payload)) = protocol::read_frame(&mut host).unwrap() {
            match kind {
                protocol::JSON => {
                    assert_eq!(left, 0, "JSON interrupted an artifact body");
                    let message = protocol::parse::<GuestMessage>(&payload).unwrap();
                    if let GuestMessage::File { size, .. } = &message {
                        left = *size;
                    }
                    messages.push(message);
                }
                protocol::DATA => {
                    left = left.checked_sub(payload.len() as u64).unwrap();
                }
                other => panic!("unexpected frame kind {other}"),
            }
        }
        assert_eq!(left, 0, "artifact body was truncated");
        (messages, task.join().unwrap())
    }

    #[test]
    fn artifact_search_stops_cycles_and_preserves_external_output_aliases() {
        let workspace = Workspace::new();
        fs::create_dir(&workspace.root).unwrap();
        let outside = workspace.path.join("outside");
        fs::create_dir_all(outside.join("nested")).unwrap();
        fs::write(workspace.root.join("local.bin"), b"local").unwrap();
        fs::write(outside.join("nested/firmware.bin"), b"firmware").unwrap();
        for name in ["self-one", "self-two"] {
            symlink(".", workspace.root.join(name)).unwrap();
        }
        for name in ["bazel-bin", "other-output"] {
            symlink(&outside, workspace.root.join(name)).unwrap();
        }
        symlink(&outside, outside.join("nested/again")).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut search = ArtifactSearch {
            cancel: &cancel,
            remaining: 500,
            ancestors: HashSet::new(),
        };
        let mut found = BTreeMap::new();
        search
            .expand(
                &workspace.root,
                "",
                &["**", "*.bin"],
                0,
                &mut |name, path| {
                    found.insert(name.to_string(), fs::read(path).unwrap());
                    Ok(())
                },
            )
            .unwrap();
        assert!(search.ancestors.is_empty());
        assert_eq!(
            found.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "bazel-bin/nested/firmware.bin",
                "local.bin",
                "other-output/nested/firmware.bin"
            ]
        );
        assert_eq!(found["bazel-bin/nested/firmware.bin"], b"firmware");
        let (messages, result) = collect_artifacts(
            &workspace.root,
            &["**/*.bin", "bazel-bin/**/*.bin", "absent/*.bin"],
            3,
            21,
            cancel,
        );
        result.unwrap();
        assert_eq!(messages.len(), 4, "overlapping patterns duplicated files");
        assert!(matches!(
            messages.last().unwrap(),
            GuestMessage::Collected { files: 3, bytes: 21, missing }
                if missing == &["absent/*.bin"]
        ));
    }

    #[test]
    fn artifact_search_bounds_unmatched_work_and_observes_cancellation() {
        let workspace = Workspace::new();
        fs::create_dir(&workspace.root).unwrap();
        for index in 0..32 {
            fs::write(workspace.root.join(format!("{index}.bin")), b"data").unwrap();
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let mut search = ArtifactSearch {
            cancel: &cancel,
            remaining: 8,
            ancestors: HashSet::new(),
        };
        let mut visits = 0;
        let error = search
            .expand(&workspace.root, "", &["*.missing"], 0, &mut |_, _| {
                visits += 1;
                Ok(())
            })
            .unwrap_err();
        assert!(error.contains("traversal limit"));
        assert_eq!(visits, 0);
        assert!(search.ancestors.is_empty());
        search.remaining = MAX_ARTIFACT_SEARCH_WORK;
        assert_eq!(
            search.expand(&workspace.root, "", &["*.bin"], 0, &mut |_, _| {
                visits += 1;
                cancel.store(true, Ordering::SeqCst);
                Ok(())
            }),
            Err("cancelled".into())
        );
        assert_eq!(visits, 1);
        assert!(search.ancestors.is_empty());
        let (messages, result) = collect_artifacts(&workspace.root, &["*.bin"], 32, 128, cancel);
        assert!(messages.is_empty());
        assert_eq!(result, Err("cancelled".into()));
    }

    #[test]
    fn artifacts_enforce_file_and_byte_limits_while_streaming() {
        let workspace = Workspace::new();
        fs::create_dir(&workspace.root).unwrap();
        for name in ["one.bin", "two.bin", "three.bin"] {
            fs::write(workspace.root.join(name), b"data").unwrap();
        }
        for (max_files, max_bytes, sent) in [(1, u64::MAX, 1), (u32::MAX, 3, 0), (0, 12, 0)] {
            let (messages, result) = collect_artifacts(
                &workspace.root,
                &["*.bin"],
                max_files,
                max_bytes,
                Arc::new(AtomicBool::new(false)),
            );
            assert!(result.unwrap_err().contains("artifacts exceed the limit"));
            assert_eq!(messages.len(), sent);
            assert!(messages
                .iter()
                .all(|message| matches!(message, GuestMessage::File { size: 4, .. })));
        }
    }

    #[test]
    fn artifacts_complete_announced_body_before_cancelling() {
        let workspace = Workspace::new();
        fs::create_dir(&workspace.root).unwrap();
        let body = vec![0x5a; DATA_CHUNK * 2 + 7];
        fs::write(workspace.root.join("output.bin"), &body).unwrap();
        let (mut host, guest) = UnixStream::pair().unwrap();
        let sink = Arc::new(Mutex::new(guest));
        let cancel = Arc::new(AtomicBool::new(false));
        let (root, flag) = (workspace.root.clone(), cancel.clone());
        let task = std::thread::spawn(move || {
            collect(&sink, &root, &["output.bin".into()], 1, u64::MAX, &flag)
        });
        let (kind, payload) = protocol::read_frame(&mut host).unwrap().unwrap();
        assert_eq!(kind, protocol::JSON);
        assert!(
            matches!(protocol::parse::<GuestMessage>(&payload).unwrap(), GuestMessage::File { size, .. } if size == body.len() as u64)
        );
        cancel.store(true, Ordering::SeqCst);
        let mut received = Vec::new();
        while let Some((kind, payload)) = protocol::read_frame(&mut host).unwrap() {
            assert_eq!(
                kind,
                protocol::DATA,
                "cancellation interrupted the file body"
            );
            received.extend(payload);
        }
        assert_eq!(received, body);
        assert_eq!(task.join().unwrap(), Err("cancelled".into()));
    }

    #[test]
    fn sync_delete_below_retyped_parent_is_absent_and_forgets_manifest_descendants() {
        for replacement in ["missing", "file", "link", "dangling-link", "self-link"] {
            let workspace = Workspace::new();
            let originals = [
                source("parent", EntryKind::Dir),
                source("parent/child", EntryKind::Dir),
                source("parent/child/nested", EntryKind::File),
                source("parent/child-other", EntryKind::File),
            ];
            workspace
                .exchange(|stream| {
                    for entry in &originals {
                        put(stream, entry, b"orig");
                    }
                    done(stream);
                })
                .1
                .unwrap();
            let outside = workspace.path.join("outside");
            fs::create_dir_all(outside.join("child")).unwrap();
            fs::write(outside.join("child/nested"), b"keep").unwrap();
            let parent = workspace.root.join("parent");
            remove_any(&parent).unwrap();
            match replacement {
                "file" => fs::write(&parent, b"keep parent").unwrap(),
                "link" => symlink(&outside, &parent).unwrap(),
                "dangling-link" => symlink("missing", &parent).unwrap(),
                "self-link" => symlink("parent", &parent).unwrap(),
                _ => {}
            }
            workspace
                .exchange(|stream| {
                    protocol::write_json(
                        stream,
                        &HostMessage::Delete {
                            paths: vec!["parent/child".into()],
                        },
                    )
                    .unwrap();
                    done(stream);
                })
                .1
                .unwrap();
            let saved = load_manifest(&workspace.manifest);
            assert_eq!(
                saved.keys().map(String::as_str).collect::<Vec<_>>(),
                ["parent", "parent/child-other"],
                "{replacement}"
            );
            assert_eq!(fs::read(outside.join("child/nested")).unwrap(), b"keep");
            if replacement == "file" {
                assert_eq!(fs::read(&parent).unwrap(), b"keep parent");
            } else if replacement.ends_with("link") {
                assert!(fs::symlink_metadata(&parent)
                    .unwrap()
                    .file_type()
                    .is_symlink());
            }
        }
    }

    #[test]
    fn sync_normalizes_directory_special_bits_and_preserves_host_identity() {
        let workspace = Workspace::new();
        let originals: Vec<_> = [0o2775, 0o4775, 0o6775, 0o1775, 0o7444]
            .into_iter()
            .enumerate()
            .map(|(index, mode)| Entry {
                mode,
                ..source(&format!("dir-{index}"), EntryKind::Dir)
            })
            .collect();
        workspace
            .exchange(|stream| {
                for entry in &originals {
                    put(stream, entry, &[]);
                }
                done(stream);
            })
            .1
            .unwrap();
        for entry in &originals {
            let mode = fs::metadata(workspace.root.join(&entry.path))
                .unwrap()
                .mode()
                & 0o7777;
            assert_eq!(mode, (entry.mode & 0o1777) | 0o700);
        }
        let (advertised, result) = workspace.exchange(done);
        result.unwrap();
        assert_eq!(advertised, originals);
    }

    #[test]
    fn manifest_many_deletes_preserve_siblings_and_remove_only_actual_descendants() {
        let mut manifest = BTreeMap::new();
        for index in 0..10_000 {
            for suffix in ["", "/nested/file", "-sibling", ".sibling", "0"] {
                let path = format!("tree-{index:05}{suffix}");
                manifest.insert(
                    path.clone(),
                    SavedEntry {
                        host: source(&path, EntryKind::File),
                        guest: None,
                    },
                );
            }
        }
        for index in 0..10_000 {
            forget_manifest_tree(&mut manifest, &format!("tree-{index:05}"));
        }
        assert_eq!(manifest.len(), 30_000);
        assert!(manifest.keys().all(|path| ["-sibling", ".sibling", "0"]
            .iter()
            .any(|suffix| path.ends_with(suffix))));
    }

    #[test]
    fn sync_does_not_follow_preexisting_temporary_symlink() {
        let workspace = Workspace::new();
        fs::create_dir(&workspace.root).unwrap();
        let outside = workspace.path.join("outside");
        fs::write(&outside, b"keep").unwrap();
        let planted = workspace
            .root
            .join(format!(".gzm-sync-{}-1", std::process::id()));
        symlink(&outside, &planted).unwrap();
        let result = workspace
            .exchange(|stream| {
                put(stream, &source("source", EntryKind::File), b"safe");
                done(stream);
            })
            .1;
        assert_eq!(
            fs::read(&outside).unwrap(),
            b"keep",
            "temporary symlink was followed"
        );
        result.unwrap();
        assert_eq!(fs::read_link(planted).unwrap(), outside);
        assert_eq!(fs::read(workspace.root.join("source")).unwrap(), b"safe");
    }

    #[test]
    fn sync_rejects_symlink_ancestors_for_mutations() {
        for operation in ["file", "dir", "link", "mkdir-parent"] {
            let workspace = Workspace::new();
            fs::create_dir(&workspace.root).unwrap();
            let outside = workspace.path.join("outside");
            fs::create_dir_all(outside.join("sub")).unwrap();
            fs::write(outside.join("source"), b"keep").unwrap();
            fs::write(outside.join("sub/keep"), b"keep").unwrap();
            symlink(&outside, workspace.root.join("pivot")).unwrap();
            let result = workspace
                .exchange(|stream| {
                    let mut messages = Vec::new();
                    let mut entry = source("pivot/source", EntryKind::File);
                    match operation {
                        "dir" => entry.kind = EntryKind::Dir,
                        "link" => {
                            entry.kind = EntryKind::Link;
                            entry.target = Some("elsewhere".into());
                        }
                        "mkdir-parent" => entry.path = "pivot/new/source".into(),
                        _ => {}
                    }
                    protocol::write_json(
                        &mut messages,
                        &HostMessage::Put {
                            entry: entry.clone(),
                        },
                    )
                    .unwrap();
                    if entry.kind == EntryKind::File {
                        protocol::write_frame(&mut messages, protocol::DATA, b"evil").unwrap();
                    }
                    protocol::write_json(&mut messages, &HostMessage::SyncDone).unwrap();
                    stream.write_all(&messages).unwrap();
                })
                .1;
            assert!(result.is_err(), "{operation} traversed a symlink ancestor");
            assert_eq!(
                fs::read(outside.join("source")).unwrap(),
                b"keep",
                "{operation}"
            );
            assert_eq!(
                fs::read(outside.join("sub/keep")).unwrap(),
                b"keep",
                "{operation}"
            );
            assert!(!outside.join("new").exists(), "{operation}");
        }
    }

    #[test]
    fn sync_does_not_follow_manifest_temporary_symlink() {
        let workspace = Workspace::new();
        let outside = workspace.path.join("outside");
        fs::write(&outside, b"keep").unwrap();
        let planted = workspace.manifest.with_extension("json.tmp");
        symlink(&outside, &planted).unwrap();
        let result = workspace.exchange(done).1;
        assert_eq!(fs::read(&outside).unwrap(), b"keep");
        result.unwrap();
        assert_eq!(fs::read_link(planted).unwrap(), outside);
    }

    #[test]
    fn sync_rejects_symlink_workspace_root() {
        let workspace = Workspace::new();
        let outside = workspace.path.join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, &workspace.root).unwrap();
        let (mut host, mut guest) = UnixStream::pair().unwrap();
        done(&mut host);
        let sink = Arc::new(Mutex::new(guest.try_clone().unwrap()));
        let result = sync_workspace(&mut guest, &sink, &workspace.root, &workspace.manifest);
        assert!(result.is_err(), "sync accepted a symlink workspace root");
        assert!(!workspace.manifest.exists());
    }

    #[test]
    fn sync_holds_parent_descriptor_while_file_data_arrives() {
        let workspace = Workspace::new();
        fs::create_dir_all(workspace.root.join("parent")).unwrap();
        let outside = workspace.path.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("source"), b"keep").unwrap();
        let mut before = None;
        let result = workspace.exchange(|stream| {
            protocol::write_json(
                stream,
                &HostMessage::Put {
                    entry: source("parent/source", EntryKind::File),
                },
            )
            .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            let temporary = loop {
                if let Some(name) = fs::read_dir(workspace.root.join("parent"))
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name())
                    .find(|name| name.as_bytes().starts_with(b".gzm-sync-"))
                {
                    break name;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "sync did not open its temporary file"
                );
                std::thread::sleep(Duration::from_millis(5));
            };
            fs::rename(workspace.root.join("parent"), workspace.root.join("held")).unwrap();
            symlink(&outside, workspace.root.join("parent")).unwrap();
            let decoy = outside.join(&temporary);
            fs::write(&decoy, b"trap").unwrap();
            fs::set_permissions(&decoy, fs::Permissions::from_mode(0o400)).unwrap();
            before = Some(fs::metadata(&decoy).unwrap());
            protocol::write_frame(stream, protocol::DATA, b"safe").unwrap();
            done(stream);
        });
        result.1.unwrap();
        assert_eq!(fs::read(outside.join("source")).unwrap(), b"keep");
        let decoy = fs::read_dir(&outside)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .as_bytes()
                    .starts_with(b".gzm-sync-")
            })
            .unwrap();
        assert_eq!(fs::read(&decoy).unwrap(), b"trap");
        let (before, after) = (before.unwrap(), fs::metadata(decoy).unwrap());
        assert_eq!(after.mode(), before.mode());
        assert_eq!(after.uid(), before.uid());
        assert_eq!(after.gid(), before.gid());
        assert_eq!(mtime(&after), mtime(&before));
        assert_eq!(
            fs::read(workspace.root.join("held/source")).unwrap(),
            b"safe"
        );
    }

    #[test]
    fn sync_rejects_symlink_manifest_ancestor() {
        let mut workspace = Workspace::new();
        let outside = workspace.path.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("manifest.json"), b"keep").unwrap();
        symlink(&outside, workspace.path.join("state")).unwrap();
        workspace.manifest = workspace.path.join("state/manifest.json");
        assert!(workspace.exchange(done).1.is_err());
        assert_eq!(fs::read(outside.join("manifest.json")).unwrap(), b"keep");
        assert!(!outside.join("manifest.json.tmp").exists());
    }

    #[test]
    fn sync_replaces_leaf_symlinks_and_deletes_trees_without_following_links() {
        for kind in [EntryKind::File, EntryKind::Dir, EntryKind::Link] {
            let workspace = Workspace::new();
            fs::create_dir(&workspace.root).unwrap();
            let outside = workspace.path.join("outside");
            fs::write(&outside, b"keep").unwrap();
            symlink(&outside, workspace.root.join("target")).unwrap();
            let entry = Entry {
                target: Some("somewhere".into()),
                ..source("target", kind)
            };
            workspace
                .exchange(|stream| {
                    put(stream, &entry, b"safe");
                    done(stream);
                })
                .1
                .unwrap();
            assert_eq!(fs::read(outside).unwrap(), b"keep");
            let meta = fs::symlink_metadata(workspace.root.join("target")).unwrap();
            match kind {
                EntryKind::File => assert!(meta.is_file()),
                EntryKind::Dir => assert!(meta.is_dir()),
                EntryKind::Link => assert!(meta.file_type().is_symlink()),
            }
        }
        let workspace = Workspace::new();
        fs::create_dir_all(workspace.root.join("tree/nested")).unwrap();
        let outside = workspace.path.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), b"keep").unwrap();
        symlink(&outside, workspace.root.join("tree/nested/link")).unwrap();
        fs::write(workspace.root.join("tree/nested/file"), b"data").unwrap();
        workspace
            .exchange(|stream| {
                protocol::write_json(
                    stream,
                    &HostMessage::Delete {
                        paths: vec!["tree".into()],
                    },
                )
                .unwrap();
                done(stream);
            })
            .1
            .unwrap();
        assert!(!workspace.root.join("tree").exists());
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"keep");
    }

    #[test]
    fn sync_removes_temporary_file_on_truncated_frame() {
        let workspace = Workspace::new();
        let result = workspace
            .exchange(|stream| {
                protocol::write_json(
                    stream,
                    &HostMessage::Put {
                        entry: source("source", EntryKind::File),
                    },
                )
                .unwrap();
                stream.write_all(&[0, 0]).unwrap();
            })
            .1;
        assert!(result.is_err());
        assert_eq!(fs::read_dir(&workspace.root).unwrap().count(), 0);
        assert!(!workspace.manifest.exists());
    }

    #[test]
    fn sync_revalidates_edited_deleted_and_retyped_entries() {
        let workspace = Workspace::new();
        let file = source("a-file", EntryKind::File);
        let dir = source("b-dir", EntryKind::Dir);
        let link = Entry {
            target: Some("a-file".into()),
            ..source("c-link", EntryKind::Link)
        };
        let child = source("b-dir/child", EntryKind::File);
        let originals = vec![file.clone(), dir.clone(), child.clone(), link.clone()];
        let (advertised, result) = workspace.exchange(|stream| {
            for entry in &originals {
                put(stream, entry, b"orig");
            }
            done(stream);
        });
        result.unwrap();
        assert!(advertised.is_empty());
        let (advertised, result) = workspace.exchange(done);
        result.unwrap();
        assert_eq!(advertised, originals);

        let saved = load_manifest(&workspace.manifest);
        fs::write(workspace.root.join("a-file"), b"edit").unwrap();
        sys::set_mtime(
            &workspace.root.join("a-file"),
            saved["a-file"].guest.as_ref().unwrap().mtime,
        )
        .unwrap();
        fs::set_permissions(
            workspace.root.join("b-dir"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        assert!(matches_guest(&workspace.root, &saved["b-dir/child"]).unwrap());
        remove_any(&workspace.root.join("c-link")).unwrap();
        symlink("./a-file", workspace.root.join("c-link")).unwrap();
        let (advertised, result) = workspace.exchange(done);
        result.unwrap();
        assert_eq!(advertised.len(), 4);
        for (current, original) in advertised.iter().zip(&originals) {
            assert_eq!(current.path, original.path);
            assert!(
                !current.same_content(original),
                "{} was falsely advertised unchanged",
                original.path
            );
        }
        remove_any(&workspace.root.join("a-file")).unwrap();
        remove_any(&workspace.root.join("b-dir")).unwrap();
        symlink("elsewhere", workspace.root.join("b-dir")).unwrap();
        let (advertised, result) = workspace.exchange(|stream| {
            protocol::write_json(
                stream,
                &HostMessage::Delete {
                    paths: vec!["a-file".into(), "b-dir".into()],
                },
            )
            .unwrap();
            for entry in &originals {
                put(stream, entry, b"orig");
            }
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
        workspace
            .exchange(|stream| {
                put(stream, &file, b"orig");
                done(stream);
            })
            .1
            .unwrap();
        let changed = Entry {
            mtime: file.mtime + 1000,
            ..file.clone()
        };
        assert!(workspace
            .exchange(|stream| put(stream, &changed, b"next"))
            .1
            .is_err());
        assert_eq!(fs::read(workspace.root.join("source")).unwrap(), b"next");
        assert_eq!(load_manifest(&workspace.manifest)["source"].host, file);
        let (advertised, result) = workspace.exchange(|stream| {
            put(stream, &file, b"orig");
            done(stream);
        });
        result.unwrap();
        assert!(!advertised[0].same_content(&file));
        assert_eq!(workspace.exchange(done).0, std::slice::from_ref(&file));

        assert!(workspace
            .exchange(|stream| {
                protocol::write_json(
                    stream,
                    &HostMessage::Delete {
                        paths: vec!["source".into()],
                    },
                )
                .unwrap();
            })
            .1
            .is_err());
        let (advertised, result) = workspace.exchange(|stream| {
            put(stream, &file, b"orig");
            done(stream);
        });
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
        fs::write(
            &workspace.manifest,
            serde_json::to_vec(&vec![file.clone()]).unwrap(),
        )
        .unwrap();
        let (advertised, result) = workspace.exchange(|stream| {
            put(stream, &file, b"orig");
            done(stream);
        });
        result.unwrap();
        assert!(!advertised[0].same_content(&file));
        let mut saved = load_manifest(&workspace.manifest);
        // Model a filesystem restoring a different timestamp precision. Its
        // observed stamp is verified, while the original host identity is sent.
        sys::set_mtime(&workspace.root.join("source"), file.mtime + 99).unwrap();
        saved
            .get_mut("source")
            .unwrap()
            .guest
            .as_mut()
            .unwrap()
            .mtime = mtime(&fs::metadata(workspace.root.join("source")).unwrap());
        save_manifest(&workspace.manifest, &saved).unwrap();
        let (advertised, result) = workspace.exchange(done);
        result.unwrap();
        assert_eq!(advertised, [file]);
    }
}
