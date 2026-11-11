//! Local records and removal rules. GizMoth owns every path here; registry
//! content never chooses a record location or a deletion target.
use super::registry::{
    safe_relative, valid_commit, valid_https, valid_id, valid_sha256, valid_version, Kind,
    PLATFORMS,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

type R<T> = Result<T, String>;

pub const API_VERSION: &str = super::registry::API_VERSION;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryOrigin {
    pub repository: String,
    pub commit: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestOrigin {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Origin {
    #[serde(rename_all = "camelCase")]
    Managed {
        registry: RegistryOrigin,
        manifest: ManifestOrigin,
        prefix: String,
        source_digests: Vec<String>,
    },
    Local {
        path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Installation {
    pub api_version: String,
    pub kind: String,
    pub installation_id: String,
    #[serde(rename = "type")]
    pub kind_of: String,
    pub id: String,
    pub version: String,
    pub platform: String,
    pub origin: Origin,
    pub requested_by_user: bool,
    pub dependencies: Vec<String>,
    pub installed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstalledChip {
    pub api_version: String,
    pub kind: String,
    pub chip: String,
    pub sdk: String,
    pub toolchains: Vec<String>,
    pub installed_at: String,
}

pub fn valid_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value)
        .is_ok_and(|id| id.get_version_num() == 4 && id.hyphenated().to_string() == value)
}

fn require(condition: bool, message: impl FnOnce() -> String) -> R<()> {
    if condition {
        Ok(())
    } else {
        Err(message())
    }
}

pub fn managed_prefix(
    kind: Kind,
    id: &str,
    version: &str,
    platform: &str,
    installation: &str,
) -> String {
    format!(
        "installs/{}/{id}/{version}/{platform}/{installation}",
        kind.directory()
    )
}

impl Installation {
    pub fn kind(&self) -> Kind {
        Kind::parse(&self.kind_of).unwrap_or(Kind::Toolchain)
    }
    pub fn is_local(&self) -> bool {
        matches!(self.origin, Origin::Local { .. })
    }
    pub fn validate(&self) -> R<()> {
        let owner = format!("安装记录 {}", self.installation_id);
        require(
            self.api_version == API_VERSION && self.kind == "Installation",
            || format!("{owner}: 格式无效"),
        )?;
        require(valid_uuid(&self.installation_id), || {
            format!("{owner}: ID 无效")
        })?;
        let kind = Kind::parse(&self.kind_of)?;
        require(valid_id(&self.id), || format!("{owner}: 名称无效"))?;
        require(PLATFORMS.contains(&self.platform.as_str()), || {
            format!("{owner}: 平台无效")
        })?;
        require(
            !self.version.is_empty()
                && self.version.len() <= 96
                && self
                    .version
                    .starts_with(|c: char| c.is_ascii_alphanumeric())
                && self
                    .version
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '_' | '-')),
            || format!("{owner}: 版本无效"),
        )?;
        require(!self.dependencies.contains(&self.installation_id), || {
            format!("{owner}: 不能依赖自身")
        })?;
        require(self.dependencies.iter().all(|id| valid_uuid(id)), || {
            format!("{owner}: 依赖无效")
        })?;
        match &self.origin {
            Origin::Managed {
                registry,
                manifest,
                prefix,
                source_digests,
            } => {
                require(valid_version(&self.version), || {
                    format!("{owner}: 托管安装需要精确版本")
                })?;
                require(
                    valid_https(&registry.repository) && valid_commit(&registry.commit),
                    || format!("{owner}: 目录来源无效"),
                )?;
                require(
                    *prefix
                        == managed_prefix(
                            kind,
                            &self.id,
                            &self.version,
                            &self.platform,
                            &self.installation_id,
                        )
                        && safe_relative(prefix),
                    || format!("{owner}: 安装路径与身份不符"),
                )?;
                require(
                    manifest.path == super::registry::manifest_path(kind, &self.id, &self.version)
                        && valid_sha256(&manifest.sha256),
                    || format!("{owner}: 版本清单不匹配"),
                )?;
                require(source_digests.iter().all(|d| valid_sha256(d)), || {
                    format!("{owner}: 来源摘要无效")
                })?;
            }
            Origin::Local { path } => {
                require(kind == Kind::Toolchain, || {
                    format!("{owner}: 只有工具链可以从本地路径添加")
                })?;
                require(
                    self.requested_by_user && self.dependencies.is_empty(),
                    || format!("{owner}: 本地工具链必须由用户添加且没有托管依赖"),
                )?;
                require(
                    Path::new(path).is_absolute() && !path.contains('\0'),
                    || format!("{owner}: 本地路径无效"),
                )?;
            }
        }
        Ok(())
    }
}

/// The devenv data and cache roots.
#[derive(Debug, Clone)]
pub struct Store {
    pub data: PathBuf,
    pub cache: PathBuf,
}

impl Store {
    pub fn installs(&self) -> PathBuf {
        self.data.join("installs")
    }
    pub fn records(&self) -> PathBuf {
        self.data.join("records")
    }
    pub fn chips(&self) -> PathBuf {
        self.data.join("chips")
    }
    pub fn trash(&self) -> PathBuf {
        self.data.join("trash")
    }
    pub fn registry(&self) -> PathBuf {
        self.data.join("registry")
    }
    pub fn downloads(&self) -> PathBuf {
        self.cache.join("downloads")
    }
    pub fn path_of(&self, record: &Installation) -> PathBuf {
        match &record.origin {
            Origin::Managed { prefix, .. } => self.data.join(prefix),
            Origin::Local { path } => PathBuf::from(path),
        }
    }

    pub fn load(&self) -> R<State> {
        let mut installations = BTreeMap::new();
        for (name, content) in read_json_dir(&self.records())? {
            let record: Installation =
                serde_json::from_str(&content).map_err(|e| format!("安装记录 {name} 无效：{e}"))?;
            record.validate()?;
            require(name == format!("{}.json", record.installation_id), || {
                format!("安装记录 {name} 文件名不匹配")
            })?;
            installations.insert(record.installation_id.clone(), record);
        }
        let mut chips = BTreeMap::new();
        for (name, content) in read_json_dir(&self.chips())? {
            let record: InstalledChip =
                serde_json::from_str(&content).map_err(|e| format!("芯片记录 {name} 无效：{e}"))?;
            require(
                record.api_version == API_VERSION
                    && record.kind == "InstalledChip"
                    && valid_id(&record.chip)
                    && name == format!("{}.json", record.chip),
                || format!("芯片记录 {name} 无效"),
            )?;
            chips.insert(record.chip.clone(), record);
        }
        let state = State {
            installations,
            chips,
        };
        state.check()?;
        Ok(state)
    }

    pub fn write_installation(&self, record: &Installation) -> R<()> {
        record.validate()?;
        write_atomic(
            &self.records(),
            &format!("{}.json", record.installation_id),
            record,
        )
    }
    pub fn write_chip(&self, record: &InstalledChip) -> R<()> {
        write_atomic(&self.chips(), &format!("{}.json", record.chip), record)
    }

    /// Move unrecorded managed directories to trash and empty the trash. Call only when no job runs.
    pub fn recover(&self) -> R<()> {
        let state = self.load()?;
        let recorded: BTreeSet<PathBuf> = state
            .installations
            .values()
            .filter(|r| !r.is_local())
            .map(|r| self.path_of(r))
            .collect();
        let mut orphans = Vec::new();
        collect_depth(&self.installs(), 5, &mut orphans);
        let trash = self.trash().join(uuid::Uuid::new_v4().to_string());
        for orphan in orphans.into_iter().filter(|path| !recorded.contains(path)) {
            std::fs::create_dir_all(&trash).map_err(|e| format!("无法清理未完成的安装：{e}"))?;
            let _ = std::fs::rename(&orphan, trash.join(uuid::Uuid::new_v4().to_string()));
        }
        if self.trash().is_dir() {
            let _ = std::fs::remove_dir_all(self.trash());
        }
        Ok(())
    }
}

fn collect_depth(path: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() || kind.is_symlink() {
            continue;
        }
        if depth == 1 {
            found.push(entry.path());
        } else {
            collect_depth(&entry.path(), depth - 1, found);
        }
    }
}

fn read_json_dir(path: &Path) -> R<Vec<(String, String)>> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("无法读取开发环境记录：{e}")),
    };
    let mut result = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("无法读取开发环境记录：{e}"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".json") || name.starts_with('.') {
            continue;
        }
        let content =
            std::fs::read_to_string(entry.path()).map_err(|e| format!("无法读取 {name}：{e}"))?;
        result.push((name, content));
    }
    Ok(result)
}

