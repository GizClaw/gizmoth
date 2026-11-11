//! Resolve what a chip or toolchain needs on this host, and whether this GizMoth can install it.
use super::registry::{Check, Kind, Recipe, Snapshot, Source, Status, Step};
use serde::Serialize;
use std::collections::BTreeSet;

/// Why an entry cannot be installed here. The frontend localizes each code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "code", content = "detail", rename_all = "kebab-case")]
pub enum Blocked {
    Planned,
    UnsupportedHost(Vec<String>),
    UnsupportedOperation(String),
    MissingProgram(String),
    Invalid(String),
}

impl Blocked {
    pub fn message(&self) -> String {
        match self {
            Blocked::Planned => "配方待补，暂时无法安装".into(),
            Blocked::UnsupportedHost(hosts) if hosts.is_empty() => "不支持此电脑".into(),
            Blocked::UnsupportedHost(hosts) => format!("不支持此电脑，支持：{}", hosts.join("、")),
            Blocked::UnsupportedOperation(op) => format!("需要更新 GizMoth 才能执行安装步骤：{op}"),
            Blocked::MissingProgram(program) => format!("请先在本机安装 {program}"),
            Blocked::Invalid(message) => message.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Package {
    pub kind: Kind,
    pub id: String,
    pub version: String,
}

/// Packages in installation order (dependencies first; the SDK last).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub packages: Vec<Package>,
    /// Exact direct toolchain roots of a chip or generic environment.
    pub chip_toolchains: Vec<Package>,
    /// Direct toolchain dependencies of each package, by package position.
    pub dependencies: Vec<Vec<Package>>,
}

fn recipe<'a>(
    snapshot: &'a Snapshot,
    package: &Package,
    host: &str,
) -> Result<&'a Recipe, Blocked> {
    let item = snapshot
        .item(package.kind, &package.id)
        .ok_or_else(|| Blocked::Invalid(format!("目录中没有 {}", package.id)))?;
    if item.status == Status::Planned {
        return Err(Blocked::Planned);
    }
    let (release, _) = snapshot
        .release(package.kind, &package.id, &package.version)
        .ok_or_else(|| Blocked::Invalid(format!("{} {} 未发布", item.name, package.version)))?;
    release
        .platforms
        .get(host)
        .ok_or_else(|| Blocked::UnsupportedHost(release.platforms.keys().cloned().collect()))
}

struct Resolver<'a> {
    snapshot: &'a Snapshot,
    host: &'a str,
    resolved: BTreeSet<Package>,
    visiting: BTreeSet<Package>,
    order: Vec<Package>,
    dependencies: Vec<Vec<Package>>,
}

impl Resolver<'_> {
    fn toolchain(&mut self, id: &str, version: &str) -> Result<(), Blocked> {
        let package = toolchain_package(id, version);
        if self.visiting.contains(&package) {
            return Err(Blocked::Invalid(format!("工具链依赖循环：{id}@{version}")));
        }
        if self.resolved.contains(&package) {
            return Ok(());
        }
        let recipe = recipe(self.snapshot, &package, self.host)?;
        self.visiting.insert(package.clone());
        for dependency in &recipe.dependencies {
            self.toolchain(&dependency.toolchain, &dependency.version)?;
        }
        self.visiting.remove(&package);
        self.resolved.insert(package.clone());
        self.dependencies.push(
            recipe
                .dependencies
                .iter()
                .map(|d| toolchain_package(&d.toolchain, &d.version))
                .collect(),
        );
        self.order.push(package);
        Ok(())
    }
}

fn toolchain_package(id: &str, version: &str) -> Package {
    Package {
        kind: Kind::Toolchain,
        id: id.into(),
        version: version.into(),
    }
}

fn resolver<'a>(snapshot: &'a Snapshot, host: &'a str) -> Resolver<'a> {
    Resolver {
        snapshot,
        host,
        resolved: BTreeSet::new(),
        visiting: BTreeSet::new(),
        order: Vec::new(),
        dependencies: Vec::new(),
    }
}

pub fn chip(snapshot: &Snapshot, chip_id: &str, host: &str) -> Result<Resolution, Blocked> {
    chip_version(snapshot, chip_id, None, host)
}

