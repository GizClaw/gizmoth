//! One OpenVMM process per running VM. The VM powers itself off when its agent
//! exits; `--guest-shutdown-action exit:0` then ends the OpenVMM process.
use super::{assets, cache, guest, guest::protocol::GuestMessage, p9};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
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
    /// Host folders: (guest-to-host vsock port, folder, writable work export).
    pub shares: Vec<(u32, PathBuf, bool)>,
}

/// Independently stoppable child control, including its lifetime reservation.
/// Shutdown can reap a packer even while its session owns the `Vm` itself.
pub(super) struct Process {
    state: Mutex<ProcessState>,
}

struct ProcessState {
    child: Child,
    allocation: Option<Arc<super::resources::Lease>>,
}

impl Process {
    fn new(child: Child, allocation: Arc<super::resources::Lease>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ProcessState {
                child,
                allocation: Some(allocation),
            }),
        })
    }

    /// A failed share can leave the guest blocked in kernel I/O, including
    /// its control connection. Reap the VM from the host to release both.
    fn watch_shares(self: &Arc<Self>, failures: Vec<Arc<AtomicBool>>) {
        if failures.is_empty() {
            return;
        }
        let process = Arc::downgrade(self);
        std::thread::spawn(move || loop {
            let Some(process) = process.upgrade() else {
                return;
            };
            if failures.iter().any(|flag| flag.load(Ordering::SeqCst)) {
                process.stop();
                return;
            }
            if !matches!(process.state.lock().unwrap().child.try_wait(), Ok(None)) {
                return;
            }
            drop(process);
            std::thread::sleep(Duration::from_millis(200));
        });
    }

    pub(super) fn stop(&self) {
        let mut state = self.state.lock().unwrap();
        if matches!(state.child.try_wait(), Ok(None)) {
            let _ = state.child.kill();
        }
        if state.child.wait().is_ok() {
            // Child::wait has reaped the VM; remaining session bookkeeping
            // does not consume guest capacity or delay daemon retirement.
            state.allocation = None;
        }
    }
}

pub struct Vm {
    process: Arc<Process>,
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
    // A fixed `pack` name must never connect two app/test instances together.
    let dir = PathBuf::from(format!("/tmp/gizmoth-{uid}-{}", std::process::id()));
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
    /// Uses an ordinary child process to exercise ownership and teardown without
    /// starting a hypervisor or requiring bundled VM resources.
    #[cfg(test)]
    pub(super) fn test_process(
        child: Child,
        directory: &Path,
        allocation: Arc<super::resources::Lease>,
    ) -> Self {
        Self {
            process: Process::new(child, allocation),
            socket: directory.join("test.vsock"),
            console: directory.join("console.log"),
            _cache: None,
            _shares: Vec::new(),
            started_at: crate::serial::now_ms(),
        }
    }

    pub fn start(
        spec: &Spec,
        cache: Option<Arc<cache::Store>>,
        allocation: Arc<super::resources::Lease>,
    ) -> Result<Vm, String> {
        if allocation.amount().cpus != spec.cpus
            || allocation.amount().memory_mib != spec.memory_mib
        {
            return Err("VM resources do not match the shared budget reservation".into());
        }
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
        for (port, folder, writable) in &spec.shares {
            let path = PathBuf::from(format!("{}_{port}", socket.display()));
            let server = if *writable {
                p9::serve_writable(path, folder)
            } else {
                p9::serve(path, folder)
            };
            shares.push(server.map_err(|e| format!("share {}: {e}", folder.display()))?);
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
            // Reserve an address-space capacity, not a fully prefaulted RAM
            // allocation. Actual backing pages follow guest accesses.
            format!("size={}M,prefetch=off", spec.memory_mib),
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
            process: Process::new(child, allocation),
            socket,
            console: spec.console.clone(),
            _cache: cache_server,
            _shares: shares,
            started_at: crate::serial::now_ms(),
        };
        vm.process
            .watch_shares(vm._shares.iter().map(p9::Server::failure_flag).collect());
        let deadline = Instant::now() + Duration::from_secs(15);
        while !vm.socket.exists() {
            if !vm.alive() || Instant::now() > deadline {
                return Err(vm.failure("OpenVMM did not start"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(vm)
    }

    pub(super) fn process(&self) -> Arc<Process> {
        self.process.clone()
    }

    pub fn alive(&mut self) -> bool {
        matches!(
            self.process.state.lock().unwrap().child.try_wait(),
            Ok(None)
        )
    }

    pub fn failure(&mut self, what: &str) -> String {
        let status = self
            .process
            .state
            .lock()
            .unwrap()
            .child
            .try_wait()
            .ok()
            .flatten();
        let mut message = match status {
            Some(status) => format!("{what} (OpenVMM {status})"),
            None => what.to_string(),
        };
        if self
            ._shares
            .iter()
            .any(|share| share.failure_flag().load(Ordering::SeqCst))
        {
            message.push_str(": host shared-file transport failed");
        }
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
        self.process.stop();
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
        self.process.stop();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::resources::{Amount, Pool};

    #[test]
    fn dropping_a_vm_reaps_its_child_before_returning_capacity() {
        let directory = tempfile::tempdir().unwrap();
        let pool = Pool::new(Amount {
            cpus: 1,
            memory_mib: 1024,
        });
        let allocation = pool.try_reserve(pool.snapshot().limit).unwrap();
        let child = Command::new("/bin/sleep")
            .arg("300")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id() as libc::pid_t;
        let machine = Vm::test_process(child, directory.path(), allocation);
        assert_eq!(pool.snapshot().vm_count, 1);
        drop(machine);
        assert_eq!(pool.snapshot().vm_count, 0);
        assert_eq!(
            unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[test]
    fn a_failed_share_reaps_the_vm_without_waiting_for_guest_io() {
        let directory = tempfile::tempdir().unwrap();
        let pool = Pool::new(Amount {
            cpus: 1,
            memory_mib: 1024,
        });
        let allocation = pool.try_reserve(pool.snapshot().limit).unwrap();
        let child = Command::new("/bin/sleep").arg("300").spawn().unwrap();
        let mut machine = Vm::test_process(child, directory.path(), allocation);
        let failed = Arc::new(AtomicBool::new(false));
        machine.process.watch_shares(vec![failed.clone()]);
        assert!(machine.alive());
        failed.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(5);
        while machine.alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!machine.alive(), "share failure left the guest running");
        // stop() reaps and returns the lease under the same process lock.
        assert_eq!(pool.snapshot().vm_count, 0);
    }

    #[test]
    fn mismatched_vm_specs_fail_before_starting_and_return_the_lease() {
        let directory = tempfile::tempdir().unwrap();
        let pool = Pool::new(Amount {
            cpus: 2,
            memory_mib: 2048,
        });
        let allocation = pool.try_reserve(pool.snapshot().limit).unwrap();
        let spec = Spec {
            name: "mismatch".into(),
            cpus: 1,
            memory_mib: 2048,
            kernel: directory.path().join("absent-kernel"),
            initramfs: directory.path().join("absent-initramfs"),
            disks: Vec::new(),
            network: false,
            console: directory.path().join("console.log"),
            runtime: directory.path().to_owned(),
            shares: Vec::new(),
        };
        let result = Vm::start(&spec, None, allocation);
        assert!(matches!(result, Err(error) if error.contains("reservation")));
        assert_eq!(pool.snapshot().vm_count, 0);
        assert!(!spec.console.exists());
    }
}
