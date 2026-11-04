//! Lua projects in user-selected directories, plus the original app-owned projects.
//! External registrations and conversations stay in app data. Source paths are
//! checked component by component; links inside a project are never followed.

use crate::project::{self, Project, SourceFile};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::UNIX_EPOCH,
};
use tauri::Manager;

const MAX_PROJECT_BYTES: u64 = 2 * 1024 * 1024 + 32 * 1024;
const MAX_FILES: usize = 65;
const MAX_CHAT_BYTES: usize = 8 * 1024 * 1024;
const MAX_CONFIG_BYTES: u64 = 32 * 1024;

/// Serializes registry updates, project operations, and conversation writes.
#[derive(Default)]
pub struct WorkspaceStore(pub Mutex<()>);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    pub app_id: Option<String>,
    pub version: Option<String>,
    pub updated_at: u64,
    pub files: usize,
    pub directory: String,
    pub external: bool,
    pub available: bool,
}

#[derive(Serialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct DirectoryRegistry {
    #[serde(default)]
    projects: Vec<DirectoryRegistration>,
    // Cache identity before a legacy directory is moved outside the app.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    legacy_projects: Vec<DirectoryRegistration>,
    // A relocated legacy directory must not reappear after unregistering its
    // replacement, even if the old directory still exists on disk.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    retired_legacy_ids: Vec<String>,
}

#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryRegistration {
    id: String,
    directory: String,
    name: String,
    app_id: Option<String>,
    version: Option<String>,
    #[serde(default)]
    updated_at: u64,
    #[serde(default)]
    files: usize,
}

impl DirectoryRegistration {
    fn from_summary(value: &ProjectSummary) -> Self {
        Self {
            id: value.id.clone(),
            directory: value.directory.clone(),
            name: value.name.clone(),
            app_id: value.app_id.clone(),
            version: value.version.clone(),
            updated_at: value.updated_at,
            files: value.files,
        }
    }
}

#[derive(Clone)]
struct WorkspacePaths {
    data: PathBuf,
}

impl WorkspacePaths {
    fn new(data: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&data).map_err(|_| "无法创建应用数据目录")?;
        Ok(Self {
            data: canonical_directory(&data)?,
        })
    }

    fn root(&self, kind: &str) -> Result<PathBuf, String> {
        let dir = self.data.join(kind);
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err("无法创建应用数据目录".into()),
        }
        checked_directory(&dir)?;
        Ok(dir)
    }

    fn registry_path(&self) -> PathBuf {
        self.data.join("project-directories.json")
    }
}

fn with_workspace<T>(
    app: &tauri::AppHandle,
    operation: impl FnOnce(&WorkspacePaths, &mut DirectoryRegistry) -> Result<T, String>,
) -> Result<T, String> {
    let state = app
        .try_state::<WorkspaceStore>()
        .ok_or("项目存储尚未初始化")?;
    let _guard = state.0.lock().map_err(|_| "项目存储锁不可用")?;
    let paths = WorkspacePaths::new(
        app.path()
            .app_data_dir()
            .map_err(|_| "无法定位应用数据目录")?,
    )?;
    let mut registry = read_registry(&paths)?;
    operation(&paths, &mut registry)
}

fn valid_project_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

pub fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() < 192
        && !path
            .chars()
            .any(|c| c.is_control() || c == '\\' || c == ':')
        && path
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.'))
}

/// Stored roots are already canonical. Reject replacement links in the root or
/// any of its ancestors rather than resolving them to a different directory.
fn checked_directory(dir: &Path) -> Result<(), String> {
    if !dir.is_absolute() {
        return Err("项目目录必须是绝对路径".into());
    }
    let mut current = PathBuf::new();
    for component in dir.components() {
        if matches!(component, Component::ParentDir | Component::CurDir) {
            return Err("项目目录路径无效".into());
        }
        current.push(component.as_os_str());
        // A Windows drive/UNC prefix is not a complete filesystem path until
        // the following root component has been appended.
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        let meta = fs::symlink_metadata(&current).map_err(|_| "项目目录不存在或无法访问")?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err("项目目录不能包含符号链接或非目录路径".into());
        }
    }
    Ok(())
}

fn canonical_directory(dir: &Path) -> Result<PathBuf, String> {
    let dir = fs::canonicalize(dir).map_err(|_| "目录不存在或无法访问")?;
    checked_directory(&dir)?;
    if dir.to_str().is_none() {
        return Err("目录路径不是有效的 UTF-8 文本".into());
    }
    Ok(dir)
}

/// Check every existing component, including the leaf. Missing components are
/// permitted for new files, but directory creation checks each component again.
fn file_path(dir: &Path, path: &str) -> Result<PathBuf, String> {
    if !valid_path(path) {
        return Err(format!("文件路径无效：{path}"));
    }
    checked_directory(dir)?;
    let mut target = dir.to_path_buf();
    let mut parts = path.split('/').peekable();
    while let Some(part) = parts.next() {
        target.push(part);
        match fs::symlink_metadata(&target) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(format!("项目路径不能包含符号链接：{path}"));
                }
                if parts.peek().is_some() && !meta.is_dir() {
                    return Err(format!("文件的父路径不是目录：{path}"));
                }
                if !meta.is_dir() && !meta.is_file() {
                    return Err(format!("项目路径不是普通文件或目录：{path}"));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(format!("无法访问项目路径：{path}")),
        }
    }
    Ok(target)
}

fn ensure_parents(dir: &Path, path: &str) -> Result<(), String> {
    let mut relative = String::new();
    let mut parts = path.split('/').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            break;
        }
        if !relative.is_empty() {
            relative.push('/');
        }
        relative.push_str(part);
        let parent = file_path(dir, &relative)?;
        match fs::create_dir(&parent) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err("无法创建项目子目录".into()),
        }
        file_path(dir, &relative)?;
        if !fs::symlink_metadata(&parent)
            .map_err(|_| "无法读取项目子目录")?
            .is_dir()
        {
            return Err("文件的父路径不是目录".into());
        }
    }
    Ok(())
}