pub fn chip_version(
    snapshot: &Snapshot,
    chip_id: &str,
    version: Option<&str>,
    host: &str,
) -> Result<Resolution, Blocked> {
    let chip = snapshot
        .chip(chip_id)
        .ok_or_else(|| Blocked::Invalid(format!("目录中没有芯片 {chip_id}")))?;
    if chip.status == Status::Planned {
        return Err(Blocked::Planned);
    }
    let sdk_item = snapshot
        .item(Kind::Sdk, &chip.sdk)
        .ok_or(Blocked::Planned)?;
    let version = version
        .map(str::to_owned)
        .or_else(|| sdk_item.default_version.clone())
        .ok_or(Blocked::Planned)?;
    let sdk = Package {
        kind: Kind::Sdk,
        id: chip.sdk.clone(),
        version,
    };
    let sdk_recipe = recipe(snapshot, &sdk, host)?;
    let target = sdk_recipe
        .targets
        .as_ref()
        .and_then(|targets| targets.get(&chip.target))
        .ok_or_else(|| {
            Blocked::Invalid(format!(
                "{} {} 不支持 {}",
                sdk_item.name, sdk.version, chip.name
            ))
        })?;
    let mut roots: Vec<(String, String)> = sdk_recipe
        .dependencies
        .iter()
        .chain(&target.dependencies)
        .map(|d| (d.toolchain.clone(), d.version.clone()))
        .collect();
    for tool in &chip.tools {
        let item = snapshot
            .item(Kind::Toolchain, tool)
            .ok_or(Blocked::Planned)?;
        let version = item.default_version.clone().ok_or(Blocked::Planned)?;
        roots.push((tool.clone(), version));
    }
    let mut resolver = resolver(snapshot, host);
    let mut chip_toolchains = Vec::new();
    for (id, version) in roots {
        resolver.toolchain(&id, &version)?;
        let package = toolchain_package(&id, &version);
        if !chip_toolchains.contains(&package) {
            chip_toolchains.push(package);
        }
    }
    resolver.dependencies.push(
        sdk_recipe
            .dependencies
            .iter()
            .map(|d| toolchain_package(&d.toolchain, &d.version))
            .collect(),
    );
    resolver.order.push(sdk);
    Ok(Resolution {
        packages: resolver.order,
        chip_toolchains,
        dependencies: resolver.dependencies,
    })
}

pub fn toolchain(snapshot: &Snapshot, id: &str, host: &str) -> Result<Resolution, Blocked> {
    package(snapshot, Kind::Toolchain, id, None, host)
}

/// Select the exact release when supplied, otherwise retain the registry default.
pub fn package(
    snapshot: &Snapshot,
    kind: Kind,
    id: &str,
    version: Option<&str>,
    host: &str,
) -> Result<Resolution, Blocked> {
    let item = snapshot
        .item(kind, id)
        .ok_or_else(|| Blocked::Invalid(format!("目录中没有 {id}")))?;
    let version = version
        .map(str::to_owned)
        .or_else(|| item.default_version.clone())
        .ok_or(Blocked::Planned)?;
    let mut resolver = resolver(snapshot, host);
    if kind == Kind::Toolchain {
        resolver.toolchain(id, &version)?;
    } else {
        let root = Package {
            kind,
            id: id.into(),
            version,
        };
        let recipe = recipe(snapshot, &root, host)?;
        for dependency in &recipe.dependencies {
            resolver.toolchain(&dependency.toolchain, &dependency.version)?;
        }
        resolver.dependencies.push(
            recipe
                .dependencies
                .iter()
                .map(|d| toolchain_package(&d.toolchain, &d.version))
                .collect(),
        );
        resolver.order.push(root);
    }
    Ok(Resolution {
        packages: resolver.order,
        chip_toolchains: Vec::new(),
        dependencies: resolver.dependencies,
    })
}

pub fn environment(snapshot: &Snapshot, id: &str, host: &str) -> Result<Resolution, Blocked> {
    let entry = snapshot
        .environment(id)
        .ok_or_else(|| Blocked::Invalid(format!("目录中没有环境 {id}")))?;
    if entry.status == Status::Planned {
        return Err(Blocked::Planned);
    }
    let mut resolver = resolver(snapshot, host);
    let mut roots = Vec::new();
    for dependency in &entry.dependencies {
        resolver.toolchain(&dependency.toolchain, &dependency.version)?;
        roots.push(toolchain_package(
            &dependency.toolchain,
            &dependency.version,
        ));
    }
    Ok(Resolution {
        packages: resolver.order,
        chip_toolchains: roots,
        dependencies: resolver.dependencies,
    })
}

