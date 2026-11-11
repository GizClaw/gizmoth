//! Development environments: chips, SDKs and toolchains from the GizClaw/gizmoth-devenv
//! registry, installed into GizMoth-owned directories and removed by local records.
mod extract;
mod install;
mod plan;
mod registry;
mod store;

use plan::Blocked;
use registry::{host_platform, Kind, Snapshot, Status};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use store::{Action, Installation, Origin, Store, Target};
use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;

struct Admission<'a>(&'a AtomicBool);
impl Drop for Admission<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

type R<T> = Result<T, String>;

/// One job at a time; the last job stays visible until another starts.
#[derive(Default)]
pub struct DevEnv {
    snapshot: Mutex<Option<Arc<Snapshot>>>,
    refresh: tokio::sync::Mutex<()>,
    job: install::Slot,
    busy: AtomicBool,
    recovered: AtomicBool,
}

fn store(app: &AppHandle) -> R<Store> {
    let path = app.path();
    Ok(Store {
        data: path
            .app_data_dir()
            .map_err(|e| e.to_string())?
            .join("devenv"),
        cache: path
            .app_cache_dir()
            .map_err(|e| e.to_string())?
            .join("devenv"),
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// RFC 3339 UTC timestamp with milliseconds.
pub(crate) fn timestamp(ms: u64) -> String {
    let days = (ms / 86_400_000) as i64;
    let rest = ms % 86_400_000;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600_000,
        rest / 60_000 % 60,
        rest / 1000 % 60,
        rest % 1000
    )
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedSnapshot {
    commit: String,
    fetched_at: u64,
    files: BTreeMap<String, String>,
}

impl DevEnv {
    fn admit(&self) -> R<Admission<'_>> {
        self.busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| "另一个开发环境任务正在进行")?;
        let admission = Admission(&self.busy);
        if self.job_running() {
            return Err("另一个开发环境任务正在进行".into());
        }
        Ok(admission)
    }

    fn snapshot(&self, store: &Store) -> R<Option<Arc<Snapshot>>> {
        let mut cached = self.snapshot.lock().map_err(|_| "目录状态不可用")?;
        if cached.is_none() {
            let current = store.registry().join("current.json");
            let commit = match std::fs::read_to_string(&current) {
                Ok(content) => serde_json::from_str::<serde_json::Value>(&content)
                    .ok()
                    .and_then(|v| v["commit"].as_str().map(str::to_owned))
                    .filter(|c| registry::valid_commit(c))
                    .ok_or("本地目录缓存无效，请刷新")?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(format!("无法读取本地目录缓存：{e}")),
            };
            let saved: SavedSnapshot = serde_json::from_slice(
                &std::fs::read(store.registry().join(format!("{commit}.json")))
                    .map_err(|e| format!("无法读取本地目录缓存：{e}"))?,
            )
            .map_err(|_| "本地目录缓存无效，请刷新")?;
            if saved.commit != commit {
                return Err("本地目录缓存无效，请刷新".into());
            }
            *cached = Some(Arc::new(registry::parse_snapshot(
                &commit,
                saved.fetched_at,
                &saved.files,
            )?));
        }
        Ok(cached.clone())
    }

    fn publish_snapshot(&self, store: &Store, saved: SavedSnapshot) -> R<()> {
        let snapshot = registry::parse_snapshot(&saved.commit, saved.fetched_at, &saved.files)?;
        // Cold readers and every publication share this lock. Keep the current
        // pointer, retained file and in-memory snapshot on the same commit.
        let mut cached = self.snapshot.lock().map_err(|_| "目录状态不可用")?;
        let filename = format!("{}.json", saved.commit);
        store::write_atomic(&store.registry(), &filename, &saved)?;
        store::write_atomic(
            &store.registry(),
            "current.json",
            &serde_json::json!({ "commit": saved.commit }),
        )?;
        *cached = Some(Arc::new(snapshot));
        if let Ok(entries) = std::fs::read_dir(store.registry()) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name != "current.json" && name != filename && name.ends_with(".json") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        Ok(())
    }

    fn job_running(&self) -> bool {
        self.job
            .lock()
            .map(|job| job.as_ref().is_some_and(|job| job.view.state == "running"))
            .unwrap_or(true)
    }
}

// Views returned to the frontend.

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct InstallationView {
    installation_id: String,
    kind: &'static str,
    id: String,
    name: String,
    version: String,
    category: Option<String>,
    origin: &'static str,
    requested_by_user: bool,
    path: String,
    chips: Vec<String>,
    environments: Vec<String>,
    healthy: bool,
    reinstallable: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageView {
    kind: &'static str,
    id: String,
    name: String,
    version: String,
    installed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChipView {
    id: String,
    name: String,
    vendor: String,
    description: String,
    homepage: Option<String>,
    sdk: String,
    notes: Option<String>,
    status: &'static str,
    blocked: Option<Blocked>,
    reason: Option<String>,
    version: Option<String>,
    packages: Vec<PackageView>,
    installation_id: Option<String>,
    path: Option<String>,
    reinstallable: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentView {
    id: String,
    name: String,
    description: String,
    homepage: Option<String>,
    notes: Option<String>,
    status: &'static str,
    blocked: Option<Blocked>,
    reason: Option<String>,
    packages: Vec<PackageView>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct VersionView {
    version: String,
    status: &'static str,
    blocked: Option<Blocked>,
    reason: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolchainView {
    id: String,
    name: String,
    description: String,
    category: String,
    homepage: String,
    notes: Option<String>,
    status: &'static str,
    blocked: Option<Blocked>,
    reason: Option<String>,
    version: Option<String>,
    versions: Vec<VersionView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryView {
    repository: String,
    commit: String,
    fetched_at: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    host: Option<&'static str>,
    registry: Option<RegistryView>,
    error: Option<String>,
    chips: Vec<ChipView>,
    environments: Vec<EnvironmentView>,
    toolchains: Vec<ToolchainView>,
    installations: Vec<InstallationView>,
    job: Option<install::JobView>,
}

fn item_name(snapshot: Option<&Snapshot>, kind: Kind, id: &str) -> String {
    snapshot
        .and_then(|s| s.item(kind, id))
        .map_or_else(|| id.to_owned(), |item| item.name.clone())
}
fn chip_name(snapshot: Option<&Snapshot>, id: &str) -> String {
    snapshot
        .and_then(|s| s.chip(id))
        .map_or_else(|| id.to_owned(), |chip| chip.name.clone())
}

fn environment_name(snapshot: Option<&Snapshot>, local: &store::State, id: &str) -> String {
    snapshot
        .and_then(|s| s.environment(id))
        .map(|e| e.name.clone())
        .or_else(|| {
            local
                .environments
                .get(id)
                .and_then(|e| e.metadata.as_ref())
                .map(|m| m.name.clone())
        })
        .unwrap_or_else(|| id.to_owned())
}

fn installation_view(
    record: &Installation,
    local: &store::State,
    store: &Store,
    snapshot: Option<&Snapshot>,
) -> InstallationView {
    InstallationView {
        installation_id: record.installation_id.clone(),
        kind: record.kind().as_str(),
        id: record.id.clone(),
        name: item_name(snapshot, record.kind(), &record.id),
        version: record.version.clone(),
        category: snapshot
            .and_then(|s| s.item(record.kind(), &record.id))
            .and_then(|i| i.category.clone()),
        origin: if record.is_local() {
            "local"
        } else {
            "managed"
        },
        requested_by_user: record.requested_by_user,
        path: store.path_of(record).to_string_lossy().into_owned(),
        chips: local
            .chips_using(&record.installation_id)
            .iter()
            .map(|c| chip_name(snapshot, c))
            .collect(),
        environments: local
            .environments_using(&record.installation_id)
            .iter()
            .map(|id| environment_name(snapshot, local, id))
            .collect(),
        healthy: local.healthy(record, store, snapshot),
        reinstallable: reinstallable(record, snapshot),
    }
}

fn reinstallable(record: &Installation, snapshot: Option<&Snapshot>) -> bool {
    snapshot
        .zip(host_platform())
        .is_some_and(|(snapshot, host)| plan::reinstall(snapshot, record, host).is_ok())
}

fn blocked_view(blocked: Blocked) -> (&'static str, Option<String>, Option<Blocked>) {
    let status = if blocked == Blocked::Planned {
        "planned"
    } else {
        "unsupported"
    };
    (status, Some(blocked.message()), Some(blocked))
}

fn installable(
    snapshot: &Snapshot,
    resolution: &plan::Resolution,
    local: &store::State,
    store: &Store,
    host: &str,
) -> Result<Vec<PackageView>, Blocked> {
    resolution
        .packages
        .iter()
        .map(|package| {
            let installed = local
                .find(
                    package.kind,
                    &package.id,
                    &package.version,
                    host,
                    store,
                    Some(snapshot),
                )
                .is_some();
            if !installed {
                plan::supported(snapshot, package, host)?;
            }
            Ok(PackageView {
                kind: package.kind.as_str(),
                id: package.id.clone(),
                name: item_name(Some(snapshot), package.kind, &package.id),
                version: package.version.clone(),
                installed,
            })
        })
        .collect()
}

fn view(app: &AppHandle, env: &DevEnv) -> R<View> {
    let store = store(app)?;
    let host = host_platform();
    let job = env
        .job
        .lock()
        .map_err(|_| "任务状态不可用")?
        .as_ref()
        .map(|job| job.view.clone());
    if !env.recovered.load(Ordering::SeqCst) {
        if let Ok(_admission) = env.admit() {
            if !env.recovered.swap(true, Ordering::SeqCst) {
                let _ = store.recover();
            }
        }
    }
    let (snapshot, mut error) = match env.snapshot(&store) {
        Ok(snapshot) => (snapshot, None),
        Err(e) => (None, Some(e)),
    };
    let snapshot_ref = snapshot.as_deref();
    let local = match store.load() {
        Ok(local) => local,
        Err(e) => {
            error = Some(e);
            store::State::default()
        }
    };
    let installing = job
        .as_ref()
        .filter(|job| job.state == "running")
        .map(|job| (job.kind, job.target.clone()));

    let mut chips = Vec::new();
    let mut seen = BTreeSet::new();
    let catalog: Vec<_> = snapshot_ref
        .map(|s| s.chips.iter().collect())
        .unwrap_or_default();
    for chip in catalog {
        seen.insert(chip.id.clone());
        let sdk = item_name(snapshot_ref, Kind::Sdk, &chip.sdk);
        let mut view = ChipView {
            id: chip.id.clone(),
            name: chip.name.clone(),
            vendor: chip.vendor.clone(),
            description: chip.description.clone(),
            homepage: Some(chip.homepage.clone()),
            sdk,
            notes: chip.notes.clone(),
            status: "available",
            blocked: None,
            reason: None,
            version: None,
            packages: Vec::new(),
            installation_id: None,
            path: None,
            reinstallable: false,
        };
        if let Some(installed) = local.chips.get(&chip.id) {
            fill_installed(&mut view, installed, &local, &store, snapshot_ref);
        } else if installing
            .as_ref()
            .is_some_and(|(kind, id)| *kind == "chip" && *id == chip.id)
        {
            view.status = "installing";
        } else if let Some(host) = host {
            let snapshot = snapshot_ref.expect("catalog chips come from a snapshot");
            match plan::chip(snapshot, &chip.id, host)
                .and_then(|resolution| installable(snapshot, &resolution, &local, &store, host))
            {
                Ok(packages) => {
                    view.version = packages.last().map(|p| p.version.clone());
                    view.packages = packages;
                }
                Err(blocked) => (view.status, view.reason, view.blocked) = blocked_view(blocked),
            }
            if chip.status == Status::Planned {
                (view.status, view.reason, view.blocked) = blocked_view(Blocked::Planned);
            }
        } else {
            (view.status, view.reason, view.blocked) =
                blocked_view(Blocked::UnsupportedHost(Vec::new()));
        }
        chips.push(view);
    }
    // Installed chips that left the registry remain visible and removable.
    for installed in local.chips.values().filter(|c| !seen.contains(&c.chip)) {
        let mut view = ChipView {
            id: installed.chip.clone(),
            name: installed.chip.clone(),
            vendor: String::new(),
            description: String::new(),
            homepage: None,
            notes: None,
            sdk: local
                .installations
                .get(&installed.sdk)
                .map_or_else(String::new, |r| item_name(snapshot_ref, Kind::Sdk, &r.id)),
            status: "installed",
            blocked: None,
            reason: None,
            version: None,
            packages: Vec::new(),
            installation_id: None,
            path: None,
            reinstallable: false,
        };
        fill_installed(&mut view, installed, &local, &store, snapshot_ref);
        chips.push(view);
    }

    let mut environments = Vec::new();
    let mut seen = BTreeSet::new();
    for entry in snapshot_ref
        .map(|s| s.environments.as_slice())
        .unwrap_or_default()
    {
        seen.insert(entry.id.clone());
        let mut view = EnvironmentView {
            id: entry.id.clone(),
            name: entry.name.clone(),
            description: entry.description.clone(),
            homepage: Some(entry.homepage.clone()),
            notes: entry.notes.clone(),
            status: "available",
            blocked: None,
            reason: None,
            packages: Vec::new(),
        };
        if let Some(installed) = local.environments.get(&entry.id) {
            fill_environment(&mut view, installed, &local, snapshot_ref);
        } else if installing
            .as_ref()
            .is_some_and(|(kind, id)| *kind == "environment" && *id == entry.id)
        {
            view.status = "installing";
        } else if let Some(host) = host {
            let snapshot = snapshot_ref.expect("catalog environments come from a snapshot");
            match plan::environment(snapshot, &entry.id, host)
                .and_then(|r| installable(snapshot, &r, &local, &store, host))
            {
                Ok(packages) => view.packages = packages,
                Err(blocked) => (view.status, view.reason, view.blocked) = blocked_view(blocked),
            }
        } else {
            (view.status, view.reason, view.blocked) =
                blocked_view(Blocked::UnsupportedHost(Vec::new()));
        }
        environments.push(view);
    }
    for installed in local
        .environments
        .values()
        .filter(|e| !seen.contains(&e.environment))
    {
        let metadata = installed.metadata.as_ref();
        let mut view = EnvironmentView {
            id: installed.environment.clone(),
            name: environment_name(snapshot_ref, &local, &installed.environment),
            description: metadata.map_or_else(String::new, |m| m.description.clone()),
            homepage: metadata.map(|m| m.homepage.clone()),
            notes: metadata.and_then(|m| m.notes.clone()),
            status: "installed",
            blocked: None,
            reason: None,
            packages: Vec::new(),
        };
        fill_environment(&mut view, installed, &local, snapshot_ref);
        environments.push(view);
    }

    let mut toolchains = Vec::new();
    for item in snapshot_ref
        .map(|s| s.toolchains.as_slice())
        .unwrap_or_default()
    {
        let mut view = ToolchainView {
            id: item.id.clone(),
            name: item.name.clone(),
            description: item.description.clone(),
            category: item.category.clone().unwrap_or_default(),
            homepage: item.homepage.clone(),
            notes: item.notes.clone(),
            status: "available",
            blocked: None,
            reason: None,
            version: item.default_version.clone(),
            versions: Vec::new(),
        };
        let snapshot = snapshot_ref.expect("catalog toolchains come from a snapshot");
        view.versions = item
            .releases
            .iter()
            .map(|release| {
                let mut version = VersionView {
                    version: release.version.clone(),
                    status: "available",
                    blocked: None,
                    reason: None,
                };
                if job.as_ref().is_some_and(|j| {
                    j.state == "running"
                        && j.kind == "toolchain"
                        && j.target == item.id
                        && j.version.as_deref() == Some(release.version.as_str())
                }) {
                    version.status = "installing";
                } else if let Some(host) = host {
                    if local
                        .find(
                            Kind::Toolchain,
                            &item.id,
                            &release.version,
                            host,
                            &store,
                            Some(snapshot),
                        )
                        .is_some()
                    {
                        version.status = "installed";
                    } else if let Err(blocked) = plan::package(
                        snapshot,
                        Kind::Toolchain,
                        &item.id,
                        Some(&release.version),
                        host,
                    )
                    .and_then(|r| installable(snapshot, &r, &local, &store, host))
                    {
                        (version.status, version.reason, version.blocked) = blocked_view(blocked);
                    }
                } else {
                    (version.status, version.reason, version.blocked) =
                        blocked_view(Blocked::UnsupportedHost(Vec::new()));
                }
                version
            })
            .collect();
        if let Some(default) = view
            .versions
            .iter()
            .find(|v| Some(&v.version) == item.default_version.as_ref())
        {
            view.status = default.status;
            view.blocked = default.blocked.clone();
            view.reason = default.reason.clone();
        } else {
            let blocked = if host.is_none() {
                Blocked::UnsupportedHost(Vec::new())
            } else {
                Blocked::Planned
            };
            (view.status, view.reason, view.blocked) = blocked_view(blocked);
        }
        toolchains.push(view);
    }

    let installations = local
        .installations
        .values()
        .filter(|r| r.kind() == Kind::Toolchain)
        .map(|r| installation_view(r, &local, &store, snapshot_ref))
        .collect();
    Ok(View {
        host,
        registry: snapshot.as_ref().map(|s| RegistryView {
            repository: s.registry.repository.clone(),
            commit: s.commit.clone(),
            fetched_at: s.fetched_at,
        }),
        error,
        chips,
        environments,
        toolchains,
        installations,
        job,
    })
}

fn fill_installed(
    view: &mut ChipView,
    installed: &store::InstalledChip,
    local: &store::State,
    store: &Store,
    snapshot: Option<&Snapshot>,
) {
    view.status = "installed";
    if let Some(sdk) = local.installations.get(&installed.sdk) {
        view.version = Some(sdk.version.clone());
        view.installation_id = Some(sdk.installation_id.clone());
        view.path = Some(store.path_of(sdk).to_string_lossy().into_owned());
        view.reinstallable = reinstallable(sdk, snapshot);
    }
    let mut ids: Vec<&String> = installed.toolchains.iter().collect();
    ids.push(&installed.sdk);
    view.packages = ids
        .into_iter()
        .filter_map(|id| local.installations.get(id))
        .map(|r| PackageView {
            kind: r.kind().as_str(),
            id: r.id.clone(),
            name: item_name(snapshot, r.kind(), &r.id),
            version: r.version.clone(),
            installed: true,
        })
        .collect();
}

fn fill_environment(
    view: &mut EnvironmentView,
    installed: &store::InstalledEnvironment,
    local: &store::State,
    snapshot: Option<&Snapshot>,
) {
    view.status = "installed";
    view.packages = installed
        .toolchains
        .iter()
        .filter_map(|id| local.installations.get(id))
        .map(|r| PackageView {
            kind: r.kind().as_str(),
            id: r.id.clone(),
            name: item_name(snapshot, r.kind(), &r.id),
            version: r.version.clone(),
            installed: true,
        })
        .collect();
}

#[tauri::command]
pub async fn devenv_state(app: AppHandle) -> R<View> {
    let env = app.state::<DevEnv>();
    view(&app, &env)
}

#[tauri::command]
pub async fn devenv_refresh(app: AppHandle) -> R<View> {
    let env = app.state::<DevEnv>();
    let _refresh = env.refresh.lock().await;
    let store = store(&app)?;
    let (commit, files) = registry::fetch().await?;
    env.publish_snapshot(
        &store,
        SavedSnapshot {
            commit,
            fetched_at: now_ms(),
            files,
        },
    )?;
    view(&app, &env)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallTarget {
    kind: String,
    id: String,
    #[serde(default)]
    version: Option<String>,
}

#[tauri::command]
pub async fn devenv_install(app: AppHandle, target: InstallTarget) -> R<View> {
    let env = app.state::<DevEnv>();
    let store = store(&app)?;
    let host = host_platform().ok_or("不支持此电脑")?;
    let snapshot = env.snapshot(&store)?.ok_or("请先刷新开发环境目录")?;
    let admission = env.admit()?;
    let local = store.load()?;
    let (kind, name, resolution) = match target.kind.as_str() {
        "chip" => {
            if local.chips.contains_key(&target.id) {
                return Err("芯片已安装".into());
            }
            let chip = snapshot.chip(&target.id).ok_or("目录中没有这个芯片")?;
            (
                "chip",
                chip.name.clone(),
                plan::chip_version(&snapshot, &target.id, target.version.as_deref(), host)
                    .map_err(|b| b.message())?,
            )
        }
        "environment" => {
            if target.version.is_some() {
                return Err("通用环境没有独立版本".into());
            }
            if local.environments.contains_key(&target.id) {
                return Err("环境已安装".into());
            }
            let entry = snapshot
                .environment(&target.id)
                .ok_or("目录中没有这个环境")?;
            (
                "environment",
                entry.name.clone(),
                plan::environment(&snapshot, &target.id, host).map_err(|b| b.message())?,
            )
        }
        "toolchain" | "sdk" => {
            let kind = Kind::parse(&target.kind)?;
            let item = snapshot
                .item(kind, &target.id)
                .ok_or("目录中没有这个工具或 SDK")?;
            let resolution = if kind == Kind::Toolchain && target.version.is_none() {
                plan::toolchain(&snapshot, &target.id, host)
            } else {
                plan::package(&snapshot, kind, &target.id, target.version.as_deref(), host)
            }
            .map_err(|b| b.message())?;
            let root = resolution.packages.last().ok_or("缺少安装目标")?;
            if local
                .find(kind, &root.id, &root.version, host, &store, Some(&snapshot))
                .is_some()
            {
                return Err("此版本已安装".into());
            }
            (kind.as_str(), item.name.clone(), resolution)
        }
        other => return Err(format!("未知的安装目标：{other}")),
    };
    let mut reuse = Vec::new();
    for package in &resolution.packages {
        let existing = local
            .find(
                package.kind,
                &package.id,
                &package.version,
                host,
                &store,
                Some(&snapshot),
            )
            .cloned();
        if existing.is_none() {
            plan::supported(&snapshot, package, host).map_err(|b| b.message())?;
        }
        reuse.push(existing);
    }
    let work = install::Work {
        store,
        snapshot,
        host,
        kind,
        target: target.id,
        resolution,
        reuse,
        replacement: None,
    };
    install::start(&env.job, name, work)?;
    drop(admission);
    view(&app, &env)
}

#[tauri::command]
pub async fn devenv_open_folder(app: AppHandle, installation_id: String) -> R<()> {
    let path = store(&app)?.open_folder_path(&installation_id)?;
    app.opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(|e| format!("无法打开安装目录：{e}"))
}

#[tauri::command]
pub async fn devenv_reinstall(app: AppHandle, target: Target) -> R<View> {
    let env = app.state::<DevEnv>();
    let admission = env.admit()?;
    let store = store(&app)?;
    let host = host_platform().ok_or("不支持此电脑")?;
    let snapshot = env.snapshot(&store)?.ok_or("请先刷新开发环境目录")?;
    let local = store.load()?;
    let (kind, id, name, previous) = match target {
        Target::Chip(id) => {
            let chip = local.chips.get(&id).ok_or("芯片未安装")?;
            let previous = local
                .installations
                .get(&chip.sdk)
                .ok_or("SDK 安装记录缺失")?;
            (
                "chip",
                id.clone(),
                chip_name(Some(&snapshot), &id),
                previous.clone(),
            )
        }
        Target::Environment(_) => return Err("请从工具链列表重装环境使用的具体版本".into()),
        Target::Installation(id) => {
            let previous = local.installations.get(&id).ok_or("未知安装")?;
            (
                previous.kind().as_str(),
                previous.id.clone(),
                item_name(Some(&snapshot), previous.kind(), &previous.id),
                previous.clone(),
            )
        }
    };
    let package = plan::reinstall(&snapshot, &previous, host).map_err(|b| b.message())?;
    let work = install::Work {
        store,
        snapshot,
        host,
        kind,
        target: id,
        resolution: plan::Resolution {
            packages: vec![package],
            chip_toolchains: Vec::new(),
            dependencies: vec![Vec::new()],
        },
        reuse: vec![None],
        replacement: Some(previous),
    };
    install::start(&env.job, name, work)?;
    drop(admission);
    view(&app, &env)
}

#[tauri::command]
pub async fn devenv_cancel(app: AppHandle, job_id: String) -> R<View> {
    let env = app.state::<DevEnv>();
    if let Some(job) = env.job.lock().map_err(|_| "任务状态不可用")?.as_ref() {
        if job.view.id == job_id && job.view.state == "running" {
            job.cancel.store(true, Ordering::SeqCst);
        }
    }
    view(&app, &env)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovalView {
    action: Action,
    delete: Vec<InstallationView>,
    keep: Vec<InstallationView>,
    chips: Vec<String>,
    environments: Vec<String>,
    dependants: Vec<InstallationView>,
    confirm: Vec<String>,
}

#[tauri::command]
pub async fn devenv_remove_preview(app: AppHandle, target: Target) -> R<RemovalView> {
    let env = app.state::<DevEnv>();
    let store = store(&app)?;
    let snapshot = env.snapshot(&store).ok().flatten();
    let local = store.load()?;
    let plan = local.plan(&target)?;
    let describe = |ids: &[String]| -> Vec<InstallationView> {
        ids.iter()
            .filter_map(|id| local.installations.get(id))
            .map(|r| installation_view(r, &local, &store, snapshot.as_deref()))
            .collect()
    };
    Ok(RemovalView {
        action: plan.action,
        delete: describe(&plan.delete),
        keep: describe(&plan.keep),
        chips: plan
            .chips
            .iter()
            .map(|c| chip_name(snapshot.as_deref(), c))
            .collect(),
        environments: plan
            .environments
            .iter()
            .map(|id| environment_name(snapshot.as_deref(), &local, id))
            .collect(),
        dependants: describe(&plan.dependants),
        confirm: plan.delete,
    })
}

#[tauri::command]
pub async fn devenv_remove(app: AppHandle, target: Target, confirm: Vec<String>) -> R<View> {
    let env = app.state::<DevEnv>();
    let store = store(&app)?;
    let admission = env.admit()?;
    let result = tauri::async_runtime::spawn_blocking(move || store.remove(&target, &confirm))
        .await
        .map_err(|_| "后台任务失败".to_owned());
    drop(admission);
    result??;
    view(&app, &env)
}

fn valid_local_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value.starts_with(|c: char| c.is_ascii_alphanumeric())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '_' | '-'))
}

/// Register a toolchain the user downloaded. Its files are never written or deleted.
#[tauri::command]
pub async fn devenv_add_local(
    app: AppHandle,
    toolchain: String,
    version: String,
) -> R<Option<View>> {
    let env = app.state::<DevEnv>();
    let store = store(&app)?;
    let host = host_platform().ok_or("不支持此电脑")?;
    let snapshot = env.snapshot(&store)?.ok_or("请先刷新开发环境目录")?;
    snapshot
        .item(Kind::Toolchain, &toolchain)
        .ok_or("目录中没有这个工具链")?;
    let version = version.trim().to_owned();
    if !valid_local_version(&version) {
        return Err("版本只能包含字母、数字和 . + _ -".into());
    }
    let Some(path) = tauri::async_runtime::spawn_blocking(|| rfd::FileDialog::new().pick_folder())
        .await
        .map_err(|_| "无法打开文件夹选择器")?
    else {
        return Ok(None);
    };
    let admission = env.admit()?;
    let real = std::fs::canonicalize(&path).map_err(|e| format!("无法读取所选文件夹：{e}"))?;
    if !real.is_dir() {
        return Err("请选择一个文件夹".into());
    }
    if std::fs::canonicalize(&store.data).is_ok_and(|data| real.starts_with(data)) {
        return Err("不能添加 GizMoth 自己管理的安装目录".into());
    }
    if let Some((release, _)) = snapshot.release(Kind::Toolchain, &toolchain, &version) {
        if let Some(recipe) = release.platforms.get(host) {
            install::run_file_checks(&real, &recipe.checks)?;
        }
    }
    let local = store.load()?;
    if local.installations.values().any(|r| {
        r.kind() == Kind::Toolchain
            && r.id == toolchain
            && r.version == version
            && r.platform == host
    }) {
        return Err(format!("{toolchain} {version} 已在本机登记"));
    }
    let record = Installation {
        api_version: store::API_VERSION.into(),
        kind: "Installation".into(),
        installation_id: uuid::Uuid::new_v4().to_string(),
        kind_of: Kind::Toolchain.as_str().into(),
        id: toolchain,
        version,
        platform: host.into(),
        origin: Origin::Local {
            path: real.to_string_lossy().into_owned(),
        },
        requested_by_user: true,
        dependencies: Vec::new(),
        installed_at: timestamp(now_ms()),
    };
    store.write_installation(&record)?;
    drop(admission);
    view(&app, &env).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_publications_keep_disk_memory_and_cold_reload_on_one_commit() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(Store {
            data: root.path().join("data"),
            cache: root.path().join("cache"),
        });
        let env = Arc::new(DevEnv::default());
        for round in 0..8 {
            let start = Arc::new(std::sync::Barrier::new(8));
            let writers: Vec<_> = (0..8)
                .map(|index| {
                    let store = store.clone();
                    let env = env.clone();
                    let start = start.clone();
                    std::thread::spawn(move || {
                        start.wait();
                        env.publish_snapshot(
                            &store,
                            SavedSnapshot {
                                commit: format!("{:040x}", round * 8 + index + 1),
                                fetched_at: index,
                                files: registry::tests::files(),
                            },
                        )
                        .unwrap();
                    })
                })
                .collect();
            for writer in writers {
                writer.join().unwrap();
            }
            let memory = env.snapshot(&store).unwrap().unwrap();
            let current: serde_json::Value = serde_json::from_slice(
                &std::fs::read(store.registry().join("current.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(current["commit"].as_str(), Some(memory.commit.as_str()));
            let cold = DevEnv::default().snapshot(&store).unwrap().unwrap();
            assert_eq!(cold.commit, memory.commit);
            assert_eq!(cold.fetched_at, memory.fetched_at);
            assert_eq!(std::fs::read_dir(store.registry()).unwrap().count(), 2);
        }
    }

    #[test]
    fn invalid_publication_preserves_the_previous_snapshot_after_restart() {
        let root = tempfile::tempdir().unwrap();
        let store = Store {
            data: root.path().join("data"),
            cache: root.path().join("cache"),
        };
        let env = DevEnv::default();
        env.publish_snapshot(
            &store,
            SavedSnapshot {
                commit: registry::tests::COMMIT.into(),
                fetched_at: 1,
                files: registry::tests::files(),
            },
        )
        .unwrap();
        assert!(env
            .publish_snapshot(
                &store,
                SavedSnapshot {
                    commit: "b".repeat(40),
                    fetched_at: 2,
                    files: BTreeMap::new(),
                }
            )
            .is_err());
        assert_eq!(
            env.snapshot(&store).unwrap().unwrap().commit,
            registry::tests::COMMIT
        );
        assert_eq!(
            DevEnv::default().snapshot(&store).unwrap().unwrap().commit,
            registry::tests::COMMIT
        );
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        assert_eq!(super::timestamp(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            super::timestamp(1_794_355_200_123),
            "2026-11-11T00:00:00.123Z"
        );
        assert_eq!(
            super::timestamp(951_782_400_000),
            "2000-02-29T00:00:00.000Z"
        );
    }
}