fn read_text(dir: &Path, path: &str, limit: u64) -> Result<String, String> {
    let target = file_path(dir, path)?;
    let file = fs::File::open(target).map_err(|_| "文件不存在或无法读取")?;
    let meta = file.metadata().map_err(|_| "无法读取文件信息")?;
    if !meta.is_file() || meta.len() > limit {
        return Err("只能读取大小限制内的普通文件".into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "无法读取文件")?;
    if bytes.len() as u64 > limit {
        return Err("文件超过大小限制".into());
    }
    String::from_utf8(bytes).map_err(|_| "文件不是 UTF-8 文本".into())
}

/// Unique, exclusively created temporary files cannot overwrite user files or
/// reuse an existing link. The final rename atomically replaces the destination.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("文件目录无效")?;
    checked_directory(parent)?;
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
            return Err("目标不是普通文件，或是符号链接".into());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("无法访问目标文件".into()),
    }
    let temporary = parent.join(format!(".gizmoth-{}.tmp", uuid::Uuid::new_v4()));
    let mut created = false;
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|_| "无法创建临时文件")?;
        created = true;
        file.write_all(bytes).map_err(|_| "无法写入文件")?;
        file.sync_all().map_err(|_| "无法保存文件")?;
        drop(file);
        checked_directory(parent)?;
        fs::rename(&temporary, path).map_err(|_| "无法替换文件".to_string())
    })();
    if result.is_err() && created {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn read_registry(paths: &WorkspacePaths) -> Result<DirectoryRegistry, String> {
    let path = paths.registry_path();
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DirectoryRegistry::default());
        }
        Err(_) => return Err("无法读取项目目录登记".into()),
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
            return Err("项目目录登记不是普通文件".into());
        }
        Ok(_) => {}
    }
    let registry: DirectoryRegistry = serde_json::from_str(&read_text(
        &paths.data,
        "project-directories.json",
        MAX_CHAT_BYTES as u64,
    )?)
    .map_err(|_| "项目目录登记损坏，请保留 project-directories.json 以便恢复")?;
    let mut ids = HashSet::new();
    let mut directories = HashSet::new();
    for entry in registry.projects.iter().chain(&registry.legacy_projects) {
        let directory = Path::new(&entry.directory);
        if !valid_project_id(&entry.id)
            || !directory.is_absolute()
            || directory
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
            || !ids.insert(&entry.id)
            || !directories.insert(directory)
        {
            return Err("项目目录登记包含无效或重复的 ID / 路径".into());
        }
    }
    if registry
        .legacy_projects
        .iter()
        .any(|entry| Path::new(&entry.directory) != paths.data.join("projects").join(&entry.id))
    {
        return Err("旧项目目录登记超出了应用数据目录".into());
    }
    if registry
        .retired_legacy_ids
        .iter()
        .any(|id| !valid_project_id(id))
    {
        return Err("项目目录登记包含无效的旧项目 ID".into());
    }
    Ok(registry)
}

fn save_registry(paths: &WorkspacePaths, registry: &DirectoryRegistry) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(registry).map_err(|_| "无法编码项目目录登记")?;
    atomic_write(&paths.registry_path(), &bytes).map_err(|e| format!("无法保存项目目录登记：{e}"))
}

fn project_dir(
    paths: &WorkspacePaths,
    registry: &DirectoryRegistry,
    id: &str,
) -> Result<PathBuf, String> {
    if !valid_project_id(id) {
        return Err("项目 ID 无效".into());
    }
    if let Some(entry) = registry.projects.iter().find(|entry| entry.id == id) {
        let dir = PathBuf::from(&entry.directory);
        checked_directory(&dir).map_err(|_| {
            format!(
                "项目目录不可用：{}。请重新定位目录或移除登记。",
                entry.directory
            )
        })?;
        return Ok(dir);
    }
    if registry
        .retired_legacy_ids
        .iter()
        .any(|retired| retired == id)
    {
        return Err("项目未登记".into());
    }
    let dir = paths.root("projects")?.join(id);
    checked_directory(&dir).map_err(|_| "旧项目目录不存在或不可用，请重新定位目录")?;
    Ok(dir)
}

fn known_project(
    paths: &WorkspacePaths,
    registry: &DirectoryRegistry,
    id: &str,
) -> Result<(), String> {
    if !valid_project_id(id) {
        return Err("项目 ID 无效".into());
    }
    if registry.projects.iter().any(|entry| entry.id == id)
        || (registry.legacy_projects.iter().any(|entry| entry.id == id)
            && !registry
                .retired_legacy_ids
                .iter()
                .any(|retired| retired == id))
    {
        return Ok(());
    }
    project_dir(paths, registry, id).map(|_| ())
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<FileEntry>) -> Result<(), String> {
    let current = if prefix.is_empty() {
        checked_directory(dir)?;
        dir.to_path_buf()
    } else {
        file_path(dir, prefix)?
    };
    let mut entries = fs::read_dir(current)
        .map_err(|_| "无法读取项目目录")?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "无法读取项目目录项")?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let filename = entry.file_name();
        let name = filename.to_str().ok_or("项目文件名不是有效的 UTF-8 文本")?;
        if name.starts_with('.') {
            continue;
        }
        let path = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        let target = file_path(dir, &path)?;
        let meta = fs::symlink_metadata(target).map_err(|_| "无法读取项目文件信息")?;
        if meta.is_dir() {
            walk(dir, &path, out)?;
        } else if meta.is_file() {
            out.push(FileEntry {
                path,
                size: meta.len(),
            });
        }
        if out.len() > MAX_FILES {
            return Err("每个项目最多 65 个文件".into());
        }
    }
    Ok(())
}

fn summary(
    dir: &Path,
    id: String,
    external: bool,
    stored: Option<&DirectoryRegistration>,
) -> ProjectSummary {
    let available = checked_directory(dir).is_ok();
    let meta = if available {
        read_config(dir).ok()
    } else {
        None
    };
    let mut list = vec![];
    if available {
        let _ = walk(dir, "", &mut list);
    }
    let updated_at = if available {
        list.iter()
            .filter_map(|entry| {
                let path = file_path(dir, &entry.path).ok()?;
                fs::symlink_metadata(path).ok()?.modified().ok()
            })
            .chain(
                fs::symlink_metadata(dir)
                    .ok()
                    .and_then(|m| m.modified().ok()),
            )
            .filter_map(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as u64)
            .max()
            .unwrap_or(0)
    } else {
        stored.map(|entry| entry.updated_at).unwrap_or(0)
    };
    let (app_id, version, name) = match meta {
        Some(meta) => (
            Some(meta.app_id.clone()),
            Some(meta.version),
            meta.display_name.unwrap_or(meta.app_id),
        ),
        None => (
            stored.and_then(|entry| entry.app_id.clone()),
            stored.and_then(|entry| entry.version.clone()),
            stored
                .map(|entry| entry.name.clone())
                .unwrap_or_else(|| id.clone()),
        ),
    };
    ProjectSummary {
        id,
        name,
        app_id,
        version,
        updated_at,
        files: if available {
            list.len()
        } else {
            stored.map(|entry| entry.files).unwrap_or(0)
        },
        directory: dir.to_string_lossy().into_owned(),
        external,
        available,
    }
}