/// Repair only the recorded release, and refuse a recipe changed under the same version.
/// Dependencies are retained by installation ID; reinstall never resolves newer defaults.
pub fn reinstall(
    snapshot: &Snapshot,
    record: &super::store::Installation,
    host: &str,
) -> Result<Package, Blocked> {
    record.validate().map_err(Blocked::Invalid)?;
    let super::store::Origin::Managed { manifest, .. } = &record.origin else {
        return Err(Blocked::Invalid(
            "本地工具链不能重装，请管理原始文件夹".into(),
        ));
    };
    if record.platform != host {
        return Err(Blocked::UnsupportedHost(vec![record.platform.clone()]));
    }
    let package = Package {
        kind: record.kind(),
        id: record.id.clone(),
        version: record.version.clone(),
    };
    let (_, digest) = snapshot
        .release(package.kind, &package.id, &package.version)
        .ok_or_else(|| Blocked::Invalid("当前目录已不提供此安装版本".into()))?;
    if manifest.sha256 != digest {
        return Err(Blocked::Invalid(
            "此版本的安装配方已变化，无法安全重装".into(),
        ));
    }
    supported(snapshot, &package, host)?;
    Ok(package)
}

/// This version runs verified file downloads, copies, tar.gz extraction and file checks. Anything else is
/// rejected before work starts, instead of being partially executed.
pub fn supported(snapshot: &Snapshot, package: &Package, host: &str) -> Result<(), Blocked> {
    let recipe = recipe(snapshot, package, host)?;
    for source in &recipe.sources {
        if let Source::Git { .. } = source {
            return Err(Blocked::UnsupportedOperation("git".into()));
        }
    }
    for step in &recipe.install {
        if !matches!(step, Step::Copy { .. })
            && !matches!(step, Step::Extract { format, .. } if format == "tar.gz")
        {
            return Err(Blocked::UnsupportedOperation(step.name().into()));
        }
    }
    for check in &recipe.checks {
        if let Check::Command { .. } = check {
            return Err(Blocked::UnsupportedOperation("command check".into()));
        }
    }
    for program in &recipe.requires {
        if !on_path(program) {
            return Err(Blocked::MissingProgram(program.clone()));
        }
    }
    Ok(())
}

fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    let extensions: &[&str] = if cfg!(windows) {
        &["", ".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    std::env::split_paths(&path).any(|dir| {
        extensions
            .iter()
            .any(|ext| dir.join(format!("{program}{ext}")).is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::super::registry::{parse_snapshot, tests as fixtures, Kind};
    use super::*;

    const HOST: &str = "macos-arm64";

    #[test]
    fn chips_resolve_toolchains_before_their_sdk() {
        let snapshot = parse_snapshot(fixtures::COMMIT, 0, &fixtures::files()).unwrap();
        let plan = chip(&snapshot, "esp32s3", HOST).unwrap();
        let ids: Vec<_> = plan.packages.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["xtensa-esp-elf", "bazelisk", "esp-idf"]);
        assert_eq!(
            plan.chip_toolchains
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            ["xtensa-esp-elf", "bazelisk"]
        );
        assert!(supported(&snapshot, &plan.packages[0], HOST).is_ok());
        assert_eq!(
            chip(&snapshot, "esp32s3", "linux-x86_64").unwrap_err(),
            Blocked::UnsupportedHost(vec![HOST.into()])
        );
    }

    #[test]
    fn exact_versions_coexist_and_cycles_use_complete_package_identity() {
        let mut files = fixtures::environment_files();
        let path = super::super::registry::manifest_path(Kind::Toolchain, "python", "3.12.16");
        files.insert(
            path.clone(),
            fixtures::release(
                Kind::Toolchain,
                "python",
                "3.12.16",
                serde_json::json!({ HOST: fixtures::recipe(&[("python", "3.11.18")], None) }),
            ),
        );
        let snapshot = parse_snapshot(fixtures::COMMIT, 0, &files).unwrap();
        let resolution = environment(&snapshot, "matrix", HOST).unwrap();
        assert_eq!(
            resolution.packages,
            [
                toolchain_package("python", "3.11.18"),
                toolchain_package("python", "3.12.16")
            ]
        );
        assert_eq!(
            resolution.dependencies,
            [vec![], vec![toolchain_package("python", "3.11.18")]]
        );
        let specific =
            package(&snapshot, Kind::Toolchain, "python", Some("3.11.18"), HOST).unwrap();
        assert_eq!(specific.packages, [toolchain_package("python", "3.11.18")]);
        files.insert(
            super::super::registry::manifest_path(Kind::Toolchain, "python", "3.11.18"),
            fixtures::release(
                Kind::Toolchain,
                "python",
                "3.11.18",
                serde_json::json!({ HOST: fixtures::recipe(&[("python", "3.12.16")], None) }),
            ),
        );
        let snapshot = parse_snapshot(fixtures::COMMIT, 0, &files).unwrap();
        assert!(
            matches!(environment(&snapshot, "matrix", HOST), Err(Blocked::Invalid(m)) if m.contains("循环"))
        );
    }

    #[test]
    fn reinstall_requires_original_manifest_host_and_native_engine() {
        let mut files = fixtures::files();
        let snapshot = parse_snapshot(fixtures::COMMIT, 0, &files).unwrap();
        let mut record = super::super::store::tests::managed(
            1,
            Kind::Toolchain,
            "bazelisk",
            "1.30.0",
            &[],
            true,
        );
        if let super::super::store::Origin::Managed { manifest, .. } = &mut record.origin {
            manifest.sha256 = snapshot
                .release(Kind::Toolchain, "bazelisk", "1.30.0")
                .unwrap()
                .1
                .into();
        }
        assert_eq!(
            reinstall(&snapshot, &record, HOST).unwrap().version,
            "1.30.0"
        );
        assert!(matches!(
            reinstall(&snapshot, &record, "linux-x86_64"),
            Err(Blocked::UnsupportedHost(_))
        ));
        let mut local = record.clone();
        local.origin = super::super::store::Origin::Local {
            path: "/opt/user-tool".into(),
        };
        assert!(reinstall(&snapshot, &local, HOST)
            .unwrap_err()
            .message()
            .contains("本地"));
        let path = super::super::registry::manifest_path(Kind::Toolchain, "bazelisk", "1.30.0");
        let mut recipe = fixtures::recipe(&[], None);
        recipe["install"] = serde_json::json!([{ "type": "extract", "source": "tool", "to": "sdk", "format": "tar.xz", "stripComponents": 1 }]);
        recipe["checks"] =
            serde_json::json!([{ "type": "file", "path": "sdk/bin", "executable": false }]);
        files.insert(
            path,
            fixtures::release(
                Kind::Toolchain,
                "bazelisk",
                "1.30.0",
                serde_json::json!({ HOST: recipe }),
            ),
        );
        let changed = parse_snapshot(fixtures::COMMIT, 0, &files).unwrap();
        assert!(reinstall(&changed, &record, HOST)
            .unwrap_err()
            .message()
            .contains("配方已变化"));
        if let super::super::store::Origin::Managed { manifest, .. } = &mut record.origin {
            manifest.sha256 = changed
                .release(Kind::Toolchain, "bazelisk", "1.30.0")
                .unwrap()
                .1
                .into();
        }
        assert_eq!(
            reinstall(&changed, &record, HOST),
            Err(Blocked::UnsupportedOperation("extract".into()))
        );
        record.version = "9.9.9".into();
        if let super::super::store::Origin::Managed {
            manifest, prefix, ..
        } = &mut record.origin
        {
            manifest.path =
                super::super::registry::manifest_path(Kind::Toolchain, "bazelisk", "9.9.9");
            *prefix = super::super::store::managed_prefix(
                Kind::Toolchain,
                "bazelisk",
                "9.9.9",
                HOST,
                &record.installation_id,
            );
        }
        assert!(reinstall(&snapshot, &record, HOST)
            .unwrap_err()
            .message()
            .contains("不提供"));
    }

    #[test]
    fn unpublished_dependencies_and_unsupported_operations_block_installation() {
        let mut files = fixtures::files();
        let path = super::super::registry::manifest_path(Kind::Toolchain, "bazelisk", "1.30.0");
        files.insert(
            path,
            fixtures::release(
                Kind::Toolchain,
                "bazelisk",
                "1.30.0",
                serde_json::json!({ HOST: fixtures::recipe(&[("xtensa-esp-elf", "9.9.9")], None) }),
            ),
        );
        let snapshot = parse_snapshot(fixtures::COMMIT, 0, &files).unwrap();
        assert!(
            matches!(chip(&snapshot, "esp32s3", HOST), Err(Blocked::Invalid(m)) if m.contains("未发布"))
        );

        let mut files = fixtures::files();
        let path = super::super::registry::manifest_path(Kind::Toolchain, "bazelisk", "1.30.0");
        let mut recipe = fixtures::recipe(&[], None);
        recipe["install"] = serde_json::json!([{ "type": "extract", "source": "tool", "to": "sdk", "format": "tar.xz", "stripComponents": 1 }]);
        recipe["checks"] =
            serde_json::json!([{ "type": "file", "path": "sdk/bin", "executable": false }]);
        files.insert(
            path,
            fixtures::release(
                Kind::Toolchain,
                "bazelisk",
                "1.30.0",
                serde_json::json!({ HOST: recipe }),
            ),
        );
        let snapshot = parse_snapshot(fixtures::COMMIT, 0, &files).unwrap();
        let package = Package {
            kind: Kind::Toolchain,
            id: "bazelisk".into(),
            version: "1.30.0".into(),
        };
        assert_eq!(
            supported(&snapshot, &package, HOST),
            Err(Blocked::UnsupportedOperation("extract".into()))
        );
        assert_eq!(toolchain(&snapshot, "jieli", HOST), Err(Blocked::Planned));
    }
}
