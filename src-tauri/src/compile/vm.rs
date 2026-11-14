//! One OpenVMM process per running VM. The VM powers itself off when its agent
//! exits; `--guest-shutdown-action exit:0` then ends the OpenVMM process.
use super::{assets, cache, guest, guest::protocol::GuestMessage, p9};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

pub struct Spec {
    /// Short identifier used for socket names.
    pub name: String,
    pub cpus: u32,
    pub memory_mib: u64,
    pub kernel: PathBuf,
    pub initramfs: PathBuf,
    /// Disk images in attachment order (`/dev/vda`, `/dev/vdb`, ...), read-only flag.
    pub disks: Vec<(PathBuf, bool)>,
    pub network: bool,
    pub console: PathBuf,
    pub runtime: PathBuf,
    /// Host folders served read-only over 9P: (guest-to-host vsock port, folder).
    pub shares: Vec<(u32, PathBuf)>,
}

pub struct Vm {
    child: Child,
    pub socket: PathBuf,
    pub console: PathBuf,
    _cache: Option<cache::Server>,
    _shares: Vec<p9::Server>,
    pub started_at: u64,
}

/// A private, short directory for vsock sockets: macOS limits Unix socket
/// paths to 104 bytes, which Application Support paths can exceed.
pub fn runtime_dir() -> Result<PathBuf, String> {
    let uid = unsafe { libc::getuid() };
    let dir = PathBuf::from(format!("/tmp/gizmoth-{uid}"));
    match fs::create_dir(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("{}: {error}", dir.display())),
    }
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = fs::symlink_metadata(&dir).map_err(|e| e.to_string())?;
    if !meta.is_dir() || meta.uid() != uid {
        return Err(format!(
            "{} is not a private directory owned by this user",
            dir.display()
        ));
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    Ok(dir)
}

impl Vm {
    pub fn start(spec: &Spec, cache: Option<Arc<cache::Store>>) -> Result<Vm, String> {
        let openvmm = assets::openvmm()?;
        let socket = spec.runtime.join(format!("{}.vsock", spec.name));
        let _ = fs::remove_file(&socket);
        let cache_server = match cache {
            Some(store) => {
                let path = PathBuf::from(format!(
                    "{}_{}",
                    socket.display(),
                    guest::protocol::CACHE_PORT
                ));
                Some(cache::serve(path, store).map_err(|e| format!("cache socket: {e}"))?)
            }
            None => None,
        };
        let mut shares = Vec::new();
        for (port, folder) in &spec.shares {
            let path = PathBuf::from(format!("{}_{port}", socket.display()));
            shares.push(
                p9::serve(path, folder).map_err(|e| format!("share {}: {e}", folder.display()))?,
            );
        }
        // OpenVMM's Hypervisor.framework backend has no PCI MSI delivery, so
        // the VM uses Hyper-V vmbus devices: storvsc disks (LUNs in attachment
        // order), netvsc and hv_sock. The built-in virtio vsock transport would
        // otherwise claim the guest-to-host vsock slot before hv_sock.
        let mut cmdline = String::from(
            "console=ttyAMA0 loglevel=4 panic=-1 rdinit=/init initcall_blacklist=virtio_vsock_init",
        );
        if spec.network {
            cmdline.push_str(" ip=dhcp");
        }
        let mut args: Vec<String> = vec![
            "--hv".into(),
            "-k".into(),
            spec.kernel.display().to_string(),
            "-r".into(),
            spec.initramfs.display().to_string(),
            "-c".into(),
            cmdline,
            "-m".into(),
            format!("{}M", spec.memory_mib),
            "-p".into(),
            spec.cpus.to_string(),
        ];
        for (disk, read_only) in &spec.disks {
            args.extend([
                "--disk".into(),
                format!(
                    "file:{}{}",
                    disk.display(),
                    if *read_only { ",ro" } else { "" }
                ),
            ]);
        }
        if spec.network {
            args.extend(["--net".into(), "consomme".into()]);
        }
        args.extend([
            "--vmbus-vsock-path".into(),
            socket.display().to_string(),
            "--com1".into(),
            format!("file={}", spec.console.display()),
            "--guest-shutdown-action".into(),
            "exit:0".into(),
            "--guest-reset-action".into(),
            "exit:3".into(),
            "--guest-crash-action".into(),
            "exit:4".into(),
        ]);
        let log =
            File::create(spec.console.with_extension("vmm.log")).map_err(|e| e.to_string())?;
        let child = Command::new(&openvmm)
            .args(&args)
            // OpenVMM's REPL stops the VM at end of input; keep stdin open.
            .stdin(Stdio::piped())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log)
            .env("RUST_LOG", "warn")
            .spawn()
            .map_err(|e| format!("start OpenVMM ({}): {e}", openvmm.display()))?;
        let mut vm = Vm {
            child,
            socket,
            console: spec.console.clone(),
            _cache: cache_server,
            _shares: shares,
            started_at: crate::serial::now_ms(),
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        while !vm.socket.exists() {
            if !vm.alive() || Instant::now() > deadline {
                return Err(vm.failure("OpenVMM did not start"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(vm)
    }

    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn failure(&mut self, what: &str) -> String {
        let status = self.child.try_wait().ok().flatten();
        let mut message = match status {
            Some(status) => format!("{what} (OpenVMM {status})"),
            None => what.to_string(),
        };
        let vmm = guest::console_tail(&self.console.with_extension("vmm.log"), 6);
        let console = guest::console_tail(&self.console, 12);
        for tail in [vmm, console] {
            if !tail.trim().is_empty() {
                message.push('\n');
                message.push_str(tail.trim());
            }
        }
        message
    }

    /// Waits for the agent's hello; returns it.
    pub fn wait_ready(
        &mut self,
        timeout: Duration,
        cancel: &dyn Fn() -> bool,
    ) -> Result<GuestMessage, String> {
        let deadline = Instant::now() + timeout;
        loop {
            if cancel() {
                return Err("cancelled".into());
            }
            if !self.alive() {
                return Err(self.failure("The VM stopped during startup"));
            }
            if let Ok(hello) = guest::hello(&self.socket) {
                return Ok(hello);
            }
            if Instant::now() > deadline {
                return Err(self.failure("The VM's agent did not respond"));
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    /// Asks the agent to power off, then forces the process down.
    pub fn stop(&mut self, grace: Duration) {
        if self.alive() {
            let _ = guest::poweroff(&self.socket);
            let deadline = Instant::now() + grace;
            while self.alive() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        if self.alive() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }

    pub fn wait_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while self.alive() {
            if Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        true
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        if self.alive() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.socket);
        // The cache server closes its listener when `_cache` drops.
    }
}

/// Creates a sparse disk image of `bytes` if missing; never shrinks one.
pub fn ensure_disk(path: &Path, bytes: u64) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let current = file.metadata().map_err(|e| e.to_string())?.len();
    if current < bytes {
        file.set_len(bytes).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Bytes actually allocated by a sparse file.
pub fn allocated(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).map(|m| m.blocks() * 512).unwrap_or(0)
}
