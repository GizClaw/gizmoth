//! Compile service: Linux build VMs run by OpenVMM on Hypervisor.framework.
//!
//! The resident GizMoth service owns this state. Each slot is one VM with a
//! persistent work disk; all slots share a read-only system image and the
//! host-side caches. External agents submit builds over the
//! `gizmoth.compile.v1` WebSocket: connecting with a build request queues it,
//! logs stream back, and disconnecting cancels it. Collected outputs become
//! artifacts that any agent or the device WebSocket can reference by ID.
pub mod artifacts;
pub mod assets;
pub mod cache;
pub mod guest;
mod image;
pub mod jobs;
pub mod p9;
pub mod packages;
pub mod storage;
pub mod sync;
pub mod vm;
pub mod ws;

use guest::protocol::{self, BootConfig, BootMode, CacheEnv, DiskInfo, GuestMessage};
use jobs::{Job, Status};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Condvar, Mutex, RwLock,
    },
    time::Duration,
};
use tokio::sync::watch;

pub const PROTOCOL: &str = "gizmoth.compile.v1";
pub const MAX_SLOTS: usize = 16;
const HISTORY: usize = 60;
const READY_TIMEOUT: Duration = Duration::from_secs(120);

pub type Reply = Result<(u16, Value), (u16, String)>;
/// A slot's running VM, shared by its job thread and stop requests.
pub type VmHandle = Arc<Mutex<Option<vm::Vm>>>;

fn bad(message: impl Into<String>) -> (u16, String) {
    (400, message.into())
}

