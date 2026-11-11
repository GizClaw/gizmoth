//! Background installation jobs: verified downloads into a content-addressed cache,
//! then a new GizMoth-owned directory per package, recorded only after its checks pass.
use super::plan::{Package, Resolution};
use super::registry::{self, Check, Kind, Snapshot, Source, Step};
use super::store::{self, Installation, InstalledChip, Origin, Store};
use super::{now_ms, timestamp};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

type R<T> = Result<T, String>;
const MAX_LOG: usize = 200;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    pub id: String,
    pub kind: &'static str,
    pub target: String,
    pub name: String,
    pub phase: &'static str,
    pub state: &'static str,
    pub received: u64,
    pub total: u64,
    pub log: Vec<String>,
    pub error: Option<String>,
    pub started_at: u64,
    pub finished_at: Option<u64>,
}

pub struct Job {
    pub view: JobView,
    pub cancel: Arc<AtomicBool>,
}

pub struct Work {
    pub store: Store,
    pub snapshot: Arc<Snapshot>,
    pub host: &'static str,
    pub kind: &'static str,
    pub target: String,
    pub resolution: Resolution,
    pub reuse: Vec<Option<Installation>>,
    /// A repair replaces only this exact root and retains its existing dependencies.
    pub replacement: Option<Installation>,
}

pub type Slot = Arc<Mutex<Option<Job>>>;

struct Context {
    slot: Slot,
    cancel: Arc<AtomicBool>,
    http_timeout: std::time::Duration,
    #[cfg(test)]
    phase_hook: Option<Box<dyn Fn(&'static str) + Send + Sync>>,
}

impl Context {
    fn update(&self, change: impl FnOnce(&mut JobView)) {
        if let Ok(mut job) = self.slot.lock() {
            if let Some(job) = job.as_mut() {
                change(&mut job.view);
            }
        }
    }
    fn log(&self, line: String) {
        self.update(|job| {
            job.log.push(line);
            let excess = job.log.len().saturating_sub(MAX_LOG);
            job.log.drain(..excess);
        });
    }
    fn phase(&self, phase: &'static str) {
        self.update(|job| job.phase = phase);
        #[cfg(test)]
        if let Some(hook) = &self.phase_hook {
            hook(phase);
        }
    }
    fn check_cancel(&self) -> R<()> {
        if self.cancel.load(Ordering::SeqCst) {
            Err("已取消".into())
        } else {
            Ok(())
        }
    }
}

pub fn start(job: &Slot, name: String, work: Work) -> R<()> {
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut slot = job.lock().map_err(|_| "任务状态不可用")?;
        if slot.as_ref().is_some_and(|job| job.view.state == "running") {
            return Err("另一个开发环境任务正在进行".into());
        }
        *slot = Some(Job {
            view: JobView {
                id: uuid::Uuid::new_v4().to_string(),
                kind: work.kind,
                target: work.target.clone(),
                name,
                phase: "resolving",
                state: "running",
                received: 0,
                total: 0,
                log: Vec::new(),
                error: None,
                started_at: now_ms(),
                finished_at: None,
            },
            cancel: cancel.clone(),
        });
    }
    let context = Context {
        slot: job.clone(),
        cancel,
        http_timeout: std::time::Duration::from_secs(60),
        #[cfg(test)]
        phase_hook: None,
    };
    tauri::async_runtime::spawn(async move {
        let result = run(&context, work).await;
        let cancelled = context.cancel.load(Ordering::SeqCst);
        context.update(|job| {
            job.finished_at = Some(now_ms());
            match &result {
                Ok(()) => {
                    job.state = "succeeded";
                    job.phase = "done";
                }
                Err(_) if cancelled => job.state = "cancelled",
                Err(error) => {
                    job.state = "failed";
                    job.error = Some(error.clone());
                }
            }
        });
        if let Err(error) = &result {
            context.log(if cancelled {
                "已取消。已完成的安装会保留并在下次复用。".into()
            } else {
                format!("失败：{error}")
            });
        }
    });
    Ok(())
}