fn legacy_directories(paths: &WorkspacePaths) -> Result<Vec<(String, PathBuf)>, String> {
    let mut out = vec![];
    for entry in fs::read_dir(paths.root("projects")?).map_err(|_| "无法读取旧项目列表")? {
        let entry = entry.map_err(|_| "无法读取旧项目目录项")?;
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if valid_project_id(&id)
            && entry
                .file_type()
                .map_err(|_| "无法读取旧项目类型")?
                .is_dir()
            && checked_directory(&entry.path()).is_ok()
        {
            out.push((id, entry.path()));
        }
    }
    Ok(out)
}

fn cache_summary(registry: &mut DirectoryRegistry, value: &ProjectSummary) -> bool {
    let entries = if value.external {
        &mut registry.projects
    } else {
        &mut registry.legacy_projects
    };
    let refreshed = DirectoryRegistration::from_summary(value);
    if let Some(entry) = entries.iter_mut().find(|entry| entry.id == value.id) {
        if *entry == refreshed {
            return false;
        }
        *entry = refreshed;
    } else {
        entries.push(refreshed);
    }
    true
}

fn list_at(
    paths: &WorkspacePaths,
    registry: &mut DirectoryRegistry,
) -> Result<Vec<ProjectSummary>, String> {
    let mut out = vec![];
    let mut changed = false;
    let mut seen = HashSet::new();
    for (id, dir) in legacy_directories(paths)? {
        if !registry.projects.iter().any(|entry| entry.id == id)
            && !registry.retired_legacy_ids.contains(&id)
        {
            let stored = registry.legacy_projects.iter().find(|entry| entry.id == id);
            let value = summary(&dir, id.clone(), false, stored);
            changed |= cache_summary(registry, &value);
            seen.insert(id);
            out.push(value);
        }
    }
    for entry in &registry.legacy_projects {
        if !seen.contains(&entry.id) {
            out.push(summary(
                Path::new(&entry.directory),
                entry.id.clone(),
                false,
                Some(entry),
            ));
        }
    }
    for entry in &mut registry.projects {
        let value = summary(
            Path::new(&entry.directory),
            entry.id.clone(),
            true,
            Some(entry),
        );
        let refreshed = DirectoryRegistration::from_summary(&value);
        if *entry != refreshed {
            *entry = refreshed;
            changed = true;
        }
        out.push(value);
    }
    if changed {
        save_registry(paths, registry)?;
    }
    out.sort_by_key(|project| std::cmp::Reverse(project.updated_at));
    Ok(out)
}

fn refresh_registration(
    paths: &WorkspacePaths,
    registry: &mut DirectoryRegistry,
    id: &str,
) -> Result<(), String> {
    let dir = project_dir(paths, registry, id)?;
    let external = registry.projects.iter().any(|entry| entry.id == id);
    let stored = registry
        .projects
        .iter()
        .chain(&registry.legacy_projects)
        .find(|entry| entry.id == id);
    let value = summary(&dir, id.to_owned(), external, stored);
    if cache_summary(registry, &value) {
        save_registry(paths, registry)?;
    }
    Ok(())
}

fn read_config(dir: &Path) -> Result<project::Config, String> {
    let text = read_text(dir, "app.json", MAX_CONFIG_BYTES)?;
    let config: project::Config =
        serde_json::from_str(&text).map_err(|_| "app.json 无效，请检查 app_id 和 version")?;
    if !project::valid_id(&config.app_id)
        || config.version.len() > 31
        || semver::Version::parse(&config.version).is_err()
    {
        return Err("app_id 或 SemVer 版本号无效".into());
    }
    for (text, max) in [(&config.display_name, 128), (&config.description, 1024)] {
        if text
            .as_ref()
            .is_some_and(|text| text.len() > max || text.contains('\0'))
        {
            return Err("程序名称或描述超出长度限制，或含 NUL".into());
        }
    }
    Ok(config)
}

fn validate_directory(dir: &Path) -> Result<project::Config, String> {
    let config = read_config(dir)?;
    let entry = format!("{}.lua", config.app_id);
    let path = file_path(dir, &entry)?;
    if !fs::symlink_metadata(path)
        .map_err(|_| format!("缺少入口文件 {entry}"))?
        .is_file()
    {
        return Err(format!("入口 {entry} 必须是普通文件"));
    }
    Ok(config)
}

fn new_project_id(paths: &WorkspacePaths, registry: &DirectoryRegistry) -> String {
    loop {
        let id = uuid::Uuid::new_v4().to_string();
        if !registry
            .projects
            .iter()
            .chain(&registry.legacy_projects)
            .any(|entry| entry.id == id)
            && !registry.retired_legacy_ids.contains(&id)
            && fs::symlink_metadata(paths.data.join("projects").join(&id)).is_err()
        {
            return id;
        }
    }
}

fn default_parent(documents: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(documents).map_err(|_| "无法创建文稿目录")?;
    let documents = canonical_directory(documents)?;
    let parent = documents.join("GizMoth");
    match fs::create_dir(&parent) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err("无法创建 GizMoth 工作目录".into()),
    }
    checked_directory(&parent)?;
    Ok(parent)
}