// ---------------------------------------------------------------- settings

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SlotConfig {
    pub id: String,
    pub name: String,
    pub cpus: u32,
    pub memory_mib: u64,
    pub disk_gib: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImageSettings {
    /// Download source for the image's packages; the package list is fixed.
    #[serde(default)]
    pub apt_mirror: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CacheLimit {
    pub max_gib: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CcacheSettings {
    pub max_gib: u64,
    /// Per-slot local tier on the work disk.
    pub local_gib: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CustomCache {
    pub name: String,
    /// Environment variable that receives the namespace URL in builds.
    pub env: String,
    pub max_gib: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CacheSettings {
    pub bazel: CacheLimit,
    pub ccache: CcacheSettings,
    #[serde(default)]
    pub custom: Vec<CustomCache>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactPolicy {
    pub keep_last: u32,
    pub max_gib: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub slots: Vec<SlotConfig>,
    pub image: ImageSettings,
    pub caches: CacheSettings,
    pub network: bool,
    pub idle_minutes: u32,
    #[serde(default)]
    pub bazelrc: Vec<String>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// Workspace paths never synchronized, in addition to `bazel-*` links.
    #[serde(default)]
    pub excludes: Vec<String>,
    pub artifacts: ArtifactPolicy,
}

/// The Linux image's apt packages: the base userland every build gets.
/// Toolchains come from the registry packages instead (see [`packages`]).
pub const IMAGE_PACKAGES: &[&str] = &[
    "build-essential",
    "ccache",
    "cmake",
    "ninja-build",
    "pkg-config",
    "git",
    "python3",
    "python3-venv",
    "python3-pip",
    "curl",
    "ca-certificates",
    "zip",
    "unzip",
    "xz-utils",
    "file",
    "erofs-utils",
];

pub fn host_cpus() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(4)
}

pub fn host_memory_mib() -> u64 {
    #[cfg(target_os = "macos")]
    unsafe {
        let mut value: u64 = 0;
        let mut size = std::mem::size_of::<u64>();
        if libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&mut value as *mut u64).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        ) == 0
        {
            return value >> 20;
        }
    }
    16 * 1024
}

fn new_slot_id() -> String {
    format!("s{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

impl Settings {
    pub fn defaults() -> Settings {
        let cpus = (host_cpus() / 2).clamp(1, 4);
        let memory = (host_memory_mib() / 4).clamp(2048, 8192);
        Settings {
            slots: vec![SlotConfig {
                id: new_slot_id(),
                name: "Slot 1".into(),
                cpus,
                memory_mib: memory,
                disk_gib: 64,
            }],
            image: ImageSettings { apt_mirror: None },
            caches: CacheSettings {
                bazel: CacheLimit { max_gib: 50 },
                ccache: CcacheSettings {
                    max_gib: 20,
                    local_gib: 5,
                },
                custom: Vec::new(),
            },
            network: true,
            idle_minutes: 10,
            bazelrc: Vec::new(),
            env: Vec::new(),
            excludes: Vec::new(),
            artifacts: ArtifactPolicy {
                keep_last: 100,
                max_gib: 20,
            },
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        let (cpus, memory) = (host_cpus(), host_memory_mib());
        if self.slots.len() > MAX_SLOTS {
            return Err(format!("At most {MAX_SLOTS} slots"));
        }
        for slot in &self.slots {
            validate_slot(slot, cpus, memory)?;
        }
        if let Some(mirror) = self.image.apt_mirror.as_deref().filter(|m| !m.is_empty()) {
            let url = url::Url::parse(mirror).map_err(|_| "The apt mirror must be a URL")?;
            if !matches!(url.scheme(), "http" | "https") {
                return Err("The apt mirror must use http or https".into());
            }
        }
        let mut names = vec!["bazel".to_string(), "ccache".to_string()];
        for cache in &self.caches.custom {
            if !cache::valid_namespace(&cache.name) || names.contains(&cache.name) {
                return Err(format!("Invalid or duplicate cache name {}", cache.name));
            }
            if !valid_env_name(&cache.env) {
                return Err(format!("Invalid environment variable {}", cache.env));
            }
            names.push(cache.name.clone());
        }
        if self.caches.custom.len() > 16 {
            return Err("At most 16 custom caches".into());
        }
        if !(1..=24 * 60).contains(&self.idle_minutes) {
            return Err("Idle shutdown must be between 1 minute and 24 hours".into());
        }
        if self.bazelrc.len() > 100
            || self
                .bazelrc
                .iter()
                .any(|l| l.len() > 1000 || l.contains(['\n', '\r', '\0']))
        {
            return Err("Invalid bazelrc lines".into());
        }
        if self.env.len() > 64
            || !self.env.iter().all(|v| {
                valid_env_name(&v.name) && v.value.len() <= 4096 && !v.value.contains('\0')
            })
        {
            return Err("Invalid environment variables".into());
        }
        if self.excludes.len() > 100 || !self.excludes.iter().all(|p| protocol::valid_pattern(p)) {
            return Err("Invalid exclude patterns".into());
        }
        if self.artifacts.keep_last == 0 || self.artifacts.keep_last > 10_000 {
            return Err("Keep between 1 and 10000 artifacts".into());
        }
        Ok(())
    }

    fn namespaces(&self) -> Vec<cache::Namespace> {
        let gib = |n: u64| n << 30;
        let mut list = vec![
            cache::Namespace {
                name: "bazel".into(),
                max_bytes: gib(self.caches.bazel.max_gib),
            },
            cache::Namespace {
                name: "ccache".into(),
                max_bytes: gib(self.caches.ccache.max_gib),
            },
        ];
        list.extend(self.caches.custom.iter().map(|c| cache::Namespace {
            name: c.name.clone(),
            max_bytes: gib(c.max_gib),
        }));
        list
    }

    fn boot_config(&self, slot: &str) -> BootConfig {
        BootConfig {
            mode: BootMode::Worker,
            slot: slot.into(),
            caches: CacheEnv {
                // Caches are always on; only their sizes are configurable.
                bazel: true,
                ccache: true,
                ccache_local_size: format!("{}G", self.caches.ccache.local_gib.max(1)),
                custom: self
                    .caches
                    .custom
                    .iter()
                    .map(|c| (c.name.clone(), c.env.clone()))
                    .collect(),
            },
            bazelrc: self.bazelrc.clone(),
            env: self
                .env
                .iter()
                .map(|v| (v.name.clone(), v.value.clone()))
                .collect(),
            provision: None,
            path: Vec::new(),
            shares: Vec::new(),
            images: Vec::new(),
            pack: None,
        }
    }
}

pub fn valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn validate_slot(slot: &SlotConfig, cpus: u32, memory: u64) -> Result<(), String> {
    if slot.name.trim().is_empty() || slot.name.chars().count() > 40 {
        return Err("Slot names must have 1–40 characters".into());
    }
    if !(1..=cpus).contains(&slot.cpus) {
        return Err(format!("A slot can use 1–{cpus} CPUs"));
    }
    if slot.memory_mib < 1024 || slot.memory_mib > memory.saturating_sub(1024).max(1024) {
        return Err("A slot needs at least 1 GiB and must leave 1 GiB for macOS".into());
    }
    if !(8..=4096).contains(&slot.disk_gib) {
        return Err("Work disks can be 8–4096 GiB".into());
    }
    if !slot.id.starts_with('s')
        || slot.id.len() != 9
        || !slot.id[1..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("Invalid slot ID".into());
    }
    Ok(())
}

// ---------------------------------------------------------------- runtime

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SlotState {
    Stopped,
    Starting,
    Ready,
    Busy,
    Stopping,
    Failed,
}

pub struct Slot {
    pub config: SlotConfig,
    pub state: SlotState,
    pub error: Option<String>,
    pub job: Option<String>,
    pub vm: VmHandle,
    pub started_at: Option<u64>,
    pub last_used: u64,
    pub work: Option<DiskInfo>,
    pub disk_allocated: u64,
    pub dir: PathBuf,
}

impl Slot {
    fn new(config: SlotConfig, root: &Path) -> Slot {
        let dir = root.join("slots").join(&config.id);
        let mut slot = Slot {
            config,
            state: SlotState::Stopped,
            error: None,
            job: None,
            vm: Arc::new(Mutex::new(None)),
            started_at: None,
            last_used: 0,
            work: None,
            disk_allocated: 0,
            dir,
        };
        slot.refresh_disk();
        slot
    }

    pub fn disk(&self) -> PathBuf {
        self.dir.join("work.img")
    }

    pub fn refresh_disk(&mut self) {
        self.disk_allocated = vm::allocated(&self.disk());
    }

    fn view(&self, jobs: &[Arc<Job>]) -> Value {
        let job = self.job.as_ref().and_then(|id| jobs.iter().find(|j| &j.id == id)).map(|j| {
            let state = j.state.lock().unwrap();
            json!({ "id": j.id, "client": j.client, "title": j.title, "status": state.status, "startedAt": state.started_at })
        });
        json!({
            "id": self.config.id, "name": self.config.name, "cpus": self.config.cpus,
            "memoryMiB": self.config.memory_mib, "diskGiB": self.config.disk_gib,
            "state": self.state, "error": self.error, "job": job,
            "startedAt": self.started_at, "lastUsed": if self.last_used == 0 { Value::Null } else { self.last_used.into() },
            "disk": { "allocated": self.disk_allocated, "size": self.config.disk_gib << 30,
                      "used": self.work.as_ref().map(|w| w.total.saturating_sub(w.free)), "free": self.work.as_ref().map(|w| w.free) },
        })
    }
}

pub struct State {
    pub settings: Settings,
    pub slots: Vec<Slot>,
    pub jobs: Vec<Arc<Job>>,
    pub image: image::ImageState,
}

/// A working-directory move in progress.
struct Move {
    from: PathBuf,
    to: PathBuf,
    phase: &'static str,
    done: u64,
    total: u64,
    started_at: u64,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct StorageState {
    moving: Option<Move>,
    /// Why the last move failed.
    error: Option<String>,
}

pub struct Service {
    /// `<app data>/compile`: settings and the working-directory choice.
    pub config: PathBuf,
    /// The working directory holding the image, slots, caches and artifacts.
    root: RwLock<PathBuf>,
    storage: Mutex<StorageState>,
    pub state: Mutex<State>,
    wake: Condvar,
    version: watch::Sender<u64>,
    pub cache: Arc<cache::Store>,
    pub artifacts: artifacts::Store,
    packages: packages::Packages,
    order: AtomicU64,
}

/// OpenVMM and the guest agent are present on a Mac with Apple silicon.
pub fn compile_available() -> bool {
    cfg!(all(target_os = "macos", target_arch = "aarch64"))
        && assets::openvmm().is_ok()
        && assets::agent().is_ok()
}

impl Service {
    pub fn new(data_dir: &Path) -> Arc<Service> {
        let config = data_dir.join("compile");
        let _ = fs::create_dir_all(&config);
        // A custom working directory on a disk that is not connected stays
        // missing; builds wait for it instead of filling the startup disk.
        let dir = storage::configured_root(&config);
        let settings = fs::read(config.join("settings.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
            .filter(|s| s.validate().is_ok())
            .unwrap_or_else(Settings::defaults);
        let (version, _) = watch::channel(0u64);
        let version_for_cache = version.clone();
        let cache = cache::Store::new(
            dir.join("cache"),
            Box::new(move || version_for_cache.send_modify(|v| *v += 1)),
        );
        cache.configure(settings.namespaces());
        let slots = settings
            .slots
            .iter()
            .cloned()
            .map(|c| Slot::new(c, &dir))
            .collect();
        let image = image::ImageState::load(&dir);
        let packages = packages::Packages::new(&config, &dir);
        if image.status != image::ImageStatus::Missing {
            packages.activate();
        }
        let service = Arc::new(Service {
            artifacts: artifacts::Store::new(dir.join("artifacts")),
            packages,
            config,
            root: RwLock::new(dir),
            storage: Mutex::new(StorageState::default()),
            state: Mutex::new(State {
                settings,
                slots,
                jobs: Vec::new(),
                image,
            }),
            wake: Condvar::new(),
            version,
            cache,
            order: AtomicU64::new(1),
        });
        let _ = service.save_settings();
        let scheduler = service.clone();
        std::thread::Builder::new()
            .name("compile-scheduler".into())
            .spawn(move || scheduler.schedule())
            .expect("compile scheduler");
        let maintainer = service.clone();
        std::thread::Builder::new()
            .name("compile-packages".into())
            .spawn(move || maintainer.maintain())
            .expect("compile packages");
        service
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.version.subscribe()
    }

    pub fn version(&self) -> u64 {
        *self.version.borrow()
    }

    pub fn changed(&self) {
        self.version.send_modify(|v| *v += 1);
        self.wake.notify_all();
    }

    /// The working directory (see [`storage`]).
    pub fn root(&self) -> PathBuf {
        self.root.read().unwrap().clone()
    }

    fn moving(&self) -> bool {
        self.storage.lock().unwrap().moving.is_some()
    }

    fn save_settings(&self) -> Result<(), String> {
        let settings = self.state.lock().unwrap().settings.clone();
        let path = self.config.join("settings.json");
        let temporary = path.with_extension("json.tmp");
        fs::write(
            &temporary,
            serde_json::to_vec_pretty(&settings).map_err(|e| e.to_string())?,
        )
        .and_then(|_| fs::rename(&temporary, &path))
        .map_err(|e| e.to_string())
    }

    /// True while builds run, wait, or VMs are up: the service must stay alive.
    pub fn busy(&self) -> bool {
        if self.moving() || self.packages.busy() {
            return true;
        }
        let state = self.state.lock().unwrap();
        state.image.building()
            || state.jobs.iter().any(|j| !j.status().finished())
            || state
                .slots
                .iter()
                .any(|s| s.state != SlotState::Stopped && s.state != SlotState::Failed)
    }

    pub fn job(&self, id: &str) -> Option<Arc<Job>> {
        self.state
            .lock()
            .unwrap()
            .jobs
            .iter()
            .find(|j| j.id == id)
            .cloned()
    }

    pub(crate) fn job_settings(&self) -> (Vec<(String, String)>, Vec<String>) {
        let state = self.state.lock().unwrap();
        (Vec::new(), state.settings.excludes.clone())
    }

    pub fn apply_retention(&self) {
        let policy = self.state.lock().unwrap().settings.artifacts.clone();
        self.artifacts
            .retain(policy.keep_last as usize, policy.max_gib << 30);
        self.changed();
    }

    /// Queues a build. The caller keeps the returned job and cancels it when
    /// its WebSocket closes.
    pub fn submit(
        &self,
        client: String,
        title: String,
        request: jobs::BuildRequest,
    ) -> Result<(Arc<Job>, tokio::sync::broadcast::Receiver<jobs::Event>), String> {
        validate_request(&request)?;
        let workspace = request
            .workspace
            .canonicalize()
            .map_err(|e| format!("{}: {e}", request.workspace.display()))?;
        if !workspace.is_dir() {
            return Err(format!("{} is not a directory", workspace.display()));
        }
        let key = workspace_key(&workspace);
        let request = jobs::BuildRequest {
            workspace,
            ..request
        };
        let job = Arc::new(Job::new(
            self.order.fetch_add(1, Ordering::SeqCst),
            client,
            title,
            request,
            key,
        ));
        // Subscribe before the scheduler can see the job so no event is missed.
        let receiver = job.events.subscribe();
        let mut state = self.state.lock().unwrap();
        if state.slots.is_empty() {
            return Err("No compile slots are configured; add one on the Compiling page".into());
        }
        if state
            .jobs
            .iter()
            .filter(|j| j.status() == Status::Queued)
            .count()
            >= 256
        {
            return Err("The compile queue is full".into());
        }
        state.jobs.push(job.clone());
        self.packages.activate();
        prune_history(&mut state.jobs);
        drop(state);
        job.note(&format!("Queued by {}: {}", job.client, job.title));
        self.changed();
        Ok((job, receiver))
    }

    pub fn cancel(&self, id: &str) -> Reply {
        let job = self.job(id).ok_or((404, "Job not found".to_string()))?;
        self.cancel_job(&job);
        Ok((200, job.summary()))
    }

    pub fn cancel_job(&self, job: &Arc<Job>) {
        job.request_cancel();
        if job.status() == Status::Queued {
            job.finish(Status::Cancelled, None, None);
        }
        self.changed();
    }

    // ------------------------------------------------------------ scheduling

    fn schedule(self: Arc<Self>) {
        let mut state = self.state.lock().unwrap();
        loop {
            self.dispatch(&mut state);
            self.stop_idle(&mut state);
            state = self
                .wake
                .wait_timeout(state, Duration::from_secs(2))
                .unwrap()
                .0;
        }
    }

    fn dispatch(self: &Arc<Self>, state: &mut State) {
        let mut queued: Vec<Arc<Job>> = state
            .jobs
            .iter()
            .filter(|j| j.status() == Status::Queued && !j.cancelled())
            .cloned()
            .collect();
        queued.sort_by_key(|j| j.order);
        // Builds wait while the working directory moves.
        if queued.is_empty() || self.moving() {
            return;
        }
        let root = self.root();
        if !root.is_dir() {
            let message = format!(
                "The working directory {} is not available. Connect its disk, or choose another location in Settings → Storage.",
                root.display()
            );
            for job in queued {
                job.finish(Status::Failed, Some(message.clone()), None);
            }
            self.version.send_modify(|v| *v += 1);
            return;
        }
        match state.image.status {
            image::ImageStatus::Ready => {}
            image::ImageStatus::Building => return,
            image::ImageStatus::Missing => {
                for job in &queued {
                    job.note("Preparing the Linux system image (first use)…");
                }
                self.start_image_build(state);
                return;
            }
            image::ImageStatus::Failed => {
                let message = format!(
                    "The Linux system image could not be built: {}. Rebuild it on the Compiling page.",
                    state.image.error.clone().unwrap_or_default()
                );
                for job in queued {
                    job.finish(Status::Failed, Some(message.clone()), None);
                }
                self.version.send_modify(|v| *v += 1);
                return;
            }
        }
        let mut remaining = Vec::new();
        for job in queued {
            // Prefer a warm VM, then a stopped slot.
            let index = state
                .slots
                .iter()
                .position(|s| s.state == SlotState::Ready)
                .or_else(|| {
                    state
                        .slots
                        .iter()
                        .position(|s| matches!(s.state, SlotState::Stopped | SlotState::Failed))
                });
            match index {
                Some(index) => {
                    let slot = &mut state.slots[index];
                    slot.state = if slot.state == SlotState::Ready {
                        SlotState::Busy
                    } else {
                        SlotState::Starting
                    };
                    slot.error = None;
                    slot.job = Some(job.id.clone());
                    let slot_id = slot.config.id.clone();
                    job.state.lock().unwrap().slot = Some(slot_id.clone());
                    job.set_status(Status::Booting);
                    job.emit(json!({ "type": "started", "jobId": job.id, "slot": slot_id, "slotName": slot.config.name }));
                    job.note(&format!("Running on {}", slot.config.name));
                    let (service, job) = (self.clone(), job.clone());
                    std::thread::spawn(move || jobs::run(service, job, slot_id));
                }
                None => remaining.push(job),
            }
        }
        for (position, job) in remaining.iter().enumerate() {
            job.set_position(position + 1);
        }
        self.version.send_modify(|v| *v += 1);
    }

    fn stop_idle(self: &Arc<Self>, state: &mut State) {
        let idle = Duration::from_secs(state.settings.idle_minutes as u64 * 60).as_millis() as u64;
        let now = crate::serial::now_ms();
        for slot in state.slots.iter_mut() {
            if slot.state == SlotState::Ready && now.saturating_sub(slot.last_used) > idle {
                slot.state = SlotState::Stopping;
                let (service, id, vm) = (self.clone(), slot.config.id.clone(), slot.vm.clone());
                std::thread::spawn(move || service.stop_vm(&id, vm));
            }
        }
    }

    fn stop_vm(&self, id: &str, vm: VmHandle) {
        if let Some(mut running) = vm.lock().unwrap().take() {
            running.stop(Duration::from_secs(20));
        }
        let mut state = self.state.lock().unwrap();
        if let Some(slot) = state.slots.iter_mut().find(|s| s.config.id == id) {
            if slot.state == SlotState::Stopping {
                slot.state = SlotState::Stopped;
            }
            slot.started_at = None;
            slot.refresh_disk();
        }
        drop(state);
        self.changed();
    }

    /// Starts the slot's VM if needed and returns its vsock socket.
    pub(crate) fn ensure_vm(&self, slot_id: &str, job: &Job) -> Result<PathBuf, String> {
        let (config, handle, settings, dir) = {
            let state = self.state.lock().unwrap();
            let slot = state
                .slots
                .iter()
                .find(|s| s.config.id == slot_id)
                .ok_or("The slot was removed")?;
            (
                slot.config.clone(),
                slot.vm.clone(),
                state.settings.clone(),
                slot.dir.clone(),
            )
        };
        let mut guard = handle.lock().unwrap();
        if let Some(vm) = guard.as_mut() {
            if vm.alive() {
                return Ok(vm.socket.clone());
            }
        }
        *guard = None;
        self.set_slot(slot_id, |slot| slot.state = SlotState::Starting);
        job.note(&format!(
            "Starting VM ({} CPU, {} MiB)",
            config.cpus, config.memory_mib
        ));
        let (kernel, system) = self.image_files()?;
        let agent = assets::agent()?;
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let work = dir.join("work.img");
        vm::ensure_disk(&work, config.disk_gib << 30)?;
        let initramfs = dir.join("initramfs.cpio");
        // Packages: the latest packed erofs disk, read-only in every slot.
        let mut boot = settings.boot_config(slot_id);
        let mut disks = vec![(system, true), (work, false)];
        if let Some((pack, packed)) = self.package_mount() {
            boot.images.push(protocol::ImageMount {
                lun: disks.len() as u32,
                path: packages::GUEST_ROOT.into(),
            });
            disks.push((pack, true));
            let (path, variables) = packages::guest_environment(&packed);
            boot.path = path;
            // Settings' variables come later and win.
            boot.env.splice(0..0, variables);
        }
        assets::write_initramfs(&initramfs, &agent, &boot, &[])?;
        let spec = vm::Spec {
            name: slot_id.to_string(),
            cpus: config.cpus,
            memory_mib: config.memory_mib,
            kernel,
            initramfs,
            disks,
            network: settings.network,
            console: dir.join("console.log"),
            runtime: vm::runtime_dir()?,
            shares: Vec::new(),
        };
        let mut started = vm::Vm::start(&spec, Some(self.cache.clone()))?;
        let hello = started.wait_ready(READY_TIMEOUT, &|| job.cancelled())?;
        let work_info = match hello {
            GuestMessage::Hello {
                version,
                work,
                image,
                ..
            } => {
                if version != protocol::AGENT_VERSION {
                    return Err("The VM agent version does not match this GizMoth".into());
                }
                if image.as_ref().and_then(|i| i["agentVersion"].as_u64())
                    != Some(protocol::AGENT_VERSION as u64)
                {
                    job.note("The system image was built by another GizMoth version; rebuild it if builds misbehave");
                }
                work
            }
            _ => None,
        };
        let socket = started.socket.clone();
        let started_at = started.started_at;
        *guard = Some(started);
        drop(guard);
        self.set_slot(slot_id, |slot| {
            slot.state = SlotState::Busy;
            slot.started_at = Some(started_at);
            slot.work = work_info;
        });
        job.note("VM ready");
        Ok(socket)
    }

    fn set_slot(&self, id: &str, update: impl FnOnce(&mut Slot)) {
        let mut state = self.state.lock().unwrap();
        if let Some(slot) = state.slots.iter_mut().find(|s| s.config.id == id) {
            update(slot);
        }
        drop(state);
        self.changed();
    }

    // ------------------------------------------------------------ views

    pub fn snapshot(&self) -> Value {
        let state = self.state.lock().unwrap();
        let mut jobs: Vec<Value> = state.jobs.iter().rev().map(|j| j.summary()).collect();
        jobs.truncate(HISTORY);
        let queue: Vec<Value> = {
            let mut queued: Vec<&Arc<Job>> = state
                .jobs
                .iter()
                .filter(|j| j.status() == Status::Queued)
                .collect();
            queued.sort_by_key(|j| j.order);
            queued.iter().map(|j| j.summary()).collect()
        };
        let caches = self.cache_view(&state.settings);
        let artifacts = self.artifacts.list();
        let available = match (assets::openvmm(), assets::agent()) {
            _ if compile_available() => json!({ "ok": true }),
            (Ok(_), Ok(_)) => {
                json!({ "ok": false, "message": "Compiling currently requires macOS on Apple silicon" })
            }
            (Err(error), _) | (_, Err(error)) => json!({ "ok": false, "message": error }),
        };
        json!({
            "protocol": PROTOCOL,
            "version": self.version(),
            "available": available,
            "host": { "cpus": host_cpus(), "memoryMiB": host_memory_mib() },
            "settings": state.settings,
            "image": state.image.view(&state.settings, &self.root()),
            "slots": state.slots.iter().map(|s| s.view(&state.jobs)).collect::<Vec<_>>(),
            "queue": queue,
            "jobs": jobs,
            "caches": caches,
            "artifacts": { "count": artifacts.len(), "bytes": artifacts.iter().map(|a| a.bytes).sum::<u64>(), "dir": self.artifacts.root().display().to_string() },
            "storage": self.storage_view(&state, &artifacts),
            "packages": self.packages_view(),
        })
    }

    /// Working directory, its usage by kind and any move in progress. Uses
    /// tracked sizes so a snapshot never walks the cache tree.
    fn storage_view(&self, state: &State, artifacts: &[artifacts::Artifact]) -> Value {
        let root = self.root();
        let caches: u64 = state
            .settings
            .namespaces()
            .iter()
            .map(|n| self.cache.usage(&n.name).bytes)
            .sum();
        // One entry per kind of data, as a folder below the working directory.
        let item = |id: &str, dir: &str, bytes: u64| {
            let path = root.join(dir);
            json!({ "id": id, "path": path, "bytes": bytes, "exists": path.exists() })
        };
        let items = [
            item(
                "image",
                "image",
                vm::allocated(&root.join("image").join("system.img")),
            ),
            item(
                "slots",
                "slots",
                state.slots.iter().map(|s| s.disk_allocated).sum::<u64>(),
            ),
            item("caches", "cache", caches),
            item(
                "artifacts",
                "artifacts",
                artifacts.iter().map(|a| a.bytes).sum::<u64>(),
            ),
            item("packages", packages::PACK, self.packages_bytes()),
            item("assets", "assets", storage::allocated(&root.join("assets"))),
        ];
        let storage = self.storage.lock().unwrap();
        let moving = storage.moving.as_ref().map(|m| {
            json!({ "from": m.from, "to": m.to, "phase": m.phase, "done": m.done, "total": m.total, "startedAt": m.started_at })
        });
        json!({
            "root": root,
            "default": self.config,
            "isDefault": root == self.config,
            "available": root.is_dir(),
            "free": storage::free_space(&root),
            "error": storage.error,
            "moving": moving,
            "items": items,
        })
    }

    fn cache_view(&self, settings: &Settings) -> Vec<Value> {
        let row = |name: &str, kind: &str, max_gib: u64, env: Option<&str>| {
            let usage = self.cache.usage(name);
            json!({ "name": name, "kind": kind, "maxBytes": max_gib << 30, "env": env,
                    "bytes": usage.bytes, "files": usage.files, "hits": usage.hits, "misses": usage.misses, "uploads": usage.uploads })
        };
        let mut rows = vec![
            row("bazel", "bazel", settings.caches.bazel.max_gib, None),
            row("ccache", "ccache", settings.caches.ccache.max_gib, None),
        ];
        rows.extend(
            settings
                .caches
                .custom
                .iter()
                .map(|c| row(&c.name, "custom", c.max_gib, Some(&c.env))),
        );
        rows
    }

    pub fn artifact_list(&self) -> Value {
        Value::Array(
            self.artifacts
                .list()
                .iter()
                .map(|a| self.artifacts.view(a))
                .collect(),
        )
    }

    /// Resolves an artifact file for device installation.
    pub fn artifact_file(&self, id: &str, file: Option<&str>) -> Result<PathBuf, String> {
        if !artifacts::valid_id(id) {
            return Err("artifact must be an artifact ID".into());
        }
        self.artifacts.file(id, file, ".update.tar.zlib")
    }

    // ------------------------------------------------------------ HTTP API

    /// Routes `/v1/compile/...`. Reads are open to every authenticated caller;
    /// configuration and management are the desktop app's.
    pub fn handle(
        self: &Arc<Self>,
        method: &str,
        rest: &[&str],
        query: &str,
        body: Value,
        app: bool,
    ) -> Reply {
        let forbidden = || Err((403, "Only GizMoth itself can manage compiling".to_string()));
        let moving = self.moving();
        let param = |key: &str| {
            url::form_urlencoded::parse(query.as_bytes())
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.into_owned())
        };
        match (method, rest) {
            ("GET", []) => Ok((200, self.snapshot())),
            ("GET", ["artifacts"]) => Ok((200, self.artifact_list())),
            ("GET", ["artifacts", id]) => {
                let artifact = self
                    .artifacts
                    .get(id)
                    .ok_or((404, "Artifact not found".to_string()))?;
                Ok((200, self.artifacts.view(&artifact)))
            }
            ("GET", ["jobs", id]) => Ok((
                200,
                self.job(id)
                    .ok_or((404, "Job not found".to_string()))?
                    .summary(),
            )),
            ("GET", ["jobs", id, "log"]) => {
                let job = self.job(id).ok_or((404, "Job not found".to_string()))?;
                let after = param("after").and_then(|v| v.parse().ok());
                let limit = param("limit")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(256 * 1024)
                    .min(1 << 20);
                let (bytes, next, truncated) = job.log.lock().unwrap().read(after, limit);
                Ok((
                    200,
                    json!({ "text": String::from_utf8_lossy(&bytes), "next": next, "truncated": truncated }),
                ))
            }
            ("GET", ["image", "log"]) => {
                let after = param("after").and_then(|v| v.parse().ok());
                let (bytes, next, truncated) =
                    self.state.lock().unwrap().image.log.read(after, 256 * 1024);
                Ok((
                    200,
                    json!({ "text": String::from_utf8_lossy(&bytes), "next": next, "truncated": truncated }),
                ))
            }
            _ if !app => forbidden(),
            ("POST", ["jobs", id, "cancel"]) => self.cancel(id),
            ("POST", ["prepare"]) => {
                self.packages_request(false);
                Ok((200, self.snapshot()))
            }
            ("POST", ["packages", "refresh"]) => {
                self.packages_request(true);
                Ok((200, self.snapshot()))
            }
            ("POST", ["storage", "cancel"]) => {
                if let Some(moving) = self.storage.lock().unwrap().moving.as_ref() {
                    moving.cancel.store(true, Ordering::SeqCst);
                }
                Ok((200, self.snapshot()))
            }
            _ if moving => Err((
                409,
                "The working directory is being moved; try again when it finishes".into(),
            )),
            ("POST", ["storage"]) => self.move_storage(body),
            ("PATCH", ["settings"]) => self.patch_settings(body),
            ("POST", ["slots"]) => self.add_slot(body),
            ("PATCH", ["slots", id]) => self.patch_slot(id, body),
            ("DELETE", ["slots", id]) => self.remove_slot(id),
            ("POST", ["slots", id, "stop"]) => self.stop_slot(id),
            ("POST", ["slots", id, "reset"]) => self.reset_slot(id),
            ("POST", ["image", "build"]) => {
                let mut state = self.state.lock().unwrap();
                if state
                    .slots
                    .iter()
                    .any(|s| matches!(s.state, SlotState::Busy | SlotState::Starting))
                {
                    return Err((
                        409,
                        "Wait for running builds to finish before rebuilding the image".into(),
                    ));
                }
                if state.image.building() {
                    return Err((409, "The image is already being built".into()));
                }
                self.start_image_build(&mut state);
                drop(state);
                Ok((200, self.snapshot()))
            }
            ("POST", ["image", "cancel"]) => {
                self.state.lock().unwrap().image.cancel();
                self.changed();
                Ok((200, json!({ "cancelling": true })))
            }
            ("DELETE", ["caches", name]) => {
                self.cache.clear(name).map_err(|e| bad(e.to_string()))?;
                Ok((200, self.snapshot()))
            }
            ("PATCH", ["artifacts", id]) => {
                let pinned = body["pinned"]
                    .as_bool()
                    .ok_or_else(|| bad("pinned must be a boolean"))?;
                let artifact = self
                    .artifacts
                    .set_pinned(id, pinned)
                    .map_err(|e| (404, e))?;
                self.changed();
                Ok((200, self.artifacts.view(&artifact)))
            }
            ("DELETE", ["artifacts", id]) => {
                self.artifacts.delete(id).map_err(|e| (404, e))?;
                self.changed();
                Ok((200, json!({ "deleted": id })))
            }
            _ => Err((404, "not found".into())),
        }
    }

    fn patch_settings(self: &Arc<Self>, body: Value) -> Reply {
        let mut state = self.state.lock().unwrap();
        let mut merged = serde_json::to_value(&state.settings).unwrap();
        let Some(patch) = body.as_object() else {
            return Err(bad("Expected an object"));
        };
        for (key, value) in patch {
            if key == "slots" {
                return Err(bad("Use the slot endpoints to change slots"));
            }
            if merged.get(key).is_none() {
                return Err(bad(format!("Unknown setting {key}")));
            }
            merged[key] = value.clone();
        }
        let settings: Settings = serde_json::from_value(merged).map_err(|e| bad(e.to_string()))?;
        settings.validate().map_err(bad)?;
        let boot_changed = settings.boot_config("").bazelrc
            != state.settings.boot_config("").bazelrc
            || settings.caches != state.settings.caches
            || settings.env != state.settings.env
            || settings.network != state.settings.network;
        state.settings = settings.clone();
        // Idle VMs restart with the new boot configuration on their next build.
        if boot_changed {
            for slot in state
                .slots
                .iter_mut()
                .filter(|s| s.state == SlotState::Ready)
            {
                slot.last_used = 0;
            }
        }
        drop(state);
        self.cache.configure(settings.namespaces());
        self.save_settings().map_err(bad)?;
        self.apply_retention();
        Ok((200, self.snapshot()))
    }

    fn add_slot(self: &Arc<Self>, body: Value) -> Reply {
        let mut state = self.state.lock().unwrap();
        if state.slots.len() >= MAX_SLOTS {
            return Err(bad(format!("At most {MAX_SLOTS} slots")));
        }
        let template = state
            .slots
            .last()
            .map(|s| s.config.clone())
            .unwrap_or_else(|| Settings::defaults().slots[0].clone());
        let config = SlotConfig {
            id: new_slot_id(),
            name: body["name"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Slot {}", state.slots.len() + 1)),
            cpus: body["cpus"]
                .as_u64()
                .map(|v| v as u32)
                .unwrap_or(template.cpus),
            memory_mib: body["memoryMiB"].as_u64().unwrap_or(template.memory_mib),
            disk_gib: body["diskGiB"].as_u64().unwrap_or(template.disk_gib),
        };
        validate_slot(&config, host_cpus(), host_memory_mib()).map_err(bad)?;
        state.settings.slots.push(config.clone());
        state.slots.push(Slot::new(config, &self.root()));
        drop(state);
        self.save_settings().map_err(bad)?;
        self.changed();
        Ok((200, self.snapshot()))
    }

    fn patch_slot(self: &Arc<Self>, id: &str, body: Value) -> Reply {
        let mut state = self.state.lock().unwrap();
        let index = state
            .slots
            .iter()
            .position(|s| s.config.id == id)
            .ok_or((404, "Slot not found".to_string()))?;
        let slot = &state.slots[index];
        let mut config = slot.config.clone();
        if let Some(name) = body["name"].as_str() {
            config.name = name.trim().to_string();
        }
        let resources = ["cpus", "memoryMiB", "diskGiB"]
            .iter()
            .any(|k| body.get(*k).is_some());
        if resources && slot.state != SlotState::Stopped && slot.state != SlotState::Failed {
            return Err((409, "Stop the slot before changing its resources".into()));
        }
        if let Some(cpus) = body["cpus"].as_u64() {
            config.cpus = cpus as u32;
        }
        if let Some(memory) = body["memoryMiB"].as_u64() {
            config.memory_mib = memory;
        }
        if let Some(disk) = body["diskGiB"].as_u64() {
            if slot.disk().exists() && disk != config.disk_gib {
                return Err((409, "Reset the work disk before changing its size".into()));
            }
            config.disk_gib = disk;
        }
        validate_slot(&config, host_cpus(), host_memory_mib()).map_err(bad)?;
        state.slots[index].config = config.clone();
        if let Some(saved) = state.settings.slots.iter_mut().find(|s| s.id == id) {
            *saved = config;
        }
        drop(state);
        self.save_settings().map_err(bad)?;
        self.changed();
        Ok((200, self.snapshot()))
    }

    fn take_idle_slot(&self, id: &str, remove: bool) -> Result<(VmHandle, PathBuf), (u16, String)> {
        let mut state = self.state.lock().unwrap();
        let index = state
            .slots
            .iter()
            .position(|s| s.config.id == id)
            .ok_or((404, "Slot not found".to_string()))?;
        let slot = &mut state.slots[index];
        if matches!(slot.state, SlotState::Busy | SlotState::Starting) {
            return Err((409, "The slot is running a build; cancel it first".into()));
        }
        let handle = (slot.vm.clone(), slot.dir.clone());
        if remove {
            state.slots.remove(index);
            state.settings.slots.retain(|s| s.id != id);
        } else {
            slot.state = SlotState::Stopping;
        }
        Ok(handle)
    }

    fn remove_slot(self: &Arc<Self>, id: &str) -> Reply {
        let (vm, dir) = self.take_idle_slot(id, true)?;
        self.save_settings().map_err(bad)?;
        self.changed();
        std::thread::spawn(move || {
            if let Some(mut running) = vm.lock().unwrap().take() {
                running.stop(Duration::from_secs(20));
            }
            let _ = fs::remove_dir_all(dir);
        });
        Ok((200, self.snapshot()))
    }

    fn stop_slot(self: &Arc<Self>, id: &str) -> Reply {
        let (vm, _) = self.take_idle_slot(id, false)?;
        let (service, id) = (self.clone(), id.to_string());
        std::thread::spawn(move || service.stop_vm(&id, vm));
        self.changed();
        Ok((200, self.snapshot()))
    }

    /// Stops the VM and deletes its work disk: sources, Bazel output base,
    /// local ccache tier and home are recreated on the next build.
    fn reset_slot(self: &Arc<Self>, id: &str) -> Reply {
        let (vm, dir) = self.take_idle_slot(id, false)?;
        let (service, id) = (self.clone(), id.to_string());
        std::thread::spawn(move || {
            if let Some(mut running) = vm.lock().unwrap().take() {
                running.stop(Duration::from_secs(20));
            }
            let _ = fs::remove_file(dir.join("work.img"));
            service.set_slot(&id, |slot| {
                slot.work = None;
                slot.refresh_disk();
            });
            service.stop_vm(&id, Arc::new(Mutex::new(None)));
        });
        Ok((200, self.snapshot()))
    }

    // ------------------------------------------------------------ storage

    /// Moves the working directory: stops idle VMs, then renames (same
    /// volume) or copies with holes preserved. Builds wait until it finishes.
    fn move_storage(self: &Arc<Self>, body: Value) -> Reply {
        let target = if body["default"].as_bool() == Some(true) {
            self.config.clone()
        } else {
            PathBuf::from(
                body["path"]
                    .as_str()
                    .filter(|p| !p.is_empty())
                    .ok_or_else(|| bad("path must be a folder path"))?,
            )
        };
        let from = self.root();
        let mut state = self.state.lock().unwrap();
        if state.image.building() {
            return Err((409, "Wait for the system image build to finish".into()));
        }
        if state.jobs.iter().any(|j| !j.status().finished())
            || state
                .slots
                .iter()
                .any(|s| matches!(s.state, SlotState::Busy | SlotState::Starting))
        {
            return Err((
                409,
                "Wait for builds to finish before moving the working directory".into(),
            ));
        }
        let to = storage::validate_target(&from, &self.config, &target).map_err(bad)?;
        // The default location keeps its own spelling so no storage.json is written.
        let to = if self.config.canonicalize().ok() == Some(to.clone()) {
            self.config.clone()
        } else {
            to
        };
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut storage = self.storage.lock().unwrap();
            if storage.moving.is_some() {
                return Err((409, "The working directory is already being moved".into()));
            }
            if self.packages.busy() {
                return Err((
                    409,
                    "Packages are being installed or packed; try again when they finish".into(),
                ));
            }
            storage.error = None;
            storage.moving = Some(Move {
                from: from.clone(),
                to: to.clone(),
                phase: "stopping",
                done: 0,
                total: 0,
                started_at: crate::serial::now_ms(),
                cancel: cancel.clone(),
            });
        }
        let mut vms = Vec::new();
        for slot in state.slots.iter_mut() {
            if slot.state == SlotState::Ready {
                slot.state = SlotState::Stopping;
                vms.push((slot.config.id.clone(), slot.vm.clone()));
            }
        }
        drop(state);
        self.changed();
        let service = self.clone();
        std::thread::spawn(move || {
            for (id, vm) in vms {
                service.stop_vm(&id, vm);
            }
            // Slots another request is stopping finish on their own threads.
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            while std::time::Instant::now() < deadline
                && service
                    .state
                    .lock()
                    .unwrap()
                    .slots
                    .iter()
                    .any(|s| s.state == SlotState::Stopping)
            {
                std::thread::sleep(Duration::from_millis(200));
            }
            service.set_move(|m| m.phase = "moving");
            let result = storage::move_items(
                &from,
                &to,
                &|done, total| {
                    service.set_move(|m| {
                        m.done = done;
                        m.total = total;
                    })
                },
                &|| cancel.load(Ordering::SeqCst),
            )
            .and_then(|()| {
                storage::save_root(&service.config, &to).map_err(|error| {
                    // Keep data and configuration together.
                    let _ = storage::move_items(&to, &from, &|_, _| {}, &|| false);
                    format!("Could not save the working directory: {error}")
                })
            });
            if result.is_ok() {
                service.relocate(&to);
            }
            let mut storage = service.storage.lock().unwrap();
            storage.moving = None;
            storage.error = result.err();
            drop(storage);
            service.changed();
        });
        Ok((200, self.snapshot()))
    }

    fn set_move(&self, update: impl FnOnce(&mut Move)) {
        if let Some(moving) = self.storage.lock().unwrap().moving.as_mut() {
            update(moving);
        }
        self.version.send_modify(|v| *v += 1);
    }

    /// Points every store at the new working directory.
    fn relocate(&self, root: &Path) {
        *self.root.write().unwrap() = root.to_path_buf();
        self.cache.relocate(root.join("cache"));
        self.artifacts.relocate(root.join("artifacts"));
        let mut state = self.state.lock().unwrap();
        for slot in state.slots.iter_mut() {
            slot.dir = root.join("slots").join(&slot.config.id);
            slot.refresh_disk();
        }
        let log = std::mem::take(&mut state.image.log);
        state.image = image::ImageState::load(root);
        state.image.log = log;
        drop(state);
        self.reload_packages(root);
    }

    /// Stops every VM; used when the service exits.
    pub fn shutdown(&self) {
        self.packages.stop();
        if let Some(moving) = self.storage.lock().unwrap().moving.as_ref() {
            moving.cancel.store(true, Ordering::SeqCst);
        }
        let handles: Vec<_> = self
            .state
            .lock()
            .unwrap()
            .slots
            .iter()
            .map(|s| s.vm.clone())
            .collect();
        for job in self.state.lock().unwrap().jobs.iter() {
            job.request_cancel();
        }
        for handle in handles {
            if let Ok(mut guard) = handle.try_lock() {
                if let Some(mut running) = guard.take() {
                    running.stop(Duration::from_secs(10));
                }
            }
        }
    }
}

fn prune_history(jobs: &mut Vec<Arc<Job>>) {
    let finished = jobs.iter().filter(|j| j.status().finished()).count();
    if finished > HISTORY {
        let mut excess = finished - HISTORY;
        jobs.retain(|j| {
            if excess > 0 && j.status().finished() {
                excess -= 1;
                false
            } else {
                true
            }
        });
    }
}

/// Stable guest directory name for a host workspace.
pub fn workspace_key(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(path.as_os_str().as_encoded_bytes());
    digest[..10].iter().map(|b| format!("{b:02x}")).collect()
}

pub fn validate_request(request: &jobs::BuildRequest) -> Result<(), String> {
    if !request.workspace.is_absolute() {
        return Err("workspace must be an absolute path on the service computer".into());
    }
    if request.command.is_empty()
        || request.command.len() > 256
        || request
            .command
            .iter()
            .any(|a| a.len() > 8192 || a.contains('\0'))
    {
        return Err("command must be a nonempty argument array".into());
    }
    if !request.cwd.is_empty() && !protocol::valid_relative(&request.cwd) {
        return Err("cwd must be a relative path inside the workspace".into());
    }
    if request.env.len() > 64
        || !request
            .env
            .iter()
            .all(|(k, v)| valid_env_name(k) && v.len() <= 8192 && !v.contains('\0'))
    {
        return Err("env must map variable names to strings".into());
    }
    if request.artifacts.len() > 64 || !request.artifacts.iter().all(|p| protocol::valid_pattern(p))
    {
        return Err("artifacts must be relative glob patterns (*, ?, **)".into());
    }
    if request.exclude.len() > 64 || !request.exclude.iter().all(|p| protocol::valid_pattern(p)) {
        return Err("exclude must be relative glob patterns".into());
    }
    if request
        .timeout_seconds
        .is_some_and(|t| t == 0 || t > 7 * 24 * 3600)
    {
        return Err("timeoutSeconds must be between 1 second and 7 days".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate_and_reject_unsafe_settings() {
        let settings = Settings::defaults();
        settings.validate().unwrap();
        let mut bad = settings.clone();
        bad.image.apt_mirror = Some("ftp://mirror".into());
        assert!(bad.validate().is_err());
        let mut bad = settings.clone();
        bad.caches.custom.push(CustomCache {
            name: "bazel".into(),
            env: "X".into(),
            max_gib: 1,
        });
        assert!(bad.validate().is_err());
        let mut bad = settings.clone();
        bad.bazelrc.push("build --x\nstartup --y".into());
        assert!(bad.validate().is_err());
        let mut bad = settings;
        bad.slots[0].cpus = host_cpus() + 1;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn the_working_directory_moves_and_survives_a_restart() {
        let base = tempfile::tempdir().unwrap();
        let data = base.path().join("data");
        let service = Service::new(&data);
        let config = data.join("compile");
        assert_eq!(service.root(), config);
        fs::write(config.join("cache/bazel-entry"), b"x").unwrap();
        let wait = |service: &Arc<Service>| {
            for _ in 0..200 {
                if !service.moving() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            panic!("the move did not finish");
        };
        let target = base.path().join("work");
        let reply = service.handle("POST", &["storage"], "", json!({ "path": target }), false);
        assert_eq!(reply.unwrap_err().0, 403);
        service
            .handle("POST", &["storage"], "", json!({ "path": target }), true)
            .unwrap();
        wait(&service);
        let target = target.canonicalize().unwrap();
        assert_eq!(service.root(), target);
        assert!(target.join("cache/bazel-entry").is_file());
        assert!(!config.join("cache").exists());
        assert!(config.join("settings.json").is_file());
        let view = service.snapshot()["storage"].clone();
        assert_eq!(view["isDefault"], false);
        let caches = view["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == "caches")
            .unwrap()
            .clone();
        assert_eq!(caches["path"], json!(target.join("cache")));
        assert_eq!(caches["exists"], true);
        assert_eq!(Service::new(&data).root(), target);
        // Back to the default location.
        service
            .handle("POST", &["storage"], "", json!({ "default": true }), true)
            .unwrap();
        wait(&service);
        assert_eq!(service.root(), config);
        assert!(config.join("cache/bazel-entry").is_file());
        assert!(service.snapshot()["storage"]["error"].is_null());
    }

    #[test]
    fn build_requests_are_validated() {
        let ok = jobs::BuildRequest {
            workspace: "/tmp".into(),
            command: vec!["bazel".into(), "build".into(), "//...".into()],
            cwd: "app".into(),
            env: [("CC".to_string(), "gcc".to_string())].into(),
            artifacts: vec!["bazel-bin/**/*.update.tar.zlib".into()],
            exclude: vec![],
            timeout_seconds: Some(600),
            collect_on_failure: false,
        };
        validate_request(&ok).unwrap();
        for bad in [
            jobs::BuildRequest {
                workspace: "relative".into(),
                ..ok.clone()
            },
            jobs::BuildRequest {
                command: vec![],
                ..ok.clone()
            },
            jobs::BuildRequest {
                cwd: "../x".into(),
                ..ok.clone()
            },
            jobs::BuildRequest {
                artifacts: vec!["/etc/passwd".into()],
                ..ok.clone()
            },
            jobs::BuildRequest {
                timeout_seconds: Some(0),
                ..ok.clone()
            },
        ] {
            assert!(validate_request(&bad).is_err());
        }
        assert_eq!(workspace_key(Path::new("/a")).len(), 20);
        assert!(protocol::valid_key(&workspace_key(Path::new("/a"))));
    }
}