async fn run(context: &Context, work: Work) -> R<()> {
    let Work {
        store,
        snapshot,
        host,
        kind,
        target,
        resolution,
        reuse,
        replacement,
    } = work;
    // Admission serializes mutations, but work may have been prepared earlier.
    // Never publish a dependency ID whose record or payload disappeared.
    let local = store.load()?;
    for existing in reuse.iter().flatten().chain(replacement.iter()) {
        if local.installations.get(&existing.installation_id) != Some(existing) {
            return Err("安装记录已变化，请重试".into());
        }
    }
    for existing in reuse.iter().flatten() {
        store.open_folder_path(&existing.installation_id)?;
    }
    if let Some(previous) = &replacement {
        let package = super::plan::reinstall(&snapshot, previous, host).map_err(|b| b.message())?;
        if resolution.packages != [package] || reuse.len() != 1 || reuse[0].is_some() {
            return Err("重装只能替换原安装的一个新目录".into());
        }
    }
    let total: u64 = resolution
        .packages
        .iter()
        .zip(&reuse)
        .filter(|(_, existing)| existing.is_none())
        .filter_map(|(package, _)| snapshot.release(package.kind, &package.id, &package.version))
        .filter_map(|(release, _)| release.platforms.get(host))
        .flat_map(|recipe| &recipe.sources)
        .map(|source| match source {
            Source::File { size, .. } => *size,
            Source::Git { .. } => 0,
        })
        .sum();
    context.update(|job| job.total = total);
    let client = registry::client()?;
    let mut installed: BTreeMap<(Kind, String), String> = BTreeMap::new();
    let last = resolution.packages.len().saturating_sub(1);
    for (index, (package, existing)) in resolution.packages.iter().zip(&reuse).enumerate() {
        context.check_cancel()?;
        let name = snapshot
            .item(package.kind, &package.id)
            .map_or(package.id.as_str(), |i| i.name.as_str());
        if let Some(existing) = existing {
            context.log(format!("复用 {name} {}", package.version));
            installed.insert(
                (package.kind, package.id.clone()),
                existing.installation_id.clone(),
            );
            continue;
        }
        let dependencies = if let Some(previous) = &replacement {
            previous.dependencies.clone()
        } else {
            resolution.dependencies[index]
                .iter()
                .map(|id| {
                    installed
                        .get(&(Kind::Toolchain, id.clone()))
                        .cloned()
                        .ok_or_else(|| format!("依赖 {id} 尚未安装"))
                })
                .collect::<R<Vec<_>>>()?
        };
        let requested = replacement
            .as_ref()
            .map_or(kind == "toolchain" && index == last, |r| {
                r.requested_by_user
            });
        context.log(format!(
            "{} {name} {}",
            if replacement.is_some() {
                "重装"
            } else {
                "安装"
            },
            package.version
        ));
        let record = install_package(
            context,
            &client,
            &store,
            &snapshot,
            host,
            package,
            dependencies,
            requested,
        )
        .await?;
        if let Some(previous) = &replacement {
            // This is the last cancellation point. Once the journal is published,
            // finish/replay its reference update rather than tearing it in half.
            if let Err(error) = context.check_cancel() {
                let _ = std::fs::remove_dir_all(store.path_of(&record));
                return Err(error);
            }
            context.log("新安装已校验，正在替换引用；保留共享依赖。".into());
            store.replace(previous, &record)?;
        } else {
            store.write_installation(&record)?;
        }
        installed.insert((package.kind, package.id.clone()), record.installation_id);
    }
    if kind == "chip" && replacement.is_none() {
        context.check_cancel()?;
        context.phase("committing");
        let sdk = resolution.packages.last().ok_or("缺少 SDK")?;
        let record = InstalledChip {
            api_version: store::API_VERSION.into(),
            kind: "InstalledChip".into(),
            chip: target,
            sdk: installed[&(Kind::Sdk, sdk.id.clone())].clone(),
            toolchains: resolution
                .chip_toolchains
                .iter()
                .map(|id| installed[&(Kind::Toolchain, id.clone())].clone())
                .collect(),
            installed_at: timestamp(now_ms()),
        };
        store.write_chip(&record)?;
    }
    context.log("完成。".into());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn install_package(
    context: &Context,
    client: &reqwest::Client,
    store: &Store,
    snapshot: &Snapshot,
    host: &str,
    package: &Package,
    dependencies: Vec<String>,
    requested: bool,
) -> R<Installation> {
    let (release, manifest_digest) = snapshot
        .release(package.kind, &package.id, &package.version)
        .ok_or("目录中没有这个版本")?;
    let recipe = release.platforms.get(host).ok_or("不支持此电脑")?;
    let installation_id = uuid::Uuid::new_v4().to_string();
    let prefix = store::managed_prefix(
        package.kind,
        &package.id,
        &package.version,
        host,
        &installation_id,
    );
    let directory = store.data.join(&prefix);
    std::fs::create_dir_all(directory.parent().ok_or("安装路径无效")?)
        .map_err(|e| format!("无法创建安装目录：{e}"))?;
    std::fs::create_dir(&directory).map_err(|e| format!("无法创建安装目录：{e}"))?;

    let result = async {
        context.phase("downloading");
        let mut files = BTreeMap::new();
        let mut digests = Vec::new();
        for source in &recipe.sources {
            context.check_cancel()?;
            let Source::File {
                id,
                url,
                sha256,
                size,
            } = source
            else {
                return Err("需要更新 GizMoth 才能使用 Git 来源".to_owned());
            };
            files.insert(
                id.as_str(),
                download(context, client, store, url, sha256, *size).await?,
            );
            digests.push(sha256.clone());
        }
        context.phase("installing");
        for step in &recipe.install {
            context.check_cancel()?;
            let Step::Copy {
                source,
                to,
                executable,
            } = step
            else {
                return Err(format!(
                    "需要更新 GizMoth 才能执行安装步骤：{}",
                    step.name()
                ));
            };
            let from = files.get(source.as_str()).ok_or("未声明的来源")?;
            let destination = directory.join(to);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("无法创建安装目录：{e}"))?;
            }
            if std::fs::symlink_metadata(&destination).is_ok() {
                return Err(format!("安装路径已存在：{to}"));
            }
            std::fs::copy(from, &destination).map_err(|e| format!("无法复制 {to}：{e}"))?;
            set_executable(&destination, *executable)?;
            context.log(format!("已写入 {to}"));
        }
        context.phase("checking");
        run_file_checks(&directory, &recipe.checks)?;
        context.check_cancel()?;
        context.phase("committing");
        digests.sort();
        digests.dedup();
        let record = Installation {
            api_version: store::API_VERSION.into(),
            kind: "Installation".into(),
            installation_id: installation_id.clone(),
            kind_of: package.kind.as_str().into(),
            id: package.id.clone(),
            version: package.version.clone(),
            platform: host.into(),
            origin: Origin::Managed {
                registry: store::RegistryOrigin {
                    repository: snapshot.registry.repository.clone(),
                    commit: snapshot.commit.clone(),
                },
                manifest: store::ManifestOrigin {
                    path: registry::manifest_path(package.kind, &package.id, &package.version),
                    sha256: manifest_digest.to_owned(),
                },
                prefix,
                source_digests: digests,
            },
            requested_by_user: requested,
            dependencies,
            installed_at: timestamp(now_ms()),
        };
        Ok(record)
    }
    .await;
    if result.is_err() {
        // Unrecorded, so this directory is ours alone; remove it before releasing the job.
        context.phase("cleaning");
        let _ = std::fs::remove_dir_all(&directory);
    }
    result
}