pub fn write_atomic<T: Serialize>(directory: &Path, name: &str, value: &T) -> R<()> {
    std::fs::create_dir_all(directory).map_err(|e| format!("无法写入开发环境记录：{e}"))?;
    let temporary = directory.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let content = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(&temporary, content).map_err(|e| format!("无法写入开发环境记录：{e}"))?;
    std::fs::rename(&temporary, directory.join(name)).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        format!("无法写入开发环境记录：{e}")
    })
}

/// All local records, checked for internal consistency.
#[derive(Debug, Clone, Default)]
pub struct State {
    pub installations: BTreeMap<String, Installation>,
    pub chips: BTreeMap<String, InstalledChip>,
}

impl State {
    pub fn check(&self) -> R<()> {
        for record in self.installations.values() {
            for dependency in &record.dependencies {
                require(
                    self.installations
                        .get(dependency)
                        .is_some_and(|d| d.kind() == Kind::Toolchain),
                    || format!("安装记录 {} 缺少依赖 {dependency}", record.installation_id),
                )?;
            }
        }
        for chip in self.chips.values() {
            require(
                self.installations
                    .get(&chip.sdk)
                    .is_some_and(|r| r.kind() == Kind::Sdk),
                || format!("芯片 {} 的 SDK 记录缺失", chip.chip),
            )?;
            require(!chip.toolchains.contains(&chip.sdk), || {
                format!("芯片 {} 记录无效", chip.chip)
            })?;
            for id in &chip.toolchains {
                require(
                    self.installations
                        .get(id)
                        .is_some_and(|r| r.kind() == Kind::Toolchain),
                    || format!("芯片 {} 的工具链记录缺失：{id}", chip.chip),
                )?;
            }
        }
        Ok(())
    }

