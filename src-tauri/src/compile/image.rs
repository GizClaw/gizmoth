//! The shared read-only system disk: Ubuntu base plus the configured packages,
//! built inside a provisioning VM and swapped in atomically.
use super::{
    assets,
    guest::{
        self,
        protocol::{self, BootConfig, BootMode, GuestMessage, HostMessage, ProvisionSpec},
    },
    jobs::LogRing,
    vm, Service, Settings, SlotState, State, IMAGE_PACKAGES,
};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

const SYSTEM_BYTES: u64 = 24 << 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageStatus {
    Missing,
    Building,
    Ready,
    Failed,
}

pub struct ImageState {
    pub status: ImageStatus,
    pub error: Option<String>,
    pub phase: Option<String>,
    pub progress: Option<(u64, u64)>,
    pub info: Option<Value>,
    pub log: LogRing,
    cancel: Arc<AtomicBool>,
}

/// Identifies what goes into the image. The mirror is only a download source
/// and the package list is fixed, so only a GizMoth update changes it.
pub fn spec_hash() -> String {
    let spec = json!({
        "packages": IMAGE_PACKAGES,
        "base": assets::UBUNTU_BASE.sha256,
        "kernel": assets::KERNEL.sha256,
        "bazelisk": assets::BAZELISK.sha256,
        "agent": protocol::AGENT_VERSION,
    });
    Sha256::digest(spec.to_string().as_bytes())[..12]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn image_dir(dir: &Path) -> PathBuf {
    dir.join("image")
}

impl ImageState {
    pub fn load(dir: &Path) -> ImageState {
        let image = image_dir(dir);
        let info = fs::read(image.join("image.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        let ready = info.is_some() && image.join("system.img").is_file();
        let _ = fs::remove_file(image.join("system.img.new"));
        ImageState {
            status: if ready {
                ImageStatus::Ready
            } else {
                ImageStatus::Missing
            },
            error: None,
            phase: None,
            progress: None,
            info: if ready { info } else { None },
            log: LogRing::default(),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn building(&self) -> bool {
        self.status == ImageStatus::Building
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Built by this GizMoth version's image specification.
    pub fn current(&self, _settings: &Settings) -> bool {
        self.info.as_ref().and_then(|i| i["specHash"].as_str()) == Some(spec_hash().as_str())
    }

    pub fn view(&self, settings: &Settings, dir: &Path) -> Value {
        let current = self.current(settings);
        json!({
            "status": self.status,
            "error": self.error,
            "phase": self.phase,
            "progress": self.progress.map(|(done, total)| json!({ "done": done, "total": total })),
            "info": self.info,
            "current": current,
            "allocated": vm::allocated(&image_dir(dir).join("system.img")),
            "logBytes": self.log.end(),
            "base": assets::UBUNTU_BASE.name,
            "kernel": assets::KERNEL.name,
            "bazelisk": assets::BAZELISK.name,
            "packages": IMAGE_PACKAGES,
        })
    }
}

impl Service {
    /// Kernel `Image` and system disk for worker VMs.
    pub(crate) fn image_files(&self) -> Result<(PathBuf, PathBuf), String> {
        let root = self.root();
        let system = image_dir(&root).join("system.img");
        let kernel = root
            .join("assets")
            .join(format!("{}.Image", assets::KERNEL.sha256));
        if !system.is_file() || !kernel.is_file() {
            return Err("The Linux system image is missing; build it on the Compiling page".into());
        }
        Ok((kernel, system))
    }

    pub(crate) fn start_image_build(self: &Arc<Self>, state: &mut State) {
        if state.image.building() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        state.image.status = ImageStatus::Building;
        state.image.error = None;
        state.image.phase = Some("Downloading".into());
        state.image.progress = None;
        state.image.log = LogRing::default();
        state.image.cancel = cancel.clone();
        let settings = state.settings.clone();
        let service = self.clone();
        std::thread::spawn(move || {
            let result = service.build_image(&settings, &cancel);
            let mut state = service.state.lock().unwrap();
            match result {
                Ok(info) => {
                    state.image.status = ImageStatus::Ready;
                    state.image.info = Some(info);
                    state.image.phase = None;
                    // Warm VMs restart on the new image before their next build.
                    for slot in state
                        .slots
                        .iter_mut()
                        .filter(|s| s.state == SlotState::Ready)
                    {
                        slot.last_used = 0;
                    }
                }
                Err(error) => {
                    let cancelled = cancel.load(Ordering::SeqCst);
                    state.image.status = if state.image.info.is_some()
                        && image_dir(&service.root()).join("system.img").is_file()
                    {
                        ImageStatus::Ready
                    } else if cancelled {
                        ImageStatus::Missing
                    } else {
                        ImageStatus::Failed
                    };
                    state.image.error = Some(if cancelled {
                        "Cancelled".into()
                    } else {
                        error.clone()
                    });
                    state.image.phase = None;
                    state
                        .image
                        .log
                        .push(format!("\n[gizmoth] {error}\n").as_bytes());
                }
            }
            state.image.progress = None;
            drop(state);
            service.changed();
        });
        self.changed();
    }

    fn image_log(&self, bytes: &[u8]) {
        self.state.lock().unwrap().image.log.push(bytes);
        self.changed();
    }

    fn image_phase(&self, phase: &str, progress: Option<(u64, u64)>) {
        let mut state = self.state.lock().unwrap();
        state.image.phase = Some(phase.into());
        state.image.progress = progress;
        drop(state);
        self.changed();
    }

    fn build_image(
        self: &Arc<Self>,
        settings: &Settings,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Value, String> {
        let stop = || cancel.load(Ordering::SeqCst);
        let root = self.root();
        let assets_dir = root.join("assets");
        let mut files = Vec::new();
        for asset in [&assets::KERNEL, &assets::UBUNTU_BASE, &assets::BAZELISK] {
            self.image_log(format!("[gizmoth] {}\n", asset.name).as_bytes());
            let last = Mutex::new(Instant::now() - Duration::from_secs(1));
            let path = assets::fetch(
                &assets_dir,
                asset,
                &|done, total| {
                    let mut last = last.lock().unwrap();
                    if last.elapsed() >= Duration::from_millis(250) {
                        *last = Instant::now();
                        self.image_phase(
                            &format!("Downloading {}", asset.name),
                            Some((done, total)),
                        );
                    }
                },
                &stop,
            )?;
            files.push(path);
        }
        let kernel = assets::kernel_image(&files[0])?;
        let agent = assets::agent()?;
        let dir = image_dir(&root);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let target = dir.join("system.img.new");
        let _ = fs::remove_file(&target);
        vm::ensure_disk(&target, SYSTEM_BYTES)?;
        let info = json!({
            "specHash": spec_hash(),
            "builtAt": crate::serial::now_ms(),
            "packages": IMAGE_PACKAGES,
            "aptMirror": settings.image.apt_mirror,
            "base": assets::UBUNTU_BASE.name,
            "kernel": assets::KERNEL.name,
            "bazelisk": assets::BAZELISK.name,
            "agentVersion": protocol::AGENT_VERSION,
        });
        let config = BootConfig {
            mode: BootMode::Provision,
            slot: "image".into(),
            caches: Default::default(),
            bazelrc: Vec::new(),
            env: Vec::new(),
            path: Vec::new(),
            provision: Some(ProvisionSpec {
                packages: IMAGE_PACKAGES.iter().map(|p| p.to_string()).collect(),
                apt_mirror: settings.image.apt_mirror.clone().filter(|m| !m.is_empty()),
                image: info.clone(),
            }),
            shares: Vec::new(),
            images: Vec::new(),
            pack: None,
        };
        let initramfs = dir.join("provision.cpio");
        assets::write_initramfs(
            &initramfs,
            &agent,
            &config,
            &[("base.tar.gz", &files[1]), ("bazelisk", &files[2])],
        )?;
        self.image_phase("Starting installer VM", None);
        let spec = vm::Spec {
            name: "image".into(),
            cpus: super::host_cpus().clamp(1, 4),
            memory_mib: 2048,
            kernel,
            initramfs: initramfs.clone(),
            disks: vec![(target.clone(), false)],
            network: true,
            console: dir.join("console.log"),
            runtime: vm::runtime_dir()?,
            shares: Vec::new(),
        };
        let mut machine = vm::Vm::start(&spec, None)?;
        let result = self.provision(&mut machine, cancel);
        let _ = fs::remove_file(&initramfs);
        if let Err(error) = result {
            machine.stop(Duration::from_secs(5));
            let _ = fs::remove_file(&target);
            return Err(error);
        }
        if !machine.wait_exit(Duration::from_secs(60)) {
            machine.stop(Duration::from_secs(5));
        }
        drop(machine);
        fs::rename(&target, dir.join("system.img")).map_err(|e| e.to_string())?;
        fs::write(
            dir.join("image.json"),
            serde_json::to_vec_pretty(&info).unwrap_or_default(),
        )
        .map_err(|e| e.to_string())?;
        self.image_log(b"[gizmoth] System image ready\n");
        Ok(info)
    }

    fn provision(&self, machine: &mut vm::Vm, cancel: &Arc<AtomicBool>) -> Result<(), String> {
        session(machine, cancel, &|bytes| self.image_log(bytes), &|phase| {
            self.image_phase(phase, None)
        })
    }
}

/// Holds one agent connection for a whole one-shot run (image build or
/// package pack): streams output and phases, and forwards cancellation.
pub(crate) fn session(
    machine: &mut vm::Vm,
    cancel: &Arc<AtomicBool>,
    log: &dyn Fn(&[u8]),
    phase: &dyn Fn(&str),
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut stream = loop {
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        if !machine.alive() {
            return Err(machine.failure("The installer VM stopped during startup"));
        }
        if let Ok(mut stream) = guest::connect(
            &machine.socket,
            protocol::CONTROL_PORT,
            Duration::from_secs(10),
        ) {
            if protocol::write_json(&mut stream, &HostMessage::Hello).is_ok() {
                if let Ok(Some((protocol::JSON, payload))) = protocol::read_frame(&mut stream) {
                    if matches!(
                        protocol::parse::<GuestMessage>(&payload),
                        Ok(GuestMessage::Hello { .. })
                    ) {
                        break stream;
                    }
                }
            }
        }
        if Instant::now() > deadline {
            return Err(machine.failure("The installer VM did not respond"));
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    stream.set_read_timeout(None).map_err(|e| e.to_string())?;
    let finished = Arc::new(AtomicBool::new(false));
    {
        let (cancel, finished) = (cancel.clone(), finished.clone());
        let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
        std::thread::spawn(move || {
            while !finished.load(Ordering::SeqCst) {
                if cancel.load(Ordering::SeqCst) {
                    let _ = protocol::write_json(&mut writer, &HostMessage::Cancel);
                    return;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });
    }
    let result = loop {
        match protocol::read_frame(&mut stream) {
            Ok(Some((protocol::STDOUT | protocol::STDERR, bytes))) => log(&bytes),
            Ok(Some((protocol::JSON, payload))) => {
                match protocol::parse::<GuestMessage>(&payload) {
                    Ok(GuestMessage::Phase { phase: name }) => phase(&name),
                    Ok(GuestMessage::Done { ok: true, .. }) => break Ok(()),
                    Ok(GuestMessage::Done { ok: false, message }) => {
                        break Err(message.unwrap_or_else(|| "provisioning failed".into()))
                    }
                    Ok(_) => {}
                    Err(error) => break Err(error.to_string()),
                }
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break Err(machine.failure("The installer VM disconnected")),
        }
    };
    finished.store(true, Ordering::SeqCst);
    result
}