fn create_at(
    paths: &WorkspacePaths,
    registry: &mut DirectoryRegistry,
    app_id: &str,
    name: &str,
    parent: &Path,
) -> Result<ProjectSummary, String> {
    if !project::valid_id(app_id) {
        return Err("程序 ID 需为 1–32 位小写字母、数字、下划线、点或连字符。".into());
    }
    let name = name.trim();
    if name.is_empty() || name.len() > 128 || name.contains('\0') {
        return Err("项目名称需为 1–128 字节，且不能含 NUL".into());
    }
    let parent = canonical_directory(parent)?;
    let dir = parent.join(app_id);
    fs::create_dir(&dir).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            format!("目录已存在，不会覆盖：{}", dir.display())
        } else {
            "无法创建项目目录".into()
        }
    })?;
    let config =
        json!({ "app_id": app_id, "version": "0.1.0", "display_name": name, "description": "" });
    let entry = format!(
        "-- {name}\n-- Runs on the device after it is installed and started.\n\nlocal message = \"Hello from {app_id}\"\nprint(message)\n\nreturn {{ message = message }}\n"
    );
    let id = new_project_id(paths, registry);
    let mut created_files = vec![];
    let result = (|| {
        for (path, content) in [
            (
                "app.json".to_string(),
                format!(
                    "{}\n",
                    serde_json::to_string_pretty(&config).map_err(|_| "无法生成 app.json")?
                ),
            ),
            (format!("{app_id}.lua"), entry),
        ] {
            let target = file_path(&dir, &path)?;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|_| "无法创建项目文件")?;
            created_files.push(target);
            file.write_all(content.as_bytes())
                .map_err(|_| "无法写入项目文件")?;
        }
        let value = summary(&dir, id.clone(), true, None);
        registry
            .projects
            .push(DirectoryRegistration::from_summary(&value));
        if let Err(error) = save_registry(paths, registry) {
            registry.projects.retain(|entry| entry.id != id);
            return Err(error);
        }
        Ok(value)
    })();
    if result.is_err() {
        // Only remove files created by this attempt, never a directory tree
        // that another process may have populated in the meantime.
        for path in created_files {
            let _ = fs::remove_file(path);
        }
        let _ = fs::remove_dir(&dir);
    }
    result
}

fn matching_legacy(
    paths: &WorkspacePaths,
    registry: &DirectoryRegistry,
    directory: &Path,
    include_retired: bool,
) -> Result<Option<String>, String> {
    Ok(legacy_directories(paths)?
        .into_iter()
        .find(|(id, dir)| {
            dir == directory
                && !registry.projects.iter().any(|entry| &entry.id == id)
                && (include_retired || !registry.retired_legacy_ids.contains(id))
        })
        .map(|(id, _)| id))
}

fn open_at(
    paths: &WorkspacePaths,
    registry: &mut DirectoryRegistry,
    directory: &Path,
) -> Result<ProjectSummary, String> {
    let dir = canonical_directory(directory)?;
    validate_directory(&dir)?;
    if let Some(entry) = registry
        .projects
        .iter()
        .find(|entry| Path::new(&entry.directory) == dir)
    {
        let id = entry.id.clone();
        refresh_registration(paths, registry, &id)?;
        let entry = registry
            .projects
            .iter()
            .find(|entry| entry.id == id)
            .unwrap();
        return Ok(summary(&dir, id, true, Some(entry)));
    }
    if let Some(id) = matching_legacy(paths, registry, &dir, true)? {
        let retired = registry.retired_legacy_ids.contains(&id);
        registry.retired_legacy_ids.retain(|retired| retired != &id);
        let stored = registry.legacy_projects.iter().find(|entry| entry.id == id);
        let value = summary(&dir, id, false, stored);
        if cache_summary(registry, &value) || retired {
            save_registry(paths, registry)?;
        }
        return Ok(value);
    }
    let value = summary(&dir, new_project_id(paths, registry), true, None);
    registry
        .projects
        .push(DirectoryRegistration::from_summary(&value));
    save_registry(paths, registry)?;
    Ok(value)
}

fn relocate_at(
    paths: &WorkspacePaths,
    registry: &mut DirectoryRegistry,
    id: &str,
    directory: &Path,
) -> Result<ProjectSummary, String> {
    known_project(paths, registry, id)?;
    let was_legacy = !registry.projects.iter().any(|entry| entry.id == id);
    let previous = registry
        .projects
        .iter()
        .chain(&registry.legacy_projects)
        .find(|entry| entry.id == id);
    let old_id = project_dir(paths, registry, id)
        .ok()
        .and_then(|dir| read_config(&dir).ok())
        .map(|config| config.app_id)
        .or_else(|| previous.and_then(|entry| entry.app_id.clone()));
    let dir = canonical_directory(directory)?;
    let config = validate_directory(&dir)?;
    if old_id.as_ref().is_some_and(|old| old != &config.app_id) {
        return Err(format!(
            "程序 ID 不匹配，需要 {}，所选目录为 {}",
            old_id.unwrap(),
            config.app_id
        ));
    }
    if registry
        .projects
        .iter()
        .any(|entry| entry.id != id && Path::new(&entry.directory) == dir)
        || matching_legacy(paths, registry, &dir, false)?.is_some_and(|other| other != id)
    {
        return Err("该目录已属于另一个项目".into());
    }
    let value = summary(&dir, id.to_owned(), true, previous);
    let mut updated = registry.clone();
    if was_legacy && !updated.retired_legacy_ids.iter().any(|old| old == id) {
        updated.retired_legacy_ids.push(id.to_owned());
    }
    updated.legacy_projects.retain(|entry| entry.id != id);
    updated.projects.retain(|entry| entry.id != id);
    updated
        .projects
        .push(DirectoryRegistration::from_summary(&value));
    save_registry(paths, &updated)?;
    *registry = updated;
    Ok(value)
}

pub fn list(app: &tauri::AppHandle) -> Result<Vec<ProjectSummary>, String> {
    with_workspace(app, list_at)
}

pub fn choose_directory() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("选择新项目的父目录")
        .pick_folder()?
        .to_str()
        .map(str::to_owned)
}

pub fn create(
    app: &tauri::AppHandle,
    app_id: &str,
    name: &str,
    parent_dir: Option<&str>,
) -> Result<ProjectSummary, String> {
    with_workspace(app, |paths, registry| {
        let parent = match parent_dir {
            Some(parent) => canonical_directory(Path::new(parent))?,
            None => default_parent(
                &app.path()
                    .document_dir()
                    .map_err(|_| "无法定位文稿目录，请选择项目父目录")?,
            )?,
        };
        create_at(paths, registry, app_id, name, &parent)
    })
}

pub fn open(app: &tauri::AppHandle) -> Result<Option<ProjectSummary>, String> {
    let Some(directory) = rfd::FileDialog::new()
        .set_title("打开 GizMoth 项目目录")
        .pick_folder()
    else {
        return Ok(None);
    };
    with_workspace(app, |paths, registry| open_at(paths, registry, &directory)).map(Some)
}

pub fn relocate(app: &tauri::AppHandle, id: &str) -> Result<Option<ProjectSummary>, String> {
    with_workspace(app, |paths, registry| known_project(paths, registry, id))?;
    let Some(directory) = rfd::FileDialog::new()
        .set_title("重新定位项目目录")
        .pick_folder()
    else {
        return Ok(None);
    };
    with_workspace(app, |paths, registry| {
        relocate_at(paths, registry, id, &directory)
    })
    .map(Some)
}