    /// A reusable installation of exactly this package on this host.
    pub fn find(
        &self,
        kind: Kind,
        id: &str,
        version: &str,
        platform: &str,
        store: &Store,
    ) -> Option<&Installation> {
        let mut candidates: Vec<_> = self
            .installations
            .values()
            .filter(|r| {
                r.kind() == kind && r.id == id && r.version == version && r.platform == platform
            })
            .filter(|r| store.path_of(r).is_dir())
            .collect();
        // Prefer a user's local toolchain, then the earliest managed installation.
        candidates.sort_by_key(|r| (!r.is_local(), r.installed_at.clone()));
        candidates.into_iter().next()
    }

    fn closure(&self, roots: impl IntoIterator<Item = String>) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut pending: Vec<String> = roots.into_iter().collect();
        while let Some(id) = pending.pop() {
            if seen.insert(id.clone()) {
                if let Some(record) = self.installations.get(&id) {
                    pending.extend(record.dependencies.iter().cloned());
                }
            }
        }
        seen
    }
    fn chip_roots(chip: &InstalledChip) -> Vec<String> {
        std::iter::once(chip.sdk.clone())
            .chain(chip.toolchains.iter().cloned())
            .collect()
    }
    /// Installed chips and user-requested installations are roots; everything they reach is kept.
    fn kept(&self, skip_chip: Option<&str>, skip_installation: Option<&str>) -> BTreeSet<String> {
        let roots = self
            .chips
            .values()
            .filter(|chip| Some(chip.chip.as_str()) != skip_chip)
            .flat_map(Self::chip_roots)
            .chain(
                self.installations
                    .values()
                    .filter(|r| {
                        r.requested_by_user && Some(r.installation_id.as_str()) != skip_installation
                    })
                    .map(|r| r.installation_id.clone()),
            );
        self.closure(roots)
    }

    pub fn chips_using(&self, installation: &str) -> Vec<String> {
        self.chips
            .values()
            .filter(|chip| self.closure(Self::chip_roots(chip)).contains(installation))
            .map(|chip| chip.chip.clone())
            .collect()
    }

    pub fn plan(&self, target: &Target) -> R<Removal> {
        match target {
            Target::Chip(id) => {
                let chip = self
                    .chips
                    .get(id)
                    .ok_or_else(|| format!("芯片未安装：{id}"))?;
                let remaining = self.kept(Some(id), None);
                let reached = self.closure(Self::chip_roots(chip));
                let (keep, delete): (Vec<_>, Vec<_>) =
                    reached.into_iter().partition(|i| remaining.contains(i));
                Ok(Removal {
                    action: Action::Delete,
                    delete,
                    keep,
                    chips: Vec::new(),
                    dependants: Vec::new(),
                })
            }
            Target::Installation(id) => {
                let target = self
                    .installations
                    .get(id)
                    .ok_or_else(|| format!("未知安装：{id}"))?;
                let chips = self.chips_using(id);
                let dependants: Vec<String> = self
                    .installations
                    .values()
                    .filter(|r| r.dependencies.contains(id))
                    .map(|r| r.installation_id.clone())
                    .collect();
                if !chips.is_empty() || !dependants.is_empty() {
                    return Ok(Removal {
                        action: Action::Blocked,
                        delete: Vec::new(),
                        keep: Vec::new(),
                        chips,
                        dependants,
                    });
                }
                if target.is_local() {
                    return Ok(Removal {
                        action: Action::Unregister,
                        delete: Vec::new(),
                        keep: vec![id.clone()],
                        chips,
                        dependants,
                    });
                }
                let remaining = self.kept(None, Some(id));
                let reached = self.closure(target.dependencies.iter().cloned());
                let (keep, orphaned): (Vec<_>, Vec<_>) =
                    reached.into_iter().partition(|i| remaining.contains(i));
                let mut delete = vec![id.clone()];
                delete.extend(orphaned);
                delete.sort();
                Ok(Removal {
                    action: Action::Delete,
                    delete,
                    keep,
                    chips,
                    dependants,
                })
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "lowercase")]
pub enum Target {
    Chip(String),
    Installation(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Delete,
    Unregister,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    pub action: Action,
    /// Managed installations whose directories and records are removed. Local entries never appear here.
    pub delete: Vec<String>,
    pub keep: Vec<String>,
    pub chips: Vec<String>,
    pub dependants: Vec<String>,
}

impl Store {
    /// Recompute the plan, require it to match what the user confirmed, then detach and delete.
    pub fn remove(&self, target: &Target, confirmed: &[String]) -> R<()> {
        let state = self.load()?;
        let plan = state.plan(target)?;
        let mut confirmed = confirmed.to_vec();
        confirmed.sort();
        let mut planned = plan.delete.clone();
        planned.sort();
        require(plan.action != Action::Blocked, || {
            "仍有芯片或其他工具链在使用，无法删除".into()
        })?;
        require(planned == confirmed, || "删除内容已变化，请重新确认".into())?;

        let installs = std::fs::canonicalize(self.installs()).ok();
        let trash = self.trash().join(uuid::Uuid::new_v4().to_string());
        let mut moved = HashMap::new();
        for id in &plan.delete {
            let record = &state.installations[id];
            require(!record.is_local(), || "本地工具链不会被删除".into())?;
            let path = self.path_of(record);
            if !path.exists() {
                continue;
            }
            let real =
                std::fs::canonicalize(&path).map_err(|e| format!("无法确认安装路径：{e}"))?;
            require(
                installs
                    .as_ref()
                    .is_some_and(|root| real.starts_with(root) && real != *root)
                    && std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()),
                || format!("安装路径不在 GizMoth 管理目录内：{}", path.display()),
            )?;
            moved.insert(id.clone(), real);
        }
        if let Target::Chip(chip) = target {
            remove_file(&self.chips().join(format!("{chip}.json")))?;
        }
        if moved.is_empty() {
            // Nothing to detach.
        } else {
            std::fs::create_dir_all(&trash).map_err(|e| format!("无法准备删除：{e}"))?;
        }
        for (id, path) in &moved {
            std::fs::rename(path, trash.join(id)).map_err(|e| format!("无法移除安装目录：{e}"))?;
        }
        let records: Vec<&String> = match (&plan.action, target) {
            (Action::Unregister, Target::Installation(id)) => vec![id],
            _ => plan.delete.iter().collect(),
        };
        for id in records {
            remove_file(&self.records().join(format!("{id}.json")))?;
        }
        if trash.exists() {
            std::fs::remove_dir_all(&trash)
                .map_err(|e| format!("记录已移除，但删除文件失败，下次启动会重试：{e}"))?;
        }
        Ok(())
    }
}

fn remove_file(path: &Path) -> R<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("无法移除记录：{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "macos-arm64";
    fn uuid(n: u32) -> String {
        format!("00000000-0000-4000-8000-{n:012}")
    }
    fn managed(
        n: u32,
        kind: Kind,
        id: &str,
        version: &str,
        dependencies: &[u32],
        requested: bool,
    ) -> Installation {
        let installation_id = uuid(n);
        Installation {
            api_version: API_VERSION.into(),
            kind: "Installation".into(),
            installation_id: installation_id.clone(),
            kind_of: kind.as_str().into(),
            id: id.into(),
            version: version.into(),
            platform: HOST.into(),
            origin: Origin::Managed {
                registry: RegistryOrigin {
                    repository: super::super::registry::REPOSITORY.into(),
                    commit: "a".repeat(40),
                },
                manifest: ManifestOrigin {
                    path: super::super::registry::manifest_path(kind, id, version),
                    sha256: "b".repeat(64),
                },
                prefix: managed_prefix(kind, id, version, HOST, &installation_id),
                source_digests: Vec::new(),
            },
            requested_by_user: requested,
            dependencies: dependencies.iter().map(|d| uuid(*d)).collect(),
            installed_at: "2026-11-11T00:00:00.000Z".into(),
        }
    }
    fn local(n: u32, id: &str, path: &str) -> Installation {
        Installation {
            origin: Origin::Local { path: path.into() },
            requested_by_user: true,
            ..managed(n, Kind::Toolchain, id, "1.0.0", &[], true)
        }
    }
    fn chip(id: &str, sdk: u32, toolchains: &[u32]) -> InstalledChip {
        InstalledChip {
            api_version: API_VERSION.into(),
            kind: "InstalledChip".into(),
            chip: id.into(),
            sdk: uuid(sdk),
            toolchains: toolchains.iter().map(|t| uuid(*t)).collect(),
            installed_at: "2026-11-11T00:00:00.000Z".into(),
        }
    }
    // 10 esp-idf, 11 bk7258 sdk -> 21 arm, 20 xtensa, 22 bazelisk
    fn state() -> State {
        let installations = [
            managed(10, Kind::Sdk, "esp-idf", "6.2.0", &[], false),
            managed(11, Kind::Sdk, "beken-bk7258", "3.1.1", &[21], false),
            managed(20, Kind::Toolchain, "xtensa-esp-elf", "14.2.0", &[], false),
            managed(21, Kind::Toolchain, "arm-gnu", "10.3.0", &[], false),
            managed(22, Kind::Toolchain, "bazelisk", "1.30.0", &[], false),
        ];
        State {
            installations: installations
                .into_iter()
                .map(|r| (r.installation_id.clone(), r))
                .collect(),
            chips: [chip("esp32s3", 10, &[20, 22]), chip("bk7258", 11, &[22])]
                .into_iter()
                .map(|c| (c.chip.clone(), c))
                .collect(),
        }
    }
    fn ids(values: &[u32]) -> Vec<String> {
        values.iter().map(|v| uuid(*v)).collect()
    }

    #[test]
    fn records_validate_identity_paths_and_local_rules() {
        let record = managed(22, Kind::Toolchain, "bazelisk", "1.30.0", &[], false);
        record.validate().unwrap();
        let mut moved = record.clone();
        if let Origin::Managed { prefix, .. } = &mut moved.origin {
            *prefix = "../external-sdk".into();
        }
        assert!(moved.validate().is_err());
        assert!(Installation {
            version: "latest".into(),
            ..record.clone()
        }
        .validate()
        .is_err());
        assert!(Installation {
            dependencies: ids(&[22]),
            ..record
        }
        .validate()
        .is_err());
        assert!(local(30, "arm-gnu", "relative/path").validate().is_err());
        assert!(Installation {
            kind_of: "sdk".into(),
            ..local(30, "esp-idf", "/opt/esp-idf")
        }
        .validate()
        .is_err());
        assert!(Installation {
            requested_by_user: false,
            ..local(30, "arm-gnu", "/opt/arm")
        }
        .validate()
        .is_err());
        local(30, "arm-gnu", "/opt/arm").validate().unwrap();
    }

    #[test]
    fn removing_a_chip_keeps_shared_and_requested_installations() {
        let plan = state().plan(&Target::Chip("esp32s3".into())).unwrap();
        assert_eq!((plan.delete, plan.keep), (ids(&[10, 20]), ids(&[22])));
        assert_eq!(
            state().plan(&Target::Chip("bk7258".into())).unwrap().delete,
            ids(&[11, 21])
        );
        let mut requested = state();
        requested
            .installations
            .get_mut(&uuid(20))
            .unwrap()
            .requested_by_user = true;
        let plan = requested.plan(&Target::Chip("esp32s3".into())).unwrap();
        assert_eq!((plan.delete, plan.keep), (ids(&[10]), ids(&[20, 22])));
    }

    #[test]
    fn toolchains_in_use_are_blocked() {
        let plan = state().plan(&Target::Installation(uuid(22))).unwrap();
        assert_eq!(
            (plan.action, plan.chips),
            (Action::Blocked, vec!["bk7258".into(), "esp32s3".into()])
        );
        let plan = state().plan(&Target::Installation(uuid(21))).unwrap();
        assert_eq!(
            (plan.action, plan.dependants),
            (Action::Blocked, ids(&[11]))
        );
    }

    #[test]
    fn removing_a_requested_toolchain_takes_unneeded_dependencies() {
        let mut s = state();
        for record in [
            managed(24, Kind::Toolchain, "helper", "1.0.0", &[], false),
            managed(23, Kind::Toolchain, "tool", "1.0.0", &[24, 21], true),
        ] {
            s.installations
                .insert(record.installation_id.clone(), record);
        }
        let plan = s.plan(&Target::Installation(uuid(23))).unwrap();
        assert_eq!((plan.action, plan.delete), (Action::Delete, ids(&[23, 24])));
    }

    #[test]
    fn local_toolchains_are_only_unregistered() {
        let mut s = state();
        let jieli = local(30, "jieli", "/opt/jieli");
        s.installations.insert(jieli.installation_id.clone(), jieli);
        let sdk = managed(12, Kind::Sdk, "jieli-ac707n", "1.0.0", &[], false);
        s.installations.insert(sdk.installation_id.clone(), sdk);
        s.chips
            .insert("ac707n".into(), chip("ac707n", 12, &[30, 22]));
        let plan = s.plan(&Target::Chip("ac707n".into())).unwrap();
        assert_eq!((plan.delete, plan.keep), (ids(&[12]), ids(&[22, 30])));
        assert_eq!(
            s.plan(&Target::Installation(uuid(30))).unwrap().action,
            Action::Blocked
        );
        s.chips.remove("ac707n");
        let plan = s.plan(&Target::Installation(uuid(30))).unwrap();
        assert_eq!(
            (plan.action, plan.delete),
            (Action::Unregister, Vec::<String>::new())
        );
    }

    #[test]
    fn inconsistent_state_is_rejected() {
        let mut wrong = state();
        wrong.chips.get_mut("esp32s3").unwrap().sdk = uuid(21);
        assert!(wrong.check().is_err());
        let mut missing = state();
        missing
            .installations
            .get_mut(&uuid(11))
            .unwrap()
            .dependencies = ids(&[99]);
        assert!(missing.check().is_err());
    }

    #[test]
    fn removal_detaches_owned_directories_and_requires_the_confirmed_plan() {
        let root = tempfile::tempdir().unwrap();
        let store = Store {
            data: root.path().join("data"),
            cache: root.path().join("cache"),
        };
        let s = state();
        for record in s.installations.values() {
            std::fs::create_dir_all(store.path_of(record)).unwrap();
            store.write_installation(record).unwrap();
        }
        for chip in s.chips.values() {
            store.write_chip(chip).unwrap();
        }
        let target = Target::Chip("esp32s3".into());
        assert!(store
            .remove(&target, &ids(&[10]))
            .unwrap_err()
            .contains("重新确认"));
        store.remove(&target, &ids(&[10, 20])).unwrap();
        let after = store.load().unwrap();
        assert!(!after.chips.contains_key("esp32s3"));
        assert!(
            !after.installations.contains_key(&uuid(10))
                && after.installations.contains_key(&uuid(22))
        );
        assert!(!store.path_of(&s.installations[&uuid(20)]).exists());
        assert!(store.path_of(&s.installations[&uuid(22)]).exists());
        assert!(
            !store.trash().exists() || std::fs::read_dir(store.trash()).unwrap().next().is_none()
        );

        // An unrecorded directory left by an interrupted install is swept on recovery.
        let orphan = store.data.join(managed_prefix(
            Kind::Toolchain,
            "arm-gnu",
            "14.3.0",
            HOST,
            &uuid(40),
        ));
        std::fs::create_dir_all(&orphan).unwrap();
        store.recover().unwrap();
        assert!(!orphan.exists() && store.path_of(&s.installations[&uuid(22)]).exists());
    }
}
