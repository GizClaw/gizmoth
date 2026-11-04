//! Local Lua projects. Each project is a plain directory under the app data
//! directory, so users can also open it in their own editor. Every path that
//! crosses IPC is validated with the same rules the firmware applies to
//! package payloads, and resolved strictly inside the project directory.

use crate::project::{self, Project, SourceFile};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use tauri::Manager;

const MAX_PROJECT_BYTES: u64 = 2 * 1024 * 1024 + 32 * 1024;
const MAX_FILES: usize = 65;
const MAX_CHAT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    pub app_id: Option<String>,
    pub version: Option<String>,
    pub updated_at: u64,
    pub files: usize,
}

#[derive(Serialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
}

#[derive(Deserialize)]
struct Meta {
    app_id: Option<String>,
    version: Option<String>,
    display_name: Option<String>,
}

fn root(app: &tauri::AppHandle, kind: &str) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|_| "无法定位应用数据目录")?
        .join(kind);
    fs::create_dir_all(&dir).map_err(|_| "无法创建应用数据目录")?;
    Ok(dir)
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

fn project_dir(app: &tauri::AppHandle, id: &str) -> Result<PathBuf, String> {
    if !valid_project_id(id) {
        return Err("项目 ID 无效".into());
    }
    let dir = root(app, "projects")?.join(id);
    if !dir.is_dir() {
        return Err("项目不存在".into());
    }
    Ok(dir)
}

fn file_path(dir: &Path, path: &str) -> Result<PathBuf, String> {
    if !valid_path(path) {
        return Err(format!("文件路径无效：{path}"));
    }
    Ok(dir.join(path))
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<FileEntry>) -> Result<(), String> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .map_err(|_| "无法读取项目目录")?
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        // Do not follow links out of the project directory.
        let kind = entry.file_type().map_err(|_| "无法读取项目目录")?;
        if kind.is_dir() {
            walk(&entry.path(), &path, out)?;
        } else if kind.is_file() {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            out.push(FileEntry { path, size });
        }
        if out.len() > MAX_FILES {
            return Err("项目文件过多".into());
        }
    }
    Ok(())
}

pub fn files(app: &tauri::AppHandle, id: &str) -> Result<Vec<FileEntry>, String> {
    let dir = project_dir(app, id)?;
    let mut out = vec![];
    walk(&dir, "", &mut out)?;
    Ok(out)
}