pub fn files(app: &tauri::AppHandle, id: &str) -> Result<Vec<FileEntry>, String> {
    with_workspace(app, |paths, registry| {
        let dir = project_dir(paths, registry, id)?;
        let mut out = vec![];
        walk(&dir, "", &mut out)?;
        Ok(out)
    })
}

pub fn read(app: &tauri::AppHandle, id: &str, path: &str) -> Result<String, String> {
    with_workspace(app, |paths, registry| {
        read_text(&project_dir(paths, registry, id)?, path, MAX_PROJECT_BYTES)
    })
}

fn write_at(dir: &Path, path: &str, content: &str) -> Result<(), String> {
    let target = file_path(dir, path)?;
    if fs::symlink_metadata(&target).is_ok_and(|meta| !meta.is_file()) {
        return Err("目标不是普通文件".into());
    }
    let mut existing = vec![];
    walk(dir, "", &mut existing)?;
    let previous = existing.iter().find(|file| file.path == path);
    if previous.is_none() && existing.len() >= MAX_FILES {
        return Err("每个项目最多 65 个文件".into());
    }
    let lower = path.to_lowercase();
    if existing.iter().any(|file| {
        let other = file.path.to_lowercase();
        (other == lower && file.path != path)
            || other.starts_with(&format!("{lower}/"))
            || lower.starts_with(&format!("{other}/"))
    }) {
        return Err("文件路径重复（不区分大小写）或与目录冲突".into());
    }
    let total = existing.iter().map(|file| file.size).sum::<u64>()
        - previous.map(|file| file.size).unwrap_or(0)
        + content.len() as u64;
    if total > MAX_PROJECT_BYTES {
        return Err("项目内容超过 2 MiB 限制".into());
    }
    ensure_parents(dir, path)?;
    let target = file_path(dir, path)?;
    atomic_write(&target, content.as_bytes())
}

pub fn write(app: &tauri::AppHandle, id: &str, path: &str, content: &str) -> Result<(), String> {
    with_workspace(app, |paths, registry| {
        write_at(&project_dir(paths, registry, id)?, path, content)?;
        refresh_registration(paths, registry, id)
    })
}

fn prune_empty(dir: &Path, mut current: PathBuf) {
    while current != dir && current.starts_with(dir) {
        if checked_directory(&current).is_err() || fs::remove_dir(&current).is_err() {
            break;
        }
        if !current.pop() {
            break;
        }
    }
}

fn remove_at(dir: &Path, path: &str) -> Result<(), String> {
    if path == "app.json" {
        return Err("app.json 不能删除".into());
    }
    let target = file_path(dir, path)?;
    if !fs::symlink_metadata(&target)
        .map_err(|_| "文件不存在")?
        .is_file()
    {
        return Err("只能删除普通文件".into());
    }
    fs::remove_file(&target).map_err(|_| "无法删除文件")?;
    if let Some(parent) = target.parent() {
        prune_empty(dir, parent.to_path_buf());
    }
    Ok(())
}

pub fn remove(app: &tauri::AppHandle, id: &str, path: &str) -> Result<(), String> {
    with_workspace(app, |paths, registry| {
        remove_at(&project_dir(paths, registry, id)?, path)?;
        refresh_registration(paths, registry, id)
    })
}

fn rename_at(dir: &Path, from: &str, to: &str) -> Result<(), String> {
    if from == "app.json" || to == "app.json" {
        return Err("app.json 不能重命名".into());
    }
    file_path(dir, to)?;
    let content = read_text(dir, from, MAX_PROJECT_BYTES)?;
    let mut existing = vec![];
    walk(dir, "", &mut existing)?;
    if existing
        .iter()
        .any(|file| file.path.eq_ignore_ascii_case(to))
    {
        return Err("目标文件已存在".into());
    }
    write_at(dir, to, &content)?;
    remove_at(dir, from)
}

pub fn rename(app: &tauri::AppHandle, id: &str, from: &str, to: &str) -> Result<(), String> {
    with_workspace(app, |paths, registry| {
        rename_at(&project_dir(paths, registry, id)?, from, to)?;
        refresh_registration(paths, registry, id)
    })
}

fn delete_at(
    paths: &WorkspacePaths,
    registry: &mut DirectoryRegistry,
    id: &str,
) -> Result<(), String> {
    known_project(paths, registry, id)?;
    if registry.projects.iter().any(|entry| entry.id == id) {
        let mut updated = registry.clone();
        updated.projects.retain(|entry| entry.id != id);
        save_registry(paths, &updated)?;
        *registry = updated;
    } else {
        let dir = paths.root("projects")?.join(id);
        match fs::symlink_metadata(&dir) {
            Ok(_) => {
                fs::remove_dir_all(project_dir(paths, registry, id)?)
                    .map_err(|_| "无法删除旧项目")?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("无法访问旧项目目录".into()),
        }
        if registry.legacy_projects.iter().any(|entry| entry.id == id) {
            registry.legacy_projects.retain(|entry| entry.id != id);
            save_registry(paths, registry)?;
        }
    }
    let chat = file_path(&paths.root("chats")?, &format!("{id}.json"))?;
    match fs::remove_file(chat) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("项目已移除，但无法删除对话记录".into()),
    }
}

pub fn delete(app: &tauri::AppHandle, id: &str) -> Result<(), String> {
    with_workspace(app, |paths, registry| delete_at(paths, registry, id))
}

fn load_at(dir: &Path) -> Result<Project, String> {
    let mut entries = vec![];
    walk(dir, "", &mut entries)?;
    let mut files = vec![];
    for entry in entries {
        files.push(SourceFile {
            content: read_text(dir, &entry.path, MAX_PROJECT_BYTES)?,
            path: entry.path,
        });
    }
    Ok(Project { version: 1, files })
}

/// Snapshot UTF-8 source files for the existing firmware package validator.
pub fn load(app: &tauri::AppHandle, id: &str) -> Result<Project, String> {
    with_workspace(app, |paths, registry| {
        load_at(&project_dir(paths, registry, id)?)
    })
}

pub fn chat_load(app: &tauri::AppHandle, id: &str) -> Result<Option<String>, String> {
    with_workspace(app, |paths, registry| {
        // A missing external directory must not make its conversation disappear.
        known_project(paths, registry, id)?;
        let chats = paths.root("chats")?;
        let path = format!("{id}.json");
        let target = file_path(&chats, &path)?;
        if let Err(error) = fs::symlink_metadata(target) {
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err("无法读取对话记录".into());
        }
        read_text(&chats, &path, MAX_CHAT_BYTES as u64).map(Some)
    })
}

