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
pub mod resources;
pub mod storage;
pub mod sync;
pub mod vm;
mod work;
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
        Arc, Condvar, Mutex, RwLock, Weak,
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
pub struct ArtifactPolicy {
    pub keep_last: u32,
    pub max_gib: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub slots: Vec<SlotConfig>,
    /// One capacity budget shared by every worker and preparation VM.
    #[serde(default = "default_vm_resources")]
    pub vm_resources: resources::Amount,
    pub caches: CacheSettings,
    #[serde(default = "default_auto_stop_idle")]
    pub auto_stop_idle: bool,
    pub idle_minutes: u32,
    pub artifacts: ArtifactPolicy,
}

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

fn default_auto_stop_idle() -> bool {
    true
}

fn default_vm_resources() -> resources::Amount {
    resources::Amount {
        cpus: host_cpus(),
        memory_mib: (host_memory_mib() / 2).max(2048).min(host_memory_mib()),
    }
}

fn new_slot_id() -> String {
    format!("s{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

impl Settings {
    pub fn defaults() -> Settings {
        Settings {
            vm_resources: default_vm_resources(),
            slots: vec![SlotConfig {
                id: new_slot_id(),
                name: "Slot 1".into(),
            }],
            caches: CacheSettings {
                bazel: CacheLimit { max_gib: 50 },
                ccache: CcacheSettings {
                    max_gib: 20,
                    local_gib: 5,
                },
                custom: Vec::new(),
            },
            auto_stop_idle: true,
            idle_minutes: 10,
            artifacts: ArtifactPolicy {
                keep_last: 100,
                max_gib: 20,
            },
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.vm_resources.cpus == 0 || self.vm_resources.cpus > host_cpus() {
            return Err(format!(
                "The total VM CPU budget must be 1–{} virtual CPUs",
                host_cpus()
            ));
        }
        if self.vm_resources.memory_mib < 2048 || self.vm_resources.memory_mib > host_memory_mib() {
            return Err(
                "The total VM memory budget must be at least 2 GiB and no larger than host memory"
                    .into(),
            );
        }
        if self.slots.len() > MAX_SLOTS {
            return Err(format!("At most {MAX_SLOTS} slots"));
        }
        for slot in &self.slots {
            validate_slot(slot)?;
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
        if self.artifacts.keep_last == 0 || self.artifacts.keep_last > 10_000 {
            return Err("Keep between 1 and 10000 artifacts".into());
        }
        Ok(())
    }

    fn worker_resources(&self, slots: usize) -> resources::Amount {
        let count = slots.max(1) as u64;
        resources::Amount {
            cpus: (self.vm_resources.cpus / count as u32).max(1),
            memory_mib: (self.vm_resources.memory_mib / count).max(1024),
        }
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
            // Runtime/cache policy belongs to GizMoth. Project-specific flags
            // and variables are supplied by each build, never global settings.
            bazelrc: Vec::new(),
            env: Vec::new(),
            path: Vec::new(),
            shares: Vec::new(),
            legacy_work: String::new(),
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

/// CPU and RAM are configured through one shared budget, then reserved for
/// each worker or preparation VM. Slot endpoints only configure work disks.
fn reject_slot_resource_limits(body: &Value) -> Result<(), (u16, String)> {
    if body.get("diskGiB").is_some() || body.get("diskGib").is_some() {
        return Err(bad(
            "Work storage uses host directories; per-slot disk limits are not supported",
        ));
    }
    if body.get("cpus").is_some() || body.get("memoryMiB").is_some() {
        return Err(bad(
            "CPU and memory are configured globally in Settings > Compiling, not per slot",
        ));
    }
    Ok(())
}

fn validate_slot(slot: &SlotConfig) -> Result<(), String> {
    if slot.name.trim().is_empty() || slot.name.chars().count() > 40 {
        return Err("Slot names must have 1–40 characters".into());
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
    /// Remains booked through startup, warm reuse and complete VM teardown.
    allocation: Option<Arc<resources::Lease>>,
    pub started_at: Option<u64>,
    pub last_used: u64,
    /// Boot settings used by the current VM; busy VMs may finish on an old generation.
    boot_generation: u64,
    /// Source share attached to this warm VM.
    workspace: Option<String>,
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
            allocation: None,
            started_at: None,
            last_used: 0,
            boot_generation: 0,
            workspace: None,
            work: None,
            disk_allocated: 0,
            dir,
        };
        slot.refresh_disk();
        slot
    }

    fn reusable(&self, generation: u64) -> bool {
        self.state == SlotState::Ready && self.boot_generation == generation
    }

    pub fn legacy_disk(&self) -> PathBuf {
        self.dir.join("work.img")
    }
    pub fn work_dir(&self) -> PathBuf {
        self.dir.join("work")
    }

    pub fn refresh_disk(&mut self) {
        self.disk_allocated = work::allocated(&self.dir);
    }

    fn view(&self, jobs: &[Arc<Job>], planned: resources::Amount) -> Value {
        let amount = self
            .allocation
            .as_ref()
            .map_or(planned, |lease| lease.amount());
        let job = self.job.as_ref().and_then(|id| jobs.iter().find(|j| &j.id == id)).map(|j| {
            let state = j.state.lock().unwrap();
            json!({ "id": j.id, "client": j.client, "title": j.title, "status": state.status, "startedAt": state.started_at })
        });
        json!({
            "id": self.config.id, "name": self.config.name, "cpus": amount.cpus,
            "memoryMiB": amount.memory_mib, "resourcePolicy": "shared", "allocated": self.allocation.is_some(),
            "state": self.state, "error": self.error, "job": job,
            "startedAt": self.started_at, "lastUsed": if self.last_used == 0 { Value::Null } else { self.last_used.into() },
            "disk": { "allocated": self.disk_allocated, "path": self.work_dir(), "size": null,
                      "used": self.disk_allocated, "free": null },
        })
    }
}

pub struct State {
    pub settings: Settings,
    boot_generation: u64,
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
    resources: Arc<resources::Pool>,
    /// Serializes VM starts with shutdown, including preparation and removed slots.
    processes: Mutex<Vec<Weak<vm::Process>>>,
    stopping: AtomicBool,
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
        let service = Self::load(data_dir);
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

    /// Loads persisted state separately from starting background work.
    fn load(data_dir: &Path) -> Arc<Service> {
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
        // Earlier versions built the image in the working directory.
        image::remove_legacy(&dir);
        let image = image::ImageState::load();
        let packages = packages::Packages::new(&config, &dir);
        let resource_pool = resources::Pool::new(settings.vm_resources);
        let service = Arc::new(Service {
            resources: resource_pool,
            artifacts: artifacts::Store::new(dir.join("artifacts")),
            packages,
            config,
            root: RwLock::new(dir),
            storage: Mutex::new(StorageState::default()),
            state: Mutex::new(State {
                settings,
                boot_generation: 1,
                slots,
                jobs: Vec::new(),
                image,
            }),
            wake: Condvar::new(),
            version,
            cache,
            processes: Mutex::new(Vec::new()),
            stopping: AtomicBool::new(false),
            order: AtomicU64::new(1),
        });
        let _ = service.save_settings();
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

    /// Kernel and system disk of the bundled Linux image.
    pub(crate) fn image_files(&self) -> Result<PathBuf, String> {
        self.state.lock().unwrap().image.files()
    }

    fn moving(&self) -> bool {
        self.storage.lock().unwrap().moving.is_some()
    }

    fn save_settings(&self) -> Result<(), String> {
        let settings = self.state.lock().unwrap().settings.clone();
        self.write_settings(&settings)
    }

    fn write_settings(&self, settings: &Settings) -> Result<(), String> {
        let path = self.config.join("settings.json");
        let temporary = path.with_extension("json.tmp");
        fs::write(
            &temporary,
            serde_json::to_vec_pretty(&settings).map_err(|e| e.to_string())?,
        )
        .and_then(|_| fs::rename(&temporary, &path))
        .map_err(|e| e.to_string())
    }

    /// Warm idle VMs may be stopped during an upgrade; unfinished work may not.
    pub(crate) fn retirement_blocked(&self) -> bool {
        if self.moving() || self.packages.busy() {
            return true;
        }
        let state = self.state.lock().unwrap();
        state.jobs.iter().any(|job| !job.status().finished())
            || state.slots.iter().any(|slot| {
                matches!(
                    slot.state,
                    SlotState::Starting | SlotState::Busy | SlotState::Stopping
                )
            })
    }

    /// True while builds run, wait, or VMs are up: the service must stay alive.
    pub fn busy(&self) -> bool {
        if self.moving() || self.packages.busy() {
            return true;
        }
        let state = self.state.lock().unwrap();
        state.jobs.iter().any(|j| !j.status().finished())
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
        if self.stopping.load(Ordering::SeqCst) {
            return Err("The compile service is shutting down".into());
        }
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
        while !self.stopping.load(Ordering::SeqCst) {
            // Retire stale warm VMs before dispatch can reuse them.
            self.stop_idle(&mut state);
            self.dispatch(&mut state);
            state = self
                .wake
                .wait_timeout(state, Duration::from_secs(2))
                .unwrap()
                .0;
        }
    }

    fn dispatch(self: &Arc<Self>, state: &mut State) {
        if self.stopping.load(Ordering::SeqCst) {
            return;
        }
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
        if !state.image.ready() {
            for job in queued {
                job.finish(Status::Failed, Some(image::MISSING.into()), None);
            }
            self.version.send_modify(|v| *v += 1);
            return;
        }
        if self.package_mount().is_none() {
            return;
        }
        let mut remaining = Vec::new();
        for job in queued {
            // Prefer a warm VM, then a stopped slot.
            let index = state
                .slots
                .iter()
                .position(|s| s.reusable(state.boot_generation))
                .or_else(|| {
                    state
                        .slots
                        .iter()
                        .position(|s| matches!(s.state, SlotState::Stopped | SlotState::Failed))
                });
            match index {
                Some(index) => {
                    let planned = state.settings.worker_resources(state.slots.len());
                    let slot = &mut state.slots[index];
                    if slot.state != SlotState::Ready {
                        let Some(lease) = self.resources.try_reserve(planned) else {
                            remaining.push(job);
                            continue;
                        };
                        slot.allocation = Some(lease);
                    }
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
            if slot.state == SlotState::Ready
                && (slot.boot_generation != state.boot_generation
                    || (state.settings.auto_stop_idle && now.saturating_sub(slot.last_used) > idle))
            {
                slot.state = SlotState::Stopping;
                let (service, id, vm) = (self.clone(), slot.config.id.clone(), slot.vm.clone());
                std::thread::spawn(move || service.stop_vm(&id, vm));
            }
        }
    }

    /// Releases unused warm capacity without interrupting running builds.
    pub(crate) fn stop_idle_vms(self: &Arc<Self>) {
        let mut state = self.state.lock().unwrap();
        self.stop_ready_vms(&mut state);
    }

    fn stop_ready_vms(self: &Arc<Self>, state: &mut State) {
        for slot in &mut state.slots {
            if slot.state == SlotState::Ready {
                slot.state = SlotState::Stopping;
                let (service, id, vm) = (self.clone(), slot.config.id.clone(), slot.vm.clone());
                std::thread::spawn(move || service.stop_vm(&id, vm));
            }
        }
    }

    /// Preparation shares exactly the same pool as workers. Cached roots let
    /// queued builds take precedence over background repacking.
    pub(crate) fn reserve_preparation(self: &Arc<Self>) -> Option<Arc<resources::Lease>> {
        let mut state = self.state.lock().unwrap();
        if self.stopping.load(Ordering::SeqCst) {
            return None;
        }
        let amount = resources::Amount {
            cpus: state.settings.vm_resources.cpus.min(4),
            memory_mib: 2048,
        };
        if let Some(lease) = self.resources.try_reserve(amount) {
            return Some(lease);
        }
        let queued = state.jobs.iter().any(|j| !j.status().finished());
        if !queued {
            self.stop_ready_vms(&mut state);
        }
        None
    }

    fn stop_vm(&self, id: &str, vm: VmHandle) {
        if let Some(mut running) = vm.lock().unwrap().take() {
            running.stop(Duration::from_secs(
                if self.stopping.load(Ordering::SeqCst) {
                    10
                } else {
                    20
                },
            ));
        }
        let mut state = self.state.lock().unwrap();
        if let Some(slot) = state.slots.iter_mut().find(|s| s.config.id == id) {
            if slot.state == SlotState::Stopping {
                slot.state = SlotState::Stopped;
            }
            slot.started_at = None;
            slot.allocation = None;
            slot.refresh_disk();
        }
        drop(state);
        self.changed();
        self.packages.notify();
    }

    /// Own every child before shutdown may release the daemon's lifetime lock.
    /// The start callback must not take service state or package locks. A real
    /// start is bounded by OpenVMM's 15-second socket readiness timeout.
    fn launch_vm(&self, start: impl FnOnce() -> Result<vm::Vm, String>) -> Result<vm::Vm, String> {
        let mut processes = self.processes.lock().unwrap();
        if self.stopping.load(Ordering::SeqCst) {
            return Err("The compile service is shutting down".into());
        }
        let machine = start()?;
        processes.retain(|process| process.strong_count() > 0);
        processes.push(Arc::downgrade(&machine.process()));
        Ok(machine)
    }

    /// Starts the slot's VM if needed and returns its vsock socket.
    pub(crate) fn ensure_vm(&self, slot_id: &str, job: &Job) -> Result<PathBuf, String> {
        let (handle, settings, dir, generation, vm_generation, workspace, allocation, planned) = {
            let state = self.state.lock().unwrap();
            let slot = state
                .slots
                .iter()
                .find(|s| s.config.id == slot_id)
                .ok_or("The slot was removed")?;
            (
                slot.vm.clone(),
                state.settings.clone(),
                slot.dir.clone(),
                state.boot_generation,
                slot.boot_generation,
                slot.workspace.clone(),
                slot.allocation
                    .clone()
                    .ok_or("The slot has no VM budget reservation")?,
                state.settings.worker_resources(state.slots.len()),
            )
        };
        let mut guard = handle.lock().unwrap();
        if self.stopping.load(Ordering::SeqCst) || job.cancelled() {
            return Err("cancelled".into());
        }
        if let Some(vm) = guard.as_mut() {
            if vm.alive() && vm_generation == generation && workspace.as_ref() == Some(&job.key) {
                return Ok(vm.socket.clone());
            }
            vm.stop(Duration::from_secs(20));
        }
        *guard = None;
        self.set_slot(slot_id, |slot| {
            if slot.state != SlotState::Stopping {
                slot.state = SlotState::Starting;
            }
        });
        job.note(&format!(
            "Starting VM ({} CPU, {} MiB)",
            allocation.amount().cpus,
            allocation.amount().memory_mib
        ));
        let kernel = self.image_files()?;
        let agent = assets::agent()?;
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut work = work::Directory::prepare(&dir)?;
        let initramfs = dir.join("initramfs.cpio");
        // Packages: the latest packed erofs disk, read-only in every slot.
        let mut boot = settings.boot_config(slot_id);
        let (pack, packed) = self
            .package_mount()
            .ok_or("Linux formula downloads are not prepared yet")?;
        let mut disks = vec![(pack, true)];
        boot.legacy_work = work.fingerprint.clone();
        if let Some(legacy) = &work.legacy_clone {
            disks.push((legacy.clone(), false));
        }
        let (path, variables) = packages::guest_environment(&packed)?;
        boot.path = path;
        boot.env.splice(0..0, variables);
        boot.shares.push(protocol::Share {
            port: protocol::SHARE_PORT,
            path: "/src".into(),
            writable: false,
        });
        boot.shares.push(protocol::Share {
            port: protocol::WORK_SHARE_PORT,
            path: "/work".into(),
            writable: true,
        });
        assets::write_initramfs(&initramfs, &agent, &boot, &[])?;
        let spec = vm::Spec {
            name: slot_id.to_string(),
            cpus: allocation.amount().cpus,
            memory_mib: allocation.amount().memory_mib,
            kernel,
            initramfs,
            disks,
            // Compile workers need dependency access; GizMoth owns networking.
            network: true,
            console: dir.join("console.log"),
            runtime: vm::runtime_dir()?,
            shares: vec![
                (protocol::SHARE_PORT, job.request.workspace.clone(), false),
                (protocol::WORK_SHARE_PORT, work.path.clone(), true),
            ],
        };
        if self.stopping.load(Ordering::SeqCst) || job.cancelled() {
            return Err("cancelled".into());
        }
        let mut started =
            self.launch_vm(|| vm::Vm::start(&spec, Some(self.cache.clone()), allocation.clone()))?;
        let hello = started.wait_ready(
            if work.fingerprint.is_empty() {
                READY_TIMEOUT
            } else {
                Duration::from_secs(15 * 60)
            },
            &|| self.stopping.load(Ordering::SeqCst) || job.cancelled(),
        )?;
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
                let _ = image;
                work
            }
            _ => None,
        };
        work.publish()?;
        let socket = started.socket.clone();
        let started_at = started.started_at;
        *guard = Some(started);
        // Publish boot metadata before a concurrent shutdown can take the VM
        // and clear it; late startup must not revive an already stopped slot.
        self.set_slot(slot_id, |slot| {
            if slot.state != SlotState::Stopping {
                slot.state = SlotState::Busy;
            }
            slot.boot_generation = if allocation.amount() == planned {
                generation
            } else {
                0
            };
            slot.workspace = Some(job.key.clone());
            slot.started_at = Some(started_at);
            slot.work = work_info;
        });
        drop(guard);
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
            _ if compile_available() && !state.image.ready() => {
                json!({ "ok": false, "message": image::MISSING })
            }
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
            "image": state.image.view(),
            "slots": state.slots.iter().map(|s| s.view(&state.jobs, state.settings.worker_resources(state.slots.len()))).collect::<Vec<_>>(),
            "resources": self.resources.snapshot(),
            "queue": queue,
            "jobs": jobs,
            "caches": caches,
            "artifacts": { "count": artifacts.len(), "bytes": artifacts.iter().map(|a| a.bytes).sum::<u64>(), "dir": self.artifacts.root().display().to_string() },
            "storage": self.storage_view(&state, &artifacts),
            "packages": self.packages_view(&state),
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
            _ if !app => forbidden(),
            ("POST", ["jobs", id, "cancel"]) => self.cancel(id),
            ("POST", ["prepare"]) => {
                self.packages_resume();
                Ok((200, self.snapshot()))
            }
            ("POST", ["packages", "refresh"]) => {
                self.packages_request(true);
                Ok((200, self.snapshot()))
            }
            ("POST", ["packages", "pause"]) => {
                self.packages_pause();
                Ok((200, self.snapshot()))
            }
            ("DELETE", ["packages", "downloads"]) if !moving => {
                self.clear_downloads()?;
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
            ("DELETE", ["packages"]) => {
                self.mutate_package(body, false)?;
                Ok((200, self.snapshot()))
            }
            ("POST", ["packages", "reinstall"]) => {
                self.mutate_package(body, true)?;
                Ok((200, self.snapshot()))
            }
            ("POST", ["storage"]) => self.move_storage(body),
            ("PATCH", ["settings"]) => self.patch_settings(body),
            ("POST", ["slots"]) => self.add_slot(body),
            ("POST", ["slots", "stop-idle"]) => {
                self.stop_idle_vms();
                Ok((200, self.snapshot()))
            }
            ("PATCH", ["slots", id]) => self.patch_slot(id, body),
            ("DELETE", ["slots", id]) => self.remove_slot(id),
            ("POST", ["slots", id, "stop"]) => self.stop_slot(id),
            ("POST", ["slots", id, "reset"]) => self.reset_slot(id),
            ("DELETE", ["caches", name]) => {
                self.cache.clear(name).map_err(|e| bad(e.to_string()))?;
                Ok((200, self.snapshot()))
            }
            ("DELETE", ["artifacts"]) => {
                let storage = self.storage.lock().unwrap();
                if storage.moving.is_some() {
                    return Err((
                        409,
                        "The working directory is being moved; try again when it finishes".into(),
                    ));
                }
                let result = self.artifacts.clear_unpinned();
                drop(storage);
                self.changed();
                result.map_err(bad)?;
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
        let boot_changed = settings.caches != state.settings.caches
            || settings.vm_resources != state.settings.vm_resources;
        let allocated = self.resources.snapshot().allocated;
        if allocated.cpus > settings.vm_resources.cpus
            || allocated.memory_mib > settings.vm_resources.memory_mib
        {
            return Err((409, format!("Stop idle VMs or wait for builds before lowering the total VM budget below allocated capacity ({} vCPUs / {} MiB)", allocated.cpus, allocated.memory_mib)));
        }
        self.resources
            .reconfigure(settings.vm_resources, || self.write_settings(&settings))
            .map_err(bad)?;
        state.settings = settings.clone();
        // Includes Starting and Busy VMs: they may finish their current job,
        // but cannot be reused with an obsolete environment or network policy.
        if boot_changed {
            state.boot_generation += 1;
        }
        drop(state);
        self.cache.configure(settings.namespaces());
        self.apply_retention();
        self.changed();
        Ok((200, self.snapshot()))
    }

    fn add_slot(self: &Arc<Self>, body: Value) -> Reply {
        reject_slot_resource_limits(&body)?;
        let mut state = self.state.lock().unwrap();
        if state.slots.len() >= MAX_SLOTS {
            return Err(bad(format!("At most {MAX_SLOTS} slots")));
        }
        let config = SlotConfig {
            id: new_slot_id(),
            name: body["name"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Slot {}", state.slots.len() + 1)),
        };
        validate_slot(&config).map_err(bad)?;
        state.settings.slots.push(config.clone());
        state.slots.push(Slot::new(config, &self.root()));
        state.boot_generation += 1;
        drop(state);
        self.save_settings().map_err(bad)?;
        self.changed();
        Ok((200, self.snapshot()))
    }

    fn patch_slot(self: &Arc<Self>, id: &str, body: Value) -> Reply {
        reject_slot_resource_limits(&body)?;
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
        validate_slot(&config).map_err(bad)?;
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
        if slot.state == SlotState::Stopping {
            return Err((409, "The slot is already stopping".into()));
        }
        if matches!(slot.state, SlotState::Busy | SlotState::Starting) {
            return Err((409, "The slot is running a build; cancel it first".into()));
        }
        let handle = (slot.vm.clone(), slot.dir.clone());
        if remove {
            let mut settings = state.settings.clone();
            settings.slots.retain(|s| s.id != id);
            self.write_settings(&settings).map_err(bad)?;
            state.settings = settings;
            state.slots.remove(index);
            state.boot_generation += 1;
        } else {
            slot.state = SlotState::Stopping;
        }
        Ok(handle)
    }

    fn remove_slot(self: &Arc<Self>, id: &str) -> Reply {
        let (vm, dir) = self.take_idle_slot(id, true)?;
        self.changed();
        let service = self.clone();
        std::thread::spawn(move || {
            if let Some(mut running) = vm.lock().unwrap().take() {
                running.stop(Duration::from_secs(20));
            }
            {
                let mut storage = service.storage.lock().unwrap();
                if let Err(error) = work::clear(&dir) {
                    storage.error = Some(error);
                }
            }
            service.changed();
            service.packages.notify();
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

    /// Stops the VM and clears its private host work directory and any retained
    /// legacy image. Sources, Bazel state, local ccache and home are recreated.
    fn reset_slot(self: &Arc<Self>, id: &str) -> Reply {
        let (vm, dir) = self.take_idle_slot(id, false)?;
        let (service, id) = (self.clone(), id.to_string());
        std::thread::spawn(move || {
            if let Some(mut running) = vm.lock().unwrap().take() {
                running.stop(Duration::from_secs(20));
            }
            let result = {
                let _storage = service.storage.lock().unwrap();
                work::clear(&dir)
            };
            service.set_slot(&id, |slot| {
                slot.work = None;
                slot.error = result.err();
                if slot.error.is_some() {
                    slot.state = SlotState::Failed;
                }
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
        drop(state);
        self.reload_packages(root);
    }

    /// Stops every VM; used when the service exits.
    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.packages.stop();
        if let Some(moving) = self.storage.lock().unwrap().moving.as_ref() {
            moving.cancel.store(true, Ordering::SeqCst);
        }
        let handles: Vec<_> = {
            let mut state = self.state.lock().unwrap();
            for job in &state.jobs {
                job.request_cancel();
                if job.status() == Status::Queued {
                    job.finish(Status::Cancelled, None, None);
                }
            }
            state
                .slots
                .iter_mut()
                .map(|slot| {
                    slot.state = SlotState::Stopping;
                    (slot.config.id.clone(), slot.vm.clone())
                })
                .collect()
        };
        self.changed();
        let processes: Vec<_> = self
            .processes
            .lock()
            .unwrap()
            .drain(..)
            .filter_map(|process| process.upgrade())
            .collect();
        // No service, package, startup or VM-handle locks are held here. Reap
        // preparation and removed-slot children too, even if their session or
        // teardown thread is blocked and cannot cooperate with cancellation.
        for process in processes {
            process.stop();
        }
        // Startup owns the VM mutex until it either installs a VM or tears
        // down its failed boot. Wait for that teardown instead of skipping it.
        for (id, handle) in handles {
            self.stop_vm(&id, handle);
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
    use std::{
        process::{Command, Stdio},
        time::Instant,
    };

    fn wait_for(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(
                Instant::now() < deadline,
                "lifecycle operation did not finish"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn queued_job(service: &Service) -> Arc<Job> {
        try_queue(service).unwrap()
    }

    fn try_queue(service: &Service) -> Result<Arc<Job>, String> {
        service
            .submit(
                "test".into(),
                "test build".into(),
                jobs::BuildRequest {
                    workspace: service.root(),
                    command: vec!["true".into()],
                    cwd: String::new(),
                    env: Default::default(),
                    artifacts: Vec::new(),
                    exclude: Vec::new(),
                    timeout_seconds: None,
                    collect_on_failure: false,
                },
            )
            .map(|(job, _)| job)
    }

    fn child_alive(pid: u32) -> bool {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    fn kill_child(pid: u32) {
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) }, 0);
    }

    fn attach_process(service: &Service, slot_state: SlotState) -> (VmHandle, u32) {
        let mut state = service.state.lock().unwrap();
        let amount = state.settings.worker_resources(state.slots.len());
        let generation = state.boot_generation;
        let slot = &mut state.slots[0];
        let lease = service.resources.try_reserve(amount).unwrap();
        fs::create_dir_all(&slot.dir).unwrap();
        fs::write(slot.legacy_disk(), b"persistent work disk").unwrap();
        let child = Command::new("/bin/sleep")
            .arg("300")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let machine = service
            .launch_vm(|| Ok(vm::Vm::test_process(child, &slot.dir, lease.clone())))
            .unwrap();
        slot.started_at = Some(machine.started_at);
        *slot.vm.lock().unwrap() = Some(machine);
        slot.allocation = Some(lease);
        slot.state = slot_state;
        slot.boot_generation = generation;
        slot.last_used = crate::serial::now_ms();
        (slot.vm.clone(), pid)
    }

    fn service_with_pack(data: &Path) -> Arc<Service> {
        let root = data.join("compile");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(packages::PACK), b"test pack").unwrap();
        fs::write(
            root.join(packages::PACK_INFO),
            serde_json::to_vec(&json!({
                "agentVersion": protocol::AGENT_VERSION, "hash": "test", "bytes": 9,
                "builtAt": 0, "packages": []
            }))
            .unwrap(),
        )
        .unwrap();
        let service = Service::load(data);
        service.state.lock().unwrap().image.kernel = Some(root.join("test-kernel"));
        service
    }

    #[test]
    fn defaults_validate_and_reject_unsafe_settings() {
        let settings = Settings::defaults();
        settings.validate().unwrap();
        let mut bad = settings.clone();
        bad.caches.custom.push(CustomCache {
            name: "bazel".into(),
            env: "X".into(),
            max_gib: 1,
        });
        assert!(bad.validate().is_err());
        let mut bad = settings;
        bad.slots[0].name.clear();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn legacy_settings_preserve_slots_and_disks_with_the_default_shared_budget() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("compile");
        fs::create_dir_all(&config).unwrap();
        let mut legacy = serde_json::to_value(Settings::defaults()).unwrap();
        legacy.as_object_mut().unwrap().remove("vmResources");
        legacy.as_object_mut().unwrap().remove("autoStopIdle");
        legacy["network"] = json!(false);
        legacy["bazelrc"] = json!(["build --jobs=99"]);
        legacy["env"] = json!([{ "name": "LEGACY_GLOBAL", "value": "ignored" }]);
        legacy["excludes"] = json!(["src/**"]);
        legacy["slots"] = json!([
            { "id": "s12345678", "name": "Existing slot", "cpus": 1, "memoryMib": 2048, "diskGib": 128 },
            { "id": "s87654321", "name": "Second slot", "cpus": 2, "memoryMiB": 4096, "diskGib": 64 }
        ]);
        for id in ["s12345678", "s87654321"] {
            let directory = config.join("slots").join(id);
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("work.img"), id).unwrap();
        }
        fs::write(
            config.join("settings.json"),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        let service = Service::load(directory.path());
        let view = service.snapshot();
        let budget = default_vm_resources();
        assert_eq!(view["slots"][0]["id"], "s12345678");
        assert_eq!(view["slots"][0]["name"], "Existing slot");
        assert!(view["slots"][0].get("diskGiB").is_none());
        assert_eq!(view["slots"][1]["id"], "s87654321");
        assert!(view["slots"][1].get("diskGiB").is_none());
        assert_eq!(view["settings"]["vmResources"], json!(budget));
        assert_eq!(view["resources"]["limit"], json!(budget));
        assert_eq!(view["resources"]["vmCount"], 0);
        for (index, id) in ["s12345678", "s87654321"].iter().enumerate() {
            assert_eq!(view["slots"][index]["resourcePolicy"], "shared");
            assert_eq!(view["slots"][index]["cpus"], (budget.cpus / 2).max(1));
            assert_eq!(
                view["slots"][index]["memoryMiB"],
                (budget.memory_mib / 2).max(1024)
            );
            assert_eq!(
                fs::read(config.join("slots").join(id).join("work.img")).unwrap(),
                id.as_bytes()
            );
            assert!(view["settings"]["slots"][index].get("cpus").is_none());
            assert!(view["settings"]["slots"][index].get("memoryMib").is_none());
            assert!(view["settings"]["slots"][index].get("memoryMiB").is_none());
        }
        let saved: Settings =
            serde_json::from_slice(&fs::read(config.join("settings.json")).unwrap()).unwrap();
        let saved_value = serde_json::to_value(&saved).unwrap();
        for key in ["bazelrc", "env", "excludes", "network"] {
            assert!(view["settings"].get(key).is_none());
            assert!(saved_value.get(key).is_none());
        }
        let boot = saved.boot_config("test");
        assert!(boot.bazelrc.is_empty());
        assert!(boot.env.is_empty());
        assert!(saved.auto_stop_idle);
        assert_eq!(saved.vm_resources, budget);
        assert_eq!(saved.slots, service.state.lock().unwrap().settings.slots);
    }

    #[test]
    fn parallel_slots_plan_within_the_shared_budget_and_reject_manual_limits() {
        let directory = tempfile::tempdir().unwrap();
        let service = Service::load(directory.path());
        let id = service.state.lock().unwrap().slots[0].config.id.clone();
        for limits in [json!({"cpus": 1}), json!({"memoryMiB": 1024})] {
            assert_eq!(service.add_slot(limits.clone()).unwrap_err().0, 400);
            assert_eq!(service.patch_slot(&id, limits).unwrap_err().0, 400);
        }
        service.add_slot(json!({"name": "Parallel slot"})).unwrap();
        let view = service.snapshot();
        let planned = service.state.lock().unwrap().settings.worker_resources(2);
        for slot in view["slots"].as_array().unwrap() {
            assert_eq!(slot["cpus"], planned.cpus);
            assert_eq!(slot["memoryMiB"], planned.memory_mib);
            assert_eq!(slot["resourcePolicy"], "shared");
            assert_eq!(slot["allocated"], false);
        }
        assert_eq!(
            view["resources"]["allocated"],
            json!({ "cpus": 0, "memoryMiB": 0 })
        );
    }

    #[test]
    fn preparation_and_workers_share_admission_and_exhausted_jobs_stay_queued() {
        let directory = tempfile::tempdir().unwrap();
        let service = service_with_pack(directory.path());
        let preparation = service.reserve_preparation().unwrap();
        let job = queued_job(&service);
        let handle = service.state.lock().unwrap().slots[0].vm.clone();
        let guard = handle.lock().unwrap();
        {
            let mut state = service.state.lock().unwrap();
            service.dispatch(&mut state);
            assert_eq!(job.status(), Status::Queued);
            let view = job.state.lock().unwrap();
            assert_eq!(view.position, Some(1));
            assert_eq!(view.slot, None);
            assert_eq!(view.started_at, None);
            assert_eq!(state.slots[0].state, SlotState::Stopped);
            assert!(state.slots[0].allocation.is_none());
            assert_eq!(service.resources.snapshot().allocated, preparation.amount());
            drop(view);
            drop(preparation);
            service.dispatch(&mut state);
            assert_eq!(job.status(), Status::Booting);
            assert_eq!(state.slots[0].state, SlotState::Starting);
            assert_eq!(
                service.resources.snapshot().allocated,
                state.settings.vm_resources
            );
            assert_eq!(service.resources.snapshot().vm_count, 1);
            // The worker cannot acquire service state until this guard drops.
            // Force an early boot error, before assets or any hypervisor launch.
            state.image.kernel = None;
        }
        assert!(service.reserve_preparation().is_none());
        drop(guard);
        wait_for(|| job.status().finished() && service.resources.snapshot().vm_count == 0);
        assert_eq!(job.status(), Status::Failed);
        assert_eq!(
            service.state.lock().unwrap().slots[0].state,
            SlotState::Stopped
        );
        assert!(service.reserve_preparation().is_some());
    }

    #[test]
    fn lowering_the_budget_below_workers_or_preparation_never_changes_saved_settings() {
        let directory = tempfile::tempdir().unwrap();
        let service = Service::load(directory.path());
        service
            .patch_settings(json!({ "vmResources": { "cpus": host_cpus(), "memoryMiB": 4096 } }))
            .unwrap();
        let id = service.state.lock().unwrap().slots[0].config.id.clone();
        let (handle, pid) = attach_process(&service, SlotState::Ready);
        let saved = fs::read(service.config.join("settings.json")).unwrap();
        let generation = service.state.lock().unwrap().boot_generation;
        let before = service.resources.snapshot();
        let error = service
            .patch_settings(json!({ "vmResources": { "cpus": host_cpus(), "memoryMiB": 2048 } }))
            .unwrap_err();
        assert_eq!(error.0, 409);
        assert_eq!(
            fs::read(service.config.join("settings.json")).unwrap(),
            saved
        );
        assert_eq!(service.resources.snapshot(), before);
        assert_eq!(
            service.state.lock().unwrap().settings.vm_resources,
            before.limit
        );
        assert_eq!(service.state.lock().unwrap().boot_generation, generation);
        drop(handle);
        kill_child(pid);
        service.stop_slot(&id).unwrap();
        wait_for(|| service.resources.snapshot().vm_count == 0);
        service
            .patch_settings(json!({ "vmResources": { "cpus": host_cpus(), "memoryMiB": 2048 } }))
            .unwrap();
        assert_eq!(service.resources.snapshot().limit.memory_mib, 2048);

        // A preparation lease is included in the same rejection. On a single
        // CPU host there is no smaller valid CPU setting to request.
        if host_cpus() > 1 {
            let preparation = service.reserve_preparation().unwrap();
            let saved = fs::read(service.config.join("settings.json")).unwrap();
            let error = service
                .patch_settings(json!({ "vmResources": { "cpus": 1, "memoryMiB": 2048 } }))
                .unwrap_err();
            assert_eq!(error.0, 409);
            assert_eq!(
                fs::read(service.config.join("settings.json")).unwrap(),
                saved
            );
            assert_eq!(service.resources.snapshot().allocated, preparation.amount());
        }
    }

    #[test]
    fn stop_reset_and_remove_keep_capacity_until_process_teardown() {
        for operation in ["stop", "reset", "remove"] {
            let directory = tempfile::tempdir().unwrap();
            let service = Service::load(directory.path());
            let (handle, pid) = attach_process(&service, SlotState::Ready);
            let (id, disk) = {
                let state = service.state.lock().unwrap();
                (
                    state.slots[0].config.id.clone(),
                    state.slots[0].legacy_disk(),
                )
            };
            let guard = handle.lock().unwrap();
            let before = service.resources.snapshot();
            match operation {
                "stop" => service.stop_slot(&id),
                "reset" => service.reset_slot(&id),
                "remove" => service.remove_slot(&id),
                _ => unreachable!(),
            }
            .unwrap();
            wait_for(|| {
                let state = service.state.lock().unwrap();
                state.slots.is_empty() || state.slots[0].state == SlotState::Stopping
            });
            assert_eq!(
                service.resources.snapshot(),
                before,
                "{operation} released a live VM"
            );
            assert!(child_alive(pid));
            assert!(disk.is_file(), "{operation} deleted a disk still in use");
            assert!(service.reserve_preparation().is_none());
            if operation != "remove" {
                for reply in [
                    service.stop_slot(&id),
                    service.reset_slot(&id),
                    service.remove_slot(&id),
                ] {
                    assert_eq!(reply.unwrap_err().0, 409);
                }
            }
            kill_child(pid);
            drop(guard);
            wait_for(|| service.resources.snapshot().vm_count == 0);
            assert!(!child_alive(pid));
            if matches!(operation, "reset" | "remove") {
                wait_for(|| !disk.exists());
            } else {
                assert!(disk.is_file());
                assert_eq!(
                    service.state.lock().unwrap().slots[0].state,
                    SlotState::Stopped
                );
            }
        }
    }

    #[test]
    fn reset_reports_detach_failure_and_can_be_retried_without_partial_deletion() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return; // Root bypasses the write permission failure used here.
        }
        let directory = tempfile::tempdir().unwrap();
        let service = Service::load(directory.path());
        let (id, dir) = {
            let state = service.state.lock().unwrap();
            (state.slots[0].config.id.clone(), state.slots[0].dir.clone())
        };
        fs::create_dir_all(dir.join("work/home")).unwrap();
        fs::write(dir.join("work/home/kept"), b"keep until reset succeeds").unwrap();
        let parent = dir.parent().unwrap();
        fs::set_permissions(parent, fs::Permissions::from_mode(0o500)).unwrap();
        service.reset_slot(&id).unwrap();
        wait_for(|| service.state.lock().unwrap().slots[0].state == SlotState::Failed);
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            fs::read(dir.join("work/home/kept")).unwrap(),
            b"keep until reset succeeds"
        );
        assert!(service.state.lock().unwrap().slots[0]
            .error
            .as_ref()
            .unwrap()
            .contains("Could not clear"));
        service.reset_slot(&id).unwrap();
        wait_for(|| service.state.lock().unwrap().slots[0].state == SlotState::Stopped);
        assert!(!dir.exists());
        assert!(service.state.lock().unwrap().slots[0].error.is_none());
    }

    #[test]
    fn shutdown_reaps_workers_and_removed_slots_without_their_vm_handle_lock() {
        for removed in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let service = Service::load(directory.path());
            let (handle, pid) = attach_process(&service, SlotState::Ready);
            let (id, disk) = {
                let state = service.state.lock().unwrap();
                (
                    state.slots[0].config.id.clone(),
                    state.slots[0].legacy_disk(),
                )
            };
            let guard = handle.lock().unwrap();
            if removed {
                service.remove_slot(&id).unwrap();
            }
            let (done, received) = std::sync::mpsc::channel();
            let shutdown = {
                let service = service.clone();
                std::thread::spawn(move || {
                    service.shutdown();
                    done.send(()).unwrap();
                })
            };
            // The owner/teardown thread cannot acquire this VM handle, but
            // shutdown must still reap the independently registered child.
            wait_for(|| !child_alive(pid));
            if removed {
                received.recv_timeout(Duration::from_secs(5)).unwrap();
                assert_eq!(service.resources.snapshot().vm_count, 0);
            } else {
                // The remaining slot lease is cleared after handle cleanup.
                assert_eq!(service.resources.snapshot().vm_count, 1);
                service.release_slot(&id, true);
                assert_eq!(
                    service.state.lock().unwrap().slots[0].state,
                    SlotState::Stopping
                );
                assert!(matches!(
                    received.try_recv(),
                    Err(std::sync::mpsc::TryRecvError::Empty)
                ));
            }
            assert!(disk.is_file());
            assert!(service.reserve_preparation().is_none());
            assert!(try_queue(&service).err().unwrap().contains("shutting down"));
            drop(guard);
            shutdown.join().unwrap();
            wait_for(|| service.resources.snapshot().vm_count == 0);
            if removed {
                wait_for(|| !disk.exists());
            }
        }
    }

    #[test]
    fn shutdown_waits_for_preparation_spawn_and_reaps_a_noncooperative_session() {
        let directory = tempfile::tempdir().unwrap();
        let service = Service::load(directory.path());
        let allocation = service.reserve_preparation().unwrap();
        let (starting, started) = std::sync::mpsc::channel();
        let (proceed, permitted) = std::sync::mpsc::channel();
        let (process, pid) = std::sync::mpsc::channel();
        let (finish, finished) = std::sync::mpsc::channel();
        let session = {
            let service = service.clone();
            std::thread::spawn(move || {
                let mut child_pid = 0;
                let machine = service
                    .launch_vm(|| {
                        starting.send(()).unwrap();
                        permitted.recv().unwrap();
                        let child = Command::new("/bin/sleep")
                            .arg("300")
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .spawn()
                            .unwrap();
                        child_pid = child.id();
                        Ok(vm::Vm::test_process(child, &service.root(), allocation))
                    })
                    .unwrap();
                process.send(child_pid).unwrap();
                // Model a pack session that ignores cancellation and keeps
                // owning its Vm until well after shutdown must have returned.
                finished.recv().unwrap();
                drop(machine);
            })
        };
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        let (done, received) = std::sync::mpsc::channel();
        let shutdown = {
            let service = service.clone();
            std::thread::spawn(move || {
                service.shutdown();
                done.send(()).unwrap();
            })
        };
        wait_for(|| service.stopping.load(Ordering::SeqCst));
        assert!(matches!(
            received.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        assert_eq!(service.resources.snapshot().vm_count, 1);
        proceed.send(()).unwrap();
        let pid = pid.recv_timeout(Duration::from_secs(5)).unwrap();
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!child_alive(pid));
        assert_eq!(service.resources.snapshot().vm_count, 0);
        assert!(service
            .launch_vm(|| panic!("shutdown admitted another VM"))
            .is_err());
        assert!(service.reserve_preparation().is_none());
        finish.send(()).unwrap();
        session.join().unwrap();
        shutdown.join().unwrap();
    }

    #[test]
    fn shutdown_waits_for_startup_and_cancels_queued_jobs() {
        let directory = tempfile::tempdir().unwrap();
        let service = service_with_pack(directory.path());
        let starting = queued_job(&service);
        let queued = queued_job(&service);
        let handle = service.state.lock().unwrap().slots[0].vm.clone();
        let guard = handle.lock().unwrap();
        {
            let mut state = service.state.lock().unwrap();
            service.dispatch(&mut state);
            assert_eq!(starting.status(), Status::Booting);
            assert_eq!(queued.status(), Status::Queued);
            state.image.kernel = None;
        }
        let (done, received) = std::sync::mpsc::channel();
        let shutdown = {
            let service = service.clone();
            std::thread::spawn(move || {
                service.shutdown();
                done.send(()).unwrap();
            })
        };
        wait_for(|| queued.status() == Status::Cancelled);
        assert!(starting.cancelled());
        assert_eq!(service.resources.snapshot().vm_count, 1);
        assert!(matches!(
            received.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        assert!(service.reserve_preparation().is_none());
        drop(guard);
        shutdown.join().unwrap();
        received.recv().unwrap();
        wait_for(|| {
            starting.status() == Status::Cancelled && service.resources.snapshot().vm_count == 0
        });
        assert_eq!(
            service.state.lock().unwrap().slots[0].state,
            SlotState::Stopped
        );
    }

    #[test]
    fn failed_removal_persistence_keeps_the_slot_vm_and_reservation() {
        let directory = tempfile::tempdir().unwrap();
        let service = Service::load(directory.path());
        let (_, pid) = attach_process(&service, SlotState::Ready);
        let (id, generation) = {
            let state = service.state.lock().unwrap();
            (state.slots[0].config.id.clone(), state.boot_generation)
        };
        let saved = fs::read(service.config.join("settings.json")).unwrap();
        let before = service.resources.snapshot();
        let temporary = service.config.join("settings.json.tmp");
        fs::create_dir(&temporary).unwrap();
        assert_eq!(service.remove_slot(&id).unwrap_err().0, 400);
        assert_eq!(
            fs::read(service.config.join("settings.json")).unwrap(),
            saved
        );
        assert_eq!(service.resources.snapshot(), before);
        assert!(child_alive(pid));
        {
            let state = service.state.lock().unwrap();
            assert_eq!(state.boot_generation, generation);
            assert_eq!(state.settings.slots.len(), 1);
            assert_eq!(state.slots[0].state, SlotState::Ready);
            assert!(state.slots[0].vm.lock().unwrap().is_some());
        }
        fs::remove_dir(&temporary).unwrap();
        kill_child(pid);
        service.remove_slot(&id).unwrap();
        wait_for(|| service.resources.snapshot().vm_count == 0);
    }

    #[test]
    fn guest_session_errors_retire_a_live_vm_and_release_its_reservation() {
        let directory = tempfile::tempdir().unwrap();
        let service = Service::load(directory.path());
        let job = queued_job(&service);
        let (_, pid) = attach_process(&service, SlotState::Busy);
        let id = {
            let mut state = service.state.lock().unwrap();
            let slot = &mut state.slots[0];
            slot.workspace = Some(job.key.clone());
            slot.job = Some(job.id.clone());
            slot.config.id.clone()
        };
        // Warm reuse succeeds, then the absent guest socket fails immediately.
        // The ordinary child is still alive and must be torn down on error.
        jobs::run(service.clone(), job.clone(), id);
        assert_eq!(job.status(), Status::Failed);
        assert!(job
            .state
            .lock()
            .unwrap()
            .error
            .as_ref()
            .unwrap()
            .contains("VM agent"));
        assert_eq!(service.resources.snapshot().vm_count, 0);
        assert!(!child_alive(pid));
        let state = service.state.lock().unwrap();
        assert_eq!(state.slots[0].state, SlotState::Stopped);
        assert!(state.slots[0].allocation.is_none());
        assert!(state.slots[0].vm.lock().unwrap().is_none());
    }

    #[test]
    fn slot_count_and_budget_changes_retire_stale_warm_capacity() {
        for change in ["add", "remove", "budget"] {
            for initial in [SlotState::Ready, SlotState::Busy] {
                let directory = tempfile::tempdir().unwrap();
                let service = Service::load(directory.path());
                service
                    .patch_settings(
                        json!({ "vmResources": { "cpus": host_cpus(), "memoryMiB": 4096 } }),
                    )
                    .unwrap();
                service.add_slot(json!({})).unwrap();
                let (handle, pid) = attach_process(&service, initial);
                let guard = handle.lock().unwrap();
                let (id, other, generation) = {
                    let state = service.state.lock().unwrap();
                    (
                        state.slots[0].config.id.clone(),
                        state.slots[1].config.id.clone(),
                        state.boot_generation,
                    )
                };
                match change {
                    "add" => service.add_slot(json!({})),
                    "remove" => service.remove_slot(&other),
                    "budget" => service.patch_settings(
                        json!({ "vmResources": { "cpus": host_cpus(), "memoryMiB": 3072 } }),
                    ),
                    _ => unreachable!(),
                }
                .unwrap();
                {
                    let state = service.state.lock().unwrap();
                    assert_eq!(state.boot_generation, generation + 1);
                    assert_eq!(state.slots[0].state, initial);
                    assert!(!state.slots[0].reusable(state.boot_generation));
                }
                if initial == SlotState::Busy {
                    service.release_slot(&id, true);
                }
                {
                    let mut state = service.state.lock().unwrap();
                    service.stop_idle(&mut state);
                    assert_eq!(state.slots[0].state, SlotState::Stopping);
                }
                assert_eq!(service.resources.snapshot().vm_count, 1);
                assert!(child_alive(pid));
                kill_child(pid);
                drop(guard);
                wait_for(|| service.resources.snapshot().vm_count == 0);
                assert_eq!(
                    service.state.lock().unwrap().slots[0].state,
                    SlotState::Stopped
                );
            }
        }
    }

    #[test]
    fn the_working_directory_moves_and_survives_a_restart() {
        let base = tempfile::tempdir().unwrap();
        let data = base.path().join("data");
        let service = Service::load(&data);
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
        assert_eq!(Service::load(&data).root(), target);
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
    fn boot_settings_invalidate_busy_and_ready_vms_before_reuse() {
        for initial in [SlotState::Busy, SlotState::Ready] {
            let directory = tempfile::tempdir().unwrap();
            let service = Service::load(directory.path());
            let id = {
                let mut state = service.state.lock().unwrap();
                let generation = state.boot_generation;
                let slot = &mut state.slots[0];
                slot.boot_generation = generation;
                slot.state = initial;
                slot.last_used = crate::serial::now_ms();
                slot.config.id.clone()
            };
            let mut caches = service.state.lock().unwrap().settings.caches.clone();
            caches.ccache.local_gib += 1;
            service.patch_settings(json!({ "caches": caches })).unwrap();
            {
                let state = service.state.lock().unwrap();
                assert_eq!(state.boot_generation, 2);
                assert!(!state.slots[0].reusable(state.boot_generation));
                if initial == SlotState::Busy {
                    assert_eq!(
                        state.slots[0].state,
                        SlotState::Busy,
                        "settings must not interrupt this build"
                    );
                }
            }
            if initial == SlotState::Busy {
                // Completion must not make the old environment reusable.
                service.release_slot(&id, true);
            }
            let mut state = service.state.lock().unwrap();
            assert!(!state.slots[0].reusable(state.boot_generation));
            service.stop_idle(&mut state);
            assert!(matches!(
                state.slots[0].state,
                SlotState::Stopping | SlotState::Stopped
            ));
        }
    }

    #[test]
    fn auto_stop_idle_toggle_preserves_warm_vms_but_not_stale_boot_policy() {
        let directory = tempfile::tempdir().unwrap();
        let service = Service::load(directory.path());
        service
            .patch_settings(json!({"autoStopIdle": false, "idleMinutes": 1}))
            .unwrap();
        let saved: Settings = serde_json::from_slice(
            &fs::read(directory.path().join("compile/settings.json")).unwrap(),
        )
        .unwrap();
        assert!(!saved.auto_stop_idle);
        {
            let mut state = service.state.lock().unwrap();
            state.slots[0].state = SlotState::Ready;
            state.slots[0].boot_generation = state.boot_generation;
            state.slots[0].last_used = 0;
            service.stop_idle(&mut state);
            assert_eq!(state.slots[0].state, SlotState::Ready);
            state.slots[0].boot_generation = 0;
            service.stop_idle(&mut state);
            assert_eq!(state.slots[0].state, SlotState::Stopping);
        }
        wait_for(|| service.state.lock().unwrap().slots[0].state == SlotState::Stopped);
        service
            .patch_settings(json!({"autoStopIdle": true}))
            .unwrap();
        let mut state = service.state.lock().unwrap();
        state.slots[0].state = SlotState::Ready;
        state.slots[0].boot_generation = state.boot_generation;
        state.slots[0].last_used = 0;
        service.stop_idle(&mut state);
        assert_eq!(state.slots[0].state, SlotState::Stopping);
    }

    #[test]
    fn global_build_overrides_are_rejected_without_changing_settings() {
        let directory = tempfile::tempdir().unwrap();
        let service = Service::load(directory.path());
        let path = directory.path().join("compile/settings.json");
        let before = fs::read(&path).unwrap();
        for patch in [
            json!({"network": false}),
            json!({"bazelrc": ["build --jobs=99"]}),
            json!({"env": [{"name": "GLOBAL", "value": "bad"}]}),
            json!({"excludes": ["src/**"]}),
        ] {
            let (status, message) = service.patch_settings(patch).unwrap_err();
            assert_eq!(status, 400);
            assert!(message.contains("Unknown setting"));
            assert_eq!(fs::read(&path).unwrap(), before);
        }
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
