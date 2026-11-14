//! Worker VMs: a read-only shared system disk under a tmpfs overlay, plus the
//! slot's own persistent `/work` disk (sources, Bazel output base, ccache tier,
//! home). Jobs arrive over vsock; each connection is one job or one probe.
use crate::{
    command::{self, Sink},
    log,
    protocol::{
        self, BootConfig, BootMode, Entry, EntryKind, GuestJob, GuestMessage, HostMessage, BUILDER_UID, CACHE_PORT,
        DATA_CHUNK, MANIFEST_CHUNK,
    },
    provision, sys,
};
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
    process::Command,
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

/// Runs in PID 1 before the agent forks: builds `/newroot` and mounts `/work`.
pub fn prepare(config: &BootConfig) -> io::Result<()> {
    let lower = Path::new("/lower");
    // Disks are vmbus SCSI LUNs in host attachment order.
    let system_disk = sys::scsi_disk(0)?;
    let work_disk = sys::scsi_disk(1)?;
    let (system_disk, work_disk) = (system_disk.to_string_lossy().into_owned(), work_disk.to_string_lossy().into_owned());
    // `noload` skips journal replay: several VMs share this disk read-only.
    sys::mount(&system_disk, lower, "ext4", libc::MS_RDONLY, "noload")?;
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
    sys::mount("tmpfs", &root.join("tmp"), "tmpfs", libc::MS_NOSUID | libc::MS_NODEV, "mode=1777")?;
    sys::mount("tmpfs", &root.join("run"), "tmpfs", libc::MS_NOSUID | libc::MS_NODEV, "mode=0755")?;
    if !sys::has_ext4(Path::new(&work_disk))? {
        log("formatting the slot work disk");
        // The host file is sparse and reads as zeros: skip zeroing inode tables
        // and the journal so a new disk allocates megabytes, not gigabytes.
        let status = Command::new("/usr/sbin/mke2fs")
            .args(["-q", "-t", "ext4", "-L", "gzm-work", "-E", "lazy_itable_init=1,lazy_journal_init=1,nodiscard", "-F", &work_disk])
            .env_clear()
            .env("PATH", BASE_PATH)
            .current_dir("/")
            .chroot_into(root)
            .status()?;
        if !status.success() {
            return Err(io::Error::other(format!("mke2fs {work_disk}: {status}")));
        }
    }
    sys::mount(&work_disk, &root.join("work"), "ext4", libc::MS_NOATIME, "noinit_itable")?;
    for dir in ["home", "src", "src/.manifests", "bazel", "bazel-repo-cache", "ccache", "tmp"] {
        let path = root.join("work").join(dir);
        fs::create_dir_all(&path)?;
        sys::lchown(&path, BUILDER_UID, BUILDER_UID)?;
    }
    for share in &config.shares {
        mount_share(root, share)?;
    }
    for image in &config.images {
        let device = sys::scsi_disk(image.lun)?;
        let target = root.join(image.path.trim_start_matches('/'));
        sys::mount(&device.to_string_lossy(), &target, "erofs", libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV, "")?;
        log(format!("mounted image {} from LUN {}", image.path, image.lun));
    }
    provision::write_resolv(root)?;
    fs::write(root.join("etc/hosts"), "127.0.0.1 localhost gizmoth-build\n::1 localhost\n")?;
    fs::write(root.join("etc/bazel.bazelrc"), bazelrc(config))?;
    fs::write(root.join("etc/ccache.conf"), ccache_conf(config))?;
    Ok(())
}

/// Mounts a host folder read-only over 9P on its own vsock connection: the
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
        "trans=fd,rfdno={fd},wfdno={fd},version=9p2000.L,msize={},cache=loose,access=any",
        protocol::SHARE_MSIZE
    );
    let target = root.join(share.path.trim_start_matches('/'));
    sys::mount("gizmoth", &target, "9p", libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV, &options)?;
    // The kernel keeps its own reference to the connection.
    drop(stream);
    log(format!("mounted share {} from host port {}", share.path, share.port));
    Ok(())
}

trait ChrootExt {
    fn chroot_into(&mut self, root: &Path) -> &mut Self;
}

impl ChrootExt for Command {
    fn chroot_into(&mut self, root: &Path) -> &mut Self {
        use std::os::unix::process::CommandExt;
        let root = std::ffi::CString::new(root.as_os_str().as_encoded_bytes()).unwrap();
        unsafe {
            self.pre_exec(move || {
                if libc::chroot(root.as_ptr()) < 0 || libc::chdir(c"/".as_ptr()) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            })
        }
    }
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
    let config = Arc::new(config.clone());
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

fn load_manifest(key: &str) -> BTreeMap<String, Entry> {
    fs::read(Path::new(MANIFESTS).join(format!("{key}.json")))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<Entry>>(&bytes).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|entry| (entry.path.clone(), entry))
        .collect()
}

fn save_manifest(key: &str, manifest: &BTreeMap<String, Entry>) -> io::Result<()> {
    let path = Path::new(MANIFESTS).join(format!("{key}.json"));
    let temporary = path.with_extension("json.tmp");
    let entries: Vec<&Entry> = manifest.values().collect();
    fs::write(&temporary, serde_json::to_vec(&entries)?)?;
    fs::rename(temporary, path)
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
    let _ = command::send_json(sink, &GuestMessage::Phase { phase: "syncing".into() });
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    sys::lchown(root, BUILDER_UID, BUILDER_UID).map_err(|e| e.to_string())?;
    let mut manifest = load_manifest(&job.workspace);
    let entries: Vec<Entry> = manifest.values().cloned().collect();
    let mut chunks = entries.chunks(MANIFEST_CHUNK).peekable();
    if chunks.peek().is_none() {
        command::send_json(sink, &GuestMessage::Manifest { entries: vec![], done: true }).map_err(|e| e.to_string())?;
    }
    while let Some(chunk) = chunks.next() {
        let done = chunks.peek().is_none();
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
                        let mut left = entry.size;
                        while left > 0 {
                            match protocol::read_frame(stream).map_err(|e| e.to_string())? {
                                Some((protocol::DATA, data)) if data.len() as u64 <= left => {
                                    file.write_all(&data).map_err(|e| e.to_string())?;
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
                        files += 1;
                        bytes += entry.size;
                    }
                }
                sys::lchown(&target, BUILDER_UID, BUILDER_UID).map_err(|e| e.to_string())?;
                manifest.insert(entry.path.clone(), entry);
            }
            HostMessage::SyncDone => break,
            HostMessage::Cancel => return Err("cancelled".into()),
            other => return Err(format!("unexpected {other:?} during sync")),
        }
    }
    save_manifest(&job.workspace, &manifest).map_err(|e| format!("save manifest: {e}"))?;
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
