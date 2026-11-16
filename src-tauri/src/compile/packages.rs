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
    collections::{BTreeMap, BTreeSet},
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
pub const GUEST_ROOT: &str = protocol::PACKAGE_ROOT;
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
    /// Explicit repairs must build a fresh runtime even for identical recipes.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repair: String,
    /// Every declared file input, including sources not used by a copy step.
    #[serde(default)]
    pub sources: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageTarget {
    kind: String,
    id: String,
    version: String,
    platform: String,
}

impl PackageTarget {
    fn matches(&self, package: &Package) -> bool {
        self.kind == package.kind
            && self.id == package.id
            && self.version == package.version
            && self.platform == package.platform
    }

    fn matches_wanted(&self, item: &Wanted) -> bool {
        self.kind == item.key.0.as_str()
            && self.id == item.key.1
            && self.version == item.key.2
            && self.platform == item.platform
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Repair {
    target: PackageTarget,
    nonce: String,
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
    /// Tombstones include display metadata for offline starts/catalog removals.
    #[serde(default)]
    removed: Vec<Package>,
    #[serde(default)]
    repairs: Vec<Repair>,
    /// Explicit changes finish even if automatic upkeep is paused.
    #[serde(default)]
    rebuild: bool,
}

/// Identifies the installed contents and activation metadata of a package set.
/// Download timestamps and display names do not change the pack or build env.
fn hash(packages: &[Package]) -> String {
    use sha2::{Digest, Sha256};
    let mut entries: Vec<_> = packages
        .iter()
        .filter(|p| !p.guest_root || p.default)
        .map(|p| {
            let mut identity = serde_json::to_string(&(
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
            .expect("package identity is serializable");
            // Keep pre-repair hashes compatible with existing runtimes.
            if !p.repair.is_empty() {
                identity.push_str(&format!("\nrepair:{}", p.repair));
            }
            identity
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
    let recovered = recover_publication(root);
    state.index = fs::read(root.join(INDEX))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    state.pack_info = fs::read(root.join(PACK_INFO))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(|info: &PackInfo| {
            recovered.is_ok()
                && info.agent_version == protocol::AGENT_VERSION
                && root.join(PACK).is_file()
        });
    state.active = !state.index.paused;
    if state
        .pack_info
        .as_ref()
        .is_some_and(|info| info.hash == hash(&state.index.packages))
        && !repair_pending(&state.index)
    {
        // A successful publication may precede the final index write at shutdown.
        state.index.repairs.clear();
        state.index.rebuild = false;
    }
    if let Err(error) = recovered {
        state.pack.error = Some((hash(&state.index.packages), error, Instant::now()));
    }
}

/// A tiny rollback journal protects the image/metadata pair across errors and restarts.
#[derive(Serialize, Deserialize)]
struct Publication {
    had_pack: bool,
    had_info: bool,
}

fn recover_publication(root: &Path) -> Result<(), String> {
    let dir = root.join(".pack");
    let marker = dir.join("publishing.json");
    let bytes = match fs::read(&marker) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    let journal: Publication = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    for (name, previous, existed) in [
        (PACK, "previous.erofs", journal.had_pack),
        (PACK_INFO, "previous.json", journal.had_info),
    ] {
        if existed {
            let restore = dir.join(format!("{previous}.restore"));
            let _ = fs::remove_file(&restore);
            // Keep the backup until the entire rollback finishes.
            fs::hard_link(dir.join(previous), &restore)
                .and_then(|_| fs::rename(&restore, root.join(name)))
                .map_err(|e| e.to_string())?;
        } else {
            match fs::remove_file(root.join(name)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.to_string()),
                _ => {}
            }
        }
    }
    fs::remove_file(marker).map_err(|e| e.to_string())?;
    let _ = fs::remove_file(dir.join("previous.erofs"));
    let _ = fs::remove_file(dir.join("previous.json"));
    Ok(())
}

fn publish_pack(root: &Path, target: &Path, info: &PackInfo) -> Result<(), String> {
    publish_pack_with(root, target, info, |from, to| fs::rename(from, to))
}

fn publish_pack_with(
    root: &Path,
    target: &Path,
    info: &PackInfo,
    publish_metadata: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), String> {
    recover_publication(root)?;
    let dir = root.join(".pack");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let next = dir.join("next.json");
    write_atomic(&next, info)?;
    let journal = Publication {
        had_pack: root.join(PACK).exists(),
        had_info: root.join(PACK_INFO).exists(),
    };
    for (name, previous, existed) in [
        (PACK, "previous.erofs", journal.had_pack),
        (PACK_INFO, "previous.json", journal.had_info),
    ] {
        let _ = fs::remove_file(dir.join(previous));
        if existed {
            fs::hard_link(root.join(name), dir.join(previous)).map_err(|e| e.to_string())?;
        }
    }
    let marker = dir.join("publishing.json");
    write_atomic(&marker, &journal)?;
    let result = fs::rename(target, root.join(PACK))
        .and_then(|_| publish_metadata(&next, &root.join(PACK_INFO)))
        .and_then(|_| fs::remove_file(&marker));
    if let Err(error) = result {
        recover_publication(root)
            .map_err(|rollback| format!("{error}; restore previous runtime: {rollback}"))?;
        return Err(error.to_string());
    }
    let _ = fs::remove_file(dir.join("previous.erofs"));
    let _ = fs::remove_file(dir.join("previous.json"));
    Ok(())
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

/// A release the registry publishes for a Linux platform.
#[derive(Clone)]
struct Wanted {
    key: Key,
    name: String,
    manifest: String,
    default: bool,
    platform: String,
    download: u64,
    unsupported: Option<String>,
    managed: bool,
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
                    managed: true,
                });
            }
        }
    }
    // Shared root layers cannot coexist by version. Keep legacy releases visible
    // in the catalog, but do not install them or tools pinned to inactive roots.
    let recipe = |w: &Wanted| {
        snapshot
            .release(w.key.0, &w.key.1, &w.key.2)
            .and_then(|(release, _)| release.platforms.get(&w.platform))
    };
    let mut inactive: BTreeSet<_> = list
        .iter()
        .filter(|w| !w.default && recipe(w).is_some_and(Recipe::is_guest_root))
        .map(|w| w.key.clone())
        .collect();
    loop {
        let before = inactive.len();
        for w in &list {
            if recipe(w)
                .and_then(|r| recipe_dependencies(r).ok())
                .is_some_and(|dependencies| {
                    dependencies
                        .into_iter()
                        .any(|(id, version)| inactive.contains(&(Kind::Toolchain, id, version)))
                })
            {
                inactive.insert(w.key.clone());
            }
        }
        if inactive.len() == before {
            break;
        }
    }
    list.retain(|w| !inactive.contains(&w.key));
    list
}

/// Browsing includes every declared Linux recipe; upkeep still uses wanted().
fn catalog_items(snapshot: &Snapshot) -> Vec<Wanted> {
    let mut list = wanted(snapshot);
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
                for (platform, recipe) in &manifest.platforms {
                    if !platform.starts_with("linux-")
                        || list.iter().any(|w| {
                            w.key == (kind, item.id.clone(), release.version.clone())
                                && &w.platform == platform
                        })
                    {
                        continue;
                    }
                    list.push(Wanted {
                        key: (kind, item.id.clone(), release.version.clone()),
                        name: item.name.clone(),
                        manifest: digest.into(),
                        default: item
                            .default_version
                            .as_ref()
                            .map_or(index == 0, |v| v == &release.version),
                        platform: platform.clone(),
                        download: recipe
                            .sources
                            .iter()
                            .map(|s| match s {
                                Source::File { size, .. } => *size,
                                _ => 0,
                            })
                            .sum(),
                        unsupported: Some(
                            "This Linux recipe is not selected for the ARM64 guest".into(),
                        ),
                        managed: false,
                    });
                }
            }
        }
    }
    list
}

fn removed(index: &Index, item: &Wanted) -> bool {
    index
        .removed
        .iter()
        .any(|p| p.key().as_ref() == Some(&item.key) && p.platform == item.platform)
}

fn repair_pending(index: &Index) -> bool {
    index.repairs.iter().any(|repair| {
        !index
            .packages
            .iter()
            .any(|p| repair.target.matches(p) && p.repair == repair.nonce)
    })
}

fn removal_reason(state: &State, item: &Wanted, recipe: &Recipe) -> Option<String> {
    if removed(&state.index, item) {
        return Some("This package is already removed; reinstall to restore it".into());
    }
    if recipe.bootstrap || recipe.is_guest_root() {
        return Some("Required Linux runtime/bootstrap input; keep it installed".into());
    }
    if item.key.0 == Kind::Toolchain {
        let retained = state
            .index
            .packages
            .iter()
            .chain(state.pack_info.iter().flat_map(|i| &i.packages));
        for dependency in retained {
            if state
                .index
                .removed
                .iter()
                .any(|p| p.key() == dependency.key() && p.platform == dependency.platform)
            {
                continue;
            }
            if dependency.dependencies.get(&item.key.1) == Some(&item.key.2) {
                return Some(format!(
                    "Required by {} {}; remove that package first",
                    dependency.name, dependency.version
                ));
            }
        }
    }
    None
}

fn reinstall_reason(state: &State, recipe: &Recipe, item: &Wanted) -> Option<String> {
    if recipe.is_guest_root() && !item.default {
        return Some(
            "Only the default Linux root is assembled; reinstall its default version".into(),
        );
    }
    for dependency in &recipe.dependencies {
        if state.index.removed.iter().any(|p| {
            p.kind == "toolchain" && p.id == dependency.toolchain && p.version == dependency.version
        }) {
            return Some(format!(
                "Reinstall required toolchain {} {} first",
                dependency.toolchain, dependency.version
            ));
        }
    }
    None
}

fn package_management_reason(
    service: &super::State,
    moving: bool,
    state: &State,
) -> Option<String> {
    if moving {
        Some("Wait for the working directory move to finish".into())
    } else if state.busy || state.refreshing {
        Some("Wait for downloads and runtime preparation to finish".into())
    } else if service.jobs.iter().any(|j| !j.status().finished())
        || service.slots.iter().any(|s| {
            matches!(
                s.state,
                super::SlotState::Busy | super::SlotState::Starting | super::SlotState::Stopping
            )
        })
    {
        Some("Wait for builds and VM shutdowns to finish".into())
    } else {
        None
    }
}