pub fn chat_save(app: &tauri::AppHandle, id: &str, json: &str) -> Result<(), String> {
    with_workspace(app, |paths, registry| {
        known_project(paths, registry, id)?;
        if json.len() > MAX_CHAT_BYTES || serde_json::from_str::<serde_json::Value>(json).is_err() {
            return Err("对话记录无效或过大".into());
        }
        let path = file_path(&paths.root("chats")?, &format!("{id}.json"))?;
        atomic_write(&path, json.as_bytes())
    })
}

pub fn reveal_dir(app: &tauri::AppHandle, id: &str) -> Result<String, String> {
    with_workspace(app, |paths, registry| {
        Ok(project_dir(paths, registry, id)?.to_string_lossy().into())
    })
}

pub fn reveal(app: &tauri::AppHandle, id: &str) -> Result<(), String> {
    let dir = reveal_dir(app, id)?;
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let program = "xdg-open";
    Command::new(program)
        .arg(dir)
        .spawn()
        .map(|_| ())
        .map_err(|_| "无法在文件管理器中打开项目目录".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestWorkspace {
        root: PathBuf,
        paths: WorkspacePaths,
    }

    impl TestWorkspace {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("gizmoth-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&root).unwrap();
            let root = fs::canonicalize(root).unwrap();
            let paths = WorkspacePaths::new(root.join("app-data")).unwrap();
            Self { root, paths }
        }

        fn folder(&self, name: &str) -> PathBuf {
            let path = self.root.join(name);
            fs::create_dir_all(&path).unwrap();
            path
        }

        fn project(&self, name: &str, app_id: &str) -> PathBuf {
            let dir = self.folder(name);
            fs::write(
                dir.join("app.json"),
                serde_json::to_vec(&json!({
                    "app_id": app_id,
                    "version": "1.2.3",
                    "display_name": "Example"
                }))
                .unwrap(),
            )
            .unwrap();
            fs::write(dir.join(format!("{app_id}.lua")), "return 1\n").unwrap();
            dir
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn rejects_paths_that_escape_or_hide() {
        for bad in [
            "", "../x", "a/../b", "/abs", "a//b", ".env", "a/.git/x", "a\\b", "c:x", "a/", "a/./b",
            "a\0b",
        ] {
            assert!(!valid_path(bad), "{bad}");
        }
        for good in ["app.json", "hello.lua", "hello/data/sprite.txt"] {
            assert!(valid_path(good), "{good}");
        }
        assert!(!valid_project_id("../x"));
        assert!(valid_project_id("hello-1"));
    }

    #[test]
    fn creates_in_arbitrary_parent_without_overwriting_any_existing_content() {
        let test = TestWorkspace::new();
        let parent = test.folder("chosen parent");
        let mut registry = DirectoryRegistry::default();
        let value = create_at(&test.paths, &mut registry, "hello_app", "Hello", &parent).unwrap();
        let dir = parent.join("hello_app");
        assert_eq!(Path::new(&value.directory), dir);
        assert!(value.external && value.available);
        assert!(valid_project_id(&value.id));
        assert!(dir.join("app.json").is_file());
        assert!(dir.join("hello_app.lua").is_file());
        assert!(project::package(&load_at(&dir).unwrap()).is_ok());
        fs::write(dir.join("keep.txt"), "untouched").unwrap();
        let original = fs::read(dir.join("app.json")).unwrap();
        assert!(create_at(&test.paths, &mut registry, "hello_app", "Other", &parent).is_err());
        assert_eq!(fs::read(dir.join("app.json")).unwrap(), original);
        assert_eq!(
            fs::read_to_string(dir.join("keep.txt")).unwrap(),
            "untouched"
        );
        let saved = read_registry(&test.paths).unwrap();
        assert_eq!(saved.projects.len(), 1);
        assert_eq!(saved.projects[0].directory, value.directory);
        assert!(!test.paths.data.join("projects").join(&value.id).exists());

        fs::write(parent.join("blocked"), "existing file").unwrap();
        assert!(create_at(&test.paths, &mut registry, "blocked", "Other", &parent).is_err());
        assert_eq!(
            fs::read_to_string(parent.join("blocked")).unwrap(),
            "existing file"
        );
    }

    #[test]
    fn default_workspace_is_the_documents_gizmoth_folder() {
        let test = TestWorkspace::new();
        let documents = test.folder("Documents");
        let parent = default_parent(&documents).unwrap();
        assert_eq!(parent, documents.join("GizMoth"));
        let mut registry = DirectoryRegistry::default();
        let value = create_at(&test.paths, &mut registry, "hello", "Hello", &parent).unwrap();
        assert_eq!(Path::new(&value.directory), parent.join("hello"));
        assert_eq!(default_parent(&documents).unwrap(), parent);
    }

    #[test]
    fn open_deduplicates_canonical_directories_without_package_validation() {
        let test = TestWorkspace::new();
        let dir = test.project("existing", "hello");
        // Opening must allow repairing invalid Lua and unrelated source files.
        fs::write(dir.join("hello.lua"), b"\0\x1bLua").unwrap();
        fs::write(dir.join("unrelated.txt"), "fix me").unwrap();
        let mut registry = DirectoryRegistry::default();
        let first = open_at(&test.paths, &mut registry, &dir).unwrap();
        let second = open_at(&test.paths, &mut registry, &dir.join("..").join("existing")).unwrap();
        assert_eq!(first.id, second.id);
        assert!(first.external);
        assert_eq!(read_registry(&test.paths).unwrap().projects.len(), 1);
        assert!(project::package(&load_at(&dir).unwrap()).is_err());
    }

    #[test]
    fn opening_checks_metadata_and_requires_a_regular_entry() {
        let test = TestWorkspace::new();
        let dir = test.project("existing", "hello");
        let mut registry = DirectoryRegistry::default();
        fs::remove_file(dir.join("hello.lua")).unwrap();
        assert!(open_at(&test.paths, &mut registry, &dir).is_err());
        fs::write(dir.join("hello.lua"), "return 1").unwrap();
        for config in [
            json!({"app_id":"../hello", "version":"1.0.0"}),
            json!({"app_id":"hello", "version":"broken"}),
            json!({"app_id":"hello", "version":"1.0.0", "display_name":"\0"}),
        ] {
            fs::write(dir.join("app.json"), config.to_string()).unwrap();
            assert!(open_at(&test.paths, &mut registry, &dir).is_err());
        }
        assert!(registry.projects.is_empty());
    }

    #[test]
    fn legacy_ids_chat_paths_and_owned_deletion_still_work() {
        let test = TestWorkspace::new();
        let dir = test.project("app-data/projects/legacy-id", "hello");
        let mut registry = DirectoryRegistry::default();
        let chat = test.paths.root("chats").unwrap().join("legacy-id.json");
        fs::write(&chat, "[]").unwrap();
        let value = open_at(&test.paths, &mut registry, &dir).unwrap();
        assert_eq!(value.id, "legacy-id");
        assert!(!value.external);
        assert!(registry.projects.is_empty());
        assert_eq!(
            project_dir(&test.paths, &registry, "legacy-id").unwrap(),
            dir
        );
        assert_eq!(list_at(&test.paths, &mut registry).unwrap().len(), 1);
        assert_eq!(fs::read_to_string(&chat).unwrap(), "[]");
        delete_at(&test.paths, &mut registry, "legacy-id").unwrap();
        assert!(!dir.exists());
        assert!(!chat.exists());
    }

    #[test]
    fn unregistering_external_project_preserves_all_directory_files() {
        let test = TestWorkspace::new();
        let dir = test.project("external", "hello");
        fs::create_dir(dir.join(".git")).unwrap();
        fs::write(dir.join(".git/config"), "keep git data").unwrap();
        fs::write(dir.join("personal.txt"), "keep personal data").unwrap();
        let mut registry = DirectoryRegistry::default();
        let value = open_at(&test.paths, &mut registry, &dir).unwrap();
        let chat = test
            .paths
            .root("chats")
            .unwrap()
            .join(format!("{}.json", value.id));
        fs::write(&chat, "[]").unwrap();
        delete_at(&test.paths, &mut registry, &value.id).unwrap();
        assert!(dir.join("hello.lua").is_file());
        assert!(dir.join("app.json").is_file());
        assert_eq!(
            fs::read_to_string(dir.join(".git/config")).unwrap(),
            "keep git data"
        );
        assert_eq!(
            fs::read_to_string(dir.join("personal.txt")).unwrap(),
            "keep personal data"
        );
        assert!(!chat.exists());
        assert!(read_registry(&test.paths).unwrap().projects.is_empty());
    }

    #[test]
    fn missing_external_directories_keep_metadata_and_can_be_unregistered() {
        let test = TestWorkspace::new();
        let dir = test.project("external", "hello");
        let mut registry = DirectoryRegistry::default();
        let original = open_at(&test.paths, &mut registry, &dir).unwrap();
        fs::rename(&dir, test.root.join("moved")).unwrap();
        let list = list_at(&test.paths, &mut registry).unwrap();
        assert_eq!(list.len(), 1);
        assert!(!list[0].available);
        assert!(list[0].external);
        assert_eq!(list[0].name, original.name);
        assert_eq!(list[0].app_id, original.app_id);
        assert_eq!(list[0].version, original.version);
        assert!(project_dir(&test.paths, &registry, &original.id)
            .unwrap_err()
            .contains("项目目录不可用"));
        assert!(known_project(&test.paths, &registry, &original.id).is_ok());
        assert_eq!(read_registry(&test.paths).unwrap().projects.len(), 1);
        delete_at(&test.paths, &mut registry, &original.id).unwrap();
        assert!(test.root.join("moved/hello.lua").is_file());
    }

    #[test]
    fn relocation_preserves_id_and_chat_and_rejects_wrong_or_duplicate_projects() {
        let test = TestWorkspace::new();
        let dir = test.project("external", "hello");
        let wrong = test.project("wrong", "other");
        let duplicate = test.project("duplicate", "hello");
        let mut registry = DirectoryRegistry::default();
        let original = open_at(&test.paths, &mut registry, &dir).unwrap();
        let other = open_at(&test.paths, &mut registry, &duplicate).unwrap();
        let chat = test
            .paths
            .root("chats")
            .unwrap()
            .join(format!("{}.json", original.id));
        fs::write(&chat, "[1]").unwrap();
        let moved = test.root.join("moved");
        fs::rename(&dir, &moved).unwrap();
        assert!(relocate_at(&test.paths, &mut registry, &original.id, &wrong).is_err());
        assert!(relocate_at(&test.paths, &mut registry, &original.id, &duplicate).is_err());
        assert_ne!(original.id, other.id);
        let relocated = relocate_at(&test.paths, &mut registry, &original.id, &moved).unwrap();
        assert_eq!(relocated.id, original.id);
        assert_eq!(Path::new(&relocated.directory), moved);
        assert!(relocated.available);
        assert_eq!(fs::read_to_string(&chat).unwrap(), "[1]");
        assert_eq!(
            open_at(&test.paths, &mut registry, &moved).unwrap().id,
            original.id
        );
    }

    #[test]
    fn relocating_legacy_directory_does_not_delete_or_rediscover_old_source() {
        let test = TestWorkspace::new();
        let legacy = test.project("app-data/projects/legacy-id", "hello");
        let replacement = test.project("replacement", "hello");
        let mut registry = DirectoryRegistry::default();
        let value = relocate_at(&test.paths, &mut registry, "legacy-id", &replacement).unwrap();
        assert_eq!(value.id, "legacy-id");
        assert!(value.external);
        assert!(legacy.join("hello.lua").is_file());
        assert_eq!(list_at(&test.paths, &mut registry).unwrap().len(), 1);
        delete_at(&test.paths, &mut registry, "legacy-id").unwrap();
        assert!(legacy.join("hello.lua").is_file());
        assert!(replacement.join("hello.lua").is_file());
        assert!(list_at(&test.paths, &mut registry).unwrap().is_empty());
        assert_eq!(
            open_at(&test.paths, &mut registry, &legacy).unwrap().id,
            "legacy-id"
        );
    }

    #[test]
    fn walking_ignores_hidden_directories_but_checks_paths_and_file_limits() {
        let test = TestWorkspace::new();
        let dir = test.project("external", "hello");
        fs::create_dir(dir.join(".git")).unwrap();
        fs::write(dir.join(".git/hidden"), "ignored").unwrap();
        let mut files = vec![];
        walk(&dir, "", &mut files).unwrap();
        assert_eq!(files.len(), 2);
        #[cfg(unix)]
        {
            // Colons are legal filenames on Unix, but invalid source paths.
            fs::write(dir.join("bad:name"), "bad").unwrap();
            assert!(walk(&dir, "", &mut vec![]).is_err());
            fs::remove_file(dir.join("bad:name")).unwrap();
        }
        for index in 0..MAX_FILES {
            fs::write(dir.join(format!("file-{index}")), "").unwrap();
        }
        assert!(walk(&dir, "", &mut vec![]).is_err());
    }

    #[test]
    fn nested_writes_renames_and_removals_preserve_package_behavior() {
        let test = TestWorkspace::new();
        let dir = test.project("external", "hello");
        write_at(&dir, "hello/assets/note.txt", "example").unwrap();
        assert_eq!(
            read_text(&dir, "hello/assets/note.txt", 100).unwrap(),
            "example"
        );
        rename_at(&dir, "hello/assets/note.txt", "hello/renamed.txt").unwrap();
        assert!(!dir.join("hello/assets").exists());
        assert!(project::package(&load_at(&dir).unwrap()).is_ok());
        remove_at(&dir, "hello/renamed.txt").unwrap();
        assert!(!dir.join("hello").exists());
        assert!(remove_at(&dir, "app.json").is_err());
        for path in ["../outside", "hello/../outside", "hello/.hidden"] {
            assert!(write_at(&dir, path, "bad").is_err());
            assert!(read_text(&dir, path, 100).is_err());
            assert!(remove_at(&dir, path).is_err());
            assert!(rename_at(&dir, "hello.lua", path).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_links_at_leaves_and_every_nested_parent_for_all_file_operations() {
        use std::os::unix::fs::symlink;
        let test = TestWorkspace::new();
        let dir = test.project("external", "hello");
        let outside = test.folder("outside");
        fs::write(outside.join("secret.txt"), "untouched").unwrap();
        fs::create_dir_all(dir.join("hello/nested")).unwrap();
        symlink(&outside, dir.join("hello/nested/link")).unwrap();
        symlink(outside.join("secret.txt"), dir.join("leaf.txt")).unwrap();
        symlink(&outside, dir.join(".ignored-link")).unwrap();
        for path in [
            "leaf.txt",
            "hello/nested/link/secret.txt",
            "hello/nested/link/new.txt",
        ] {
            assert!(read_text(&dir, path, 100).is_err(), "{path}");
            assert!(write_at(&dir, path, "bad").is_err(), "{path}");
            assert!(remove_at(&dir, path).is_err(), "{path}");
            assert!(rename_at(&dir, "hello.lua", path).is_err(), "{path}");
            assert!(rename_at(&dir, path, "hello/new.txt").is_err(), "{path}");
        }
        assert!(load_at(&dir).is_err());
        assert_eq!(
            fs::read_to_string(outside.join("secret.txt")).unwrap(),
            "untouched"
        );
        assert!(!outside.join("new.txt").exists());
        fs::remove_file(dir.join("leaf.txt")).unwrap();
        fs::remove_file(dir.join("hello/nested/link")).unwrap();
        assert!(load_at(&dir).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn canonical_open_deduplicates_aliases_and_rejects_replaced_root_ancestors() {
        use std::os::unix::fs::symlink;
        let test = TestWorkspace::new();
        let parent = test.folder("parent");
        let dir = test.project("parent/external", "hello");
        let alias = test.root.join("alias");
        symlink(&dir, &alias).unwrap();
        let mut registry = DirectoryRegistry::default();
        let original = open_at(&test.paths, &mut registry, &dir).unwrap();
        assert_eq!(
            open_at(&test.paths, &mut registry, &alias).unwrap().id,
            original.id
        );
        let moved = test.root.join("moved-parent");
        fs::rename(&parent, &moved).unwrap();
        symlink(&moved, &parent).unwrap();
        assert!(project_dir(&test.paths, &registry, &original.id).is_err());
        assert!(read_text(&dir, "app.json", MAX_CONFIG_BYTES).is_err());
        assert!(!list_at(&test.paths, &mut registry).unwrap()[0].available);
    }

    #[test]
    fn moved_legacy_directories_keep_cached_identity_for_relocation() {
        let test = TestWorkspace::new();
        let dir = test.project("app-data/projects/legacy-id", "hello");
        let wrong = test.project("wrong", "other");
        let mut registry = DirectoryRegistry::default();
        list_at(&test.paths, &mut registry).unwrap();
        let moved = test.root.join("moved");
        fs::rename(&dir, &moved).unwrap();
        let mut registry = read_registry(&test.paths).unwrap();
        let missing = list_at(&test.paths, &mut registry).unwrap();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].id, "legacy-id");
        assert!(!missing[0].available);
        assert!(!missing[0].external);
        assert_eq!(missing[0].app_id.as_deref(), Some("hello"));
        assert!(relocate_at(&test.paths, &mut registry, "legacy-id", &wrong).is_err());
        let value = relocate_at(&test.paths, &mut registry, "legacy-id", &moved).unwrap();
        assert_eq!(value.id, "legacy-id");
        assert!(value.external && value.available);
        assert!(read_registry(&test.paths)
            .unwrap()
            .legacy_projects
            .is_empty());
    }

    #[test]
    fn invalid_metadata_during_editing_does_not_erase_cached_identity() {
        let test = TestWorkspace::new();
        let dir = test.project("external", "hello");
        let mut registry = DirectoryRegistry::default();
        let value = open_at(&test.paths, &mut registry, &dir).unwrap();
        fs::write(dir.join("app.json"), "{}").unwrap();
        let values = list_at(&test.paths, &mut registry).unwrap();
        assert_eq!(values[0].app_id.as_deref(), Some("hello"));
        assert_eq!(values[0].name, value.name);
        let wrong = test.project("wrong", "other");
        assert!(relocate_at(&test.paths, &mut registry, &value.id, &wrong).is_err());
        fs::rename(&dir, test.root.join("moved")).unwrap();
        assert_eq!(
            list_at(&test.paths, &mut registry).unwrap()[0]
                .app_id
                .as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn guarded_registry_updates_do_not_lose_concurrent_registrations() {
        use std::sync::Arc;
        let test = TestWorkspace::new();
        let state = Arc::new(WorkspaceStore::default());
        let mut threads = vec![];
        for index in 0..6 {
            let state = Arc::clone(&state);
            let paths = test.paths.clone();
            let dir = test.project(&format!("external-{index}"), "hello");
            threads.push(std::thread::spawn(move || {
                let _guard = state.0.lock().unwrap();
                let mut registry = read_registry(&paths).unwrap();
                open_at(&paths, &mut registry, &dir).unwrap();
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(read_registry(&test.paths).unwrap().projects.len(), 6);
        assert!(fs::read_dir(&test.paths.data).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".gizmoth-")));
    }
}
