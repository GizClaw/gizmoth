//! The GizClaw/gizmoth-devenv registry: one pinned commit, fetched as a single
//! archive, validated before any of its recipes can drive local changes.
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;

type R<T> = Result<T, String>;

pub const API_VERSION: &str = "devenv.gizmoth.dev/v1alpha1";
pub const REPOSITORY: &str = "https://github.com/GizClaw/gizmoth-devenv";
const OWNER_REPO: &str = "GizClaw/gizmoth-devenv";
const BRANCH: &str = "main";
pub const PLATFORMS: [&str; 6] = [
    "macos-arm64",
    "macos-x86_64",
    "linux-arm64",
    "linux-x86_64",
    "windows-arm64",
    "windows-x86_64",
];
const MAX_ARCHIVE: usize = 8 * 1024 * 1024;
const MAX_FILE: u64 = 2 * 1024 * 1024;
const MAX_TOTAL: u64 = 16 * 1024 * 1024;

/// The platform this build runs on, in registry notation.
pub fn host_platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("macos-arm64"),
        ("macos", "x86_64") => Some("macos-x86_64"),
        ("linux", "aarch64") => Some("linux-arm64"),
        ("linux", "x86_64") => Some("linux-x86_64"),
        ("windows", "aarch64") => Some("windows-arm64"),
        ("windows", "x86_64") => Some("windows-x86_64"),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Sdk,
    Toolchain,
}
impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Sdk => "sdk",
            Kind::Toolchain => "toolchain",
        }
    }
    pub fn directory(self) -> &'static str {
        match self {
            Kind::Sdk => "sdks",
            Kind::Toolchain => "toolchains",
        }
    }
    pub fn parse(value: &str) -> R<Self> {
        match value {
            "sdk" => Ok(Kind::Sdk),
            "toolchain" => Ok(Kind::Toolchain),
            _ => Err(format!("未知的环境类型：{value}")),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Registry {
    #[serde(rename = "$schema", default)]
    _schema: Option<String>,
    api_version: String,
    kind: String,
    pub name: String,
    #[allow(dead_code)] // Parsed so the whole document is validated.
    pub description: String,
    pub repository: String,
    pub chips: Vec<String>,
    pub sdks: Vec<String>,
    pub toolchains: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Planned,
    Published,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Chip {
    #[serde(rename = "$schema", default)]
    _schema: Option<String>,
    api_version: String,
    kind: String,
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub description: String,
    pub homepage: String,
    pub status: Status,
    #[serde(default)]
    pub notes: Option<String>,
    pub sdk: String,
    pub target: String,
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseRef {
    pub version: String,
    pub manifest: String,
}

/// An SDK or toolchain entry and its version index.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Item {
    #[serde(rename = "$schema", default)]
    _schema: Option<String>,
    api_version: String,
    kind: String,
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub category: Option<String>,
    pub homepage: String,
    pub license: String,
    pub status: Status,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub default_version: Option<String>,
    pub releases: Vec<ReleaseRef>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Dependency {
    pub toolchain: String,
    pub version: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Target {
    pub dependencies: Vec<Dependency>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Source {
    File {
        id: String,
        url: String,
        sha256: String,
        size: u64,
    },
    Git {
        id: String,
        url: String,
        commit: String,
        #[allow(dead_code)] // Read once Git sources are supported.
        submodules: bool,
    },
}
impl Source {
    pub fn id(&self) -> &str {
        match self {
            Source::File { id, .. } | Source::Git { id, .. } => id,
        }
    }
}

/// Only `copy` runs in this version; the other operations are parsed and rejected before work starts.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Step {
    Copy {
        source: String,
        to: String,
        executable: bool,
    },
    Extract {
        source: String,
        to: String,
        format: String,
        #[serde(rename = "stripComponents")]
        strip_components: u32,
    },
    Checkout {
        source: String,
        to: String,
    },
    Run {
        program: String,
        args: Vec<String>,
        cwd: String,
        environment: BTreeMap<String, String>,
        #[serde(rename = "timeoutSeconds")]
        timeout_seconds: u64,
    },
}
impl Step {
    pub fn name(&self) -> &'static str {
        match self {
            Step::Copy { .. } => "copy",
            Step::Extract { .. } => "extract",
            Step::Checkout { .. } => "checkout",
            Step::Run { .. } => "run",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Environment {
    pub prepend_path: Vec<String>,
    pub variables: BTreeMap<String, String>,
}

#[allow(dead_code)] // Command checks are parsed and rejected before work starts.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Check {
    File {
        path: String,
        executable: bool,
    },
    Command {
        program: String,
        args: Vec<String>,
        #[serde(rename = "stdoutContains", default)]
        stdout_contains: Option<String>,
        #[serde(rename = "timeoutSeconds")]
        timeout_seconds: u64,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Recipe {
    pub requires: Vec<String>,
    pub dependencies: Vec<Dependency>,
    #[serde(default)]
    pub targets: Option<BTreeMap<String, Target>>,
    pub sources: Vec<Source>,
    pub install: Vec<Step>,
    pub environment: Environment,
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Release {
    #[serde(rename = "$schema", default)]
    _schema: Option<String>,
    api_version: String,
    kind: String,
    pub id: String,
    pub version: String,
    pub platforms: BTreeMap<String, Recipe>,
}

/// A validated registry commit. Releases carry the SHA-256 of their exact bytes.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub commit: String,
    pub fetched_at: u64,
    pub registry: Registry,
    pub chips: Vec<Chip>,
    pub sdks: Vec<Item>,
    pub toolchains: Vec<Item>,
    releases: HashMap<(Kind, String, String), (Release, String)>,
}

impl Snapshot {
    pub fn chip(&self, id: &str) -> Option<&Chip> {
        self.chips.iter().find(|chip| chip.id == id)
    }
    pub fn item(&self, kind: Kind, id: &str) -> Option<&Item> {
        let items = match kind {
            Kind::Sdk => &self.sdks,
            Kind::Toolchain => &self.toolchains,
        };
        items.iter().find(|item| item.id == id)
    }
    /// The release and the SHA-256 of its manifest bytes.
    pub fn release(&self, kind: Kind, id: &str, version: &str) -> Option<(&Release, &str)> {
        self.releases
            .get(&(kind, id.to_owned(), version.to_owned()))
            .map(|(release, digest)| (release, digest.as_str()))
    }
}

pub fn manifest_path(kind: Kind, id: &str, version: &str) -> String {
    format!("{}/{id}/versions/{version}.json", kind.directory())
}

fn require(condition: bool, message: impl FnOnce() -> String) -> R<()> {
    if condition {
        Ok(())
    } else {
        Err(message())
    }
}

pub fn valid_id(value: &str) -> bool {
    value.len() <= 64
        && value.starts_with(|c: char| c.is_ascii_lowercase())
        && value.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

pub fn valid_version(value: &str) -> bool {
    value.len() <= 96 && semver::Version::parse(value).is_ok()
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}
pub fn valid_sha256(value: &str) -> bool {
    hex(value, 64)
}
pub fn valid_commit(value: &str) -> bool {
    hex(value, 40)
}

pub fn valid_https(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
    })
}

/// A normalized relative path that cannot leave its root on any supported OS.
pub fn safe_relative(value: &str) -> bool {
    value.len() <= 512
        && !value.is_empty()
        && value.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && !segment.ends_with('.')
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
                && !reserved_windows_name(segment)
        })
}
fn reserved_windows_name(segment: &str) -> bool {
    let stem = segment.split('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

fn text(value: &str, field: &str, owner: &str) -> R<()> {
    require(!value.trim().is_empty() && value.len() <= 2000, || {
        format!("{owner}: {field} 无效")
    })
}

fn header(api: &str, kind: &str, expected: &str, owner: &str) -> R<()> {
    require(api == API_VERSION, || {
        format!("{owner}: 不支持的格式版本 {api}")
    })?;
    require(kind == expected, || {
        format!("{owner}: 文档类型应为 {expected}")
    })
}

fn parse<T: for<'de> Deserialize<'de>>(files: &BTreeMap<String, String>, path: &str) -> R<T> {
    let content = files
        .get(path)
        .ok_or_else(|| format!("目录缺少文件：{path}"))?;
    serde_json::from_str(content).map_err(|e| format!("{path}: {e}"))
}

fn unique(ids: &[String], what: &str) -> R<()> {
    let mut seen = BTreeSet::new();
    for id in ids {
        require(valid_id(id), || format!("{what} ID 无效：{id}"))?;
        require(seen.insert(id), || format!("{what} 重复：{id}"))?;
    }
    Ok(())
}

fn validate_item(item: &Item, kind: Kind, id: &str) -> R<()> {
    let owner = format!("{}/{id}", kind.directory());
    let expected = match kind {
        Kind::Sdk => "Sdk",
        Kind::Toolchain => "Toolchain",
    };
    header(&item.api_version, &item.kind, expected, &owner)?;
    require(item.id == id, || format!("{owner}: ID 不匹配"))?;
    text(&item.name, "name", &owner)?;
    text(&item.description, "description", &owner)?;
    text(&item.license, "license", &owner)?;
    require(valid_https(&item.homepage), || {
        format!("{owner}: homepage 无效")
    })?;
    match kind {
        Kind::Sdk => require(item.category.is_none(), || {
            format!("{owner}: SDK 不能声明 category")
        })?,
        Kind::Toolchain => require(
            item.category.as_deref().is_some_and(|c| {
                matches!(
                    c,
                    "compiler" | "build-tool" | "post-build" | "debugger" | "utility"
                )
            }),
            || format!("{owner}: category 无效"),
        )?,
    }
    let mut versions = BTreeSet::new();
    for reference in &item.releases {
        require(valid_version(&reference.version), || {
            format!("{owner}: 版本无效 {}", reference.version)
        })?;
        require(versions.insert(reference.version.as_str()), || {
            format!("{owner}: 版本重复 {}", reference.version)
        })?;
        require(
            reference.manifest == format!("versions/{}.json", reference.version),
            || format!("{owner}: 版本路径不匹配"),
        )?;
    }
    match item.status {
        Status::Planned => require(
            item.releases.is_empty() && item.default_version.is_none(),
            || format!("{owner}: 待补条目不能发布版本"),
        ),
        Status::Published => require(
            item.default_version
                .as_deref()
                .is_some_and(|version| versions.contains(version)),
            || format!("{owner}: 默认版本未发布"),
        ),
    }
}

fn validate_dependencies(dependencies: &[Dependency], owner: &str) -> R<()> {
    let mut seen = BTreeSet::new();
    for dependency in dependencies {
        require(valid_id(&dependency.toolchain), || {
            format!("{owner}: 依赖 ID 无效")
        })?;
        require(valid_version(&dependency.version), || {
            format!("{owner}: 依赖版本无效")
        })?;
        require(seen.insert(&dependency.toolchain), || {
            format!("{owner}: 依赖重复 {}", dependency.toolchain)
        })?;
    }
    Ok(())
}

fn validate_recipe(recipe: &Recipe, kind: Kind, owner: &str) -> R<()> {
    for program in &recipe.requires {
        require(
            !program.is_empty()
                && program
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-')),
            || format!("{owner}: 主机程序名无效"),
        )?;
    }
    validate_dependencies(&recipe.dependencies, owner)?;
    match (kind, &recipe.targets) {
        (Kind::Sdk, Some(targets)) if !targets.is_empty() => {
            for (target, extra) in targets {
                require(valid_id(target), || format!("{owner}: target 无效"))?;
                validate_dependencies(&extra.dependencies, owner)?;
            }
        }
        (Kind::Sdk, _) => return Err(format!("{owner}: SDK 版本必须声明芯片 target")),
        (Kind::Toolchain, None) => {}
        (Kind::Toolchain, Some(_)) => return Err(format!("{owner}: 工具链版本不能声明 target")),
    }
    require(!recipe.sources.is_empty(), || format!("{owner}: 缺少来源"))?;
    let mut sources = HashMap::new();
    for source in &recipe.sources {
        require(valid_id(source.id()), || format!("{owner}: 来源 ID 无效"))?;
        require(sources.insert(source.id(), source).is_none(), || {
            format!("{owner}: 来源重复 {}", source.id())
        })?;
        match source {
            Source::File {
                url, sha256, size, ..
            } => {
                require(valid_https(url), || format!("{owner}: 下载地址无效"))?;
                require(
                    valid_sha256(sha256) && sha256.chars().any(|c| c != '0'),
                    || format!("{owner}: SHA-256 无效"),
                )?;
                require(*size > 0, || format!("{owner}: 文件大小无效"))?;
            }
            Source::Git { url, commit, .. } => {
                require(valid_https(url), || format!("{owner}: Git 地址无效"))?;
                require(
                    valid_commit(commit) && commit.chars().any(|c| c != '0'),
                    || format!("{owner}: Git 提交无效"),
                )?;
            }
        }
    }
    require(!recipe.install.is_empty(), || {
        format!("{owner}: 缺少安装步骤")
    })?;
    let mut destinations = BTreeSet::new();
    for step in &recipe.install {
        let (source, to, git) = match step {
            Step::Copy { source, to, .. } => (source, to, false),
            Step::Extract {
                source,
                to,
                format,
                strip_components,
            } => {
                require(
                    matches!(
                        format.as_str(),
                        "zip" | "tar.gz" | "tar.xz" | "tar.bz2" | "tar.zst"
                    ) && *strip_components <= 16,
                    || format!("{owner}: 解压参数无效"),
                )?;
                (source, to, false)
            }
            Step::Checkout { source, to } => (source, to, true),
            Step::Run {
                cwd,
                timeout_seconds,
                ..
            } => {
                require(
                    safe_relative(cwd) && (1..=7200).contains(timeout_seconds),
                    || format!("{owner}: 运行步骤无效"),
                )?;
                continue;
            }
        };
        require(safe_relative(to), || {
            format!("{owner}: 安装路径不安全 {to}")
        })?;
        require(destinations.insert(to.to_ascii_lowercase()), || {
            format!("{owner}: 安装路径重复 {to}")
        })?;
        let source = sources
            .get(source.as_str())
            .ok_or_else(|| format!("{owner}: 未声明的来源 {source}"))?;
        require(matches!(source, Source::Git { .. }) == git, || {
            format!("{owner}: 来源类型与步骤不匹配")
        })?;
    }
    for path in &recipe.environment.prepend_path {
        require(safe_relative(path), || format!("{owner}: PATH 条目不安全"))?;
    }
    for name in recipe.environment.variables.keys() {
        require(
            !name.eq_ignore_ascii_case("path")
                && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            || format!("{owner}: 环境变量名无效 {name}"),
        )?;
    }
    require(!recipe.checks.is_empty(), || format!("{owner}: 缺少检查"))?;
    for check in &recipe.checks {
        match check {
            Check::File { path, .. } => {
                require(safe_relative(path), || format!("{owner}: 检查路径不安全"))?
            }
            Check::Command {
                timeout_seconds, ..
            } => require((1..=60).contains(timeout_seconds), || {
                format!("{owner}: 检查超时无效")
            })?,
        }
    }
    Ok(())
}

/// Validate every indexed document. Unindexed entries under chips/, sdks/ and toolchains/ are invalid.
pub fn parse_snapshot(
    commit: &str,
    fetched_at: u64,
    files: &BTreeMap<String, String>,
) -> R<Snapshot> {
    require(valid_commit(commit), || "目录提交无效".into())?;
    let registry: Registry = parse(files, "registry.json")?;
    header(
        &registry.api_version,
        &registry.kind,
        "Registry",
        "registry.json",
    )?;
    require(registry.repository == REPOSITORY, || {
        format!("目录来源不匹配：{}", registry.repository)
    })?;
    text(&registry.name, "name", "registry.json")?;
    unique(&registry.chips, "芯片")?;
    unique(&registry.sdks, "SDK")?;
    unique(&registry.toolchains, "工具链")?;

    let mut expected: BTreeSet<String> = BTreeSet::from(["registry.json".to_owned()]);
    let mut releases = HashMap::new();
    let mut items = |kind: Kind, ids: &[String]| -> R<Vec<Item>> {
        let mut result = Vec::new();
        for id in ids {
            let path = format!("{}/{id}/{}.json", kind.directory(), kind.as_str());
            let item: Item = parse(files, &path)?;
            expected.insert(path);
            validate_item(&item, kind, id)?;
            for reference in &item.releases {
                let path = manifest_path(kind, id, &reference.version);
                let release: Release = parse(files, &path)?;
                let owner = format!("{id}@{}", reference.version);
                header(
                    &release.api_version,
                    &release.kind,
                    match kind {
                        Kind::Sdk => "SdkRelease",
                        Kind::Toolchain => "ToolchainRelease",
                    },
                    &owner,
                )?;
                require(
                    release.id == *id && release.version == reference.version,
                    || format!("{owner}: 版本标识不匹配"),
                )?;
                require(!release.platforms.is_empty(), || {
                    format!("{owner}: 缺少平台")
                })?;
                for (platform, recipe) in &release.platforms {
                    require(PLATFORMS.contains(&platform.as_str()), || {
                        format!("{owner}: 未知平台 {platform}")
                    })?;
                    validate_recipe(recipe, kind, &format!("{owner}:{platform}"))?;
                }
                let digest = hex_digest(files[&path].as_bytes());
                expected.insert(path);
                releases.insert(
                    (kind, id.clone(), reference.version.clone()),
                    (release, digest),
                );
            }
            result.push(item);
        }
        Ok(result)
    };
    let sdks = items(Kind::Sdk, &registry.sdks)?;
    let toolchains = items(Kind::Toolchain, &registry.toolchains)?;

    let mut chips = Vec::new();
    let mut targets = BTreeSet::new();
    for id in &registry.chips {
        let path = format!("chips/{id}/chip.json");
        let chip: Chip = parse(files, &path)?;
        expected.insert(path);
        let owner = format!("chips/{id}");
        header(&chip.api_version, &chip.kind, "Chip", &owner)?;
        require(chip.id == *id, || format!("{owner}: ID 不匹配"))?;
        for (value, field) in [
            (&chip.name, "name"),
            (&chip.vendor, "vendor"),
            (&chip.description, "description"),
        ] {
            text(value, field, &owner)?;
        }
        require(valid_https(&chip.homepage), || {
            format!("{owner}: homepage 无效")
        })?;
        require(valid_id(&chip.target), || format!("{owner}: target 无效"))?;
        let sdk = sdks
            .iter()
            .find(|sdk| sdk.id == chip.sdk)
            .ok_or_else(|| format!("{owner}: 未知 SDK {}", chip.sdk))?;
        require(
            targets.insert(format!("{}/{}", chip.sdk, chip.target)),
            || format!("{owner}: SDK target 重复"),
        )?;
        let mut tools = BTreeSet::new();
        for tool in &chip.tools {
            let toolchain = toolchains
                .iter()
                .find(|item| item.id == *tool)
                .ok_or_else(|| format!("{owner}: 未知工具链 {tool}"))?;
            require(tools.insert(tool), || format!("{owner}: 工具重复 {tool}"))?;
            if chip.status == Status::Published {
                require(toolchain.status == Status::Published, || {
                    format!("{owner}: 工具链 {tool} 未发布")
                })?;
            }
        }
        if chip.status == Status::Published {
            require(sdk.status == Status::Published, || {
                format!("{owner}: SDK {} 未发布", chip.sdk)
            })?;
        }
        chips.push(chip);
    }
    for path in files.keys() {
        require(expected.contains(path), || {
            format!("目录包含未索引的文件：{path}")
        })?;
    }
    Ok(Snapshot {
        commit: commit.to_owned(),
        fetched_at,
        registry,
        chips,
        sdks,
        toolchains,
        releases,
    })
}

pub fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn client() -> R<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .user_agent(concat!("GizMoth/", env!("CARGO_PKG_VERSION")))
        // Release downloads redirect to storage hosts; only HTTPS hops are followed.
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.url().scheme() == "https" && attempt.previous().len() < 10 {
                attempt.follow()
            } else {
                attempt.error("只允许 HTTPS 跳转")
            }
        }))
        .build()
        .map_err(|e| format!("无法创建网络请求：{e}"))
}

async fn bytes_limited(response: reqwest::Response, limit: usize) -> R<Vec<u8>> {
    let mut response = response
        .error_for_status()
        .map_err(|e| format!("下载目录失败：{e}"))?;
    let mut bytes = Vec::new();
    while let Some(chunk) =
        tokio::time::timeout(std::time::Duration::from_secs(60), response.chunk())
            .await
            .map_err(|_| "下载目录超时".to_owned())?
            .map_err(|e| format!("下载目录失败：{e}"))?
    {
        require(bytes.len() + chunk.len() <= limit, || "目录过大".into())?;
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Resolve the branch to one commit, then read every registry file from that commit's archive.
pub async fn fetch() -> R<(String, BTreeMap<String, String>)> {
    let client = client()?;
    let commit = bytes_limited(
        client
            .get(format!(
                "https://api.github.com/repos/{OWNER_REPO}/commits/{BRANCH}"
            ))
            .header(reqwest::header::ACCEPT, "application/vnd.github.sha")
            .send()
            .await
            .map_err(|e| format!("无法连接 GitHub：{e}"))?,
        128,
    )
    .await?;
    let commit = String::from_utf8_lossy(&commit).trim().to_owned();
    require(valid_commit(&commit), || "GitHub 返回的提交无效".into())?;
    let archive = bytes_limited(
        client
            .get(format!(
                "https://codeload.github.com/{OWNER_REPO}/tar.gz/{commit}"
            ))
            .send()
            .await
            .map_err(|e| format!("无法下载目录：{e}"))?,
        MAX_ARCHIVE,
    )
    .await?;
    let files = tauri::async_runtime::spawn_blocking(move || extract(&archive))
        .await
        .map_err(|_| "解析目录失败".to_owned())??;
    Ok((commit, files))
}

/// Keep regular JSON files under the registry paths; any other entry there is rejected.
pub fn extract(archive: &[u8]) -> R<BTreeMap<String, String>> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let mut files = BTreeMap::new();
    let mut root: Option<String> = None;
    let mut total = 0u64;
    for entry in tar.entries().map_err(|_| "目录归档无效")? {
        let mut entry = entry.map_err(|_| "目录归档无效")?;
        let entry_type = entry.header().entry_type();
        if entry_type.is_pax_global_extensions() {
            continue;
        }
        let path = entry.path().map_err(|_| "目录归档路径无效")?;
        let path = path
            .to_str()
            .ok_or("目录归档路径无效")?
            .trim_end_matches('/')
            .to_owned();
        let (first, rest) = path.split_once('/').unwrap_or((path.as_str(), ""));
        match &root {
            None => root = Some(first.to_owned()),
            Some(root) => require(root == first, || "目录归档结构无效".into())?,
        }
        if rest.is_empty() {
            continue;
        }
        let relevant = rest == "registry.json"
            || ["chips/", "sdks/", "toolchains/"]
                .iter()
                .any(|dir| rest.starts_with(dir));
        if !relevant
            || rest.split('/').any(|segment| segment.starts_with('.'))
            || entry_type.is_dir()
        {
            continue;
        }
        require(entry_type.is_file() && rest.ends_with(".json"), || {
            format!("目录包含非 JSON 条目：{rest}")
        })?;
        require(safe_relative(rest), || format!("目录路径不安全：{rest}"))?;
        let size = entry.size();
        total += size;
        require(size <= MAX_FILE && total <= MAX_TOTAL, || {
            "目录文件过大".into()
        })?;
        let mut content = String::new();
        entry
            .read_to_string(&mut content)
            .map_err(|_| format!("目录文件不是 UTF-8：{rest}"))?;
        files.insert(rest.to_owned(), content);
    }
    Ok(files)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const COMMIT: &str = "4e8027152adf7d9e6da9dcff9760b81d6e53bbc3";

    fn item(kind: Kind, id: &str, status: &str, versions: &[&str]) -> String {
        let releases: Vec<_> = versions
            .iter()
            .map(|v| serde_json::json!({ "version": v, "manifest": format!("versions/{v}.json") }))
            .collect();
        let mut value = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": if kind == Kind::Sdk { "Sdk" } else { "Toolchain" },
            "id": id, "name": id, "description": "test", "homepage": "https://example.com",
            "license": "MIT", "status": status, "releases": releases,
        });
        if kind == Kind::Toolchain {
            value["category"] = "compiler".into();
        }
        if let Some(version) = versions.first() {
            value["defaultVersion"] = (*version).into();
        }
        value.to_string()
    }

    pub fn recipe(dependencies: &[(&str, &str)], targets: Option<&[&str]>) -> serde_json::Value {
        let dependencies: Vec<_> = dependencies
            .iter()
            .map(|(t, v)| serde_json::json!({ "toolchain": t, "version": v }))
            .collect();
        let mut value = serde_json::json!({
            "requires": [], "dependencies": dependencies,
            "sources": [{ "id": "tool", "type": "file", "url": "https://example.com/tool", "sha256": "a".repeat(64), "size": 4 }],
            "install": [{ "type": "copy", "source": "tool", "to": "bin/tool", "executable": true }],
            "environment": { "prependPath": ["bin"], "variables": {} },
            "checks": [{ "type": "file", "path": "bin/tool", "executable": true }],
        });
        if let Some(targets) = targets {
            value["targets"] = targets
                .iter()
                .map(|t| (t.to_string(), serde_json::json!({ "dependencies": [] })))
                .collect::<serde_json::Map<_, _>>()
                .into();
        }
        value
    }

    pub fn release(kind: Kind, id: &str, version: &str, platforms: serde_json::Value) -> String {
        serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": if kind == Kind::Sdk { "SdkRelease" } else { "ToolchainRelease" },
            "id": id, "version": version, "platforms": platforms,
        })
        .to_string()
    }

    /// esp32s3 on esp-idf 6.2.0 with xtensa 14.2.0 and bazelisk 1.30.0, all for macos-arm64.
    pub fn files() -> BTreeMap<String, String> {
        let host = "macos-arm64";
        let mut files = BTreeMap::new();
        files.insert(
            "registry.json".into(),
            serde_json::json!({
                "apiVersion": API_VERSION, "kind": "Registry", "name": "Test", "description": "Test",
                "repository": REPOSITORY, "chips": ["esp32s3"], "sdks": ["esp-idf"],
                "toolchains": ["bazelisk", "xtensa-esp-elf", "jieli"],
            })
            .to_string(),
        );
        files.insert(
            "chips/esp32s3/chip.json".into(),
            serde_json::json!({
                "apiVersion": API_VERSION, "kind": "Chip", "id": "esp32s3", "name": "ESP32-S3",
                "vendor": "Espressif", "description": "Test", "homepage": "https://example.com",
                "status": "published", "sdk": "esp-idf", "target": "esp32s3", "tools": ["bazelisk"],
            })
            .to_string(),
        );
        files.insert(
            "sdks/esp-idf/sdk.json".into(),
            item(Kind::Sdk, "esp-idf", "published", &["6.2.0"]),
        );
        let mut sdk = recipe(&[], Some(&["esp32s3"]));
        sdk["targets"]["esp32s3"]["dependencies"] =
            serde_json::json!([{ "toolchain": "xtensa-esp-elf", "version": "14.2.0" }]);
        files.insert(
            manifest_path(Kind::Sdk, "esp-idf", "6.2.0"),
            release(
                Kind::Sdk,
                "esp-idf",
                "6.2.0",
                serde_json::json!({ host: sdk }),
            ),
        );
        for (id, version) in [("bazelisk", "1.30.0"), ("xtensa-esp-elf", "14.2.0")] {
            files.insert(
                format!("toolchains/{id}/toolchain.json"),
                item(Kind::Toolchain, id, "published", &[version]),
            );
            files.insert(
                manifest_path(Kind::Toolchain, id, version),
                release(
                    Kind::Toolchain,
                    id,
                    version,
                    serde_json::json!({ host: recipe(&[], None) }),
                ),
            );
        }
        files.insert(
            "toolchains/jieli/toolchain.json".into(),
            item(Kind::Toolchain, "jieli", "planned", &[]),
        );
        files
    }

    #[test]
    fn valid_snapshot_parses_with_manifest_digests() {
        let files = files();
        let snapshot = parse_snapshot(COMMIT, 0, &files).unwrap();
        let (release, digest) = snapshot.release(Kind::Sdk, "esp-idf", "6.2.0").unwrap();
        assert_eq!(release.platforms.len(), 1);
        assert_eq!(
            digest,
            hex_digest(files["sdks/esp-idf/versions/6.2.0.json"].as_bytes())
        );
    }

    #[test]
    fn rejects_unindexed_files_foreign_origins_and_unsafe_recipes() {
        let mut extra = files();
        extra.insert(
            "chips/esp32c3/chip.json".into(),
            extra["chips/esp32s3/chip.json"].clone(),
        );
        assert!(parse_snapshot(COMMIT, 0, &extra)
            .unwrap_err()
            .contains("未索引"));

        let mut foreign = files();
        foreign.insert(
            "registry.json".into(),
            foreign["registry.json"].replace(REPOSITORY, "https://github.com/someone/else"),
        );
        assert!(parse_snapshot(COMMIT, 0, &foreign)
            .unwrap_err()
            .contains("来源不匹配"));

        for (from, to) in [
            ("\"bin/tool\"", "\"../outside\""),
            (
                "https://example.com/tool",
                "https://user:pass@example.com/tool",
            ),
            (&"a".repeat(64), &"0".repeat(64)),
        ] {
            let mut unsafe_files = files();
            let path = manifest_path(Kind::Toolchain, "bazelisk", "1.30.0");
            unsafe_files.insert(path.clone(), unsafe_files[&path].replacen(from, to, 1));
            assert!(parse_snapshot(COMMIT, 0, &unsafe_files).is_err(), "{to}");
        }

        let mut unknown = files();
        let path = manifest_path(Kind::Toolchain, "bazelisk", "1.30.0");
        unknown.insert(
            path.clone(),
            unknown[&path].replacen("\"checks\"", "\"uninstall\":[],\"checks\"", 1),
        );
        assert!(parse_snapshot(COMMIT, 0, &unknown).is_err());
    }

    #[test]
    fn published_chips_need_published_tools() {
        let mut files = files();
        files.insert(
            "chips/esp32s3/chip.json".into(),
            files["chips/esp32s3/chip.json"].replace("[\"bazelisk\"]", "[\"jieli\"]"),
        );
        assert!(parse_snapshot(COMMIT, 0, &files)
            .unwrap_err()
            .contains("未发布"));
    }

    /// `cargo test --lib devenv::registry::tests::live -- --ignored` fetches and validates the published registry.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "network"]
    async fn live_registry_validates() {
        let (commit, files) = fetch().await.unwrap();
        let snapshot = parse_snapshot(&commit, 0, &files).unwrap();
        let (release, _) = snapshot
            .release(Kind::Toolchain, "bazelisk", "1.30.0")
            .unwrap();
        assert_eq!(release.platforms.len(), 6);
        println!(
            "{commit}: {} chips, {} SDKs, {} toolchains",
            snapshot.chips.len(),
            snapshot.sdks.len(),
            snapshot.toolchains.len()
        );
    }

    fn archive(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, kind, content) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(*kind);
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            if *kind == tar::EntryType::Symlink {
                header.set_link_name("../../registry.json").unwrap();
            }
            header.set_cksum();
            builder.append_data(&mut header, path, *content).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn archives_keep_only_registry_json_and_reject_links() {
        let file = tar::EntryType::Regular;
        let files = extract(&archive(&[
            ("repo-abc/README.md", file, b"readme"),
            ("repo-abc/registry.json", file, b"{}"),
            ("repo-abc/chips/esp32s3/chip.json", file, b"{}"),
            ("repo-abc/chips/.DS_Store", file, b"x"),
            ("repo-abc/scripts/validate.mjs", file, b"x"),
        ]))
        .unwrap();
        assert_eq!(
            files.keys().collect::<Vec<_>>(),
            ["chips/esp32s3/chip.json", "registry.json"]
        );
        assert!(extract(&archive(&[(
            "repo-abc/chips/a/chip.json",
            tar::EntryType::Symlink,
            b""
        )]))
        .is_err());
        assert!(extract(&archive(&[("repo-abc/sdks/a/run.sh", file, b"x")])).is_err());
        assert!(extract(&archive(&[
            ("a/registry.json", file, b"{}"),
            ("b/registry.json", file, b"{}")
        ]))
        .is_err());
    }

    #[test]
    fn relative_paths_cannot_escape_on_any_platform() {
        for path in [
            "../sdk",
            "/tmp/sdk",
            "C:/sdk",
            "bin\\bazel.exe",
            "bin//sdk",
            "CON",
            "bin/nul.exe",
            "bin/tool.",
            "bin/com1",
        ] {
            assert!(!safe_relative(path), "{path}");
        }
        assert!(safe_relative("sdk/.venv/bin/python3"));
        assert!(safe_relative("bin/compiler"));
    }
}