/// Return a verified cached copy of a file source, downloading it if needed.
async fn download(
    context: &Context,
    client: &reqwest::Client,
    store: &Store,
    url: &str,
    sha256: &str,
    size: u64,
) -> R<PathBuf> {
    let cache = store.downloads().join("sha256");
    let cached = cache.join(sha256);
    if cached.is_file() {
        if verify_file(&cached, sha256, size).unwrap_or(false) {
            context.update(|job| job.received += size);
            context.log(format!("使用已缓存的下载 {}", &sha256[..12]));
            return Ok(cached);
        }
        let _ = std::fs::remove_file(&cached);
    }
    let temporary_dir = store.downloads().join("tmp");
    std::fs::create_dir_all(&temporary_dir).map_err(|e| format!("无法创建下载缓存：{e}"))?;
    std::fs::create_dir_all(&cache).map_err(|e| format!("无法创建下载缓存：{e}"))?;
    let temporary = temporary_dir.join(format!("{}.part", uuid::Uuid::new_v4()));
    context.log(format!("下载 {url}"));
    let result = async {
        let mut response = http_wait(context, client.get(url).send())
            .await?
            .error_for_status()
            .map_err(|e| format!("下载失败：{e}"))?;
        let mut file =
            std::fs::File::create(&temporary).map_err(|e| format!("无法写入下载：{e}"))?;
        let mut hasher = Sha256::new();
        let mut received = 0u64;
        loop {
            context.check_cancel()?;
            let chunk = http_wait(context, response.chunk()).await?;
            let Some(chunk) = chunk else { break };
            received += chunk.len() as u64;
            if received > size {
                return Err("下载的文件比目录声明的更大".to_owned());
            }
            hasher.update(&chunk);
            file.write_all(&chunk)
                .map_err(|e| format!("无法写入下载：{e}"))?;
            let length = chunk.len() as u64;
            context.update(|job| job.received += length);
        }
        file.sync_all().map_err(|e| format!("无法写入下载：{e}"))?;
        drop(file);
        let digest: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if received != size || digest != sha256 {
            return Err("下载的文件与目录的 SHA-256 不一致，已丢弃".to_owned());
        }
        std::fs::rename(&temporary, &cached).map_err(|e| format!("无法保存下载：{e}"))?;
        context.log(format!("已校验 SHA-256 {}", &sha256[..12]));
        Ok(cached.clone())
    }
    .await;
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Both headers and body chunks have a deadline and respond to cancellation
/// even when the server never sends another byte.
async fn http_wait<T>(
    context: &Context,
    request: impl std::future::Future<Output = Result<T, reqwest::Error>>,
) -> R<T> {
    tokio::select! {
        result = tokio::time::timeout(context.http_timeout, request) => {
            result.map_err(|_| "下载超时".to_owned())?
                .map_err(|e| format!("下载失败：{e}"))
        }
        _ = async {
            while !context.cancel.load(Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        } => Err("已取消".into()),
    }
}

fn verify_file(path: &Path, sha256: &str, size: u64) -> R<bool> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() != size {
        return Ok(false);
    }
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| e.to_string())?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
        == sha256)
}

