//! Linux development packages for compile VMs.
//!
//! Nobody picks packages: the service downloads every linux-arm64 release of
//! the GizClaw/gizmoth-devenv registry into `<working directory>/assets`
//! (verified, by SHA-256), drops releases the registry removes, and has a
//! packer VM unpack them and build the read-only erofs disk `packages.erofs`.
//! Every slot attaches the latest pack at `/opt/gizmoth/packages`, with the
//! default versions on `PATH`.
//!
//! Unpacking happens in Linux, not here: Linux trees can hold names that differ
//! only by case, which a case-insensitive APFS volume cannot. The packer reads
//! the downloads over 9P.
use super::guest::protocol::{
    self, BootConfig, BootMode, PackCheck, PackItem, PackPlan, PackStep, Share,
};
use super::{assets, image, jobs::LogRing, vm, Service, SlotState};
use crate::devenv::registry::{self, Check, Kind, Recipe, Snapshot, Source, Step};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{Duration, Instant},
};

pub const PLATFORM: &str = "linux-arm64";
/// Where builds see the packages.
pub const GUEST_ROOT: &str = "/opt/gizmoth/packages";
/// `${cache}` in registry variables: a per-slot cache on the work disk.
const GUEST_CACHE: &str = "/work/home/.cache";
const REFRESH_EVERY: Duration = Duration::from_secs(12 * 3600);
const RETRY_AFTER: Duration = Duration::from_secs(30 * 60);
const TICK: Duration = Duration::from_secs(30);

/// Downloaded releases, in the working directory.
pub const INDEX: &str = "packages.json";
pub const PACK: &str = "packages.erofs";
pub const PACK_INFO: &str = "packages.erofs.json";

type Key = (Kind, String, String);

/// One downloaded release and how the packer installs it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Package {
    pub kind: String,
    pub id: String,
    pub name: String,
    pub version: String,
    /// SHA-256 of the release manifest.
    pub manifest: String,
    pub download: u64,
    pub downloaded_at: u64,
    /// The item's default version: on `PATH` in builds.
    pub default: bool,
    pub prepend_path: Vec<String>,
    pub variables: BTreeMap<String, String>,
    pub steps: Vec<PackStep>,
    pub checks: Vec<PackCheck>,
}

impl Package {
    fn key(&self) -> Option<Key> {
        Some((
            Kind::parse(&self.kind).ok()?,
            self.id.clone(),
            self.version.clone(),
        ))
    }