/// An explicit replacement cannot silently drop a retained installed package
/// when rebuilding after archives were cleared or a download failed.
fn retained_download_pending(state: &State) -> bool {
    let wanted = state.catalog.as_deref().map(wanted).unwrap_or_default();
    // A prefix migration can retire an old root release and its dependents.
    // Preserve the current runtime until replacement defaults are downloaded.
    let replacement_pending = state
        .pack_info
        .iter()
        .flat_map(|info| &info.packages)
        .filter(|old| old.default)
        .any(|old| {
            wanted
                .iter()
                .filter(|w| w.default && !removed(&state.index, w))
                .find(|w| {
                    w.key.0.as_str() == old.kind && w.key.1 == old.id && w.key.2 != old.version
                })
                .is_some_and(|w| {
                    !state.index.packages.iter().any(|p| {
                        p.key().as_ref() == Some(&w.key)
                            && p.platform == w.platform
                            && p.manifest == w.manifest
                    })
                })
        });
    if replacement_pending {
        return true;
    }
    if !state.index.rebuild {
        return false;
    }
    state.pack_info.iter().flat_map(|i| &i.packages).any(|old| {
        wanted
            .iter()
            .find(|w| old.key().as_ref() == Some(&w.key) && old.platform == w.platform)
            .filter(|w| !removed(&state.index, w))
            .is_some_and(|w| {
                !state.index.packages.iter().any(|p| {
                    p.key().as_ref() == Some(&w.key)
                        && p.platform == w.platform
                        && p.manifest == w.manifest
                })
            })
    })
}

fn pack_candidate(state: &State) -> Option<Vec<Package>> {
    let set = hash(&state.index.packages);
    let current = state.pack_info.as_ref().is_some_and(|i| i.hash == set);
    let failed = state
        .pack
        .error
        .as_ref()
        .is_some_and(|(failed, _, at)| *failed == set && at.elapsed() < RETRY_AFTER);
    if state.index.packages.is_empty()
        || current
        || failed
        || (!state.active && !state.index.rebuild)
        || repair_pending(&state.index)
        || retained_download_pending(state)
        || !state
            .index
            .packages
            .iter()
            .any(|p| p.default && p.guest_root && p.bootstrap)
    {
        return None;
    }
    Some(state.index.packages.clone())
}

fn planned_package(item: &Wanted, recipe: &Recipe) -> Result<Package, String> {
    let (steps, checks, command_checks) = plan_steps(recipe)?;
    Ok(Package {
        kind: item.key.0.as_str().into(),
        id: item.key.1.clone(),
        name: item.name.clone(),
        version: item.key.2.clone(),
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
        dependencies: recipe_dependencies(recipe)?,
        plan_version: 1,
        repair: String::new(),
        sources: recipe_inputs(recipe).map(str::to_owned).collect(),
    })
}

/// Display-only destination; canonical prefix and runtime identity stay intact.
fn recipe_install_path(recipe: &Recipe) -> Option<String> {
    let destinations: BTreeSet<_> = recipe
        .install
        .iter()
        .filter_map(|step| match step {
            Step::Extract { to, .. } => Some(to.as_str()),
            Step::Copy { to, .. } => Some(to.rsplit_once('/').map_or(".", |(parent, _)| parent)),
            _ => None,
        })
        .map(|to| {
            format!(
                "/{}",
                to.split('/')
                    .filter(|part| !part.is_empty() && *part != ".")
                    .collect::<Vec<_>>()
                    .join("/")
            )
        })
        .collect();
    (destinations.len() == 1).then(|| destinations.into_iter().next().unwrap())
}

/// Display labels describe the payload, independent of the recipe selection,
/// compiler output ISA, and persisted package/pack identity. Keep the exceptions
/// tied to known registry conventions; URLs and executable names are not evidence
/// of execution architecture (for example, qemu-x86_64-static is an ARM64 binary).
fn display_labels<'a>(
    id: &str,
    platform: &str,
    guest_root: bool,
    paths: impl IntoIterator<Item = &'a str>,
    dependencies: impl IntoIterator<Item = &'a str>,
    variables: &BTreeMap<String, String>,
) -> Vec<&'static str> {
    let dependencies: BTreeSet<_> = dependencies.into_iter().collect();
    let foreign_root = guest_root
        && paths.into_iter().any(|path| {
            let path = path.trim_start_matches('/').trim_start_matches("./");
            path == "opt/gizmoth/x86_64" || path.starts_with("opt/gizmoth/x86_64/")
        });
    // These two ARM64 recipes install the same amd64 archives as their x86
    // variants and explicitly require both the emulator and foreign runtime.
    let translated_jieli = platform == PLATFORM
        && matches!(id, "jieli" | "jieli-postbuild")
        && dependencies.contains("linux-qemu-user")
        && dependencies.contains("linux-x86-runtime");
    let architecture = if foreign_root || translated_jieli {
        Some("x86_64")
    } else if id == "esp-rom-elfs" {
        // The any-host archive contains target ROM symbol data, no host code.
        None
    } else {
        match platform.rsplit_once('-').map(|(_, arch)| arch) {
            Some("arm64") => Some("arm64"),
            Some("x86_64") => Some("x86_64"),
            _ => None,
        }
    };
    let family = if matches!(
        id,
        "esp-idf" | "xtensa-esp-elf" | "riscv32-esp-elf" | "esp32ulp-elf" | "esp-rom-elfs"
    ) {
        Some("esp32")
    } else if id == "jieli" || id.starts_with("jieli-") {
        Some("jieli")
    } else if id == "beken"
        || id.starts_with("beken-")
        || (id == "arm-gnu" && variables.contains_key("GIZOS_BK_TOOLCHAIN_ROOT"))
    {
        Some("beken")
    } else {
        None
    };
    architecture.into_iter().chain(family).collect()
}

fn recipe_labels(id: &str, platform: &str, recipe: &Recipe) -> Vec<&'static str> {
    display_labels(
        id,
        platform,
        recipe.is_guest_root(),
        recipe
            .install
            .iter()
            .filter_map(|step| match step {
                Step::Extract { to, .. } | Step::Copy { to, .. } => Some(to.as_str()),
                _ => None,
            })
            .chain(recipe.checks.iter().filter_map(|check| match check {
                Check::File { path, .. } => Some(path.as_str()),
                _ => None,
            })),
        recipe.dependencies.iter().map(|d| d.toolchain.as_str()),
        &recipe.environment.variables,
    )
}

fn cached_labels(package: &Package) -> Vec<&'static str> {
    display_labels(
        &package.id,
        &package.platform,
        package.guest_root,
        package
            .steps
            .iter()
            .filter_map(|step| match step {
                PackStep::Extract { to, .. } | PackStep::Copy { to, .. } => Some(to.as_str()),
                _ => None,
            })
            .chain(package.checks.iter().map(|check| check.path.as_str())),
        package.dependencies.keys().map(String::as_str),
        &package.variables,
    )
}

/// Root checks are relative to `/`, not to an extract destination. Keep this
/// display metadata separate from installation prefixes and pack identity.
fn linux_paths<'a>(
    guest_root: bool,
    install_path: &str,
    checks: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    if !guest_root {
        return vec![install_path.to_owned()];
    }
    let mut seen = BTreeSet::new();
    let mut paths: Vec<_> = checks
        .into_iter()
        .map(|path| format!("/{}", path.trim_start_matches('/')))
        .filter(|path| seen.insert(path.clone()))
        .collect();
    if paths.is_empty() {
        paths.push(install_path.to_owned());
    }
    paths
}

/// Host originals and the shared mounted image, never a fictitious host extraction tree.
fn host_paths<'a>(root: &Path, inputs: impl IntoIterator<Item = &'a str>, packed: bool) -> Value {
    let mut seen = BTreeSet::new();
    let downloads: Vec<_> = inputs
        .into_iter()
        .filter(|sha| registry::valid_sha256(sha) && seen.insert(*sha))
        .map(|sha| root.join("assets").join(sha))
        .collect();
    json!({
        "downloads": downloads,
        "runtime": packed.then(|| root.join(PACK)),
    })
}

fn recipe_inputs(recipe: &Recipe) -> impl Iterator<Item = &str> {
    recipe.sources.iter().filter_map(|s| match s {
        Source::File { sha256, .. } => Some(sha256.as_str()),
        _ => None,
    })
}

fn cached_inputs(package: &Package) -> impl Iterator<Item = &str> {
    package
        .sources
        .iter()
        .map(String::as_str)
        .chain(package.steps.iter().filter_map(|s| match s {
            PackStep::Copy { source, .. } | PackStep::Extract { source, .. } => {
                Some(source.as_str())
            }
            PackStep::Run { .. } => None,
        }))
}

/// Shared inputs stay if any retained recipe or installed version uses them.
fn unshared_inputs(state: &State, target: &PackageTarget, recipe: &Recipe) -> BTreeSet<String> {
    let mut candidates: BTreeSet<String> = recipe_inputs(recipe).map(str::to_owned).collect();
    let cached: Vec<_> = state
        .index
        .packages
        .iter()
        .chain(state.pack_info.iter().flat_map(|p| &p.packages))
        .collect();
    for p in cached.iter().filter(|p| target.matches(p)) {
        candidates.extend(cached_inputs(p).map(str::to_owned));
    }
    let mut retained = BTreeSet::new();
    for p in cached.iter().filter(|p| {
        !target.matches(p)
            && !state
                .index
                .removed
                .iter()
                .any(|r| r.key() == p.key() && r.platform == p.platform)
    }) {
        retained.extend(cached_inputs(p).map(str::to_owned));
    }
    if let Some(catalog) = &state.catalog {
        for item in wanted(catalog)
            .iter()
            .filter(|w| !target.matches_wanted(w) && !removed(&state.index, w))
        {
            if let Some((release, _)) = catalog.release(item.key.0, &item.key.1, &item.key.2) {
                retained
                    .extend(recipe_inputs(&release.platforms[&item.platform]).map(str::to_owned));
            }
        }
    }
    candidates.retain(|s| registry::valid_sha256(s) && !retained.contains(s));
    candidates
}

/// Rename only verified single-component digest names through directory handles.
/// The caller can restore them if saving the tombstone fails.
struct RemovedInputs {
    assets: cap_std::fs::Dir,
    trash: cap_std::fs::Dir,
    path: PathBuf,
    names: Vec<String>,
    committed: bool,
}

