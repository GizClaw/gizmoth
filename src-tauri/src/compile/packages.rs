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
    self, BootConfig, BootMode, PackCheck, PackCommandCheck, PackItem, PackPlan, PackStep, Share,
};
use super::{assets, image, jobs::LogRing, vm, Service};
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
type RecipePlan = (Vec<PackStep>, Vec<PackCheck>, Vec<PackCommandCheck>);
type GuestEnvironment = (Vec<String>, Vec<(String, String)>);

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
    #[serde(default = "native_platform")]
    pub platform: String,
    #[serde(default)]
    pub guest_root: bool,
    #[serde(default)]
    pub bootstrap: bool,
    /// The item's default version: on `PATH` in builds.
    pub default: bool,
    pub prepend_path: Vec<String>,
    pub variables: BTreeMap<String, String>,
    pub steps: Vec<PackStep>,
    pub checks: Vec<PackCheck>,
    #[serde(default)]
    pub command_checks: Vec<PackCommandCheck>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    #[serde(default)]
    pub plan_version: u32,
}

fn native_platform() -> String {
    PLATFORM.into()
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
    #[serde(default)]
    pub paused: bool,
}

/// Identifies the installed contents and activation metadata of a package set.
/// Download timestamps and display names do not change the pack or build env.
fn hash(packages: &[Package]) -> String {
    use sha2::{Digest, Sha256};
    let mut entries: Vec<_> = packages
        .iter()
        .filter(|p| !p.guest_root || p.default)
        .map(|p| {
            serde_json::to_string(&(
                &p.kind,
                &p.id,
                &p.version,
                &p.manifest,
                &p.platform,
                p.guest_root,
                p.bootstrap,
                protocol::AGENT_VERSION,
                p.default,
                &p.prepend_path,
                &p.variables,
                (
                    &p.steps,
                    &p.checks,
                    &p.command_checks,
                    &p.dependencies,
                    p.plan_version,
                ),
            ))
            .expect("package identity is serializable")
        })
        .collect();
    entries.sort();
    registry::hex_digest(&Sha256::digest(entries.join("\n").as_bytes()))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackInfo {
    #[serde(default)]
    agent_version: u32,
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
    /// Downloads start automatically unless the user saved a pause.
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
    cancel: Arc<AtomicBool>,
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
            state: Mutex::new(State {
                // A cached catalog is immediately usable, but each app start
                // still checks main for formulas published since the last run.
                refresh_requested: true,
                ..State::default()
            }),
            wake: Condvar::new(),
            cancel: Arc::new(AtomicBool::new(false)),
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

    pub(super) fn notify(&self) {
        self.wake.notify_all();
    }

    pub fn busy(&self) -> bool {
        self.state.lock().unwrap().busy
    }

    pub fn activate(&self) {
        let mut state = self.state.lock().unwrap();
        if !state.active && !state.index.paused {
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
        .filter(|info: &PackInfo| {
            info.agent_version == protocol::AGENT_VERSION && root.join(PACK).is_file()
        });
    state.active = !state.index.paused;
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
    platform: String,
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
        Step::Run { .. } if !recipe.bootstrap => None,
        Step::Extract { format, .. }
            if matches!(
                format.as_str(),
                "tar.gz" | "tar.xz" | "tar.bz2" | "zip" | "deb"
            ) =>
        {
            None
        }
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
                let (platform, recipe) = if let Some(recipe) = manifest.platforms.get(PLATFORM) {
                    (PLATFORM, recipe)
                } else if let Some(recipe) = manifest
                    .platforms
                    .get("linux-x86_64")
                    .filter(|r| !r.guest_root)
                {
                    // An ARM64 kernel runs foreign user programs through the
                    // downloaded QEMU formula and matching foreign runtime.
                    ("linux-x86_64", recipe)
                } else {
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
                    platform: platform.into(),
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
fn plan_steps(recipe: &Recipe) -> Result<RecipePlan, String> {
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
                format,
            } => PackStep::Extract {
                source: digest(source)?,
                to: to.clone(),
                format: format.clone(),
                strip_components: *strip_components,
            },
            Step::Run {
                program,
                args,
                cwd,
                environment,
                timeout_seconds,
            } => PackStep::Run {
                program: program.clone(),
                args: args.clone(),
                cwd: cwd.clone(),
                environment: environment.clone(),
                timeout_seconds: *timeout_seconds,
            },
            other => return Err(format!("install step {} is not supported", other.name())),
        });
    }
    // Commands are qualified in the completed Linux tree before publication.
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
    let command_checks = recipe
        .checks
        .iter()
        .filter_map(|check| match check {
            Check::Command {
                program,
                args,
                stdout_contains,
                timeout_seconds,
            } => Some(PackCommandCheck {
                program: program.clone(),
                args: args.clone(),
                stdout_contains: stdout_contains.clone(),
                timeout_seconds: *timeout_seconds,
            }),
            Check::File { .. } => None,
        })
        .collect();
    Ok((steps, checks, command_checks))
}

fn prefix(package: &Package) -> String {
    if package.guest_root {
        "/".into()
    } else {
        format!("{GUEST_ROOT}/{}", package.dir())
    }
}

fn recipe_dependencies(recipe: &Recipe) -> Result<BTreeMap<String, String>, String> {
    let mut dependencies = BTreeMap::new();
    for dependency in &recipe.dependencies {
        if dependencies
            .insert(dependency.toolchain.clone(), dependency.version.clone())
            .is_some()
        {
            return Err(format!("ambiguous dependency {}", dependency.toolchain));
        }
    }
    Ok(dependencies)
}

fn replan_cached(package: &mut Package, recipe: &Recipe) -> Result<(), String> {
    let (steps, checks, commands) = plan_steps(recipe)?;
    let dependencies = recipe_dependencies(recipe)?;
    package.steps = steps;
    package.checks = checks;
    package.command_checks = commands;
    package.dependencies = dependencies;
    package.plan_version = 1;
    Ok(())
}

/// Root payloads precede tools. Each setup command sees its exact dependencies,
/// independent of catalog order or which versions are default on worker PATH.
fn ordered_packages(packages: &[Package]) -> Result<Vec<&Package>, String> {
    let mut pending: Vec<_> = packages
        .iter()
        .filter(|p| !p.guest_root || p.default)
        .collect();
    pending.sort_by_key(|p| (!p.guest_root, !p.bootstrap, p.dir()));
    let mut ordered: Vec<&Package> = Vec::new();
    while !pending.is_empty() {
        let root_pending = pending.iter().any(|p| p.guest_root);
        let next = pending.iter().position(|p| {
            (!root_pending || p.guest_root)
                && p.dependencies.iter().all(|(id, version)| {
                    ordered
                        .iter()
                        .any(|d| d.kind == "toolchain" && &d.id == id && &d.version == version)
                })
        });
        let Some(next) = next else {
            return Err(format!(
                "{}: missing or cyclic exact package dependencies",
                pending[0].dir()
            ));
        };
        ordered.push(pending.remove(next));
    }
    Ok(ordered)
}

fn expand(
    value: &str,
    package: &Package,
    packages: &[&Package],
    setup: bool,
) -> Result<String, String> {
    let replace_path = |value: &str, token: &str, base: &str| {
        value
            .replace(
                &format!("{token}/"),
                &format!("{}/", base.trim_end_matches('/')),
            )
            .replace(token, base)
    };
    let cache = if setup { "/work/cache" } else { GUEST_CACHE };
    let mut value = replace_path(value, "${prefix}", &prefix(package)).replace("${cache}", cache);
    if setup {
        value = value.replace("${work}", "/work/build");
    }
    for (id, version) in &package.dependencies {
        let dependency = packages
            .iter()
            .find(|p| p.kind == "toolchain" && &p.id == id && &p.version == version)
            .ok_or_else(|| format!("{}: missing dependency {id}@{version}", package.dir()))?;
        value = replace_path(&value, &format!("${{toolchain:{id}}}"), &prefix(dependency));
    }
    if value.contains("${toolchain:") {
        return Err(format!(
            "{}: undeclared toolchain placeholder in {value}",
            package.dir()
        ));
    }
    Ok(value)
}

fn dependency_environment(
    package: &Package,
    packages: &[&Package],
) -> Result<BTreeMap<String, String>, String> {
    fn activate<'a>(
        p: &'a Package,
        packages: &[&'a Package],
        selected: &mut Vec<&'a Package>,
    ) -> Result<(), String> {
        if selected.iter().any(|d| d.key() == p.key()) {
            return Ok(());
        }
        for (id, version) in &p.dependencies {
            let dependency = packages
                .iter()
                .find(|d| d.kind == "toolchain" && &d.id == id && &d.version == version)
                .ok_or_else(|| format!("{}: missing dependency {id}@{version}", p.dir()))?;
            activate(dependency, packages, selected)?;
        }
        selected.push(p);
        Ok(())
    }
    let mut selected = Vec::new();
    activate(package, packages, &mut selected)?;
    let mut env = BTreeMap::new();
    let mut path = Vec::new();
    for p in selected.iter().rev() {
        for entry in &p.prepend_path {
            path.push(format!("{}/{entry}", prefix(p).trim_end_matches('/')));
        }
    }
    for p in selected {
        for (name, value) in &p.variables {
            env.insert(name.clone(), expand(value, p, packages, true)?);
        }
    }
    path.push("/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into());
    env.insert("PATH".into(), path.join(":"));
    Ok(env)
}

fn pack_items(packages: &[Package]) -> Result<Vec<PackItem>, String> {
    let ordered = ordered_packages(packages)?;
    ordered
        .iter()
        .map(|p| {
            if p.plan_version != 1 {
                return Err(format!(
                    "{}: refresh the registry to qualify this cached recipe",
                    p.dir()
                ));
            }
            let expand = |v: &str| expand(v, p, &ordered, true);
            let mut steps = p.steps.clone();
            for step in &mut steps {
                if let PackStep::Run {
                    program,
                    args,
                    cwd,
                    environment,
                    ..
                } = step
                {
                    *program = expand(program)?;
                    *args = args.iter().map(|v| expand(v)).collect::<Result<_, _>>()?;
                    // Registry cwd is a safe relative path inside this package.
                    *cwd = format!("{}/{}", prefix(p).trim_end_matches('/'), cwd);
                    *environment = environment
                        .iter()
                        .map(|(k, v)| Ok((k.clone(), expand(v)?)))
                        .collect::<Result<_, String>>()?;
                }
            }
            let mut checks = p.command_checks.clone();
            for check in &mut checks {
                check.program = expand(&check.program)?;
                check.args = check
                    .args
                    .iter()
                    .map(|v| expand(v))
                    .collect::<Result<_, _>>()?;
                check.stdout_contains = check.stdout_contains.as_deref().map(expand).transpose()?;
            }
            Ok(PackItem {
                dir: p.dir(),
                guest_root: p.guest_root,
                bootstrap: p.bootstrap,
                steps,
                checks: p.checks.clone(),
                command_checks: checks,
                environment: dependency_environment(p, &ordered)?,
            })
        })
        .collect()
}

/// `PATH` entries and variables of the default versions, as builds see them.
pub fn guest_environment(packages: &[Package]) -> Result<GuestEnvironment, String> {
    let available = ordered_packages(packages)?;
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
        let prefix = if package.guest_root {
            String::new()
        } else {
            format!("{GUEST_ROOT}/{}", package.dir())
        };
        path.extend(
            package
                .prepend_path
                .iter()
                .map(|entry| format!("{prefix}/{entry}")),
        );
        for (name, value) in &package.variables {
            let value = expand(value, package, &available, false)?;
            variables.push((name.clone(), value));
        }
    }
    Ok((path, variables))
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
    /// Runs on its own thread: registry refresh, downloads and pack upkeep.
    pub(crate) fn maintain(self: Arc<Self>) {
        loop {
            if self.packages.cancel.load(Ordering::SeqCst) {
                return;
            }
            self.maintain_packages();
            let state = self.packages.state.lock().unwrap();
            let _ = self.packages.wake.wait_timeout(state, TICK).unwrap();
        }
    }

    fn maintain_packages(self: &Arc<Self>) {
        // Metadata must stay current even with downloads paused or no VM image.
        self.refresh_catalog();
        let ready = super::compile_available() && self.state.lock().unwrap().image.ready();
        if ready && self.packages.state.lock().unwrap().active {
            self.sync_packages();
            self.pack_if_needed();
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
            let due =
                state.refresh_requested || ((stale || state.refresh_error.is_some()) && retry);
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
        if storage.moving.is_some() || state.busy || self.packages.cancel.load(Ordering::SeqCst) {
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
            let mut rejected = Vec::new();
            for package in state.index.packages.iter_mut() {
                if let Some(w) = wanted
                    .iter()
                    .find(|w| Some(&w.key) == package.key().as_ref())
                {
                    package.default = w.default;
                    package.name = w.name.clone();
                    // Cached indexes from earlier agents discarded command
                    // checks. Replan verified manifests without redownloading.
                    if let Some(recipe) = catalog
                        .release(w.key.0, &w.key.1, &w.key.2)
                        .and_then(|(release, _)| release.platforms.get(&w.platform))
                    {
                        if let Err(error) = replan_cached(package, recipe) {
                            if let Some(key) = package.key() {
                                rejected.push((key, error));
                            }
                        }
                    }
                }
            }
            for (key, error) in rejected {
                state
                    .index
                    .packages
                    .retain(|p| p.key().as_ref() != Some(&key));
                state.failures.insert(key, (error, Instant::now()));
            }
            if state.index != before {
                let _ = write_atomic(&root.join(INDEX), &state.index);
            }
        }
        for item in &wanted {
            if self.packages.cancel.load(Ordering::SeqCst)
                || self.moving()
                || !self.packages.state.lock().unwrap().active
            {
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
            .get(&item.platform)
            .ok_or("no recipe for the selected guest architecture")?;
        let (steps, checks, command_checks) = plan_steps(recipe)?;
        let dependencies = recipe_dependencies(recipe)?;
        let cancel = || {
            self.packages.cancel.load(Ordering::SeqCst)
                || !self.packages.state.lock().unwrap().active
        };
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
            platform: item.platform.clone(),
            guest_root: recipe.is_guest_root(),
            bootstrap: recipe.bootstrap,
            prepend_path: recipe.environment.prepend_path.clone(),
            variables: recipe.environment.variables.clone(),
            steps,
            checks,
            command_checks,
            dependencies,
            plan_version: 1,
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
            if state.index.packages.is_empty() || current || failed || !state.active {
                return;
            }
            if !state
                .index
                .packages
                .iter()
                .any(|p| p.default && p.guest_root && p.bootstrap)
            {
                return;
            }
            state.index.packages.clone()
        };
        if !self.state.lock().unwrap().image.ready() {
            return;
        }
        let Some(allocation) = self.reserve_preparation() else {
            return;
        };
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
        let result = self.pack(&packages, allocation);
        let published = result.is_ok();
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
        if published {
            // Busy VMs also retire before reuse after a new tool set is published.
            state.boot_generation += 1;
        }
        drop(state);
        self.changed();
    }

    fn pack(
        &self,
        packages: &[Package],
        allocation: Arc<super::resources::Lease>,
    ) -> Result<PackInfo, String> {
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
        let kernel = self.image_files()?;
        let agent = assets::agent()?;
        let index: Vec<Value> = packages
            .iter()
            .map(|p| {
                json!({ "kind": p.kind, "id": p.id, "version": p.version, "default": p.default,
                        "guestRoot": p.guest_root, "path": if p.guest_root { "/".to_string() } else { prefix(p) } })
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
                writable: false,
            }],
            legacy_work: String::new(),
            images: Vec::new(),
            pack: Some(PackPlan {
                packages: pack_items(packages)?,
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
            cpus: allocation.amount().cpus,
            memory_mib: allocation.amount().memory_mib,
            kernel,
            initramfs: initramfs.clone(),
            disks: vec![(target.clone(), false), (scratch.clone(), false)],
            network: false,
            console: dir.join("console.log"),
            runtime: vm::runtime_dir()?,
            shares: vec![(protocol::SHARE_PORT, root.join("assets"), false)],
        };
        let cancel = &self.packages.cancel;
        let result = (|| {
            if cancel.load(Ordering::SeqCst) {
                return Err("cancelled".into());
            }
            // Move the preparation lease into the process: shutdown can reap
            // it and return capacity even if this session has not unwound yet.
            let mut machine = self.launch_vm(|| vm::Vm::start(&spec, None, allocation))?;
            let outcome = image::session(
                &mut machine,
                cancel,
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
                agent_version: protocol::AGENT_VERSION,
                hash: hash(packages),
                bytes: size,
                built_at: crate::serial::now_ms(),
                packages: packages
                    .iter()
                    .filter(|p| !p.guest_root || p.default)
                    .cloned()
                    .collect(),
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
        if state.index.paused && !refresh {
            return;
        }
        state.active = true;
        state.index.paused = false;
        let _ = write_atomic(&self.root().join(INDEX), &state.index);
        if refresh {
            state.refresh_requested = true;
            state.failures.clear();
            state.pack.error = None;
        }
        drop(state);
        self.packages.notify();
        self.changed();
    }

    pub(crate) fn packages_resume(&self) {
        {
            let mut state = self.packages.state.lock().unwrap();
            state.index.paused = false;
            let _ = write_atomic(&self.root().join(INDEX), &state.index);
        }
        self.packages_request(false);
    }

    pub(crate) fn packages_pause(&self) {
        let mut state = self.packages.state.lock().unwrap();
        state.active = false;
        state.index.paused = true;
        let _ = write_atomic(&self.root().join(INDEX), &state.index);
        drop(state);
        self.packages.notify();
        self.changed();
    }

    pub(crate) fn clear_downloads(&self) -> Result<(), (u16, String)> {
        let storage = self.storage.lock().unwrap();
        let mut state = self.packages.state.lock().unwrap();
        if storage.moving.is_some() || state.busy || state.refreshing {
            return Err((
                409,
                "Wait for downloads and assembly to finish, or pause downloads first".into(),
            ));
        }
        let path = self.root().join("assets");
        let trash = self
            .root()
            .join(format!(".downloads-{}", uuid::Uuid::new_v4()));
        if path.exists() {
            fs::rename(&path, &trash).map_err(|e| (400, e.to_string()))?;
        }
        let index = Index {
            packages: Vec::new(),
            paused: true,
        };
        let updated = fs::create_dir_all(&path)
            .map_err(|e| e.to_string())
            .and_then(|_| write_atomic(&self.root().join(INDEX), &index));
        if let Err(error) = updated {
            let _ = fs::remove_dir_all(&path);
            if trash.exists() {
                let _ = fs::rename(&trash, &path);
            }
            return Err((400, error));
        }
        state.index = index;
        state.active = false;
        state.failures.clear();
        drop(state);
        drop(storage);
        let _ = fs::remove_dir_all(trash);
        self.changed();
        Ok(())
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
            } else if let Some((error, _)) = state.failures.get(&item.key) {
                json!({ "state": "failed", "error": error })
            } else if packed(&item.key, &item.manifest) {
                json!({ "state": "installed" })
            } else if downloaded(&item.key, &item.manifest) {
                json!({ "state": if state.pack.running { "packing" } else { "downloaded" } })
            } else {
                json!({ "state": "pending" })
            };
            let recipe = state
                .catalog
                .as_ref()
                .and_then(|c| c.release(item.key.0, &item.key.1, &item.key.2))
                .and_then(|(r, _)| r.platforms.get(&item.platform));
            let guest_root = recipe.is_some_and(Recipe::is_guest_root);
            let mut row = json!({
                "guestRoot": guest_root, "bootstrap": recipe.is_some_and(|r| r.bootstrap),
                "kind": item.key.0.as_str(), "id": item.key.1, "version": item.key.2,
                "name": item.name, "default": item.default, "download": item.download, "platform": item.platform,
                "path": if guest_root { "/".to_string() } else { format!("{GUEST_ROOT}/{}/{}/{}", item.key.0.directory(), item.key.1, item.key.2) },
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
                    "guestRoot": package.guest_root, "bootstrap": package.bootstrap, "platform": package.platform,
                    "path": if package.guest_root { "/".to_string() } else { prefix(package) }, "state": "installed",
                }));
            }
        }
        let pack = if state.pack.running {
            json!({ "state": "packing", "phase": state.pack.phase })
        } else if state.index.packages.is_empty() && state.pack_info.is_none() {
            json!({ "state": "none" })
        } else if let Some(info) = state
            .pack_info
            .as_ref()
            .filter(|i| i.hash == set || (state.index.paused && state.index.packages.is_empty()))
        {
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
            "paused": state.index.paused,
            "downloads": { "path": self.root().join("assets"), "bytes": super::storage::allocated(&self.root().join("assets")) },
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
            platform: PLATFORM.into(),
            guest_root: false,
            bootstrap: false,
            default,
            prepend_path: vec!["bin".into()],
            variables: [("CACHE_HOME".to_string(), "${cache}/x".to_string())].into(),
            steps: Vec::new(),
            checks: Vec::new(),
            command_checks: Vec::new(),
            dependencies: BTreeMap::new(),
            plan_version: 1,
        }
    }

    #[test]
    fn preparation_setup_errors_return_the_shared_reservation() {
        let directory = tempfile::tempdir().unwrap();
        let service = Service::load(directory.path());
        service.state.lock().unwrap().image.kernel = None;
        let allocation = service.reserve_preparation().unwrap();
        assert_eq!(service.resources.snapshot().vm_count, 1);
        assert!(service.pack(&[], allocation).is_err());
        assert_eq!(service.resources.snapshot().vm_count, 0);
        assert!(service.reserve_preparation().is_some());
    }

    #[test]
    fn stopping_packages_cancels_the_preparation_session_before_releasing_capacity() {
        use std::process::{Command, Stdio};
        let directory = tempfile::tempdir().unwrap();
        let service = Service::load(directory.path());
        let allocation = service.reserve_preparation().unwrap();
        let child = Command::new("/bin/sleep")
            .arg("300")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut machine = vm::Vm::test_process(child, directory.path(), allocation);
        service.packages.stop();
        assert!(!service.begin_package_work());
        let result = image::session(&mut machine, &service.packages.cancel, &|_| {}, &|_| {});
        assert_eq!(result, Err("cancelled".into()));
        assert!(machine.alive());
        assert_eq!(service.resources.snapshot().vm_count, 1);
        machine.stop(Duration::ZERO);
        drop(machine);
        assert_eq!(service.resources.snapshot().vm_count, 0);
    }

    #[test]
    fn paused_downloads_stay_paused_and_archive_cleanup_keeps_the_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let service = Service::load(directory.path());
        let root = service.root();
        fs::create_dir_all(root.join("assets")).unwrap();
        fs::write(root.join("assets/archive"), b"download").unwrap();
        fs::write(root.join(PACK), b"existing read-only runtime").unwrap();
        service.packages_pause();
        service.packages.activate();
        assert!(!service.packages.state.lock().unwrap().active);
        service.clear_downloads().unwrap();
        assert!(!root.join("assets/archive").exists());
        assert_eq!(
            fs::read(root.join(PACK)).unwrap(),
            b"existing read-only runtime"
        );
        let saved: Index = serde_json::from_slice(&fs::read(root.join(INDEX)).unwrap()).unwrap();
        assert!(saved.paused && saved.packages.is_empty());
        let restarted = Packages::new(&service.config, &root);
        assert!(restarted.state.lock().unwrap().index.paused);
        assert!(!restarted.state.lock().unwrap().active);
    }

    #[test]
    fn archive_cleanup_rolls_back_when_the_index_cannot_be_saved() {
        let directory = tempfile::tempdir().unwrap();
        let service = Service::load(directory.path());
        let root = service.root();
        fs::create_dir_all(root.join("assets")).unwrap();
        fs::write(root.join("assets/archive"), b"keep").unwrap();
        fs::create_dir(root.join(INDEX)).unwrap();
        assert!(service.clear_downloads().is_err());
        assert_eq!(fs::read(root.join("assets/archive")).unwrap(), b"keep");
        assert!(!service.packages.state.lock().unwrap().index.paused);
    }

    #[test]
    fn defaults_go_on_path_and_hashes_follow_the_set() {
        let packages = vec![
            package("python", "3.12.16", true),
            package("python", "3.11.18", false),
            package("bazelisk", "1.30.0", true),
        ];
        let (path, variables) = guest_environment(&packages).unwrap();
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
    fn default_changes_invalidate_the_pack_and_update_the_guest_environment() {
        let before = vec![
            package("python", "3.12.16", true),
            package("python", "3.11.18", false),
        ];
        let mut after = before.clone();
        after[0].default = false;
        after[1].default = true;
        let old_info = PackInfo {
            hash: hash(&before),
            packages: before.clone(),
            ..PackInfo::default()
        };
        assert_ne!(old_info.hash, hash(&after), "the old pack must be stale");
        let (before_path, _) = guest_environment(&old_info.packages).unwrap();
        let (after_path, _) = guest_environment(&after).unwrap();
        assert_eq!(
            before_path,
            [format!("{GUEST_ROOT}/toolchains/python/3.12.16/bin")]
        );
        assert_eq!(
            after_path,
            [format!("{GUEST_ROOT}/toolchains/python/3.11.18/bin")]
        );

        let mut changed = after.clone();
        changed[1].prepend_path.push("tools".into());
        assert_ne!(hash(&after), hash(&changed));
        let mut changed = after.clone();
        changed[1]
            .variables
            .insert("SDK_HOME".into(), "${prefix}".into());
        assert_ne!(hash(&after), hash(&changed));
        let mut same = after.clone();
        same[0].downloaded_at += 1;
        same[0].name = "Python".into();
        assert_eq!(
            hash(&after),
            hash(&same),
            "display metadata must not repack"
        );
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
                    let (steps, _, _) = plan_steps(recipe).unwrap();
                    assert_eq!(steps.len(), recipe.install.len());
                    for step in steps {
                        let source = match step {
                            PackStep::Copy { source, .. } | PackStep::Extract { source, .. } => {
                                source
                            }
                            PackStep::Run { .. } => {
                                panic!("this fixture contains only file operations")
                            }
                        };
                        assert!(registry::valid_sha256(&source), "{source}");
                    }
                }
            }
        }
    }

    /// Qualify unpublished registry recipes locally before any release/main
    /// publication. All input bytes still go through the normal SHA verifier.
    #[test]
    #[ignore = "requires staged VM resources, GIZMOTH_LIVE_PACK_SEED and GIZMOTH_TEST_REGISTRY_SNAPSHOT"]
    fn live_registry_recipes_download_install_and_qualify() {
        let seed = PathBuf::from(
            std::env::var_os("GIZMOTH_LIVE_PACK_SEED").expect("set GIZMOTH_LIVE_PACK_SEED"),
        );
        let snapshot_file = std::env::var_os("GIZMOTH_TEST_REGISTRY_SNAPSHOT")
            .expect("set GIZMOTH_TEST_REGISTRY_SNAPSHOT");
        let saved: SavedSnapshot =
            serde_json::from_slice(&fs::read(snapshot_file).unwrap()).unwrap();
        let snapshot =
            registry::parse_snapshot(&saved.commit, saved.fetched_at, &saved.files).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("compile");
        fs::create_dir(&root).unwrap();
        assert!(std::process::Command::new("cp")
            .arg("-cR")
            .arg(seed.join("assets"))
            .arg(root.join("assets"))
            .status()
            .unwrap()
            .success());
        let service = Service::load(temporary.path());
        let mut packages = Vec::new();
        for item in wanted(&snapshot) {
            assert!(
                item.unsupported.is_none(),
                "{:?}: {:?}",
                item.key,
                item.unsupported
            );
            println!("Verifying {}@{}", item.key.1, item.key.2);
            packages.push(service.download(&root, &snapshot, &item).unwrap());
        }
        let allocation = service.reserve_preparation().unwrap();
        let info = service.pack(&packages, allocation).unwrap();
        assert!(info.packages.iter().any(|p| p.id == "arm-gnu"));
        assert!(info.packages.iter().any(|p| p.id == "jieli"));
        assert_eq!(service.resources.snapshot().vm_count, 0);
        write_atomic(
            &root.join(INDEX),
            &Index {
                packages,
                paused: true,
            },
        )
        .unwrap();
        service.packages.publish_catalog(saved).unwrap();
        service.shutdown();
        // Retain this isolated, paused runtime for subsequent real worker SDK
        // builds. It never shares the installed application's data directory.
        fs::write(
            temporary.path().join(".gizmoth-live-pack-test"),
            b"Verified isolated registry recipe test\n",
        )
        .unwrap();
        println!("LIVE_PACK_DATA={}", temporary.keep().display());
    }

    /// Opt-in real OpenVMM test: use verified downloads from an isolated seed,
    /// never a user's work directory or device. No registry/network is needed.
    #[test]
    #[ignore = "requires Apple silicon, staged VM resources and GIZMOTH_LIVE_PACK_SEED"]
    fn live_pack_runs_setup_and_rejects_failed_readonly_builder_qualification() {
        let seed = PathBuf::from(
            std::env::var_os("GIZMOTH_LIVE_PACK_SEED").expect("set GIZMOTH_LIVE_PACK_SEED"),
        );
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("compile");
        fs::create_dir(&root).unwrap();
        assert!(std::process::Command::new("cp")
            .arg("-cR")
            .arg(seed.join("assets"))
            .arg(root.join("assets"))
            .status()
            .unwrap()
            .success());
        let index: Index = serde_json::from_slice(&fs::read(seed.join(INDEX)).unwrap()).unwrap();
        let mut packages = index.packages;
        let mut fixture = package("runner-proof", "1.0", true);
        fixture.variables.clear();
        fixture
            .variables
            .insert("FIXTURE_FILE".into(), "${prefix}/payload/value".into());
        fixture.steps.push(PackStep::Run {
            program: "/bin/sh".into(), args: vec!["-c".into(), "set -eu; printf '%s' \"$FIXTURE_STEP\" > \"$FIXTURE_FILE\"; printf 'int main(void){return 0;}\\n' > probe.c".into()],
            cwd: "payload".into(), environment: [("FIXTURE_STEP".into(), "INSTALLED_BY_RUN".into())].into(), timeout_seconds: 10,
        });
        fixture.command_checks.push(PackCommandCheck {
            program: "/bin/sh".into(), args: vec!["-c".into(), "set -eu; test \"$(id -u)\" = 1000; test \"$(cat \"$FIXTURE_FILE\")\" = INSTALLED_BY_RUN; if touch \"$FIXTURE_FILE\" 2>/dev/null; then exit 31; fi; gcc \"${prefix}/payload/probe.c\" -o /work/build/probe; /work/build/probe; printf 'QUALIFICATION_OK\\n'".into()],
            stdout_contains: Some("QUALIFICATION_OK".into()), timeout_seconds: 20,
        });
        let mut root_setup = package("root-run-proof", "1.0", true);
        root_setup.guest_root = true;
        root_setup.variables.clear();
        root_setup
            .dependencies
            .insert("linux-runtime".into(), "12.0.0".into());
        root_setup
            .dependencies
            .insert("linux-build-tools".into(), "12.0.0".into());
        root_setup.steps.push(PackStep::Run {
            program: "/bin/sh".into(), args: vec!["-ec".into(), "test \"$ROOT_PREFIX\" = /; test -x /usr/bin/cc; printf root-setup-ok > /etc/gizmoth-root-run-proof".into()],
            cwd: "tmp".into(), environment: [("ROOT_PREFIX".into(), "${prefix}".into())].into(), timeout_seconds: 10,
        });
        root_setup.command_checks.push(PackCommandCheck {
            program: "/usr/bin/test".into(),
            args: vec!["-d".into(), "${prefix}".into()],
            stdout_contains: None,
            timeout_seconds: 10,
        });
        root_setup.command_checks.push(PackCommandCheck {
            program: "/bin/cat".into(),
            args: vec!["${prefix}/etc/gizmoth-root-run-proof".into()],
            stdout_contains: Some("root-setup-ok".into()),
            timeout_seconds: 10,
        });
        packages.push(root_setup);
        packages.push(fixture);
        let service = Service::load(temporary.path());
        let allocation = service.reserve_preparation().unwrap();
        let info = service.pack(&packages, allocation).unwrap();
        assert!(info.packages.iter().any(|p| p.id == "runner-proof"));
        assert_eq!(service.resources.snapshot().vm_count, 0);
        let before = fs::metadata(root.join(PACK)).unwrap().modified().unwrap();
        packages.last_mut().unwrap().command_checks[0].stdout_contains =
            Some("THIS_TEXT_WAS_NEVER_PRINTED".into());
        let allocation = service.reserve_preparation().unwrap();
        let error = service.pack(&packages, allocation).unwrap_err();
        assert!(error.contains("stdout did not contain"), "{error}");
        assert_eq!(
            fs::metadata(root.join(PACK)).unwrap().modified().unwrap(),
            before
        );
        assert_eq!(service.resources.snapshot().vm_count, 0);
        service.shutdown();
    }

    #[test]
    fn sdk_setup_uses_exact_dependencies_and_checks_before_publication() {
        let python = package("python", "1.0", false);
        let newer = package("python", "2.0", true);
        let mut root = package("root", "1.0", true);
        root.guest_root = true;
        root.bootstrap = true;
        let mut sdk = package("sdk", "1.0", true);
        sdk.kind = "sdk".into();
        sdk.dependencies.insert("python".into(), "1.0".into());
        sdk.variables.insert(
            "SDK_PYTHON".into(),
            "${toolchain:python}/bin/python3".into(),
        );
        sdk.steps.push(PackStep::Run {
            program: "${toolchain:python}/bin/python3".into(),
            args: vec!["${prefix}/setup.py".into(), "${work}".into()],
            cwd: "payload".into(),
            environment: [("DOWNLOADS".into(), "${cache}/sdk".into())].into(),
            timeout_seconds: 90,
        });
        sdk.command_checks.push(PackCommandCheck {
            program: "${prefix}/bin/check".into(),
            args: vec![],
            stdout_contains: Some("SDK_READY".into()),
            timeout_seconds: 10,
        });
        let packages = vec![sdk.clone(), newer, python.clone(), root];
        let items = pack_items(&packages).unwrap();
        assert!(items[0].guest_root);
        let python_index = items.iter().position(|p| p.dir == python.dir()).unwrap();
        let sdk_index = items.iter().position(|p| p.dir == sdk.dir()).unwrap();
        assert!(python_index < sdk_index);
        let item = &items[sdk_index];
        assert_eq!(
            item.environment["SDK_PYTHON"],
            format!("{GUEST_ROOT}/{}/bin/python3", python.dir())
        );
        assert!(!item.environment["PATH"].contains("python/2.0"));
        let PackStep::Run {
            program,
            args,
            cwd,
            environment,
            timeout_seconds,
        } = &item.steps[0]
        else {
            panic!()
        };
        assert_eq!(program, &item.environment["SDK_PYTHON"]);
        assert_eq!(
            args,
            &[
                format!("{GUEST_ROOT}/{}/setup.py", sdk.dir()),
                "/work/build".into()
            ]
        );
        assert_eq!(cwd, &format!("{GUEST_ROOT}/{}/payload", sdk.dir()));
        assert_eq!(environment["DOWNLOADS"], "/work/cache/sdk");
        assert_eq!(*timeout_seconds, 90);
        assert_eq!(
            item.command_checks[0].stdout_contains.as_deref(),
            Some("SDK_READY")
        );
        let (_, environment) = guest_environment(&packages).unwrap();
        assert!(environment.contains(&(
            "SDK_PYTHON".into(),
            format!("{GUEST_ROOT}/{}/bin/python3", python.dir())
        )));
        let mut changed = packages.clone();
        changed[0].command_checks[0].stdout_contains = Some("DIFFERENT".into());
        assert_ne!(hash(&changed), hash(&packages));
    }

    #[test]
    fn root_templates_are_paths_and_cached_replans_reject_ambiguous_versions() {
        let mut root = package("root", "1.0", true);
        root.guest_root = true;
        root.prepend_path = vec!["usr/bin".into()];
        let mut sdk = package("sdk", "1.0", true);
        sdk.dependencies.insert("root".into(), "1.0".into());
        let available = [&root, &sdk];
        assert_eq!(expand("${prefix}", &root, &available, true).unwrap(), "/");
        assert_eq!(
            expand("${prefix}/usr/bin", &root, &available, true).unwrap(),
            "/usr/bin"
        );
        assert_eq!(
            expand("${toolchain:root}", &sdk, &available, true).unwrap(),
            "/"
        );
        assert_eq!(
            expand("${toolchain:root}/bin/sh", &sdk, &available, true).unwrap(),
            "/bin/sh"
        );
        assert!(dependency_environment(&sdk, &available).unwrap()["PATH"].contains(":/usr/bin:"));
        let mut recipe: Recipe = serde_json::from_value(json!({
            "requires": [], "dependencies": [{"toolchain":"python", "version":"1.0"}, {"toolchain":"python", "version":"2.0"}],
            "sources": [], "install": [], "environment":{"prependPath":[],"variables":{}}, "checks":[]
        })).unwrap();
        let mut cached = sdk;
        cached.plan_version = 0;
        let before = cached.clone();
        assert!(recipe_dependencies(&recipe)
            .unwrap_err()
            .contains("ambiguous dependency python"));
        assert!(replan_cached(&mut cached, &recipe).is_err());
        assert_eq!(
            cached, before,
            "a failed cache upgrade must preserve its unqualified plan"
        );
        recipe.dependencies.pop();
        replan_cached(&mut cached, &recipe).unwrap();
        assert_eq!(cached.dependencies["python"], "1.0");
        assert_eq!(cached.plan_version, 1);
    }

    #[test]
    fn missing_cyclic_or_unqualified_dependencies_cannot_be_packed() {
        let mut a = package("a", "1.0", true);
        a.dependencies.insert("b".into(), "1.0".into());
        assert!(pack_items(&[a.clone()]).is_err());
        let mut b = package("b", "1.0", true);
        b.dependencies.insert("a".into(), "1.0".into());
        assert!(pack_items(&[a.clone(), b]).is_err());
        a.dependencies.clear();
        a.plan_version = 0;
        assert!(pack_items(&[a])
            .unwrap_err()
            .contains("refresh the registry"));
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
                fetched_at: crate::serial::now_ms(),
                files: registry::tests::files(),
            })
            .unwrap();
        let again = Packages::new(&config, &config);
        let state = again.state.lock().unwrap();
        assert_eq!(state.catalog.as_ref().unwrap().commit, commit);
        assert!(state.active, "empty downloads must start automatically");
        assert!(
            state.refresh_requested,
            "even a recent cache must check main at startup"
        );
    }

    #[test]
    fn empty_downloads_start_automatically_unless_a_pause_was_saved() {
        let dir = tempfile::tempdir().unwrap();
        let packages = Packages::new(dir.path(), dir.path());
        assert!(packages.state.lock().unwrap().active);
        write_atomic(
            &dir.path().join(INDEX),
            &Index {
                paused: true,
                ..Index::default()
            },
        )
        .unwrap();
        let again = Packages::new(dir.path(), dir.path());
        let state = again.state.lock().unwrap();
        assert!(!state.active);
        assert!(state.index.paused, "a saved pause must survive restart");
        assert!(
            state.refresh_requested,
            "pausing downloads must not freeze metadata"
        );
    }

    #[test]
    fn maintenance_refreshes_while_paused_without_an_image_and_keeps_an_offline_cache() {
        let dir = tempfile::tempdir().unwrap();
        let service = Service::load(dir.path());
        let commit = "b".repeat(40);
        service
            .packages
            .publish_catalog(SavedSnapshot {
                commit: commit.clone(),
                fetched_at: crate::serial::now_ms(),
                files: registry::tests::files(),
            })
            .unwrap();
        write_atomic(
            &service.root().join(INDEX),
            &Index {
                paused: true,
                ..Index::default()
            },
        )
        .unwrap();
        let service = Service::load(dir.path());
        service.state.lock().unwrap().image.kernel = None;
        // An incomplete checkout fails before invoking Git or accessing the network.
        fs::create_dir_all(service.packages.registry.join("repository")).unwrap();
        service.maintain_packages();
        let state = service.packages.state.lock().unwrap();
        assert!(
            state.refresh_attempt.is_some(),
            "refresh must not wait for downloads or an image"
        );
        assert!(state.refresh_error.is_some());
        assert!(!state.refreshing);
        assert!(!state.refresh_requested);
        assert_eq!(state.catalog.as_ref().unwrap().commit, commit);
        assert!(state.index.paused);
        assert!(!state.active);
        assert!(!state.busy);
        assert!(!state.pack.running);
        let first_attempt = state.refresh_attempt;
        drop(state);
        service.maintain_packages();
        let mut state = service.packages.state.lock().unwrap();
        assert_eq!(
            state.refresh_attempt, first_attempt,
            "failed refreshes must back off"
        );
        let retry_at = Instant::now() - RETRY_AFTER - Duration::from_secs(1);
        state.refresh_attempt = Some(retry_at);
        drop(state);
        service.maintain_packages();
        let state = service.packages.state.lock().unwrap();
        assert!(
            state.refresh_attempt.unwrap() > retry_at,
            "retry a failed startup refresh even with a recent cache"
        );
        assert_eq!(state.catalog.as_ref().unwrap().commit, commit);
    }

    /// Exercise automatic startup refresh and package planning without downloading payloads.
    #[test]
    #[ignore = "network"]
    fn live_registry_lists_managed_linux_formulas() {
        let dir = tempfile::tempdir().unwrap();
        let service = Service::load(dir.path());
        let cached_commit = "b".repeat(40);
        service
            .packages
            .publish_catalog(SavedSnapshot {
                commit: cached_commit.clone(),
                fetched_at: crate::serial::now_ms(),
                files: registry::tests::files(),
            })
            .unwrap();
        let service = Service::load(dir.path());
        service.state.lock().unwrap().image.kernel = None;
        service.maintain_packages();
        let state = service.packages.state.lock().unwrap();
        assert!(state.refresh_error.is_none(), "{:?}", state.refresh_error);
        assert!(state.active);
        let catalog = state.catalog.as_ref().unwrap();
        let commit = &catalog.commit;
        assert_ne!(commit, &cached_commit);
        assert_eq!(
            service.packages.load_catalog().unwrap().unwrap().commit,
            *commit
        );
        let items = wanted(catalog);
        for id in [
            "linux-runtime",
            "linux-git",
            "linux-build-tools",
            "linux-qemu-user",
            "linux-x86-runtime",
            "linux-x86-binutils",
        ] {
            assert!(items.iter().any(|w| w.key.1 == id), "missing {id}");
        }
        for item in &items {
            assert!(
                item.unsupported.is_none(),
                "{}: {:?}",
                item.key.1,
                item.unsupported
            );
            let (release, _) = catalog
                .release(item.key.0, &item.key.1, &item.key.2)
                .unwrap();
            plan_steps(&release.platforms[&item.platform]).unwrap();
        }
        println!(
            "{commit}: {} SDK declarations, {} toolchains, {} managed Linux releases",
            catalog.sdks.len(),
            catalog.toolchains.len(),
            items.len()
        );
        println!(
            "Linux packages: {}",
            items
                .iter()
                .map(|w| format!("{}@{}", w.key.1, w.key.2))
                .collect::<Vec<_>>()
                .join(", ")
        );
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