    /// Directory below the pack root, for example `toolchains/python/3.12.16`.
    pub fn dir(&self) -> String {
        let kind = Kind::parse(&self.kind).map_or("other", Kind::directory);
        format!("{kind}/{}/{}", self.id, self.version)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Index {
    pub packages: Vec<Package>,
}

/// Identifies a set of packages; a pack is current when it has this hash.
fn hash(packages: &[Package]) -> String {
    use sha2::{Digest, Sha256};
    let mut entries: Vec<_> = packages
        .iter()
        .map(|p| format!("{}/{}/{}/{}", p.kind, p.id, p.version, p.manifest))
        .collect();
    entries.sort();
    registry::hex_digest(&Sha256::digest(entries.join("\n").as_bytes()))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackInfo {
    hash: String,
    bytes: u64,
    built_at: u64,
    packages: Vec<Package>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedSnapshot {
    commit: String,
    fetched_at: u64,
    files: BTreeMap<String, String>,
}

struct Progress {
    key: Key,
    done: u64,
    total: u64,
}

#[derive(Default)]
struct PackState {
    running: bool,
    phase: Option<String>,
    /// (hash of the set, error, when).
    error: Option<(String, String, Instant)>,
    log: LogRing,
}

#[derive(Default)]
struct State {
    catalog: Option<Arc<Snapshot>>,
    refreshing: bool,
    refresh_requested: bool,
    refresh_error: Option<String>,
    refresh_attempt: Option<Instant>,
    /// Upkeep runs once compiling is in use: packages exist, a build was
    /// submitted or Settings asked to prepare.
    active: bool,
    busy: bool,
    downloading: Option<Progress>,
    failures: BTreeMap<Key, (String, Instant)>,
    index: Index,
    pack_info: Option<PackInfo>,
    pack: PackState,
}

pub struct Packages {
    state: Mutex<State>,
    wake: Condvar,
    cancel: AtomicBool,
    /// `<app data>/compile/registry`: the registry checkout and snapshots.
    registry: PathBuf,
}

impl Packages {
    pub fn new(config: &Path, root: &Path) -> Packages {
        let registry = config.join("registry");
        // Earlier versions kept the registry for the macOS Dev Env pages.
        if let Some(old) = config.parent().map(|p| p.join("devenv").join("registry")) {
            if old.is_dir() && !registry.exists() {
                let _ = fs::rename(&old, &registry);
            }
        }
        let packages = Packages {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            cancel: AtomicBool::new(false),
            registry,
        };
        {
            let mut state = packages.state.lock().unwrap();
            state.catalog = packages.load_catalog().ok().flatten().map(Arc::new);
            load_root(&mut state, root);
        }
        packages
    }

    fn load_catalog(&self) -> Result<Option<Snapshot>, String> {
        let current = match fs::read(self.registry.join("current.json")) {
            Ok(bytes) => bytes,
            Err(_) => return Ok(None),
        };
        let commit = serde_json::from_slice::<Value>(&current)
            .ok()
            .and_then(|v| v["commit"].as_str().map(str::to_owned))
            .filter(|c| registry::valid_commit(c))
            .ok_or("invalid registry cache")?;
        let saved: SavedSnapshot = serde_json::from_slice(
            &fs::read(self.registry.join(format!("{commit}.json"))).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        registry::parse_snapshot(&saved.commit, saved.fetched_at, &saved.files).map(Some)
    }

    fn publish_catalog(&self, saved: SavedSnapshot) -> Result<Snapshot, String> {
        let snapshot = registry::parse_snapshot(&saved.commit, saved.fetched_at, &saved.files)?;
        fs::create_dir_all(&self.registry).map_err(|e| e.to_string())?;
        let name = format!("{}.json", saved.commit);
        write_atomic(&self.registry.join(&name), &saved)?;
        write_atomic(
            &self.registry.join("current.json"),
            &json!({ "commit": saved.commit }),
        )?;
        if let Ok(entries) = fs::read_dir(&self.registry) {
            for entry in entries.flatten() {
                let file = entry.file_name().to_string_lossy().into_owned();
                if file != "current.json" && file != name && file.ends_with(".json") {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        Ok(snapshot)
    }

    fn notify(&self) {
        self.wake.notify_all();
    }

    pub fn busy(&self) -> bool {
        self.state.lock().unwrap().busy
    }

    pub fn activate(&self) {
        let mut state = self.state.lock().unwrap();
        if !state.active {
            state.active = true;
            drop(state);
            self.notify();
        }
    }

    pub fn stop(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.notify();
    }
}

fn load_root(state: &mut State, root: &Path) {
    state.index = fs::read(root.join(INDEX))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    state.pack_info = fs::read(root.join(PACK_INFO))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(|_| root.join(PACK).is_file());
    if !state.index.packages.is_empty() || state.pack_info.is_some() {
        state.active = true;
    }
}

fn write_atomic(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
    )
    .and_then(|_| fs::rename(&temporary, path))
    .map_err(|e| format!("{}: {e}", path.display()))
}

/// A release the registry publishes for the guest platform.
struct Wanted {
    key: Key,
    name: String,
    manifest: String,
    default: bool,
    download: u64,
    unsupported: Option<String>,
}

fn unsupported(recipe: &Recipe) -> Option<String> {
    if let Some(source) = recipe
        .sources
        .iter()
        .find(|s| !matches!(s, Source::File { .. }))
    {
        return Some(format!("source {} is not a file", source.id()));
    }
    recipe.install.iter().find_map(|step| match step {
        Step::Copy { .. } => None,
        Step::Extract { format, .. } if format == "tar.gz" => None,
        other => Some(format!("install step {} is not supported", other.name())),
    })
}

fn wanted(snapshot: &Snapshot) -> Vec<Wanted> {
    let mut list = Vec::new();
    for (kind, items) in [
        (Kind::Toolchain, &snapshot.toolchains),
        (Kind::Sdk, &snapshot.sdks),
    ] {
        for item in items {
            for (index, release) in item.releases.iter().enumerate() {
                let Some((manifest, digest)) = snapshot.release(kind, &item.id, &release.version)
                else {
                    continue;
                };
                let Some(recipe) = manifest.platforms.get(PLATFORM) else {
                    continue;
                };
                let default = match &item.default_version {
                    Some(version) => version == &release.version,
                    None => index == 0,
                };
                list.push(Wanted {
                    key: (kind, item.id.clone(), release.version.clone()),
                    name: item.name.clone(),
                    manifest: digest.to_owned(),
                    default,
                    download: recipe
                        .sources
                        .iter()
                        .map(|s| match s {
                            Source::File { size, .. } => *size,
                            _ => 0,
                        })
                        .sum(),
                    unsupported: unsupported(recipe),
                });
            }
        }
    }
    list
}

/// The packer's instructions for one recipe; sources become SHA-256 names.
fn plan_steps(recipe: &Recipe) -> Result<(Vec<PackStep>, Vec<PackCheck>), String> {
    let digest = |source: &str| {
        recipe
            .sources
            .iter()
            .find_map(|s| match s {
                Source::File { id, sha256, .. } if id == source => Some(sha256.clone()),
                _ => None,
            })
            .ok_or_else(|| format!("undeclared source {source}"))
    };
    let mut steps = Vec::new();
    for step in &recipe.install {
        steps.push(match step {
            Step::Copy {
                source,
                to,
                executable,
            } => PackStep::Copy {
                source: digest(source)?,
                to: to.clone(),
                executable: *executable,
            },
            Step::Extract {
                source,
                to,
                strip_components,
                ..
            } => PackStep::Extract {
                source: digest(source)?,
                to: to.clone(),
                strip_components: *strip_components,
            },
            other => return Err(format!("install step {} is not supported", other.name())),
        });
    }
    // Command checks run the package's own programs; the packer only checks files.
    let checks = recipe
        .checks
        .iter()
        .filter_map(|check| match check {
            Check::File { path, executable } => Some(PackCheck {
                path: path.clone(),
                executable: *executable,
            }),
            Check::Command { .. } => None,
        })
        .collect();
    Ok((steps, checks))
}

/// `PATH` entries and variables of the default versions, as builds see them.
pub fn guest_environment(packages: &[Package]) -> (Vec<String>, Vec<(String, String)>) {
    let mut path = Vec::new();
    let mut variables = vec![
        ("GIZMOTH_PACKAGES".to_string(), GUEST_ROOT.to_string()),
        // The pack is read-only: Python caches bytecode on the work disk.
        (
            "PYTHONPYCACHEPREFIX".to_string(),
            format!("{GUEST_CACHE}/pycache"),
        ),
    ];
    let mut defaults: Vec<&Package> = packages.iter().filter(|p| p.default).collect();
    defaults.sort_by(|a, b| (&a.kind, &a.id).cmp(&(&b.kind, &b.id)));
    for package in defaults {
        let prefix = format!("{GUEST_ROOT}/{}", package.dir());
        path.extend(
            package
                .prepend_path
                .iter()
                .map(|entry| format!("{prefix}/{entry}")),
        );
        for (name, value) in &package.variables {
            let value = value
                .replace("${cache}", GUEST_CACHE)
                .replace("${prefix}", &prefix);
            variables.push((name.clone(), value));
        }
    }
    (path, variables)
}

/// Reads the size of an erofs filesystem from its superblock.
fn erofs_size(path: &Path) -> Result<u64, String> {
    use std::io::Read;
    let mut header = [0u8; 1024 + 64];
    fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .map_err(|e| format!("read the pack: {e}"))?;
    let superblock = &header[1024..];
    if superblock[..4] != 0xE0F5_E1E2u32.to_le_bytes() {
        return Err("the pack is not an erofs filesystem".into());
    }
    let block_bits = superblock[12];
    let blocks = u32::from_le_bytes(superblock[36..40].try_into().unwrap()) as u64;
    if !(9..=16).contains(&block_bits) || blocks == 0 {
        return Err("the pack has an invalid superblock".into());
    }
    Ok(blocks << block_bits)
}

impl Service {
    /// Runs on its own thread: registry, downloads and pack upkeep, once the
    /// bundled image is present.
    pub(crate) fn maintain(self: Arc<Self>) {
        loop {
            if self.packages.cancel.load(Ordering::SeqCst) {
                return;
            }
            let ready = super::compile_available() && self.state.lock().unwrap().image.ready();
            if ready && self.packages.state.lock().unwrap().active {
                self.refresh_catalog();
                self.sync_packages();
                self.pack_if_needed();
            }
            let state = self.packages.state.lock().unwrap();
            let _ = self.packages.wake.wait_timeout(state, TICK).unwrap();
        }
    }

    fn refresh_catalog(&self) {
        let due = {
            let mut state = self.packages.state.lock().unwrap();
            let stale = state.catalog.as_ref().is_none_or(|c| {
                crate::serial::now_ms().saturating_sub(c.fetched_at)
                    > REFRESH_EVERY.as_millis() as u64
            });
            let retry = state
                .refresh_attempt
                .is_none_or(|at| at.elapsed() > RETRY_AFTER);
            let due = state.refresh_requested || (stale && retry);
            if due {
                state.refresh_requested = false;
                state.refreshing = true;
                state.refresh_attempt = Some(Instant::now());
            }
            due
        };
        if !due {
            return;
        }
        self.changed();
        let repository = self.packages.registry.join("repository");
        let result = tauri::async_runtime::block_on(registry::fetch(&repository)).and_then(
            |(commit, files)| {
                self.packages.publish_catalog(SavedSnapshot {
                    commit,
                    fetched_at: crate::serial::now_ms(),
                    files,
                })
            },
        );
        let mut state = self.packages.state.lock().unwrap();
        state.refreshing = false;
        match result {
            Ok(snapshot) => {
                state.catalog = Some(Arc::new(snapshot));
                state.refresh_error = None;
            }
            Err(error) => state.refresh_error = Some(error),
        }
        drop(state);
        self.changed();
    }

    /// Claims the package files unless the working directory is moving.
    fn begin_package_work(&self) -> bool {
        let storage = self.storage.lock().unwrap();
        let mut state = self.packages.state.lock().unwrap();
        if storage.moving.is_some() || state.busy {
            return false;
        }
        state.busy = true;
        true
    }

    fn end_package_work(&self) {
        let mut state = self.packages.state.lock().unwrap();
        state.busy = false;
        state.downloading = None;
        drop(state);
        self.changed();
    }

    /// Downloads every wanted release that is missing or changed, one at a
    /// time, and forgets releases the registry no longer lists.
    fn sync_packages(self: &Arc<Self>) {
        let Some(catalog) = self.packages.state.lock().unwrap().catalog.clone() else {
            return;
        };
        if !self.begin_package_work() {
            return;
        }
        let root = self.root();
        let wanted = wanted(&catalog);
        {
            let mut state = self.packages.state.lock().unwrap();
            let before = state.index.clone();
            state.index.packages.retain(|p| {
                p.key().is_some_and(|key| {
                    wanted
                        .iter()
                        .any(|w| w.key == key && w.manifest == p.manifest)
                })
            });
            for package in state.index.packages.iter_mut() {
                if let Some(w) = wanted
                    .iter()
                    .find(|w| Some(&w.key) == package.key().as_ref())
                {
                    package.default = w.default;
                    package.name = w.name.clone();
                }
            }
            if state.index != before {
                let _ = write_atomic(&root.join(INDEX), &state.index);
            }
        }
        for item in &wanted {
            if self.packages.cancel.load(Ordering::SeqCst) || self.moving() {
                break;
            }
            let skip = {
                let state = self.packages.state.lock().unwrap();
                state
                    .index
                    .packages
                    .iter()
                    .any(|p| p.key().as_ref() == Some(&item.key))
                    || state
                        .failures
                        .get(&item.key)
                        .is_some_and(|(_, at)| at.elapsed() < RETRY_AFTER)
            };
            if skip || item.unsupported.is_some() {
                continue;
            }
            let result = self.download(&root, &catalog, item);
            let mut state = self.packages.state.lock().unwrap();
            match result {
                Ok(package) => {
                    state.failures.remove(&item.key);
                    state.index.packages.push(package);
                    state.index.packages.sort_by(|a, b| {
                        (&a.kind, &a.id, &a.version).cmp(&(&b.kind, &b.id, &b.version))
                    });
                    if let Err(error) = write_atomic(&root.join(INDEX), &state.index) {
                        state
                            .failures
                            .insert(item.key.clone(), (error, Instant::now()));
                    }
                }
                Err(error) if error == "cancelled" => {}
                Err(error) => {
                    state
                        .failures
                        .insert(item.key.clone(), (error, Instant::now()));
                }
            }
        }
        self.end_package_work();
    }

    fn set_progress(&self, key: &Key, done: u64, total: u64) {
        self.packages.state.lock().unwrap().downloading = Some(Progress {
            key: key.clone(),
            done,
            total,
        });
        self.version.send_modify(|v| *v += 1);
    }

    /// Downloads and verifies a release's sources into `assets/`.
    fn download(&self, root: &Path, catalog: &Snapshot, item: &Wanted) -> Result<Package, String> {
        let (kind, id, version) = &item.key;
        let (release, _) = catalog
            .release(*kind, id, version)
            .ok_or("the release left the registry")?;
        let recipe = release
            .platforms
            .get(PLATFORM)
            .ok_or("no linux-arm64 recipe")?;
        let (steps, checks) = plan_steps(recipe)?;
        let cancel = || self.packages.cancel.load(Ordering::SeqCst);
        let last = Mutex::new(Instant::now() - Duration::from_secs(1));
        let mut offset = 0u64;
        self.set_progress(&item.key, 0, item.download);
        for source in &recipe.sources {
            let Source::File {
                url, sha256, size, ..
            } = source
            else {
                return Err("unsupported source".into());
            };
            assets::fetch_url(
                &root.join("assets"),
                &format!("{id} {version}"),
                url,
                sha256,
                *size,
                &|done, _| {
                    let mut last = last.lock().unwrap();
                    if last.elapsed() >= Duration::from_millis(250) {
                        *last = Instant::now();
                        self.set_progress(&item.key, offset + done, item.download);
                    }
                },
                &cancel,
            )?;
            offset += size;
        }
        Ok(Package {
            kind: kind.as_str().into(),
            id: id.clone(),
            name: item.name.clone(),
            version: version.clone(),
            manifest: item.manifest.clone(),
            download: item.download,
            downloaded_at: crate::serial::now_ms(),
            default: item.default,
            prepend_path: recipe.environment.prepend_path.clone(),
            variables: recipe.environment.variables.clone(),
            steps,
            checks,
        })
    }

    /// The latest pack and the packages in it, for a slot's next boot.
    pub(crate) fn package_mount(&self) -> Option<(PathBuf, Vec<Package>)> {
        let state = self.packages.state.lock().unwrap();
        let info = state.pack_info.as_ref()?;
        let pack = self.root().join(PACK);
        pack.is_file().then(|| (pack, info.packages.clone()))
    }

    /// Packs the downloaded set when no pack matches it.
    fn pack_if_needed(self: &Arc<Self>) {
        let packages = {
            let state = self.packages.state.lock().unwrap();
            let set = hash(&state.index.packages);
            let current = state.pack_info.as_ref().is_some_and(|i| i.hash == set);
            let failed = state
                .pack
                .error
                .as_ref()
                .is_some_and(|(failed, _, at)| *failed == set && at.elapsed() < RETRY_AFTER);
            if state.index.packages.is_empty() || current || failed {
                return;
            }
            state.index.packages.clone()
        };
        if !self.state.lock().unwrap().image.ready() {
            return;
        }
        if !self.begin_package_work() {
            return;
        }
        {
            let mut state = self.packages.state.lock().unwrap();
            state.pack.running = true;
            state.pack.phase = Some("Starting packer VM".into());
            state.pack.log = LogRing::default();
        }
        self.changed();
        let set = hash(&packages);
        let result = self.pack(&packages);
        let mut state = self.packages.state.lock().unwrap();
        state.pack.running = false;
        state.pack.phase = None;
        match result {
            Ok(info) => {
                state.pack_info = Some(info);
                state.pack.error = None;
            }
            Err(error) => {
                // A package that failed to install leaves the next pack.
                if let Some(package) = packages
                    .iter()
                    .find(|p| error.starts_with(&format!("{}: ", p.dir())))
                {
                    if let Some(key) = package.key() {
                        state
                            .failures
                            .insert(key.clone(), (error.clone(), Instant::now()));
                        state
                            .index
                            .packages
                            .retain(|p| p.key() != Some(key.clone()));
                        let _ = write_atomic(&self.root().join(INDEX), &state.index);
                    }
                }
                state.pack.error = Some((set, error, Instant::now()));
            }
        }
        drop(state);
        self.end_package_work();
        // Idle VMs restart on the new pack (and its PATH) before their next build.
        let mut state = self.state.lock().unwrap();
        for slot in state
            .slots
            .iter_mut()
            .filter(|s| s.state == SlotState::Ready)
        {
            slot.last_used = 0;
        }
        drop(state);
        self.changed();
    }

    fn pack(&self, packages: &[Package]) -> Result<PackInfo, String> {
        let root = self.root();
        let download: u64 = packages.iter().map(|p| p.download).sum();
        // Both disks are sparse: they cost only what is written.
        let room = (download * 8 + (4 << 30)).min(512 << 30);
        let target = root.join(format!("{PACK}.new"));
        let scratch = root.join(".pack").join("scratch.img");
        for path in [&target, &scratch] {
            let _ = fs::remove_file(path);
            vm::ensure_disk(path, room)?;
        }
        let (kernel, system) = self.image_files()?;
        let agent = assets::agent()?;
        let index: Vec<Value> = packages
            .iter()
            .map(|p| {
                json!({ "kind": p.kind, "id": p.id, "version": p.version, "default": p.default,
                        "path": format!("{GUEST_ROOT}/{}", p.dir()) })
            })
            .collect();
        let config = BootConfig {
            mode: BootMode::Pack,
            slot: "pack".into(),
            caches: Default::default(),
            bazelrc: Vec::new(),
            env: Vec::new(),
            path: Vec::new(),
            shares: vec![Share {
                port: protocol::SHARE_PORT,
                path: "/mnt".into(),
            }],
            images: Vec::new(),
            pack: Some(PackPlan {
                packages: packages
                    .iter()
                    .map(|p| PackItem {
                        dir: p.dir(),
                        steps: p.steps.clone(),
                        checks: p.checks.clone(),
                    })
                    .collect(),
                index: json!({ "packages": index }),
            }),
        };
        // Packer files stay out of the moved items (see storage::ITEMS).
        let dir = root.join(".pack");
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let initramfs = dir.join("initramfs.cpio");
        assets::write_initramfs(&initramfs, &agent, &config, &[])?;
        let spec = vm::Spec {
            name: "pack".into(),
            cpus: super::host_cpus().clamp(1, 4),
            memory_mib: 2048,
            kernel,
            initramfs: initramfs.clone(),
            disks: vec![
                (system, true),
                (target.clone(), false),
                (scratch.clone(), false),
            ],
            network: false,
            console: dir.join("console.log"),
            runtime: vm::runtime_dir()?,
            shares: vec![(protocol::SHARE_PORT, root.join("assets"))],
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let result = (|| {
            let mut machine = vm::Vm::start(&spec, None)?;
            let outcome = image::session(
                &mut machine,
                &cancel,
                &|bytes| self.packages.state.lock().unwrap().pack.log.push(bytes),
                &|phase| {
                    self.packages.state.lock().unwrap().pack.phase = Some(phase.into());
                    self.changed();
                },
            );
            if outcome.is_err() || !machine.wait_exit(Duration::from_secs(30)) {
                machine.stop(Duration::from_secs(5));
            }
            outcome
        })();
        let _ = fs::remove_file(&initramfs);
        let _ = fs::remove_file(&scratch);
        let finish = result.and_then(|()| {
            let size = erofs_size(&target)?;
            fs::OpenOptions::new()
                .write(true)
                .open(&target)
                .and_then(|file| file.set_len(size))
                .map_err(|e| e.to_string())?;
            fs::rename(&target, root.join(PACK)).map_err(|e| e.to_string())?;
            let info = PackInfo {
                hash: hash(packages),
                bytes: size,
                built_at: crate::serial::now_ms(),
                packages: packages.to_vec(),
            };
            write_atomic(&root.join(PACK_INFO), &info)?;
            Ok(info)
        });
        if finish.is_err() {
            let _ = fs::remove_file(&target);
        }
        finish
    }

    /// Reloads the downloads and pack after the working directory moved.
    pub(crate) fn reload_packages(&self, root: &Path) {
        let mut state = self.packages.state.lock().unwrap();
        let active = state.active;
        load_root(&mut state, root);
        state.active |= active;
        drop(state);
        self.packages.notify();
    }

    /// `prepare` starts upkeep; `refresh` also checks the registry now and
    /// retries anything that failed.
    pub(crate) fn packages_request(&self, refresh: bool) {
        let mut state = self.packages.state.lock().unwrap();
        state.active = true;
        if refresh {
            state.refresh_requested = true;
            state.failures.clear();
            state.pack.error = None;
        }
        drop(state);
        self.packages.notify();
        self.changed();
    }

    pub(crate) fn packages_bytes(&self) -> u64 {
        let state = self.packages.state.lock().unwrap();
        state.pack_info.as_ref().map_or(0, |p| p.bytes)
    }

    pub(crate) fn packages_view(&self) -> Value {
        let state = self.packages.state.lock().unwrap();
        let set = hash(&state.index.packages);
        let packed = |key: &Key, manifest: &str| {
            state.pack_info.as_ref().is_some_and(|info| {
                info.packages
                    .iter()
                    .any(|p| p.key().as_ref() == Some(key) && p.manifest == manifest)
            })
        };
        let downloaded = |key: &Key, manifest: &str| {
            state
                .index
                .packages
                .iter()
                .any(|p| p.key().as_ref() == Some(key) && p.manifest == manifest)
        };
        let mut items = Vec::new();
        let catalog_items = state.catalog.as_deref().map(wanted).unwrap_or_default();
        for item in &catalog_items {
            let progress = state.downloading.as_ref().filter(|c| c.key == item.key);
            let status = if let Some(reason) = &item.unsupported {
                json!({ "state": "unsupported", "error": reason })
            } else if let Some(progress) = progress {
                json!({ "state": "downloading", "done": progress.done, "total": progress.total })
            } else if downloaded(&item.key, &item.manifest) && packed(&item.key, &item.manifest) {
                json!({ "state": "installed" })
            } else if downloaded(&item.key, &item.manifest) {
                json!({ "state": if state.pack.running { "packing" } else { "downloaded" } })
            } else if let Some((error, _)) = state.failures.get(&item.key) {
                json!({ "state": "failed", "error": error })
            } else {
                json!({ "state": "pending" })
            };
            let mut row = json!({
                "kind": item.key.0.as_str(), "id": item.key.1, "version": item.key.2,
                "name": item.name, "default": item.default, "download": item.download,
                "path": format!("{GUEST_ROOT}/{}/{}/{}", item.key.0.directory(), item.key.1, item.key.2),
            });
            row.as_object_mut()
                .unwrap()
                .extend(status.as_object().unwrap().clone());
            items.push(row);
        }
        // Packed releases without registry information (an offline start).
        if let Some(info) = &state.pack_info {
            for package in &info.packages {
                if package
                    .key()
                    .is_some_and(|key| catalog_items.iter().any(|w| w.key == key))
                {
                    continue;
                }
                items.push(json!({
                    "kind": package.kind, "id": package.id, "version": package.version,
                    "name": package.name, "default": package.default, "download": package.download,
                    "path": format!("{GUEST_ROOT}/{}", package.dir()), "state": "installed",
                }));
            }
        }
        let pack = if state.pack.running {
            json!({ "state": "packing", "phase": state.pack.phase })
        } else if state.index.packages.is_empty() && state.pack_info.is_none() {
            json!({ "state": "none" })
        } else if let Some(info) = state.pack_info.as_ref().filter(|i| i.hash == set) {
            json!({ "state": "ready", "bytes": info.bytes, "builtAt": info.built_at })
        } else if let Some((_, error, _)) = state.pack.error.as_ref().filter(|(h, _, _)| *h == set)
        {
            json!({ "state": "failed", "error": error })
        } else {
            json!({ "state": "stale", "bytes": state.pack_info.as_ref().map(|i| i.bytes) })
        };
        let catalog = state.catalog.as_ref();
        json!({
            "active": state.active,
            "busy": state.busy,
            "registry": {
                "repository": registry::REPOSITORY,
                "commit": catalog.map(|c| c.commit.clone()),
                "fetchedAt": catalog.map(|c| c.fetched_at),
                "refreshing": state.refreshing,
                "error": state.refresh_error,
            },
            "platform": PLATFORM,
            "guestPath": GUEST_ROOT,
            "file": self.root().join(PACK),
            "items": items,
            "pack": pack,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(id: &str, version: &str, default: bool) -> Package {
        Package {
            kind: "toolchain".into(),
            id: id.into(),
            name: id.into(),
            version: version.into(),
            manifest: "m".repeat(64),
            download: 10,
            downloaded_at: 0,
            default,
            prepend_path: vec!["bin".into()],
            variables: [("CACHE_HOME".to_string(), "${cache}/x".to_string())].into(),
            steps: Vec::new(),
            checks: Vec::new(),
        }
    }

    #[test]
    fn defaults_go_on_path_and_hashes_follow_the_set() {
        let packages = vec![
            package("python", "3.12.16", true),
            package("python", "3.11.18", false),
            package("bazelisk", "1.30.0", true),
        ];
        let (path, variables) = guest_environment(&packages);
        assert_eq!(
            path,
            [
                "/opt/gizmoth/packages/toolchains/bazelisk/1.30.0/bin",
                "/opt/gizmoth/packages/toolchains/python/3.12.16/bin"
            ]
        );
        assert!(variables.contains(&("CACHE_HOME".into(), "/work/home/.cache/x".into())));
        assert!(variables.contains(&("GIZMOTH_PACKAGES".into(), GUEST_ROOT.into())));
        let mut reordered = packages.clone();
        reordered.reverse();
        assert_eq!(hash(&packages), hash(&reordered));
        let mut changed = packages.clone();
        changed[0].manifest = "n".repeat(64);
        assert_ne!(hash(&packages), hash(&changed));
    }

    #[test]
    fn recipes_become_packer_steps_named_by_digest() {
        let snapshot =
            registry::parse_snapshot(&"a".repeat(40), 0, &registry::tests::files()).unwrap();
        // The fixture publishes macOS recipes only.
        assert!(wanted(&snapshot).is_empty());
        for (kind, item) in snapshot
            .toolchains
            .iter()
            .map(|i| (Kind::Toolchain, i))
            .chain(snapshot.sdks.iter().map(|i| (Kind::Sdk, i)))
        {
            for release in &item.releases {
                let (manifest, _) = snapshot.release(kind, &item.id, &release.version).unwrap();
                for recipe in manifest.platforms.values() {
                    let (steps, _) = plan_steps(recipe).unwrap();
                    assert_eq!(steps.len(), recipe.install.len());
                    for step in steps {
                        let (PackStep::Copy { source, .. } | PackStep::Extract { source, .. }) =
                            step;
                        assert!(registry::valid_sha256(&source), "{source}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_registry_snapshot_survives_a_restart_and_moves_from_dev_env() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("compile");
        let old = dir.path().join("devenv/registry");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("marker"), b"x").unwrap();
        fs::create_dir_all(&config).unwrap();
        let packages = Packages::new(&config, &config);
        assert!(
            config.join("registry/marker").is_file(),
            "old checkout reused"
        );
        let commit = "b".repeat(40);
        packages
            .publish_catalog(SavedSnapshot {
                commit: commit.clone(),
                fetched_at: 7,
                files: registry::tests::files(),
            })
            .unwrap();
        let again = Packages::new(&config, &config);
        let state = again.state.lock().unwrap();
        assert_eq!(state.catalog.as_ref().unwrap().commit, commit);
        assert!(!state.active, "nothing downloaded yet");
    }

    #[test]
    fn erofs_superblocks_give_the_filesystem_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.erofs");
        let mut bytes = vec![0u8; 8192];
        bytes[1024..1028].copy_from_slice(&0xE0F5_E1E2u32.to_le_bytes());
        bytes[1024 + 12] = 12;
        bytes[1024 + 36..1024 + 40].copy_from_slice(&3u32.to_le_bytes());
        fs::write(&path, &bytes).unwrap();
        assert_eq!(erofs_size(&path).unwrap(), 3 * 4096);
        fs::write(&path, vec![0u8; 8192]).unwrap();
        assert!(erofs_size(&path).is_err());
    }
}