impl RemovedInputs {
    fn stage(root: &Path, digests: &BTreeSet<String>) -> Result<Option<Self>, String> {
        use cap_fs_ext::DirExt;
        let root_dir = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority())
            .map_err(|e| e.to_string())?;
        let assets = match root_dir.open_dir_nofollow("assets") {
            Ok(dir) => dir,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let path = root.join(format!(".package-inputs-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).map_err(|e| e.to_string())?;
        let trash = cap_std::fs::Dir::open_ambient_dir(&path, cap_std::ambient_authority())
            .map_err(|e| e.to_string())?;
        let mut staged = Self {
            assets,
            trash,
            path,
            names: Vec::new(),
            committed: false,
        };
        for digest in digests.iter().filter(|s| registry::valid_sha256(s)) {
            for name in [digest.clone(), format!("{digest}.partial")] {
                match staged.assets.rename(&name, &staged.trash, &name) {
                    Ok(()) => staged.names.push(name),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.to_string()),
                }
            }
        }
        Ok(Some(staged))
    }
}

impl Drop for RemovedInputs {
    fn drop(&mut self) {
        for name in &self.names {
            if self.committed {
                let _ = self.trash.remove_file(name);
            } else {
                let _ = self.trash.rename(name, &self.assets, name);
            }
        }
        let _ = fs::remove_dir(&self.path);
    }
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
    package.sources = recipe_inputs(recipe).map(str::to_owned).collect();
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

fn default_packages(packages: &[Package]) -> Vec<&Package> {
    let mut defaults: Vec<_> = packages.iter().filter(|p| p.default).collect();
    defaults.sort_by(|a, b| (&a.kind, &a.id).cmp(&(&b.kind, &b.id)));
    defaults
}

/// Directories exported into the unified bin, in the old PATH precedence.
fn command_paths(packages: &[Package]) -> Vec<String> {
    default_packages(packages)
        .into_iter()
        .flat_map(|p| {
            p.prepend_path
                .iter()
                .map(|entry| format!("{}/{entry}", prefix(p).trim_end_matches('/')))
        })
        .collect()
}

/// Builds use one managed bin; exact-version setup/check activation is separate.
pub fn guest_environment(packages: &[Package]) -> Result<GuestEnvironment, String> {
    let available = ordered_packages(packages)?;
    let mut variables = vec![
        (
            "GIZMOTH_PREFIX".to_string(),
            protocol::GIZMOTH_PREFIX.to_string(),
        ),
        ("GIZMOTH_PACKAGES".to_string(), GUEST_ROOT.to_string()),
        // The pack is read-only: Python caches bytecode on the work disk.
        (
            "PYTHONPYCACHEPREFIX".to_string(),
            format!("{GUEST_CACHE}/pycache"),
        ),
    ];
    for package in default_packages(packages) {
        for (name, value) in &package.variables {
            let value = expand(value, package, &available, false)?;
            variables.push((name.clone(), value));
        }
    }
    Ok((vec![protocol::COMMAND_BIN.into()], variables))
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
        let requested = {
            let state = self.packages.state.lock().unwrap();
            state.active || state.index.rebuild
        };
        if ready && requested {
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
        let wanted: Vec<_> = {
            let state = self.packages.state.lock().unwrap();
            wanted(&catalog)
                .into_iter()
                .filter(|w| {
                    !removed(&state.index, w)
                        && (state.active
                            || state
                                .index
                                .repairs
                                .iter()
                                .any(|r| r.target.matches_wanted(w))
                            || state
                                .index
                                .packages
                                .iter()
                                .chain(state.pack_info.iter().flat_map(|i| &i.packages))
                                .any(|p| {
                                    p.key().as_ref() == Some(&w.key) && p.platform == w.platform
                                }))
                })
                .collect()
        };
        {
            let mut state = self.packages.state.lock().unwrap();
            let before = state.index.clone();
            state.index.packages.retain(|p| {
                p.key().is_some_and(|key| {
                    wanted.iter().any(|w| {
                        w.key == key && w.manifest == p.manifest && w.platform == p.platform
                    })
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
            if self.packages.cancel.load(Ordering::SeqCst) || self.moving() || {
                let state = self.packages.state.lock().unwrap();
                !state.active && !state.index.rebuild
            } {
                break;
            }
            let skip = {
                let state = self.packages.state.lock().unwrap();
                state.index.packages.iter().any(|p| {
                    p.key().as_ref() == Some(&item.key)
                        && p.platform == item.platform
                        && state
                            .index
                            .repairs
                            .iter()
                            .find(|r| r.target.matches_wanted(item))
                            .is_none_or(|r| r.nonce == p.repair)
                }) || state
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
                    state
                        .index
                        .packages
                        .retain(|p| p.key().as_ref() != Some(&item.key));
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
        let mut package = planned_package(item, recipe)?;
        let cancel = || {
            let state = self.packages.state.lock().unwrap();
            self.packages.cancel.load(Ordering::SeqCst) || (!state.active && !state.index.rebuild)
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
        package.repair = self
            .packages
            .state
            .lock()
            .unwrap()
            .index
            .repairs
            .iter()
            .find(|r| r.target.matches_wanted(item))
            .map(|r| r.nonce.clone())
            .unwrap_or_default();
        Ok(package)
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
        // Claim before capturing the set: a remove/repair cannot race the plan.
        if !self.begin_package_work() {
            return;
        }
        let packages = {
            let state = self.packages.state.lock().unwrap();
            pack_candidate(&state)
        };
        let Some(packages) = packages else {
            self.end_package_work();
            return;
        };
        if !self.state.lock().unwrap().image.ready() {
            self.end_package_work();
            return;
        }
        let Some(allocation) = self.reserve_preparation() else {
            self.end_package_work();
            return;
        };
        {
            let mut state = self.packages.state.lock().unwrap();
            state.pack.running = true;
            state.pack.phase = Some("Starting packer VM".into());
            state.pack.log = LogRing::default();
        }
        self.changed();
        let set = hash(&packages);
        let result = self.pack(&packages, allocation);
        let mut state = self.packages.state.lock().unwrap();
        state.pack.running = false;
        state.pack.phase = None;
        match result {
            Ok(info) => {
                for package in &info.packages {
                    if let Some(key) = package.key() {
                        state.failures.remove(&key);
                    }
                }
                state.index.repairs.retain(|r| {
                    !info
                        .packages
                        .iter()
                        .any(|p| r.target.matches(p) && p.repair == r.nonce)
                });
                state.index.rebuild = false;
                state.pack_info = Some(info);
                state.pack.error = None;
                let _ = write_atomic(&self.root().join(INDEX), &state.index);
            }
            Err(error) => {
                // Preserve the candidate and published runtime for a retry.
                if let Some(package) = packages
                    .iter()
                    .find(|p| error.starts_with(&format!("{}: ", p.dir())))
                {
                    if let Some(key) = package.key() {
                        state
                            .failures
                            .insert(key.clone(), (error.clone(), Instant::now()));
                    }
                }
                state.pack.error = Some((set, error, Instant::now()));
            }
        }
        drop(state);
        self.end_package_work();
        // Publication advances boot_generation while holding the dispatch lock;
        // warm and busy VMs retire before reuse with the new runtime identity.
        self.changed();
    }

    fn publish_runtime(&self, target: &Path, info: &PackInfo) -> Result<(), String> {
        // A starting VM must finish opening the canonical disk path before it
        // is replaced. Already-running VMs retain their read-only disk inode.
        let mut service = self.state.lock().unwrap();
        while service
            .slots
            .iter()
            .any(|s| s.state == super::SlotState::Starting)
        {
            if self.stopping.load(Ordering::SeqCst) || self.packages.cancel.load(Ordering::SeqCst) {
                return Err("cancelled".into());
            }
            service = self
                .wake
                .wait_timeout(service, Duration::from_secs(1))
                .unwrap()
                .0;
        }
        if self.stopping.load(Ordering::SeqCst) || self.packages.cancel.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        // Dispatch and package readers cannot observe a half-published pair.
        let mut state = self.packages.state.lock().unwrap();
        publish_pack(&self.root(), target, info)?;
        state.pack_info = Some(info.clone());
        service.boot_generation += 1;
        Ok(())
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
                command_paths: command_paths(packages),
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
            self.publish_runtime(&target, &info)?;
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

    /// Serialized with job submission, storage moves and maintenance claims.
    pub(crate) fn mutate_package(
        self: &Arc<Self>,
        body: Value,
        reinstall: bool,
    ) -> Result<(), (u16, String)> {
        let target: PackageTarget =
            serde_json::from_value(body).map_err(|e| super::bad(e.to_string()))?;
        let service = self.state.lock().unwrap();
        let storage = self.storage.lock().unwrap();
        let mut state = self.packages.state.lock().unwrap();
        let catalog = state
            .catalog
            .clone()
            .ok_or_else(|| (409, "Refresh the registry before managing packages".into()))?;
        let item = wanted(&catalog)
            .into_iter()
            .find(|w| target.matches_wanted(w))
            .ok_or_else(|| {
                super::bad("The requested platform/version is not a managed Linux recipe")
            })?;
        if let Some(reason) = package_management_reason(&service, storage.moving.is_some(), &state)
        {
            return Err((409, reason));
        }
        if let Some(reason) = &item.unsupported {
            return Err((409, reason.clone()));
        }
        let (release, _) = catalog
            .release(item.key.0, &item.key.1, &item.key.2)
            .unwrap();
        let recipe = &release.platforms[&item.platform];
        let reason = if reinstall {
            reinstall_reason(&state, recipe, &item)
        } else {
            removal_reason(&state, &item, recipe)
        };
        if let Some(reason) = reason {
            return Err((409, reason));
        }
        let mut index = state.index.clone();
        let mut inputs = None;
        if reinstall {
            index.removed.retain(|p| !target.matches(p));
            index.repairs.retain(|r| r.target != target);
            index.repairs.push(Repair {
                target,
                nonce: uuid::Uuid::new_v4().to_string(),
            });
            index.rebuild = true;
        } else {
            inputs = RemovedInputs::stage(&self.root(), &unshared_inputs(&state, &target, recipe))
                .map_err(super::bad)?;
            let package = planned_package(&item, recipe).map_err(super::bad)?;
            index.packages.retain(|p| !target.matches(p));
            index.removed.push(package);
            index.repairs.retain(|r| r.target != target);
            index.rebuild = !index.repairs.is_empty()
                || state
                    .pack_info
                    .as_ref()
                    .is_some_and(|info| info.hash != hash(&index.packages));
        }
        write_atomic(&self.root().join(INDEX), &index).map_err(super::bad)?;
        if let Some(inputs) = inputs.as_mut() {
            inputs.committed = true;
        }
        state.index = index;
        state.failures.remove(&item.key);
        state.pack.error = None;
        drop(state);
        drop(storage);
        drop(service);
        drop(inputs);
        self.packages.notify();
        self.changed();
        Ok(())
    }

    pub(crate) fn clear_downloads(&self) -> Result<(), (u16, String)> {
        let storage = self.storage.lock().unwrap();
        let mut state = self.packages.state.lock().unwrap();
        if storage.moving.is_some() || state.busy || state.refreshing || state.index.rebuild {
            return Err((
                409,
                "Wait for downloads and runtime changes to finish before clearing archives".into(),
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
            removed: state.index.removed.clone(),
            ..Index::default()
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

    pub(crate) fn packages_view(&self, service: &super::State) -> Value {
        // The snapshot caller already holds the service state lock.
        let storage = self.storage.lock().unwrap();
        let state = self.packages.state.lock().unwrap();
        let blocked = package_management_reason(service, storage.moving.is_some(), &state);
        drop(storage);
        let set = hash(&state.index.packages);
        let packed = |key: &Key, manifest: &str, platform: &str| {
            state.pack_info.as_ref().is_some_and(|info| {
                info.packages.iter().any(|p| {
                    p.key().as_ref() == Some(key)
                        && p.manifest == manifest
                        && p.platform == platform
                })
            })
        };
        let downloaded = |key: &Key, manifest: &str, platform: &str| {
            state.index.packages.iter().any(|p| {
                p.key().as_ref() == Some(key) && p.manifest == manifest && p.platform == platform
            })
        };
        let mut items = Vec::new();
        let catalog_items = state
            .catalog
            .as_deref()
            .map(catalog_items)
            .unwrap_or_default();
        for item in &catalog_items {
            let progress = state.downloading.as_ref().filter(|c| c.key == item.key);
            let repairing = state
                .index
                .repairs
                .iter()
                .any(|r| r.target.matches_wanted(item));
            let status = if let Some(reason) = &item.unsupported {
                json!({ "state": "unsupported", "error": reason })
            } else if removed(&state.index, item) {
                json!({ "state": "removed" })
            } else if let Some(progress) = progress {
                json!({ "state": "downloading", "done": progress.done, "total": progress.total })
            } else if repairing && state.pack.running {
                json!({ "state": "packing" })
            } else if let Some((error, _)) = state.failures.get(&item.key) {
                json!({ "state": "failed", "error": error })
            } else if repairing {
                if let Some((_, error, _)) = state.pack.error.as_ref().filter(|(h, _, _)| *h == set)
                {
                    json!({ "state": "failed", "error": error })
                } else {
                    json!({ "state": if state.pack.running { "packing" } else if repair_pending(&state.index) { "pending" } else { "downloaded" } })
                }
            } else if packed(&item.key, &item.manifest, &item.platform) {
                json!({ "state": "installed" })
            } else if downloaded(&item.key, &item.manifest, &item.platform) {
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
            let unavailable = item.unsupported.clone().or_else(|| blocked.clone());
            let remove_reason = unavailable
                .clone()
                .or_else(|| recipe.and_then(|r| removal_reason(&state, item, r)));
            let reinstall_reason =
                unavailable.or_else(|| recipe.and_then(|r| reinstall_reason(&state, r, item)));
            let path = if !item.managed {
                String::new()
            } else if guest_root {
                "/".into()
            } else {
                format!(
                    "{GUEST_ROOT}/{}/{}/{}",
                    item.key.0.directory(),
                    item.key.1,
                    item.key.2
                )
            };
            let install_path = if item.managed && guest_root {
                recipe
                    .and_then(recipe_install_path)
                    .unwrap_or_else(|| path.clone())
            } else {
                path.clone()
            };
            let linux_paths = if item.managed {
                linux_paths(
                    guest_root,
                    &install_path,
                    recipe
                        .into_iter()
                        .flat_map(|r| &r.checks)
                        .filter_map(|check| match check {
                            Check::File { path, .. } => Some(path.as_str()),
                            Check::Command { .. } => None,
                        }),
                )
            } else {
                Vec::new()
            };
            let mut row = json!({
                "guestRoot": guest_root, "bootstrap": recipe.is_some_and(|r| r.bootstrap),
                "kind": item.key.0.as_str(), "id": item.key.1, "version": item.key.2,
                "name": item.name, "default": item.default, "download": item.download, "platform": item.platform,
                "labels": recipe.map(|r| recipe_labels(&item.key.1, &item.platform, r)).unwrap_or_default(),
                "path": path, "installPath": install_path, "linuxPaths": linux_paths,
                "hostPaths": host_paths(
                    &self.root(),
                    recipe.filter(|_| item.managed).into_iter().flat_map(recipe_inputs),
                    item.managed && packed(&item.key, &item.manifest, &item.platform),
                ),
                "commandPath": (item.managed && item.default && !guest_root
                    && recipe.is_some_and(|r| !r.environment.prepend_path.is_empty()))
                    .then_some(protocol::COMMAND_BIN),
                "repairing": item.managed && repairing,
                "canRemove": remove_reason.is_none(), "canReinstall": reinstall_reason.is_none(),
                "removeReason": remove_reason, "reinstallReason": reinstall_reason,
            });
            row.as_object_mut()
                .unwrap()
                .extend(status.as_object().unwrap().clone());
            items.push(row);
        }
        // Packed releases without registry information (an offline start).
        if let Some(info) = &state.pack_info {
            for package in &info.packages {
                if package.key().is_some_and(|key| {
                    catalog_items
                        .iter()
                        .any(|w| w.key == key && w.platform == package.platform)
                }) || state
                    .index
                    .removed
                    .iter()
                    .any(|p| p.key() == package.key() && p.platform == package.platform)
                {
                    continue;
                }
                items.push(json!({
                    "kind": package.kind, "id": package.id, "version": package.version,
                    "name": package.name, "default": package.default, "download": package.download,
                    "labels": cached_labels(package),
                    "guestRoot": package.guest_root, "bootstrap": package.bootstrap, "platform": package.platform,
                    "path": if package.guest_root { "/".to_string() } else { prefix(package) }, "state": "installed",
                    "linuxPaths": linux_paths(package.guest_root, &prefix(package), package.checks.iter().map(|check| check.path.as_str())),
                    "hostPaths": host_paths(&self.root(), cached_inputs(package), true),
                    "commandPath": (package.default && !package.guest_root && !package.prepend_path.is_empty())
                        .then_some(protocol::COMMAND_BIN),
                    "canRemove": false, "canReinstall": false,
                    "repairing": state.index.repairs.iter().any(|r| r.target.matches(package)),
                    "removeReason": "Refresh the registry before managing this package",
                    "reinstallReason": "Refresh the registry before managing this package",
                }));
            }
        }
        for package in &state.index.removed {
            if catalog_items
                .iter()
                .any(|w| package.key().as_ref() == Some(&w.key) && package.platform == w.platform)
            {
                continue;
            }
            items.push(json!({
                "kind": package.kind, "id": package.id, "version": package.version,
                "name": package.name, "default": package.default, "download": package.download,
                "labels": cached_labels(package),
                "guestRoot": package.guest_root, "bootstrap": package.bootstrap, "platform": package.platform,
                "path": "", "linuxPaths": [], "hostPaths": { "downloads": [], "runtime": null }, "state": "removed", "canRemove": false, "canReinstall": false, "repairing": false,
                "removeReason": "This package is already removed",
                "reinstallReason": "Refresh the registry before reinstalling this package",
            }));
        }
        let platforms: BTreeSet<_> = std::iter::once(PLATFORM.to_string())
            .chain(
                items
                    .iter()
                    .filter_map(|p| p["platform"].as_str())
                    .filter(|p| p.starts_with("linux-"))
                    .map(str::to_owned),
            )
            .collect();
        let pack = if state.pack.running {
            json!({ "state": "packing", "phase": state.pack.phase })
        } else if state.index.packages.is_empty() && state.pack_info.is_none() {
            json!({ "state": "none" })
        } else if let Some(info) = state.pack_info.as_ref().filter(|i| {
            !repair_pending(&state.index)
                && (i.hash == set
                    || (state.index.paused
                        && !state.index.rebuild
                        && state.index.packages.is_empty()))
        }) {
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
            "platforms": platforms,
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

    #[cfg(unix)]
    #[test]
    fn package_input_cleanup_rejects_a_symlinked_assets_directory() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let digest = "a".repeat(64);
        fs::write(outside.path().join(&digest), b"keep outside cache").unwrap();
        symlink(outside.path(), root.path().join("assets")).unwrap();
        assert!(RemovedInputs::stage(root.path(), &[digest.clone()].into()).is_err());
        assert_eq!(
            fs::read(outside.path().join(digest)).unwrap(),
            b"keep outside cache"
        );
    }

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
            repair: String::new(),
            sources: Vec::new(),
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
        assert_eq!(path, [protocol::COMMAND_BIN]);
        assert_eq!(
            command_paths(&packages),
            [
                "/opt/gizmoth/packages/toolchains/bazelisk/1.30.0/bin",
                "/opt/gizmoth/packages/toolchains/python/3.12.16/bin"
            ]
        );
        assert!(variables.contains(&("GIZMOTH_PREFIX".into(), "/opt/gizmoth".into())));
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
        assert_eq!(before_path, [protocol::COMMAND_BIN]);
        assert_eq!(
            after_path, before_path,
            "the public command directory stays stable"
        );
        assert_eq!(
            command_paths(&before),
            [format!("{GUEST_ROOT}/toolchains/python/3.12.16/bin")]
        );
        assert_eq!(
            command_paths(&after),
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

    /// Exercise activation with a copied Python venv and a location-sensitive
    /// script, in an isolated real Linux assembly rather than the formal app.
    #[test]
    #[ignore = "requires staged VM resources and GIZMOTH_LIVE_PACK_SEED"]
    fn live_managed_command_activation_preserves_venvs_and_script_paths() {
        let seed = PathBuf::from(std::env::var_os("GIZMOTH_LIVE_PACK_SEED").unwrap());
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
        let mut index: Index =
            serde_json::from_slice(&fs::read(seed.join(INDEX)).unwrap()).unwrap();
        let mut fixture = package("activation-proof", "1.0", true);
        fixture.prepend_path = vec!["venv/bin".into(), "bin".into()];
        fixture.variables = [("ACTIVATION_PREFIX".into(), "${prefix}".into())].into();
        fixture
            .dependencies
            .insert("python".into(), "3.11.18".into());
        fixture.steps.push(PackStep::Run {
            program: "${toolchain:python}/python/bin/python3".into(),
            args: [
                "-I",
                "-m",
                "venv",
                "--copies",
                "--without-pip",
                "${prefix}/venv",
            ]
            .map(String::from)
            .into(),
            cwd: ".".into(),
            environment: BTreeMap::new(),
            timeout_seconds: 30,
        });
        fixture.steps.push(PackStep::Run {
            program: "/bin/sh".into(),
            args: vec!["-ec".into(), r#"
                mv venv/bin/python3 venv/bin/gizmoth-venv-proof
                rm venv/bin/python venv/bin/python3.11
                printf 'IDENTITY = 42\n' > venv/lib/python3.11/site-packages/gizmoth_activation_fixture.py
                mkdir -p bin
                printf '%s\n' '#!/bin/sh' 'test "$(dirname "$0")" = "$ACTIVATION_PREFIX/bin" || exit 41' 'printf "SCRIPT_ORIGIN_OK\\n"' > bin/gizmoth-script-proof
                chmod +x bin/gizmoth-script-proof
            "#.into()],
            cwd: ".".into(), environment: BTreeMap::new(), timeout_seconds: 10,
        });
        fixture.command_checks.push(PackCommandCheck {
            program: "/bin/sh".into(),
            args: vec!["-ec".into(), r#"
                PATH=/opt/gizmoth/bin:/usr/bin:/bin
                export PATH
                gizmoth-venv-proof -c 'import os,sys,gizmoth_activation_fixture as f; assert sys.prefix == os.environ["ACTIVATION_PREFIX"] + "/venv"; assert f.IDENTITY == 42; print("COPIED_VENV_OK")'
                gizmoth-script-proof
                printf 'MANAGED_ACTIVATION_OK\n'
            "#.into()],
            stdout_contains: Some("MANAGED_ACTIVATION_OK".into()), timeout_seconds: 15,
        });
        index.packages.push(fixture);
        index.paused = true;
        let service = Service::load(temporary.path());
        let allocation = service.reserve_preparation().unwrap();
        service.pack(&index.packages, allocation).unwrap();
        write_atomic(&root.join(INDEX), &index).unwrap();
        assert_eq!(service.resources.snapshot().vm_count, 0);
        service.shutdown();
        fs::write(
            temporary.path().join(".gizmoth-live-pack-test"),
            b"Isolated managed activation test\n",
        )
        .unwrap();
        println!("LIVE_ACTIVATION_DATA={}", temporary.keep().display());
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
                ..Index::default()
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

    fn management_files() -> BTreeMap<String, String> {
        let mut files = registry::tests::files();
        for (kind, id, version, payload) in [
            (Kind::Toolchain, "bazelisk", "1.30.0", b"root".as_slice()),
            (
                Kind::Toolchain,
                "xtensa-esp-elf",
                "14.2.0",
                b"tool".as_slice(),
            ),
            (Kind::Sdk, "esp-idf", "6.2.0", b"sdk!".as_slice()),
        ] {
            let mut recipe = registry::tests::recipe(
                &[],
                if kind == Kind::Sdk {
                    Some(&["esp32s3"])
                } else {
                    None
                },
            );
            recipe["sources"][0]["sha256"] = registry::hex_digest(payload).into();
            recipe["sources"][0]["url"] = "https://127.0.0.1:1/unused".into();
            if id == "bazelisk" {
                recipe["bootstrap"] = true.into();
                recipe["guestRoot"] = true.into();
            }
            files.insert(
                registry::manifest_path(kind, id, version),
                registry::tests::release(kind, id, version, json!({PLATFORM: recipe})),
            );
        }
        files
    }

    fn edit_recipe(
        files: &mut BTreeMap<String, String>,
        kind: Kind,
        id: &str,
        version: &str,
        edit: impl FnOnce(&mut Value),
    ) {
        let key = registry::manifest_path(kind, id, version);
        let mut release: Value = serde_json::from_str(&files[&key]).unwrap();
        edit(&mut release["platforms"][PLATFORM]);
        files.insert(key, release.to_string());
    }

    fn management_service(files: BTreeMap<String, String>) -> (tempfile::TempDir, Arc<Service>) {
        let dir = tempfile::tempdir().unwrap();
        let service = Service::load(dir.path());
        let catalog = service
            .packages
            .publish_catalog(SavedSnapshot {
                commit: "c".repeat(40),
                fetched_at: crate::serial::now_ms(),
                files,
            })
            .unwrap();
        let root = service.root();
        fs::create_dir_all(root.join("assets")).unwrap();
        let mut packages = Vec::new();
        for item in wanted(&catalog) {
            let recipe = &catalog
                .release(item.key.0, &item.key.1, &item.key.2)
                .unwrap()
                .0
                .platforms[&item.platform];
            let payload: &[u8] = match item.key.1.as_str() {
                "bazelisk" => b"root",
                "xtensa-esp-elf" => b"tool",
                _ => b"sdk!",
            };
            for source in recipe_inputs(recipe) {
                // Shared-source tests explicitly replace this cache with matching bytes.
                fs::write(root.join("assets").join(source), payload).unwrap();
            }
            packages.push(planned_package(&item, recipe).unwrap());
        }
        let info = PackInfo {
            agent_version: protocol::AGENT_VERSION,
            hash: hash(&packages),
            bytes: 11,
            built_at: 1,
            packages: packages.clone(),
        };
        let mut state = service.packages.state.lock().unwrap();
        state.catalog = Some(Arc::new(catalog));
        state.refresh_requested = false;
        state.index.packages = packages;
        state.pack_info = Some(info.clone());
        write_atomic(&root.join(INDEX), &state.index).unwrap();
        write_atomic(&root.join(PACK_INFO), &info).unwrap();
        fs::write(root.join(PACK), b"old runtime").unwrap();
        drop(state);
        (dir, service)
    }

    fn management_body(id: &str) -> Value {
        let (kind, version) = match id {
            "bazelisk" => ("toolchain", "1.30.0"),
            "xtensa-esp-elf" => ("toolchain", "14.2.0"),
            _ => ("sdk", "6.2.0"),
        };
        json!({"kind": kind, "id": id, "version": version, "platform": PLATFORM})
    }

    fn row(service: &Service, id: &str, platform: &str) -> Value {
        service.snapshot()["packages"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id && p["platform"] == platform)
            .unwrap()
            .clone()
    }

    #[test]
    fn removal_survives_restart_and_upkeep_and_keeps_the_previous_runtime_until_rebuilt() {
        let (dir, service) = management_service(management_files());
        let root = service.root();
        let source = cached_inputs(
            &service
                .packages
                .state
                .lock()
                .unwrap()
                .index
                .packages
                .iter()
                .find(|p| p.id == "xtensa-esp-elf")
                .unwrap()
                .clone(),
        )
        .next()
        .unwrap()
        .to_owned();
        let old_info = fs::read(root.join(PACK_INFO)).unwrap();
        let result = service
            .handle(
                "DELETE",
                &["packages"],
                "",
                management_body("xtensa-esp-elf"),
                true,
            )
            .unwrap();
        assert_eq!(
            result.1["packages"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["id"] == "xtensa-esp-elf")
                .unwrap()["state"],
            "removed"
        );
        assert!(!root.join("assets").join(source).exists());
        assert_eq!(fs::read(root.join(PACK)).unwrap(), b"old runtime");
        assert_eq!(fs::read(root.join(PACK_INFO)).unwrap(), old_info);
        {
            let mut state = service.packages.state.lock().unwrap();
            let at = state
                .index
                .packages
                .iter()
                .position(|p| p.id == "esp-idf")
                .unwrap();
            let retained = state.index.packages.remove(at);
            assert!(pack_candidate(&state).is_none(), "a failed retained download cannot remove another installed package from the replacement");
            state.index.packages.push(retained);
        }
        service.sync_packages();
        assert!(!service
            .packages
            .state
            .lock()
            .unwrap()
            .index
            .packages
            .iter()
            .any(|p| p.id == "xtensa-esp-elf"));
        let restarted = Service::load(dir.path());
        restarted.sync_packages();
        let removed = row(&restarted, "xtensa-esp-elf", PLATFORM);
        assert_eq!(removed["state"], "removed");
        assert_eq!(removed["canRemove"], false);
        assert_eq!(removed["canReinstall"], true);
        restarted.packages_pause();
        assert!(
            pack_candidate(&restarted.packages.state.lock().unwrap()).is_some(),
            "an explicit removal completes even with upkeep paused"
        );
    }

    #[test]
    fn removal_protects_bootstrap_and_exact_retained_dependencies() {
        let mut files = management_files();
        edit_recipe(&mut files, Kind::Sdk, "esp-idf", "6.2.0", |recipe| {
            recipe["dependencies"] = json!([{"toolchain":"xtensa-esp-elf", "version":"14.2.0"}]);
        });
        let (_dir, service) = management_service(files);
        let error = service
            .mutate_package(management_body("bazelisk"), false)
            .unwrap_err();
        assert_eq!(error.0, 409);
        assert!(error.1.contains("bootstrap"));
        let dependency = row(&service, "xtensa-esp-elf", PLATFORM);
        assert_eq!(dependency["canRemove"], false);
        assert!(dependency["removeReason"]
            .as_str()
            .unwrap()
            .contains("esp-idf"));
        assert!(service
            .mutate_package(management_body("xtensa-esp-elf"), false)
            .is_err());
        service
            .mutate_package(management_body("esp-idf"), false)
            .unwrap();
        service
            .mutate_package(management_body("xtensa-esp-elf"), false)
            .unwrap();
        assert!(service
            .mutate_package(management_body("esp-idf"), true)
            .unwrap_err()
            .1
            .contains("Reinstall required toolchain"));
    }

    #[test]
    fn removal_preserves_shared_inputs_and_rolls_back_inputs_when_saving_fails() {
        let mut files = management_files();
        let tool_key = registry::manifest_path(Kind::Toolchain, "xtensa-esp-elf", "14.2.0");
        let tool: Value = serde_json::from_str(&files[&tool_key]).unwrap();
        let shared = tool["platforms"][PLATFORM]["sources"][0]["sha256"]
            .as_str()
            .unwrap()
            .to_owned();
        edit_recipe(&mut files, Kind::Sdk, "esp-idf", "6.2.0", |recipe| {
            recipe["sources"][0]["sha256"] = shared.clone().into()
        });
        let (_dir, service) = management_service(files);
        let root = service.root();
        fs::write(root.join("assets").join(&shared), b"tool").unwrap();
        service
            .mutate_package(management_body("xtensa-esp-elf"), false)
            .unwrap();
        assert_eq!(
            fs::read(root.join("assets").join(&shared)).unwrap(),
            b"tool"
        );
        fs::remove_file(root.join(INDEX)).unwrap();
        fs::create_dir(root.join(INDEX)).unwrap();
        assert!(service
            .mutate_package(management_body("esp-idf"), false)
            .is_err());
        assert_eq!(
            fs::read(root.join("assets").join(&shared)).unwrap(),
            b"tool"
        );
        assert_eq!(row(&service, "esp-idf", PLATFORM)["state"], "installed");
    }

    #[test]
    fn reinstall_revalidates_sources_and_forces_identical_recipes_across_failure_and_restart() {
        let (dir, service) = management_service(management_files());
        let root = service.root();
        let previous_info = fs::read(root.join(PACK_INFO)).unwrap();
        let previous_hash = service
            .packages
            .state
            .lock()
            .unwrap()
            .pack_info
            .as_ref()
            .unwrap()
            .hash
            .clone();
        let item = wanted(
            service
                .packages
                .state
                .lock()
                .unwrap()
                .catalog
                .as_ref()
                .unwrap(),
        )
        .into_iter()
        .find(|w| w.key.1 == "xtensa-esp-elf")
        .unwrap();
        let catalog = service
            .packages
            .state
            .lock()
            .unwrap()
            .catalog
            .clone()
            .unwrap();
        let source = recipe_inputs(
            &catalog
                .release(item.key.0, &item.key.1, &item.key.2)
                .unwrap()
                .0
                .platforms[PLATFORM],
        )
        .next()
        .unwrap();
        fs::write(root.join("assets").join(source), b"corrupt").unwrap();
        service.packages_pause();
        service
            .handle(
                "POST",
                &["packages", "reinstall"],
                "",
                management_body("xtensa-esp-elf"),
                true,
            )
            .unwrap();
        assert_eq!(service.snapshot()["packages"]["paused"], true);
        assert_eq!(row(&service, "xtensa-esp-elf", PLATFORM)["repairing"], true);
        let nonce = service.packages.state.lock().unwrap().index.repairs[0]
            .nonce
            .clone();
        assert!(
            pack_candidate(&service.packages.state.lock().unwrap()).is_none(),
            "sources must be revalidated before packing"
        );
        service.sync_packages(); // Loopback port 1 refuses the redownload; no external traffic.
        assert!(
            !root.join("assets").join(source).exists(),
            "the corrupt cache must be rejected"
        );
        assert_eq!(row(&service, "xtensa-esp-elf", PLATFORM)["state"], "failed");
        assert_eq!(fs::read(root.join(PACK)).unwrap(), b"old runtime");
        assert_eq!(fs::read(root.join(PACK_INFO)).unwrap(), previous_info);
        let restarted = Service::load(dir.path());
        assert_eq!(
            restarted.packages.state.lock().unwrap().index.repairs[0].nonce,
            nonce
        );
        fs::write(root.join("assets").join(source), b"tool").unwrap();
        restarted.sync_packages();
        let packages = pack_candidate(&restarted.packages.state.lock().unwrap())
            .expect("verified repair inputs must produce a fresh pack candidate after restart");
        assert_ne!(hash(&packages), previous_hash);
        assert_eq!(
            packages
                .iter()
                .find(|p| p.id == "xtensa-esp-elf")
                .unwrap()
                .manifest,
            item.manifest
        );
        assert!(pack_items(&packages).is_ok());
        let target = root.join(format!("{PACK}.new"));
        fs::write(&target, b"replacement runtime").unwrap();
        let info = PackInfo {
            agent_version: protocol::AGENT_VERSION,
            hash: hash(&packages),
            packages,
            ..PackInfo::default()
        };
        assert!(
            publish_pack_with(&root, &target, &info, |_, _| Err(std::io::Error::other(
                "metadata publication failed"
            )))
            .is_err()
        );
        assert_eq!(fs::read(root.join(PACK)).unwrap(), b"old runtime");
        assert_eq!(fs::read(root.join(PACK_INFO)).unwrap(), previous_info);
        assert!(restarted.packages.state.lock().unwrap().index.rebuild);
        let generation = {
            let mut state = restarted.state.lock().unwrap();
            let generation = state.boot_generation;
            state.slots[0].state = super::super::SlotState::Ready;
            state.slots[0].boot_generation = generation;
            generation
        };
        assert!(
            restarted.publish_runtime(&target, &info).is_err(),
            "the failed replacement target was consumed"
        );
        assert_eq!(restarted.state.lock().unwrap().boot_generation, generation);
        fs::write(&target, b"repaired runtime").unwrap();
        restarted.publish_runtime(&target, &info).unwrap();
        {
            let state = restarted.state.lock().unwrap();
            assert_eq!(state.boot_generation, generation + 1);
            assert!(
                !state.slots[0].reusable(state.boot_generation),
                "even a repair of the same manifest must retire the old warm VM"
            );
        }
        let recovered = Service::load(dir.path());
        assert_eq!(fs::read(root.join(PACK)).unwrap(), b"repaired runtime");
        assert!(recovered
            .packages
            .state
            .lock()
            .unwrap()
            .index
            .repairs
            .is_empty());
        assert_eq!(
            row(&recovered, "xtensa-esp-elf", PLATFORM)["state"],
            "installed"
        );
        assert_eq!(
            row(&recovered, "xtensa-esp-elf", PLATFORM)["repairing"],
            false
        );
        assert_eq!(recovered.snapshot()["packages"]["paused"], true);
        assert!(pack_candidate(&recovered.packages.state.lock().unwrap()).is_none());
        recovered
            .mutate_package(management_body("xtensa-esp-elf"), true)
            .unwrap();
        assert_ne!(
            recovered.packages.state.lock().unwrap().index.repairs[0].nonce,
            nonce,
            "a subsequent repair must not reuse the installed revision"
        );
    }

    #[test]
    fn runtime_publication_waits_for_vm_startup_and_retires_the_old_generation() {
        let (_dir, service) = management_service(management_files());
        let root = service.root();
        let mut info = service
            .packages
            .state
            .lock()
            .unwrap()
            .pack_info
            .clone()
            .unwrap();
        info.packages[0].repair = "fresh-repair".into();
        info.hash = hash(&info.packages);
        let target = root.join(format!("{PACK}.new"));
        fs::write(&target, b"new qualified runtime").unwrap();
        let generation = {
            let mut state = service.state.lock().unwrap();
            state.slots[0].state = super::super::SlotState::Starting;
            state.boot_generation
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let publishing = service.clone();
        let thread = std::thread::spawn(move || {
            sender
                .send(publishing.publish_runtime(&target, &info))
                .unwrap()
        });
        assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
        assert_eq!(fs::read(root.join(PACK)).unwrap(), b"old runtime");
        service.state.lock().unwrap().slots[0].state = super::super::SlotState::Busy;
        service.changed();
        receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        thread.join().unwrap();
        assert_eq!(fs::read(root.join(PACK)).unwrap(), b"new qualified runtime");
        assert_eq!(
            service.state.lock().unwrap().boot_generation,
            generation + 1
        );
    }

    #[test]
    fn interrupted_runtime_publication_restores_both_files_before_loading() {
        let (dir, service) = management_service(management_files());
        let root = service.root();
        let previous_info = fs::read(root.join(PACK_INFO)).unwrap();
        let staging = root.join(".pack");
        fs::create_dir_all(&staging).unwrap();
        fs::hard_link(root.join(PACK), staging.join("previous.erofs")).unwrap();
        fs::hard_link(root.join(PACK_INFO), staging.join("previous.json")).unwrap();
        write_atomic(
            &staging.join("publishing.json"),
            &Publication {
                had_pack: true,
                had_info: true,
            },
        )
        .unwrap();
        fs::write(root.join("new-pack"), b"interrupted replacement").unwrap();
        fs::rename(root.join("new-pack"), root.join(PACK)).unwrap();
        let restarted = Service::load(dir.path());
        assert_eq!(fs::read(root.join(PACK)).unwrap(), b"old runtime");
        assert_eq!(fs::read(root.join(PACK_INFO)).unwrap(), previous_info);
        assert!(restarted.package_mount().is_some());
        assert!(!staging.join("publishing.json").exists());
    }

    #[test]
    fn linux_catalog_projects_foreign_recipes_without_enabling_mutation_or_downloads() {
        let mut files = management_files();
        let path = registry::manifest_path(Kind::Toolchain, "xtensa-esp-elf", "14.2.0");
        let mut release: Value = serde_json::from_str(&files[&path]).unwrap();
        release["platforms"]["linux-x86_64"] = release["platforms"][PLATFORM].clone();
        release["platforms"]["windows-x86_64"] = release["platforms"][PLATFORM].clone();
        files.insert(path, release.to_string());
        let (_dir, service) = management_service(files);
        let snapshot = service.snapshot();
        assert_eq!(
            snapshot["packages"]["platforms"],
            json!(["linux-arm64", "linux-x86_64"])
        );
        assert_eq!(snapshot["packages"]["items"].as_array().unwrap().len(), 4);
        let foreign = row(&service, "xtensa-esp-elf", "linux-x86_64");
        assert_eq!(foreign["state"], "unsupported");
        assert_eq!(foreign["path"], "");
        assert_eq!(foreign["installPath"], "");
        assert_eq!(foreign["linuxPaths"], json!([]));
        assert_eq!(
            foreign["hostPaths"],
            json!({"downloads": [], "runtime": null})
        );
        assert!(foreign["commandPath"].is_null());
        assert_eq!(foreign["canRemove"], false);
        assert_eq!(foreign["canReinstall"], false);
        assert!(foreign["reinstallReason"].as_str().is_some());
        assert_eq!(
            row(&service, "xtensa-esp-elf", PLATFORM)["state"],
            "installed"
        );
        for (method, path) in [
            ("DELETE", vec!["packages"]),
            ("POST", vec!["packages", "reinstall"]),
        ] {
            let mut body = management_body("xtensa-esp-elf");
            body["platform"] = "linux-x86_64".into();
            assert_eq!(
                service
                    .handle(method, &path, "", body.clone(), false)
                    .unwrap_err()
                    .0,
                403
            );
            assert_eq!(
                service.handle(method, &path, "", body, true).unwrap_err().0,
                400
            );
            let mut missing = management_body("xtensa-esp-elf");
            missing.as_object_mut().unwrap().remove("platform");
            assert_eq!(
                service
                    .handle(method, &path, "", missing, true)
                    .unwrap_err()
                    .0,
                400
            );
            let mut arbitrary = management_body("xtensa-esp-elf");
            arbitrary["id"] = "../../outside".into();
            assert_eq!(
                service
                    .handle(method, &path, "", arbitrary, true)
                    .unwrap_err()
                    .0,
                400
            );
        }
        assert!(service
            .packages
            .state
            .lock()
            .unwrap()
            .index
            .repairs
            .is_empty());
        assert!(service
            .packages
            .state
            .lock()
            .unwrap()
            .index
            .removed
            .is_empty());
    }

    #[test]
    fn management_refuses_active_jobs_preparation_and_storage_moves() {
        let (_dir, service) = management_service(management_files());
        for slot_state in [
            super::super::SlotState::Busy,
            super::super::SlotState::Starting,
            super::super::SlotState::Stopping,
        ] {
            service.state.lock().unwrap().slots[0].state = slot_state;
            assert_eq!(
                service
                    .mutate_package(management_body("xtensa-esp-elf"), false)
                    .unwrap_err()
                    .0,
                409
            );
            assert_eq!(
                row(&service, "xtensa-esp-elf", PLATFORM)["canReinstall"],
                false
            );
        }
        service.state.lock().unwrap().slots[0].state = super::super::SlotState::Stopped;
        assert!(service.begin_package_work());
        assert_eq!(
            service
                .mutate_package(management_body("xtensa-esp-elf"), true)
                .unwrap_err()
                .0,
            409
        );
        service.end_package_work();
        service.storage.lock().unwrap().moving = Some(super::super::Move {
            from: service.root(),
            to: service.root().join("elsewhere"),
            phase: "moving",
            done: 0,
            total: 0,
            started_at: 0,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        assert_eq!(
            service
                .mutate_package(management_body("xtensa-esp-elf"), false)
                .unwrap_err()
                .0,
            409
        );
        service.storage.lock().unwrap().moving = None;
        service.packages.state.lock().unwrap().refreshing = true;
        assert_eq!(
            service
                .mutate_package(management_body("xtensa-esp-elf"), true)
                .unwrap_err()
                .0,
            409
        );
        service.packages.state.lock().unwrap().refreshing = false;
        let (job, _) = service
            .submit(
                "test".into(),
                "queued build".into(),
                super::super::jobs::BuildRequest {
                    workspace: service.root(),
                    command: vec!["true".into()],
                    cwd: String::new(),
                    env: BTreeMap::new(),
                    artifacts: Vec::new(),
                    exclude: Vec::new(),
                    timeout_seconds: None,
                    collect_on_failure: false,
                },
            )
            .unwrap();
        assert_eq!(
            service
                .mutate_package(management_body("xtensa-esp-elf"), false)
                .unwrap_err()
                .0,
            409
        );
        service.cancel_job(&job);
        assert!(service
            .mutate_package(management_body("xtensa-esp-elf"), false)
            .is_ok());
    }

    #[test]
    fn linux_paths_show_root_executables_and_keep_versioned_nonroot_directories() {
        let mut files = management_files();
        edit_recipe(
            &mut files,
            Kind::Toolchain,
            "bazelisk",
            "1.30.0",
            |recipe| {
                recipe["install"] = json!([{
                    "type": "extract", "source": "tool", "to": ".",
                    "stripComponents": 0, "format": "deb"
                }]);
                recipe["checks"] = json!([
                    {"type": "command", "program": "/usr/bin/git", "args": ["--version"], "timeoutSeconds": 10},
                    {"type": "file", "path": "usr/bin/git", "executable": true}
                ]);
            },
        );
        let (_dir, service) = management_service(files);
        let root = row(&service, "bazelisk", PLATFORM);
        assert_eq!(root["path"], "/");
        assert_eq!(root["installPath"], "/");
        assert_eq!(root["linuxPaths"], json!(["/usr/bin/git"]));
        let tool = row(&service, "xtensa-esp-elf", PLATFORM);
        assert_eq!(
            tool["linuxPaths"],
            json!([format!("{GUEST_ROOT}/toolchains/xtensa-esp-elf/14.2.0")])
        );
        assert_eq!(tool["linuxPaths"][0], tool["path"]);
        assert_eq!(tool["installPath"], tool["path"]);
    }

    #[test]
    fn linux_paths_keep_root_check_order_without_doubling_foreign_library_prefixes_offline() {
        let mut files = management_files();
        edit_recipe(
            &mut files,
            Kind::Toolchain,
            "bazelisk",
            "1.30.0",
            |recipe| {
                recipe["install"] = json!([{
                    "type": "extract", "source": "tool", "to": "opt/gizmoth/x86_64",
                    "stripComponents": 0, "format": "tar.gz"
                }]);
                recipe["checks"] = json!([
                    {"type": "file", "path": "opt/gizmoth/x86_64/lib/libz.so.1", "executable": false},
                    {"type": "file", "path": "opt/gizmoth/x86_64/lib/libc.so.6", "executable": false},
                    {"type": "file", "path": "opt/gizmoth/x86_64/lib/libz.so.1", "executable": false}
                ]);
            },
        );
        let (dir, service) = management_service(files);
        let expected = json!([
            "/opt/gizmoth/x86_64/lib/libz.so.1",
            "/opt/gizmoth/x86_64/lib/libc.so.6"
        ]);
        let root = row(&service, "bazelisk", PLATFORM);
        assert_eq!(root["path"], "/");
        assert_eq!(root["installPath"], "/opt/gizmoth/x86_64");
        assert_eq!(root["linuxPaths"], expected);
        fs::remove_dir_all(&service.packages.registry).unwrap();
        drop(service);
        let restarted = Service::load(dir.path());
        assert!(restarted.packages.state.lock().unwrap().catalog.is_none());
        let cached = row(&restarted, "bazelisk", PLATFORM);
        assert_eq!(cached["state"], "installed");
        assert_eq!(cached["path"], "/");
        assert_eq!(cached["linuxPaths"], expected);
        let tool = row(&restarted, "xtensa-esp-elf", PLATFORM);
        assert_eq!(tool["linuxPaths"], json!([tool["path"]]));
    }

    #[test]
    fn linux_paths_fall_back_to_the_root_install_destination_without_file_checks() {
        let mut files = management_files();
        edit_recipe(
            &mut files,
            Kind::Toolchain,
            "bazelisk",
            "1.30.0",
            |recipe| {
                recipe["checks"] = json!([
                    {"type": "command", "program": "/bin/tool", "args": ["--version"], "timeoutSeconds": 10}
                ]);
            },
        );
        let (dir, service) = management_service(files);
        let root = row(&service, "bazelisk", PLATFORM);
        assert_eq!(root["linuxPaths"], json!([root["installPath"]]));
        fs::remove_dir_all(&service.packages.registry).unwrap();
        drop(service);
        let restarted = Service::load(dir.path());
        assert_eq!(
            row(&restarted, "bazelisk", PLATFORM)["linuxPaths"],
            json!(["/"])
        );
    }

    fn prefix_migration_files() -> BTreeMap<String, String> {
        let mut files = management_files();
        let metadata_path = "toolchains/bazelisk/toolchain.json";
        let mut metadata: Value = serde_json::from_str(&files[metadata_path]).unwrap();
        metadata["releases"].as_array_mut().unwrap().insert(
            0,
            json!({
                "version": "1.31.0",
                "manifest": "versions/1.31.0.json",
            }),
        );
        metadata["defaultVersion"] = "1.31.0".into();
        files.insert(metadata_path.into(), metadata.to_string());
        files.insert(
            registry::manifest_path(Kind::Toolchain, "bazelisk", "1.31.0"),
            registry::tests::release(
                Kind::Toolchain,
                "bazelisk",
                "1.31.0",
                json!({PLATFORM: registry::tests::recipe(&[], None)}),
            ),
        );
        edit_recipe(
            &mut files,
            Kind::Toolchain,
            "xtensa-esp-elf",
            "14.2.0",
            |r| {
                r["dependencies"] = json!([{"toolchain": "bazelisk", "version": "1.30.0"}]);
            },
        );
        edit_recipe(&mut files, Kind::Sdk, "esp-idf", "6.2.0", |r| {
            r["dependencies"] = json!([{"toolchain": "xtensa-esp-elf", "version": "14.2.0"}]);
        });
        files
    }

    #[test]
    fn prefix_migration_keeps_inactive_root_versions_and_their_dependents_view_only() {
        let (_dir, service) = management_service(prefix_migration_files());
        let state = service.packages.state.lock().unwrap();
        let selected = wanted(state.catalog.as_ref().unwrap());
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].key.2, "1.31.0");
        drop(state);
        assert_eq!(
            row(&service, "xtensa-esp-elf", PLATFORM)["state"],
            "unsupported"
        );
        assert_eq!(row(&service, "esp-idf", PLATFORM)["canReinstall"], false);
        let snapshot = service.snapshot();
        let old = snapshot["packages"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "bazelisk" && p["version"] == "1.30.0")
            .unwrap();
        assert_eq!(old["state"], "unsupported");
        assert_eq!(old["canRemove"], false);
    }

    #[test]
    fn prefix_migration_waits_for_downloaded_replacements_before_repacking() {
        let (_dir, service) = management_service(management_files());
        let catalog =
            registry::parse_snapshot(&"d".repeat(40), 0, &prefix_migration_files()).unwrap();
        let selected = wanted(&catalog);
        let new = &selected[0];
        let recipe = &catalog
            .release(new.key.0, &new.key.1, &new.key.2)
            .unwrap()
            .0
            .platforms[PLATFORM];
        let replacement = planned_package(new, recipe).unwrap();
        let mut state = service.packages.state.lock().unwrap();
        state.catalog = Some(Arc::new(catalog));
        assert!(retained_download_pending(&state));
        state.index.packages.push(replacement);
        assert!(!retained_download_pending(&state));
    }

    #[test]
    fn host_locations_survive_offline_start_and_only_identify_a_matching_runtime() {
        let (dir, service) = management_service(management_files());
        let tool = row(&service, "xtensa-esp-elf", PLATFORM);
        let expected = json!({
            "downloads": [service.root().join("assets").join(registry::hex_digest(b"tool"))],
            "runtime": service.root().join(PACK),
        });
        assert_eq!(tool["hostPaths"], expected);
        assert_eq!(tool["commandPath"], protocol::COMMAND_BIN);
        let info = service.packages.state.lock().unwrap().pack_info.take();
        assert!(row(&service, "xtensa-esp-elf", PLATFORM)["hostPaths"]["runtime"].is_null());
        service.packages.state.lock().unwrap().pack_info = info;
        fs::remove_dir_all(&service.packages.registry).unwrap();
        drop(service);
        let restarted = Service::load(dir.path());
        let cached = row(&restarted, "xtensa-esp-elf", PLATFORM);
        assert_eq!(cached["hostPaths"], expected);
        assert_eq!(cached["commandPath"], protocol::COMMAND_BIN);
    }

    #[test]
    fn root_install_destinations_are_display_only_and_normalized() {
        let mut recipe: Recipe =
            serde_json::from_value(registry::tests::recipe(&[], None)).unwrap();
        recipe.install = vec![Step::Extract {
            source: "tool".into(),
            to: ".".into(),
            strip_components: 0,
            format: "tar.gz".into(),
        }];
        assert_eq!(recipe_install_path(&recipe).as_deref(), Some("/"));
        recipe.install = vec![Step::Extract {
            source: "tool".into(),
            to: "opt/gizmoth/x86_64".into(),
            strip_components: 0,
            format: "tar.gz".into(),
        }];
        assert_eq!(
            recipe_install_path(&recipe).as_deref(),
            Some("/opt/gizmoth/x86_64")
        );
        recipe.install.push(Step::Copy {
            source: "tool".into(),
            to: "usr/bin/tool".into(),
            executable: true,
        });
        assert_eq!(recipe_install_path(&recipe), None);
    }

    // Minimal fixtures preserve the relevant install/check/dependency metadata
    // from the current registry, without requiring the target/ registry checkout.
    fn display_label_recipe(id: &str) -> Value {
        let mut recipe = registry::tests::recipe(&[], None);
        if id == "linux-qemu-user" || id.starts_with("linux-x86-") {
            recipe["guestRoot"] = true.into();
            let foreign = id.starts_with("linux-x86-");
            recipe["install"] = json!([{
                "type": "extract", "source": "tool", "format": "deb", "stripComponents": 0,
                "to": if foreign { "opt/gizmoth/x86_64" } else { "." }
            }]);
            recipe["checks"] = json!([{
                "type": "file",
                "path": match id {
                    "linux-qemu-user" => "usr/bin/qemu-x86_64-static",
                    "linux-x86-runtime" => "opt/gizmoth/x86_64/lib/x86_64-linux-gnu/libc.so.6",
                    "linux-x86-binutils" => "opt/gizmoth/x86_64/usr/bin/x86_64-linux-gnu-as",
                    "linux-x86-binutils-common" => "opt/gizmoth/x86_64/usr/share/doc/binutils-common/copyright",
                    _ => "opt/gizmoth/x86_64/usr/lib/x86_64-linux-gnu/libstdc++.so.6"
                },
                "executable": matches!(id, "linux-qemu-user" | "linux-x86-runtime" | "linux-x86-binutils")
            }]);
        } else if matches!(id, "jieli" | "jieli-postbuild") {
            recipe["dependencies"] = json!([
                {"toolchain": "linux-qemu-user", "version": "1.0.0"},
                {"toolchain": "linux-x86-runtime", "version": "1.0.0"}
            ]);
        } else if id == "arm-gnu" {
            recipe["environment"]["variables"]["GIZOS_BK_TOOLCHAIN_ROOT"] =
                "${prefix}/toolchain".into();
        } else if id == "esp-rom-elfs" {
            recipe["checks"] = json!([
                {"type": "file", "path": "rom/esp32s3_rev0_rom.elf", "executable": false},
                {"type": "file", "path": "rom/esp32p4_rev0_rom.elf", "executable": false}
            ]);
        } else {
            recipe["checks"][0]["path"] = format!("bin/{id}-gcc").into();
        }
        recipe
    }

    fn display_label_cases() -> Vec<(&'static str, Vec<&'static str>)> {
        vec![
            ("python", vec!["arm64"]),
            ("linux-qemu-user", vec!["arm64"]),
            ("linux-x86-runtime", vec!["x86_64"]),
            ("linux-x86-binutils", vec!["x86_64"]),
            ("linux-x86-binutils-common", vec!["x86_64"]),
            ("linux-x86-libstdcxx6", vec!["x86_64"]),
            ("jieli", vec!["x86_64", "jieli"]),
            ("jieli-postbuild", vec!["x86_64", "jieli"]),
            ("xtensa-esp-elf", vec!["arm64", "esp32"]),
            ("riscv32-esp-elf", vec!["arm64", "esp32"]),
            ("esp32ulp-elf", vec!["arm64", "esp32"]),
            ("esp-rom-elfs", vec!["esp32"]),
            ("esp-idf", vec!["arm64", "esp32"]),
            ("jieli-ac707n", vec!["arm64", "jieli"]),
            ("beken-bk7258", vec!["arm64", "beken"]),
            ("arm-gnu", vec!["arm64", "beken"]),
            ("arm-none-eabi", vec!["arm64"]),
            ("riscv32-unknown-elf", vec!["arm64"]),
        ]
    }

    #[test]
    fn display_labels_classify_execution_architecture_and_ecosystems_online_and_cached() {
        for (id, expected) in display_label_cases() {
            let recipe: Recipe = serde_json::from_value(display_label_recipe(id)).unwrap();
            assert_eq!(recipe_labels(id, PLATFORM, &recipe), expected, "{id}");
            let mut cached = package(id, "1.0.0", true);
            cached.guest_root = recipe.is_guest_root();
            cached.variables = recipe.environment.variables.clone();
            replan_cached(&mut cached, &recipe).unwrap();
            let before = serde_json::to_value(&cached).unwrap();
            let identity = hash(&[cached.clone()]);
            assert_eq!(cached_labels(&cached), expected, "cached {id}");
            assert_eq!(hash(&[cached.clone()]), identity);
            assert_eq!(serde_json::to_value(&cached).unwrap(), before);
            assert!(before.get("labels").is_none());
        }
    }

    #[test]
    fn display_labels_use_narrow_metadata_without_target_isa_or_url_guesses() {
        let mut recipe: Recipe =
            serde_json::from_value(display_label_recipe("riscv32-esp-elf")).unwrap();
        assert_eq!(
            recipe_labels("riscv32-esp-elf", "linux-x86_64", &recipe),
            ["x86_64", "esp32"]
        );
        assert_eq!(
            recipe_labels("riscv32-esp-elf", "windows-x86_64", &recipe),
            ["x86_64", "esp32"]
        );
        // A foreign-looking URL and output executable do not change host ISA.
        if let Source::File { url, .. } = &mut recipe.sources[0] {
            *url = "https://example.com/amd64/jieli/esp32.tar.xz".into();
        }
        assert_eq!(recipe_labels("generic", PLATFORM, &recipe), ["arm64"]);
        assert!(recipe_labels("generic", "linux-riscv64", &recipe).is_empty());
        let mut jieli: Recipe = serde_json::from_value(display_label_recipe("jieli")).unwrap();
        jieli
            .dependencies
            .retain(|d| d.toolchain != "linux-x86-runtime");
        assert_eq!(recipe_labels("jieli", PLATFORM, &jieli), ["arm64", "jieli"]);
        let mut arm: Recipe = serde_json::from_value(display_label_recipe("arm-gnu")).unwrap();
        arm.environment.variables.clear();
        assert_eq!(recipe_labels("arm-gnu", PLATFORM, &arm), ["arm64"]);
        // A non-root directory or similar prefix is not a foreign root layer.
        let mut root: Recipe =
            serde_json::from_value(display_label_recipe("linux-x86-binutils-common")).unwrap();
        root.guest_root = false;
        assert_eq!(recipe_labels("generic", PLATFORM, &root), ["arm64"]);
        root.guest_root = true;
        root.checks.clear();
        if let Step::Extract { to, .. } = &mut root.install[0] {
            *to = "opt/gizmoth/x86_64-other".into();
        }
        assert_eq!(recipe_labels("generic", PLATFORM, &root), ["arm64"]);
    }

    #[test]
    fn display_labels_survive_offline_rows_and_removed_tombstones() {
        let mut files = management_files();
        let template: Value =
            serde_json::from_str(&files["toolchains/bazelisk/toolchain.json"]).unwrap();
        let mut registry: Value = serde_json::from_str(&files["registry.json"]).unwrap();
        for (id, _) in display_label_cases() {
            if id == "esp-idf" || id == "xtensa-esp-elf" {
                let (kind, version) = if id == "esp-idf" {
                    (Kind::Sdk, "6.2.0")
                } else {
                    (Kind::Toolchain, "14.2.0")
                };
                edit_recipe(&mut files, kind, id, version, |r| {
                    r["checks"] = display_label_recipe(id)["checks"].clone();
                });
                continue;
            }
            if !registry["toolchains"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == id)
            {
                registry["toolchains"]
                    .as_array_mut()
                    .unwrap()
                    .push(id.into());
            }
            let mut metadata = template.clone();
            metadata["id"] = id.into();
            metadata["name"] = id.into();
            metadata["releases"] = json!([{"version": "1.0.0", "manifest": "versions/1.0.0.json"}]);
            metadata["defaultVersion"] = "1.0.0".into();
            files.insert(
                format!("toolchains/{id}/toolchain.json"),
                metadata.to_string(),
            );
            files.insert(
                registry::manifest_path(Kind::Toolchain, id, "1.0.0"),
                registry::tests::release(
                    Kind::Toolchain,
                    id,
                    "1.0.0",
                    json!({PLATFORM: display_label_recipe(id)}),
                ),
            );
        }
        files.insert("registry.json".into(), registry.to_string());
        let (dir, service) = management_service(files);
        for (id, expected) in display_label_cases() {
            assert_eq!(
                row(&service, id, PLATFORM)["labels"],
                json!(expected),
                "online {id}"
            );
        }
        // Exercise each tombstone's online display and then its cached metadata
        // after the catalog disappears, while keeping other rows installed.
        {
            let mut state = service.packages.state.lock().unwrap();
            let removed = state
                .index
                .packages
                .iter()
                .filter(|p| p.id != "bazelisk")
                .cloned()
                .collect();
            state.index.removed = removed;
            write_atomic(&service.root().join(INDEX), &state.index).unwrap();
        }
        for (id, expected) in display_label_cases() {
            let removed = row(&service, id, PLATFORM);
            assert_eq!(removed["state"], "removed");
            assert_eq!(removed["labels"], json!(expected));
        }
        // First reload only the packed rows, then restore the persisted tombstones.
        let tombstones = service.packages.state.lock().unwrap().index.removed.clone();
        {
            let mut state = service.packages.state.lock().unwrap();
            state.index.removed.clear();
            write_atomic(&service.root().join(INDEX), &state.index).unwrap();
        }
        fs::remove_dir_all(&service.packages.registry).unwrap();
        drop(service);
        let restarted = Service::load(dir.path());
        assert!(restarted.packages.state.lock().unwrap().catalog.is_none());
        for (id, expected) in display_label_cases() {
            let cached = row(&restarted, id, PLATFORM);
            assert_eq!(cached["state"], "installed");
            assert_eq!(cached["labels"], json!(expected), "offline {id}");
        }
        {
            let mut state = restarted.packages.state.lock().unwrap();
            state.index.removed = tombstones;
            write_atomic(&restarted.root().join(INDEX), &state.index).unwrap();
        }
        drop(restarted);
        let restarted = Service::load(dir.path());
        for (id, expected) in display_label_cases() {
            let removed = row(&restarted, id, PLATFORM);
            assert_eq!(removed["state"], "removed");
            assert_eq!(removed["labels"], json!(expected), "removed {id}");
        }
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