fn set_executable(path: &Path, executable: bool) -> R<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|e| format!("无法设置权限：{e}"))?;
    }
    #[cfg(not(unix))]
    let _ = (path, executable);
    Ok(())
}

/// File checks: a regular file inside `root`, executable on Unix when requested.
pub fn run_file_checks(root: &Path, checks: &[Check]) -> R<()> {
    for check in checks {
        let Check::File { path, executable } = check else {
            return Err("需要更新 GizMoth 才能执行命令检查".into());
        };
        if !registry::safe_relative(path) {
            return Err(format!("检查路径不安全：{path}"));
        }
        let target = root.join(path);
        let metadata = std::fs::symlink_metadata(&target).map_err(|_| format!("缺少 {path}"))?;
        let metadata = if metadata.file_type().is_symlink() {
            // Vendor toolchains link executables; the link must stay inside the installation.
            let real =
                std::fs::canonicalize(&target).map_err(|_| format!("{path} 指向的文件不存在"))?;
            let root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
            if !real.starts_with(&root) {
                return Err(format!("{path} 指向安装目录以外"));
            }
            std::fs::metadata(&real).map_err(|e| e.to_string())?
        } else {
            metadata
        };
        if !metadata.is_file() {
            return Err(format!("{path} 不是文件"));
        }
        #[cfg(unix)]
        if *executable {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                return Err(format!("{path} 不可执行"));
            }
        }
        #[cfg(not(unix))]
        let _ = executable;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::registry::tests as fixtures;
    use super::*;

    const HOST: &str = "macos-arm64";

    fn context() -> Context {
        Context {
            slot: Arc::default(),
            cancel: Arc::new(AtomicBool::new(false)),
            http_timeout: std::time::Duration::from_secs(60),
            phase_hook: None,
        }
    }

    fn fixture(check_path: &str) -> (tempfile::TempDir, Store, Arc<Snapshot>, Installation) {
        let root = tempfile::tempdir().unwrap();
        let store = Store {
            data: root.path().join("data"),
            cache: root.path().join("cache"),
        };
        let digest = registry::hex_digest(b"tool");
        let mut files = fixtures::files();
        for (path, content) in &mut files {
            if path.contains("/versions/") {
                let mut value: serde_json::Value = serde_json::from_str(content).unwrap();
                value["platforms"][HOST]["sources"][0]["sha256"] = digest.clone().into();
                if path.contains("/bazelisk/") {
                    value["platforms"][HOST]["checks"][0]["path"] = check_path.into();
                }
                *content = value.to_string();
            }
        }
        // Latest defaults may advance independently from an installed receipt.
        let mut item: serde_json::Value =
            serde_json::from_str(&files["toolchains/bazelisk/toolchain.json"]).unwrap();
        item["defaultVersion"] = "1.31.0".into();
        item["releases"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"version": "1.31.0", "manifest": "versions/1.31.0.json"}));
        files.insert(
            "toolchains/bazelisk/toolchain.json".into(),
            item.to_string(),
        );
        let mut newer: serde_json::Value =
            serde_json::from_str(&files["toolchains/bazelisk/versions/1.30.0.json"]).unwrap();
        newer["version"] = "1.31.0".into();
        files.insert(
            "toolchains/bazelisk/versions/1.31.0.json".into(),
            newer.to_string(),
        );
        let snapshot = Arc::new(registry::parse_snapshot(fixtures::COMMIT, 0, &files).unwrap());
        let mut old = store::tests::managed(1, Kind::Toolchain, "bazelisk", "1.30.0", &[], true);
        if let Origin::Managed { manifest, .. } = &mut old.origin {
            manifest.sha256 = snapshot
                .release(Kind::Toolchain, "bazelisk", "1.30.0")
                .unwrap()
                .1
                .into();
        }
        std::fs::create_dir_all(store.path_of(&old).join("bin")).unwrap();
        std::fs::write(store.path_of(&old).join("bin/tool"), b"old usable payload").unwrap();
        store.write_installation(&old).unwrap();
        std::fs::create_dir_all(store.downloads().join("sha256")).unwrap();
        std::fs::write(store.downloads().join("sha256").join(digest), b"tool").unwrap();
        (root, store, snapshot, old)
    }

    fn repair(store: &Store, snapshot: &Arc<Snapshot>, previous: &Installation) -> Work {
        Work {
            store: store.clone(),
            snapshot: snapshot.clone(),
            host: HOST,
            kind: if previous.kind() == Kind::Sdk {
                "chip"
            } else {
                "toolchain"
            },
            target: previous.id.clone(),
            resolution: Resolution {
                packages: vec![Package {
                    kind: previous.kind(),
                    id: previous.id.clone(),
                    version: previous.version.clone(),
                }],
                dependencies: vec![Vec::new()],
                chip_toolchains: Vec::new(),
            },
            reuse: vec![None],
            replacement: Some(previous.clone()),
        }
    }

    #[tokio::test]
    async fn reinstall_uses_exact_receipt_version_and_fresh_directory_with_cached_bytes() {
        let (_root, store, snapshot, old) = fixture("bin/tool");
        assert_eq!(
            snapshot
                .item(Kind::Toolchain, "bazelisk")
                .unwrap()
                .default_version
                .as_deref(),
            Some("1.31.0")
        );
        run(&context(), repair(&store, &snapshot, &old))
            .await
            .unwrap();
        let state = store.load().unwrap();
        let fresh = state.installations.values().next().unwrap();
        assert_eq!(state.installations.len(), 1);
        assert_eq!(fresh.version, "1.30.0");
        assert!(fresh.requested_by_user && fresh.installation_id != old.installation_id);
        assert!(!store.path_of(&old).exists());
        assert_eq!(
            std::fs::read(store.path_of(fresh).join("bin/tool")).unwrap(),
            b"tool"
        );
        assert!(store
            .downloads()
            .join("sha256")
            .join(registry::hex_digest(b"tool"))
            .is_file());
    }

    #[tokio::test]
    async fn sdk_reinstall_preserves_shared_toolchain_and_chip_records() {
        let (_root, store, snapshot, _old) = fixture("bin/tool");
        let resolution = super::super::plan::chip(&snapshot, "esp32s3", HOST).unwrap();
        let reuse = resolution
            .packages
            .iter()
            .map(|p| {
                store
                    .load()
                    .unwrap()
                    .find(p.kind, &p.id, &p.version, HOST, &store)
                    .cloned()
            })
            .collect();
        run(
            &context(),
            Work {
                store: store.clone(),
                snapshot: snapshot.clone(),
                host: HOST,
                kind: "chip",
                target: "esp32s3".into(),
                reuse,
                resolution,
                replacement: None,
            },
        )
        .await
        .unwrap();
        let before = store.load().unwrap();
        let chip = &before.chips["esp32s3"];
        let previous = &before.installations[&chip.sdk];
        run(&context(), repair(&store, &snapshot, previous))
            .await
            .unwrap();
        let after = store.load().unwrap();
        assert_ne!(after.chips["esp32s3"].sdk, chip.sdk);
        assert_eq!(after.chips["esp32s3"].toolchains, chip.toolchains);
        assert_eq!(after.chips["esp32s3"].installed_at, chip.installed_at);
        for id in &chip.toolchains {
            assert_eq!(after.installations[id], before.installations[id]);
            assert!(store.path_of(&before.installations[id]).is_dir());
        }
        assert!(!after.installations[&after.chips["esp32s3"].sdk].requested_by_user);
    }

    #[tokio::test]
    async fn failed_checks_and_mid_install_cancellation_keep_old_payload_and_receipt() {
        for cancelled in [false, true] {
            let (_root, store, snapshot, old) =
                fixture(if cancelled { "bin/tool" } else { "bin/missing" });
            let mut context = context();
            if cancelled {
                let flag = context.cancel.clone();
                context.phase_hook = Some(Box::new(move |phase| {
                    if phase == "checking" {
                        flag.store(true, Ordering::SeqCst);
                    }
                }));
            }
            let error = run(&context, repair(&store, &snapshot, &old))
                .await
                .unwrap_err();
            assert!(
                error.contains(if cancelled { "已取消" } else { "缺少" }),
                "{error}"
            );
            let state = store.load().unwrap();
            assert_eq!(state.installations.len(), 1);
            assert_eq!(state.installations[&old.installation_id], old);
            assert_eq!(
                std::fs::read(store.path_of(&old).join("bin/tool")).unwrap(),
                b"old usable payload"
            );
            assert_eq!(
                std::fs::read_dir(store.path_of(&old).parent().unwrap())
                    .unwrap()
                    .count(),
                1
            );
        }
    }

    #[tokio::test]
    async fn reinstall_rejects_local_receipts_and_stale_reuse_without_deleting_payloads() {
        let (root, store, snapshot, mut old) = fixture("bin/tool");
        let local_path = root.path().join("user-toolchain");
        std::fs::create_dir_all(&local_path).unwrap();
        std::fs::write(local_path.join("tool"), b"user-owned").unwrap();
        old.origin = Origin::Local {
            path: local_path.to_string_lossy().into_owned(),
        };
        store.write_installation(&old).unwrap();
        assert!(run(&context(), repair(&store, &snapshot, &old))
            .await
            .unwrap_err()
            .contains("本地"));
        assert_eq!(
            std::fs::read(local_path.join("tool")).unwrap(),
            b"user-owned"
        );
        let mut work = repair(&store, &snapshot, &old);
        work.replacement = None;
        work.reuse = vec![Some(old.clone())];
        std::fs::remove_file(
            store
                .records()
                .join(format!("{}.json", old.installation_id)),
        )
        .unwrap();
        assert!(run(&context(), work)
            .await
            .unwrap_err()
            .contains("记录已变化"));
        assert!(local_path.is_dir());
    }

    #[tokio::test]
    async fn stalled_headers_and_body_chunks_timeout_or_cancel_and_remove_partial_downloads() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for body in [false, true] {
            for cancel in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let store = Store {
                    data: root.path().join("data"),
                    cache: root.path().join("cache"),
                };
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url = format!("http://{}/file", listener.local_addr().unwrap());
                let (ready, received) = tokio::sync::oneshot::channel();
                let server = tokio::spawn(async move {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut bytes = [0u8; 2048];
                    assert!(socket.read(&mut bytes).await.unwrap() > 0);
                    if body {
                        socket
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\n")
                            .await
                            .unwrap();
                    }
                    let _ = ready.send(());
                    std::future::pending::<()>().await;
                    drop(socket);
                });
                let mut context = context();
                context.http_timeout = std::time::Duration::from_millis(150);
                let trigger = async {
                    received.await.unwrap();
                    if cancel {
                        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                        context.cancel.store(true, Ordering::SeqCst);
                    }
                };
                let client = registry::client().unwrap();
                let digest = registry::hex_digest(b"tool");
                let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    tokio::join!(
                        download(&context, &client, &store, &url, &digest, 4),
                        trigger
                    )
                })
                .await
                .unwrap();
                server.abort();
                assert!(result.unwrap_err().contains(if cancel {
                    "已取消"
                } else {
                    "下载超时"
                }));
                assert_eq!(
                    std::fs::read_dir(store.downloads().join("tmp"))
                        .unwrap()
                        .count(),
                    0
                );
                assert!(!store.downloads().join("sha256").join(digest).exists());
            }
        }
    }

    #[test]
    fn file_checks_require_contained_regular_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("bin")).unwrap();
        std::fs::write(root.path().join("bin/tool"), b"tool").unwrap();
        set_executable(&root.path().join("bin/tool"), true).unwrap();
        let check = |path: &str| {
            vec![Check::File {
                path: path.into(),
                executable: true,
            }]
        };
        run_file_checks(root.path(), &check("bin/tool")).unwrap();
        assert!(run_file_checks(root.path(), &check("bin/missing")).is_err());
        assert!(run_file_checks(root.path(), &check("bin")).is_err());
        assert!(run_file_checks(root.path(), &check("../outside")).is_err());
        #[cfg(unix)]
        {
            set_executable(&root.path().join("bin/tool"), false).unwrap();
            assert!(run_file_checks(root.path(), &check("bin/tool"))
                .unwrap_err()
                .contains("不可执行"));
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(outside.path().join("tool"), b"x").unwrap();
            std::os::unix::fs::symlink(outside.path().join("tool"), root.path().join("bin/link"))
                .unwrap();
            assert!(run_file_checks(root.path(), &check("bin/link"))
                .unwrap_err()
                .contains("以外"));
        }
    }

    /// `cargo test --lib devenv::install::tests::live -- --ignored`: install the published
    /// Bazelisk release into a temporary store, reinstall it from cache, then remove it.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "network"]
    async fn live_bazelisk_install_and_removal() {
        let host = registry::host_platform().unwrap();
        let (commit, files) = registry::fetch().await.unwrap();
        let snapshot = Arc::new(registry::parse_snapshot(&commit, 0, &files).unwrap());
        let resolution = super::super::plan::toolchain(&snapshot, "bazelisk", host).unwrap();
        let root = tempfile::tempdir().unwrap();
        let store = Store {
            data: root.path().join("data"),
            cache: root.path().join("cache"),
        };
        let slot: Slot = Arc::default();
        let work = Work {
            store: store.clone(),
            snapshot: snapshot.clone(),
            host,
            kind: "toolchain",
            target: "bazelisk".into(),
            reuse: vec![None; resolution.packages.len()],
            resolution,
            replacement: None,
        };
        start(&slot, "Bazelisk".into(), work).unwrap();
        let view = loop {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let view = slot.lock().unwrap().as_ref().unwrap().view.clone();
            if view.state != "running" {
                break view;
            }
        };
        assert_eq!(view.state, "succeeded", "{:?} {:?}", view.error, view.log);
        assert_eq!(view.received, view.total);
        let state = store.load().unwrap();
        let record = state.installations.values().next().unwrap();
        assert!(record.requested_by_user && record.version == "1.30.0");
        let bin = store.path_of(record).join(if cfg!(windows) {
            "bin/bazel.exe"
        } else {
            "bin/bazel"
        });
        let bytes = std::fs::read(&bin).unwrap();
        let (release, _) = snapshot
            .release(Kind::Toolchain, "bazelisk", "1.30.0")
            .unwrap();
        let Source::File { sha256, .. } = &release.platforms[host].sources[0] else {
            panic!("file source")
        };
        assert_eq!(&registry::hex_digest(&bytes), sha256);
        run_file_checks(&store.path_of(record), &release.platforms[host].checks).unwrap();
        println!(
            "installed {} ({} bytes) at {}",
            record.installation_id,
            bytes.len(),
            bin.display()
        );

        let old_id = record.installation_id.clone();
        let old_directory = store.path_of(record);
        let package = super::super::plan::reinstall(&snapshot, record, host).unwrap();
        start(
            &slot,
            "Reinstall Bazelisk".into(),
            Work {
                store: store.clone(),
                snapshot: snapshot.clone(),
                host,
                kind: "toolchain",
                target: "bazelisk".into(),
                resolution: Resolution {
                    packages: vec![package],
                    dependencies: vec![Vec::new()],
                    chip_toolchains: Vec::new(),
                },
                reuse: vec![None],
                replacement: Some(record.clone()),
            },
        )
        .unwrap();
        let view = tokio::time::timeout(std::time::Duration::from_secs(120), async {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                let view = slot.lock().unwrap().as_ref().unwrap().view.clone();
                if view.state != "running" {
                    break view;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(view.state, "succeeded", "{:?} {:?}", view.error, view.log);
        assert!(view.log.iter().any(|line| line.contains("已缓存的下载")));
        let state = store.load().unwrap();
        let record = state.installations.values().next().unwrap();
        assert_ne!(record.installation_id, old_id);
        assert_eq!(record.version, "1.30.0");
        assert!(!old_directory.exists());
        run_file_checks(&store.path_of(record), &release.platforms[host].checks).unwrap();
        let directory = store.path_of(record);
        let target = store::Target::Installation(record.installation_id.clone());
        let plan = state.plan(&target).unwrap();
        assert_eq!(plan.delete, vec![record.installation_id.clone()]);
        store.remove(&target, &plan.delete).unwrap();
        assert!(store.load().unwrap().installations.is_empty() && !directory.exists());
        assert!(
            store.downloads().join("sha256").join(sha256).is_file(),
            "download cache is kept"
        );
    }

    #[test]
    fn cached_downloads_are_rehashed() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        std::fs::write(&path, b"abcd").unwrap();
        let digest = registry::hex_digest(b"abcd");
        assert!(verify_file(&path, &digest, 4).unwrap());
        assert!(!verify_file(&path, &digest, 5).unwrap());
        assert!(!verify_file(&path, &"0".repeat(64), 4).unwrap());
    }
}