fn summary(dir: &Path, id: String) -> ProjectSummary {
    let meta: Option<Meta> = fs::read(dir.join("app.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let mut list = vec![];
    let _ = walk(dir, "", &mut list);
    let updated_at = list
        .iter()
        .filter_map(|f| fs::metadata(dir.join(&f.path)).ok()?.modified().ok())
        .chain(fs::metadata(dir).ok().and_then(|m| m.modified().ok()))
        .filter_map(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .max()
        .unwrap_or(0);
    let (app_id, version, name) = match meta {
        Some(m) => (
            m.app_id.clone(),
            m.version,
            m.display_name.or(m.app_id).unwrap_or_else(|| id.clone()),
        ),
        None => (None, None, id.clone()),
    };
    ProjectSummary {
        id,
        name,
        app_id,
        version,
        updated_at,
        files: list.len(),
    }
}

pub fn list(app: &tauri::AppHandle) -> Result<Vec<ProjectSummary>, String> {
    let mut out = vec![];
    for entry in fs::read_dir(root(app, "projects")?).map_err(|_| "无法读取项目列表")? {
        let Ok(entry) = entry else { continue };
        let id = entry.file_name().to_string_lossy().to_string();
        if entry.path().is_dir() && valid_project_id(&id) {
            out.push(summary(&entry.path(), id));
        }
    }
    out.sort_by_key(|p| std::cmp::Reverse(p.updated_at));
    Ok(out)
}

pub fn create(app: &tauri::AppHandle, app_id: &str, name: &str) -> Result<ProjectSummary, String> {
    if !project::valid_id(app_id) {
        return Err("程序 ID 需为 1–32 位小写字母、数字、下划线、点或连字符。".into());
    }
    let name = name.trim();
    if name.is_empty() || name.len() > 128 {
        return Err("项目名称需为 1–128 字节".into());
    }
    let base = app_id.replace(['_', '.'], "-");
    let projects = root(app, "projects")?;
    let id = (0..)
        .map(|n| {
            if n == 0 {
                base.clone()
            } else {
                format!("{base}-{n}")
            }
        })
        .find(|id| !projects.join(id).exists())
        .unwrap();
    let dir = projects.join(&id);
    fs::create_dir_all(&dir).map_err(|_| "无法创建项目目录")?;
    let config =
        json!({ "app_id": app_id, "version": "0.1.0", "display_name": name, "description": "" });
    let entry = format!(
        "-- {name}\n-- Runs on the device after it is installed and started.\n\nlocal message = \"Hello from {app_id}\"\nprint(message)\n\nreturn {{ message = message }}\n"
    );
    let write = || -> std::io::Result<()> {
        let mut bytes = serde_json::to_vec_pretty(&config)?;
        bytes.push(b'\n');
        fs::write(dir.join("app.json"), bytes)?;
        fs::write(dir.join(format!("{app_id}.lua")), entry)
    };
    if write().is_err() {
        let _ = fs::remove_dir_all(&dir);
        return Err("无法写入项目文件".into());
    }
    Ok(summary(&dir, id))
}

pub fn read(app: &tauri::AppHandle, id: &str, path: &str) -> Result<String, String> {
    let path = file_path(&project_dir(app, id)?, path)?;
    let meta = fs::symlink_metadata(&path).map_err(|_| "文件不存在")?;
    if !meta.is_file() || meta.len() > MAX_PROJECT_BYTES {
        return Err("只能读取项目内 2 MiB 以内的普通文件".into());
    }
    String::from_utf8(fs::read(path).map_err(|_| "无法读取文件")?)
        .map_err(|_| "文件不是 UTF-8 文本".into())
}

pub fn write(app: &tauri::AppHandle, id: &str, path: &str, content: &str) -> Result<(), String> {
    let dir = project_dir(app, id)?;
    let target = file_path(&dir, path)?;
    let existing = files(app, id)?;
    let previous = existing
        .iter()
        .find(|f| f.path == path)
        .map(|f| f.size)
        .unwrap_or(0);
    if previous == 0 && existing.len() >= MAX_FILES {
        return Err("每个项目最多 65 个文件".into());
    }
    let lower = path.to_lowercase();
    if existing.iter().any(|f| {
        let other = f.path.to_lowercase();
        (other == lower && f.path != path)
            || other.starts_with(&format!("{lower}/"))
            || lower.starts_with(&format!("{other}/"))
    }) {
        return Err("文件路径重复（不区分大小写）或与目录冲突".into());
    }
    let total: u64 = existing.iter().map(|f| f.size).sum::<u64>() - previous + content.len() as u64;
    if total > MAX_PROJECT_BYTES {
        return Err("项目内容超过 2 MiB 限制".into());
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|_| "无法创建目录")?;
    }
    let temporary = target.with_extension("gizmoth-tmp");
    fs::write(&temporary, content).map_err(|_| "无法写入文件")?;
    fs::rename(&temporary, &target).map_err(|_| "无法写入文件".into())
}

fn prune_empty(dir: &Path, mut current: PathBuf) {
    while current != dir && current.starts_with(dir) {
        if fs::remove_dir(&current).is_err() {
            break;
        }
        if !current.pop() {
            break;
        }
    }
}

pub fn remove(app: &tauri::AppHandle, id: &str, path: &str) -> Result<(), String> {
    if path == "app.json" {
        return Err("app.json 不能删除".into());
    }
    let dir = project_dir(app, id)?;
    let target = file_path(&dir, path)?;
    fs::remove_file(&target).map_err(|_| "无法删除文件")?;
    if let Some(parent) = target.parent() {
        prune_empty(&dir, parent.to_path_buf());
    }
    Ok(())
}

pub fn rename(app: &tauri::AppHandle, id: &str, from: &str, to: &str) -> Result<(), String> {
    if from == "app.json" || to == "app.json" {
        return Err("app.json 不能重命名".into());
    }
    let content = read(app, id, from)?;
    if files(app, id)?
        .iter()
        .any(|f| f.path.eq_ignore_ascii_case(to))
    {
        return Err("目标文件已存在".into());
    }
    write(app, id, to, &content)?;
    remove(app, id, from)
}

pub fn delete(app: &tauri::AppHandle, id: &str) -> Result<(), String> {
    let dir = project_dir(app, id)?;
    fs::remove_dir_all(dir).map_err(|_| "无法删除项目")?;
    let chat = root(app, "chats")?.join(format!("{id}.json"));
    let _ = fs::remove_file(chat);
    Ok(())
}

/// Snapshot the directory as a package source. Only UTF-8 text payloads are
/// supported by this version.
pub fn load(app: &tauri::AppHandle, id: &str) -> Result<Project, String> {
    let mut files = vec![];
    for entry in self::files(app, id)? {
        files.push(SourceFile {
            content: read(app, id, &entry.path)?,
            path: entry.path,
        });
    }
    Ok(Project { version: 1, files })
}

pub fn chat_load(app: &tauri::AppHandle, id: &str) -> Result<Option<String>, String> {
    project_dir(app, id)?;
    match fs::read_to_string(root(app, "chats")?.join(format!("{id}.json"))) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("无法读取对话记录".into()),
    }
}

pub fn chat_save(app: &tauri::AppHandle, id: &str, json: &str) -> Result<(), String> {
    project_dir(app, id)?;
    if json.len() > MAX_CHAT_BYTES || serde_json::from_str::<serde_json::Value>(json).is_err() {
        return Err("对话记录无效或过大".into());
    }
    let path = root(app, "chats")?.join(format!("{id}.json"));
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, json).map_err(|_| "无法保存对话记录")?;
    fs::rename(&temporary, path).map_err(|_| "无法保存对话记录".into())
}

pub fn reveal_dir(app: &tauri::AppHandle, id: &str) -> Result<String, String> {
    Ok(project_dir(app, id)?.to_string_lossy().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_paths_that_escape_or_hide() {
        for bad in [
            "", "../x", "a/../b", "/abs", "a//b", ".env", "a/.git/x", "a\\b", "c:x", "a/",
        ] {
            assert!(!valid_path(bad), "{bad}");
        }
        for good in ["app.json", "hello.lua", "hello/data/sprite.txt"] {
            assert!(valid_path(good), "{good}");
        }
        assert!(!valid_project_id("../x"));
        assert!(valid_project_id("hello-1"));
    }
}
