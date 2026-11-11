//! Resolve what a chip or toolchain needs on this host, and whether this GizMoth can install it.
use super::registry::{Check, Kind, Recipe, Snapshot, Source, Status, Step};
use serde::Serialize;
use std::collections::BTreeMap;

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
    /// Toolchain IDs the chip uses directly: SDK common and target dependencies plus its tools.
    pub chip_toolchains: Vec<String>,
    /// Direct toolchain dependencies of each package, by package position.
    pub dependencies: Vec<Vec<String>>,
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
    versions: BTreeMap<String, String>,
    visiting: Vec<String>,
    order: Vec<Package>,
    dependencies: Vec<Vec<String>>,
}

impl Resolver<'_> {
    fn toolchain(&mut self, id: &str, version: &str) -> Result<(), Blocked> {
        if let Some(existing) = self.versions.get(id) {
            if existing != version {
                return Err(Blocked::Invalid(format!(
                    "工具链 {id} 版本冲突：{existing} 与 {version}"
                )));
            }
            if self.visiting.iter().any(|v| v == id) {
                return Err(Blocked::Invalid(format!("工具链依赖循环：{id}")));
            }
            return Ok(());
        }
        self.versions.insert(id.into(), version.into());
        let package = Package {
            kind: Kind::Toolchain,
            id: id.into(),
            version: version.into(),
        };
        let recipe = recipe(self.snapshot, &package, self.host)?;
        self.visiting.push(id.into());
        for dependency in &recipe.dependencies {
            self.toolchain(&dependency.toolchain, &dependency.version)?;
        }
        self.visiting.pop();
        self.dependencies.push(
            recipe
                .dependencies
                .iter()
                .map(|d| d.toolchain.clone())
                .collect(),
        );
        self.order.push(package);
        Ok(())
    }
}

pub fn chip(snapshot: &Snapshot, chip_id: &str, host: &str) -> Result<Resolution, Blocked> {
    let chip = snapshot
        .chip(chip_id)
        .ok_or_else(|| Blocked::Invalid(format!("目录中没有芯片 {chip_id}")))?;
    if chip.status == Status::Planned {
        return Err(Blocked::Planned);
    }
    let sdk_item = snapshot
        .item(Kind::Sdk, &chip.sdk)
        .ok_or(Blocked::Planned)?;
    let version = sdk_item.default_version.clone().ok_or(Blocked::Planned)?;
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
    let mut resolver = Resolver {
        snapshot,
        host,
        versions: BTreeMap::new(),
        visiting: Vec::new(),
        order: Vec::new(),
        dependencies: Vec::new(),
    };
    let mut chip_toolchains = Vec::new();
    for (id, version) in roots {
        resolver.toolchain(&id, &version)?;
        if !chip_toolchains.contains(&id) {
            chip_toolchains.push(id);
        }
    }
    resolver.dependencies.push(
        sdk_recipe
            .dependencies
            .iter()
            .map(|d| d.toolchain.clone())
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
    let item = snapshot
        .item(Kind::Toolchain, id)
        .ok_or_else(|| Blocked::Invalid(format!("目录中没有工具链 {id}")))?;
    let version = item.default_version.clone().ok_or(Blocked::Planned)?;
    let mut resolver = Resolver {
        snapshot,
        host,
        versions: BTreeMap::new(),
        visiting: Vec::new(),
        order: Vec::new(),
        dependencies: Vec::new(),
    };
    resolver.toolchain(id, &version)?;
    Ok(Resolution {
        packages: resolver.order,
        chip_toolchains: Vec::new(),
        dependencies: resolver.dependencies,
    })
}

/// This version runs verified file downloads, copies and file checks. Anything else is
/// rejected before work starts, instead of being partially executed.
pub fn supported(snapshot: &Snapshot, package: &Package, host: &str) -> Result<(), Blocked> {
    let recipe = recipe(snapshot, package, host)?;
    for source in &recipe.sources {
        if let Source::Git { .. } = source {
            return Err(Blocked::UnsupportedOperation("git".into()));
        }
    }
    for step in &recipe.install {
        if !matches!(step, Step::Copy { .. }) {
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
        assert_eq!(plan.chip_toolchains, ["xtensa-esp-elf", "bazelisk"]);
        assert!(supported(&snapshot, &plan.packages[0], HOST).is_ok());
        assert_eq!(
            chip(&snapshot, "esp32s3", "linux-x86_64").unwrap_err(),
            Blocked::UnsupportedHost(vec![HOST.into()])
        );
    }

    #[test]
    fn conflicts_and_unsupported_operations_block_installation() {
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
            matches!(chip(&snapshot, "esp32s3", HOST), Err(Blocked::Invalid(m)) if m.contains("冲突"))
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
