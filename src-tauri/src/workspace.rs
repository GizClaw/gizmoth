//! Local Lua projects. Each project is a plain directory under the app data
//! directory, so users can also open it in their own editor. Every path that
//! crosses IPC is validated with the same rules the firmware applies to
//! package payloads, and resolved strictly inside the project directory.

use crate::project::{self, Project, SourceFile};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs,
    io::{self, Read, Write},
    sync::Mutex,
    time::UNIX_EPOCH,
};
use tauri::Manager;

const MAX_PROJECT_BYTES: u64 = 2 * 1024 * 1024 + 32 * 1024;
const MAX_FILES: usize = 65;
const MAX_CHAT_BYTES: usize = 8 * 1024 * 1024;
// Serializes snapshots with every in-process project mutation, including agent writes.
static PROJECT_LOCK: Mutex<()> = Mutex::new(());

fn project_lock() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    PROJECT_LOCK
        .lock()
        .map_err(|_| "项目操作锁不可用，请重启应用".into())
}

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

fn root(app: &tauri::AppHandle, kind: &str) -> Result<Dir, String> {
    // The OS-configured app data directory is the only ambient authority. Every
    // project/store component below it is opened without following links.
    let path = app
        .path()
        .app_data_dir()
        .map_err(|_| "无法定位应用数据目录")?;
    fs::create_dir_all(&path).map_err(|_| "无法创建应用数据目录")?;
    let data = Dir::open_ambient_dir(path, cap_std::ambient_authority())
        .map_err(|_| "无法打开应用数据目录")?;
    child_dir(&data, kind, true)
}

pub(crate) fn child_dir(parent: &Dir, name: &str, create: bool) -> Result<Dir, String> {
    if create {
        match parent.create_dir(name) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err("无法创建项目目录".into()),
        }
    }
    parent
        .open_dir_nofollow(name)
        .map_err(|_| "目录不存在或包含符号链接".into())
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

fn project_dir(app: &tauri::AppHandle, id: &str) -> Result<Dir, String> {
    project_dir_at(&root(app, "projects")?, id)
}

fn project_dir_at(projects: &Dir, id: &str) -> Result<Dir, String> {
    if !valid_project_id(id) {
        return Err("项目 ID 无效".into());
    }
    child_dir(projects, id, false)
}

pub(crate) struct ProjectFile {
    pub(crate) parent: Dir,
    pub(crate) name: String,
    // Keep the directories themselves, not absolute paths that another process
    // could redirect between validation and use. Also used for empty-dir pruning.
    pub(crate) ancestors: Vec<(Dir, String)>,
}

fn file_path(dir: &Dir, path: &str, create_parents: bool) -> Result<ProjectFile, String> {
    if !valid_path(path) {
        return Err(format!("文件路径无效：{path}"));
    }
    let mut parts = path.split('/').peekable();
    let mut parent = dir.try_clone().map_err(|_| "无法打开项目目录")?;
    let mut ancestors = vec![];
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            return Ok(ProjectFile {
                parent,
                name: part.into(),
                ancestors,
            });
        }
        let child = child_dir(&parent, part, create_parents)?;
        ancestors.push((parent, part.into()));
        parent = child;
    }
    unreachable!("valid_path rejects empty paths")
}

impl ProjectFile {
    fn regular_or_missing(&self) -> Result<(), String> {
        match self.parent.symlink_metadata(&self.name) {
            Ok(meta) if meta.is_file() => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            _ => Err("只能访问项目内的普通文件，不能使用符号链接".into()),
        }
    }

    pub(crate) fn read(&self, limit: u64) -> Result<String, String> {
        self.regular_or_missing()?;
        let file = self
            .parent
            .open_with(
                &self.name,
                OpenOptions::new().read(true).follow(FollowSymlinks::No),
            )
            .map_err(|_| "文件不存在或不是项目内的普通文件")?;
        // Check the opened file rather than checking a path and then reopening it.
        let meta = file.metadata().map_err(|_| "无法读取文件信息")?;
        if !meta.is_file() || meta.len() > limit {
            return Err("文件不是普通文件或超出大小限制".into());
        }
        let mut bytes = Vec::new();
        file.take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "无法读取文件")?;
        if bytes.len() as u64 > limit {
            return Err("文件超出大小限制".into());
        }
        String::from_utf8(bytes).map_err(|_| "文件不是 UTF-8 文本".into())
    }

    pub(crate) fn write(&self, content: &str) -> Result<(), String> {
        self.write_temporary(content, &format!(".gizmoth-{}.tmp", uuid::Uuid::new_v4()))
    }

    fn write_temporary(&self, content: &str, temporary: &str) -> Result<(), String> {
        self.regular_or_missing()?;
        // Exclusive creation never truncates a planted symlink or hardlink.
        // The destination is one component relative to this same pinned parent;
        // even a last-moment destination symlink is replaced, never followed.
        let mut file = self
            .parent
            .open_with(
                temporary,
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .follow(FollowSymlinks::No),
            )
            .map_err(|_| "无法创建临时文件")?;
        let result = file
            .write_all(content.as_bytes())
            .and_then(|()| file.sync_all());
        drop(file);
        let result = result.and_then(|()| self.parent.rename(temporary, &self.parent, &self.name));
        if result.is_err() {
            let _ = self.parent.remove_file(temporary);
        }
        result.map_err(|_| "无法写入文件".into())
    }

    fn remove(self) -> Result<(), String> {
        self.regular_or_missing()?;
        self.parent
            .remove_file(&self.name)
            .map_err(|_| "无法删除文件")?;
        // Windows directory capabilities deny delete-sharing. Close each child
        // before asking its pinned parent to prune the now-empty directory.
        drop(self.parent);
        for (parent, name) in self.ancestors.into_iter().rev() {
            if parent.remove_dir(&name).is_err() {
                break;
            }
        }
        Ok(())
    }
}

fn walk(dir: &Dir, prefix: &str, out: &mut Vec<FileEntry>) -> Result<(), String> {
    let mut entries: Vec<_> = dir
        .entries()
        .map_err(|_| "无法读取项目目录")?
        .collect::<Result<_, _>>()
        .map_err(|_| "无法读取项目目录")?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name();
        let text = name.to_string_lossy();
        if text.starts_with('.') {
            continue;
        }
        let path = if prefix.is_empty() {
            text.to_string()
        } else {
            format!("{prefix}/{text}")
        };
        let meta = dir
            .symlink_metadata(&name)
            .map_err(|_| "无法读取项目目录")?;
        if meta.is_dir() {
            // Reopen each single component with no-follow, so a directory swapped
            // for a link after enumeration cannot redirect the walk or snapshot.
            let child = dir
                .open_dir_nofollow(&name)
                .map_err(|_| "无法打开项目目录")?;
            walk(&child, &path, out)?;
        } else if meta.is_file() {
            out.push(FileEntry {
                path,
                size: meta.len(),
            });
        }
        if out.len() > MAX_FILES {
            return Err("项目文件过多".into());
        }
    }
    Ok(())
}

fn files_at(dir: &Dir) -> Result<Vec<FileEntry>, String> {
    let mut out = vec![];
    walk(dir, "", &mut out)?;
    Ok(out)
}

pub fn files(app: &tauri::AppHandle, id: &str) -> Result<Vec<FileEntry>, String> {
    files_at(&project_dir(app, id)?)
}

fn summary(dir: &Dir, id: String) -> ProjectSummary {
    let meta: Option<Meta> = file_path(dir, "app.json", false)
        .and_then(|file| file.read(MAX_PROJECT_BYTES))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let mut list = vec![];
    let _ = walk(dir, "", &mut list);
    let updated_at = list
        .iter()
        .filter_map(|f| {
            let file = file_path(dir, &f.path, false).ok()?;
            file.parent
                .symlink_metadata(&file.name)
                .ok()?
                .modified()
                .ok()
        })
        .chain(dir.dir_metadata().ok().and_then(|m| m.modified().ok()))
        .filter_map(|t| t.into_std().duration_since(UNIX_EPOCH).ok())
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
    let projects = root(app, "projects")?;
    for entry in projects.entries().map_err(|_| "无法读取项目列表")? {
        let Ok(entry) = entry else { continue };
        let id = entry.file_name().to_string_lossy().to_string();
        if let Ok(dir) = project_dir_at(&projects, &id) {
            out.push(summary(&dir, id));
        }
    }
    out.sort_by_key(|p| std::cmp::Reverse(p.updated_at));
    Ok(out)
}

pub fn create(app: &tauri::AppHandle, app_id: &str, name: &str) -> Result<ProjectSummary, String> {
    let _guard = project_lock()?;
    create_at(&root(app, "projects")?, app_id, name)
}

fn create_at(projects: &Dir, app_id: &str, name: &str) -> Result<ProjectSummary, String> {
    if !project::valid_id(app_id) {
        return Err("程序 ID 需为 1–32 位小写字母、数字、下划线、点或连字符。".into());
    }
    let name = name.trim();
    if name.is_empty() || name.len() > 128 {
        return Err("项目名称需为 1–128 字节".into());
    }
    let base = app_id.replace(['_', '.'], "-");
    let mut suffix = 0u64;
    let id = loop {
        let id = if suffix == 0 {
            base.clone()
        } else {
            format!("{base}-{suffix}")
        };
        // mkdir is exclusive, including for dangling links. Never reuse a
        // directory selected by an earlier exists() check.
        match projects.create_dir(&id) {
            Ok(()) => break id,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => suffix += 1,
            Err(_) => return Err("无法创建项目目录".into()),
        }
    };
    let dir = project_dir_at(projects, &id)?;
    let config =
        json!({ "app_id": app_id, "version": "0.1.0", "display_name": name, "description": "" });
    let entry = format!(
        "-- {name}\n-- Runs on the device after it is installed and started.\n\nlocal message = \"Hello from {app_id}\"\nprint(message)\n\nreturn {{ message = message }}\n"
    );
    let write = || -> Result<(), String> {
        let content = serde_json::to_string_pretty(&config).map_err(|_| "无法生成项目配置")? + "\n";
        file_path(&dir, "app.json", false)?.write(&content)?;
        file_path(&dir, &format!("{app_id}.lua"), false)?.write(&entry)
    };
    if write().is_err() {
        let _ = dir.remove_open_dir_all();
        return Err("无法写入项目文件".into());
    }
    Ok(summary(&dir, id))
}

pub fn read(app: &tauri::AppHandle, id: &str, path: &str) -> Result<String, String> {
    file_path(&project_dir(app, id)?, path, false)?.read(MAX_PROJECT_BYTES)
}

pub fn write(app: &tauri::AppHandle, id: &str, path: &str, content: &str) -> Result<(), String> {
    let _guard = project_lock()?;
    write_unlocked(app, id, path, content)
}

fn write_unlocked(
    app: &tauri::AppHandle,
    id: &str,
    path: &str,
    content: &str,
) -> Result<(), String> {
    write_at(&project_dir(app, id)?, path, content)
}

fn write_at(dir: &Dir, path: &str, content: &str) -> Result<(), String> {
    if !valid_path(path) {
        return Err(format!("文件路径无效：{path}"));
    }
    let existing = files_at(dir)?;
    let previous = existing
        .iter()
        .find(|f| f.path == path)
        .map(|f| f.size)
        .unwrap_or(0);
    if !existing.iter().any(|f| f.path == path) && existing.len() >= MAX_FILES {
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
    file_path(dir, path, true)?.write(content)
}

pub fn remove(app: &tauri::AppHandle, id: &str, path: &str) -> Result<(), String> {
    let _guard = project_lock()?;
    remove_unlocked(app, id, path)
}

fn remove_unlocked(app: &tauri::AppHandle, id: &str, path: &str) -> Result<(), String> {
    remove_at(&project_dir(app, id)?, path)
}

fn remove_at(dir: &Dir, path: &str) -> Result<(), String> {
    if path.eq_ignore_ascii_case("app.json") {
        return Err("app.json 不能删除".into());
    }
    file_path(dir, path, false)?.remove()
}

pub fn rename(app: &tauri::AppHandle, id: &str, from: &str, to: &str) -> Result<(), String> {
    let _guard = project_lock()?;
    rename_at(&project_dir(app, id)?, from, to)
}

fn rename_at(dir: &Dir, from: &str, to: &str) -> Result<(), String> {
    if from.eq_ignore_ascii_case("app.json") || to.eq_ignore_ascii_case("app.json") {
        return Err("app.json 不能重命名".into());
    }
    // Resolve the source only once. A replaced ancestor must not redirect either
    // the read or the later removal to a different directory.
    let source = file_path(dir, from, false)?;
    let content = source.read(MAX_PROJECT_BYTES)?;
    if files_at(dir)?
        .iter()
        .any(|f| f.path.eq_ignore_ascii_case(to))
    {
        return Err("目标文件已存在".into());
    }
    write_at(dir, to, &content)?;
    source.remove()
}

pub fn delete(app: &tauri::AppHandle, id: &str) -> Result<(), String> {
    let _guard = project_lock()?;
    project_dir(app, id)?
        .remove_open_dir_all()
        .map_err(|_| "无法删除项目")?;
    let _ = root(app, "chats")?.remove_file(format!("{id}.json"));
    Ok(())
}

/// Snapshot the directory as a package source. Only UTF-8 text payloads are
/// supported by this version.
pub fn load(app: &tauri::AppHandle, id: &str) -> Result<Project, String> {
    let _guard = project_lock()?;
    snapshot(&project_dir(app, id)?)
}

fn snapshot(dir: &Dir) -> Result<Project, String> {
    let entries = files_at(dir)?;
    let mut files = vec![];
    let mut total = 0;
    for entry in entries {
        let content = file_path(dir, &entry.path, false)?.read(MAX_PROJECT_BYTES)?;
        total += content.len() as u64;
        if total > MAX_PROJECT_BYTES {
            return Err("项目内容超过 2 MiB 限制".into());
        }
        files.push(SourceFile {
            path: entry.path,
            content,
        });
    }
    Ok(Project { version: 1, files })
}

pub fn chat_load(app: &tauri::AppHandle, id: &str) -> Result<Option<String>, String> {
    project_dir(app, id)?;
    let file = file_path(&root(app, "chats")?, &format!("{id}.json"), false)?;
    match file.parent.symlink_metadata(&file.name) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("无法读取对话记录".into()),
        Ok(_) => file.read(MAX_CHAT_BYTES as u64).map(Some),
    }
}

pub fn chat_save(app: &tauri::AppHandle, id: &str, json: &str) -> Result<(), String> {
    project_dir(app, id)?;
    if json.len() > MAX_CHAT_BYTES || serde_json::from_str::<serde_json::Value>(json).is_err() {
        return Err("对话记录无效或过大".into());
    }
    file_path(&root(app, "chats")?, &format!("{id}.json"), false)?.write(json)
}

pub fn reveal_dir(app: &tauri::AppHandle, id: &str) -> Result<String, String> {
    project_dir(app, id)?;
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|_| "无法定位应用数据目录")?
        .join("projects")
        .join(id)
        .to_string_lossy()
        .into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    struct Fixture {
        // Close the directory capability before TempDir removes its tree on Windows.
        projects: Dir,
        temp: tempfile::TempDir,
        beta: Project,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let data = Dir::open_ambient_dir(temp.path(), cap_std::ambient_authority()).unwrap();
            let projects = child_dir(&data, "projects", true).unwrap();
            create_at(&projects, "alpha", "Alpha").unwrap();
            create_at(&projects, "beta", "Beta").unwrap();
            let beta = snapshot(&project_dir_at(&projects, "beta").unwrap()).unwrap();
            Self {
                temp,
                projects,
                beta,
            }
        }

        fn alpha(&self) -> Dir {
            project_dir_at(&self.projects, "alpha").unwrap()
        }

        fn path(&self, id: &str, path: &str) -> PathBuf {
            let root = self.temp.path().join("projects").join(id);
            if path.is_empty() {
                root
            } else {
                root.join(path)
            }
        }

        fn assert_beta_unchanged(&self) {
            let beta = snapshot(&project_dir_at(&self.projects, "beta").unwrap()).unwrap();
            let contents = |p: &Project| {
                p.files
                    .iter()
                    .map(|f| (f.path.clone(), f.content.clone()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(contents(&beta), contents(&self.beta));
            // Also catch newly created empty directories, which snapshots omit.
            assert_eq!(fs::read_dir(self.path("beta", "")).unwrap().count(), 2);
        }
    }

    #[cfg(unix)]
    fn link_dir(target: &Path, link: &Path) -> bool {
        std::os::unix::fs::symlink(target, link).unwrap();
        true
    }

    #[cfg(unix)]
    fn link_file(target: &Path, link: &Path) -> bool {
        std::os::unix::fs::symlink(target, link).unwrap();
        true
    }

    #[cfg(windows)]
    fn allow_windows_link(result: io::Result<()>) -> bool {
        match result {
            Ok(()) => true,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                eprintln!("symlink fixture unavailable without Windows Developer Mode: {e}");
                false
            }
            Err(e) => panic!("cannot create symlink fixture: {e}"),
        }
    }

    #[cfg(windows)]
    fn link_dir(target: &Path, link: &Path) -> bool {
        allow_windows_link(std::os::windows::fs::symlink_dir(target, link))
    }

    #[cfg(windows)]
    fn link_file(target: &Path, link: &Path) -> bool {
        allow_windows_link(std::os::windows::fs::symlink_file(target, link))
    }

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

    #[test]
    fn every_mutation_rejects_absolute_parent_and_other_project_paths() {
        let f = Fixture::new();
        let alpha = f.alpha();
        let source = file_path(&alpha, "alpha.lua", false)
            .unwrap()
            .read(MAX_PROJECT_BYTES)
            .unwrap();
        let absolute = f.path("beta", "beta.lua").to_string_lossy().into_owned();
        for path in [
            absolute.as_str(),
            "../beta/beta.lua",
            "a/../../beta/beta.lua",
            "/abs.lua",
            "a//x",
            "a/./x",
            "a\\..\\beta.lua",
            "C:beta.lua",
        ] {
            assert!(file_path(&alpha, path, false).is_err(), "read {path}");
            assert!(write_at(&alpha, path, "outside").is_err(), "write {path}");
            assert!(remove_at(&alpha, path).is_err(), "delete {path}");
            assert!(
                rename_at(&alpha, "alpha.lua", path).is_err(),
                "rename to {path}"
            );
            assert!(
                rename_at(&alpha, path, "alpha/new.lua").is_err(),
                "rename from {path}"
            );
        }
        for id in ["../beta", "/beta", "alpha/../beta", "..\\beta"] {
            assert!(project_dir_at(&f.projects, id).is_err());
            assert!(create_at(&f.projects, id, "Invalid").is_err());
        }
        assert_eq!(
            file_path(&alpha, "alpha.lua", false)
                .unwrap()
                .read(MAX_PROJECT_BYTES)
                .unwrap(),
            source
        );
        f.assert_beta_unchanged();
    }

    #[test]
    fn normal_edits_rename_pruning_and_packaging_still_work() {
        let f = Fixture::new();
        let alpha = f.alpha();
        write_at(&alpha, "alpha/lib/tool.lua", "return '你好'\n").unwrap();
        write_at(&alpha, "alpha/lib/tool.lua", "return 42\n").unwrap();
        rename_at(&alpha, "alpha/lib/tool.lua", "alpha/tools.lua").unwrap();
        assert!(!f.path("alpha", "alpha/lib").exists());
        assert_eq!(
            file_path(&alpha, "alpha/tools.lua", false)
                .unwrap()
                .read(MAX_PROJECT_BYTES)
                .unwrap(),
            "return 42\n"
        );
        project::inspect(&snapshot(&alpha).unwrap()).unwrap();
        let duplicate = create_at(&f.projects, "alpha", "Second project").unwrap();
        assert_eq!(duplicate.id, "alpha-1");
        for path in ["app.json", "APP.JSON", "App.Json"] {
            assert!(remove_at(&alpha, path).is_err());
            assert!(rename_at(&alpha, path, "other.json").is_err());
            assert!(rename_at(&alpha, "alpha/tools.lua", path).is_err());
        }
        assert!(rename_at(&alpha, "alpha/tools.lua", "alpha.lua").is_err());
        remove_at(&alpha, "alpha/tools.lua").unwrap();
        assert!(!f.path("alpha", "alpha").exists());
        project::inspect(&snapshot(&alpha).unwrap()).unwrap();
        f.assert_beta_unchanged();
    }

    #[test]
    fn project_and_store_roots_cannot_be_links() {
        let f = Fixture::new();
        if !link_dir(&f.path("beta", ""), &f.path("alias", "")) {
            return;
        }
        assert!(project_dir_at(&f.projects, "alias").is_err());
        assert!(child_dir(&f.projects, "alias", true).is_err());
        let data = Dir::open_ambient_dir(f.temp.path(), cap_std::ambient_authority()).unwrap();
        assert!(link_dir(
            &f.path("beta", ""),
            &f.temp.path().join("linked-store")
        ));
        assert!(child_dir(&data, "linked-store", true).is_err());
        // Existing (including dangling) links reserve their names. Creating a
        // project chooses a fresh directory instead of following either link.
        let created = create_at(&f.projects, "alias", "New project").unwrap();
        assert_eq!(created.id, "alias-1");
        assert!(link_dir(&f.path("missing", ""), &f.path("dangling", "")));
        let created = create_at(&f.projects, "dangling", "New project").unwrap();
        assert_eq!(created.id, "dangling-1");
        assert!(!f.path("missing", "").exists());
        f.assert_beta_unchanged();
    }

    #[test]
    fn ancestor_links_cannot_read_create_rename_or_delete_sibling_files() {
        let f = Fixture::new();
        let alpha = f.alpha();
        if !link_dir(&f.path("beta", ""), &f.path("alpha", "assets")) {
            return;
        }
        assert!(file_path(&alpha, "assets/beta.lua", false).is_err());
        assert!(write_at(&alpha, "assets/beta.lua", "outside").is_err());
        assert!(write_at(&alpha, "assets/new/deep.lua", "outside").is_err());
        assert!(remove_at(&alpha, "assets/beta.lua").is_err());
        assert!(rename_at(&alpha, "assets/beta.lua", "alpha/stolen.lua").is_err());
        assert!(rename_at(&alpha, "alpha.lua", "assets/new.lua").is_err());
        assert!(file_path(&alpha, "alpha.lua", false)
            .unwrap()
            .read(MAX_PROJECT_BYTES)
            .is_ok());
        assert_eq!(files_at(&alpha).unwrap().len(), 2);
        project::inspect(&snapshot(&alpha).unwrap()).unwrap();
        f.assert_beta_unchanged();
    }

    #[test]
    fn final_file_links_are_rejected_for_every_operation() {
        let f = Fixture::new();
        let alpha = f.alpha();
        if !link_file(&f.path("beta", "beta.lua"), &f.path("alpha", "linked.lua")) {
            return;
        }
        let file = file_path(&alpha, "linked.lua", false).unwrap();
        assert!(file.read(MAX_PROJECT_BYTES).is_err());
        assert!(write_at(&alpha, "linked.lua", "outside").is_err());
        assert!(remove_at(&alpha, "linked.lua").is_err());
        assert!(rename_at(&alpha, "linked.lua", "alpha/copied.lua").is_err());
        assert!(rename_at(&alpha, "alpha.lua", "linked.lua").is_err());
        assert!(fs::symlink_metadata(f.path("alpha", "linked.lua"))
            .unwrap()
            .file_type()
            .is_symlink());
        f.assert_beta_unchanged();
    }

    #[test]
    fn predictable_temp_symlinks_and_exclusive_creation_collisions_cannot_write_outside() {
        let f = Fixture::new();
        let alpha = f.alpha();
        if !link_file(
            &f.path("beta", "beta.lua"),
            &f.path("alpha", "safe.gizmoth-tmp"),
        ) {
            return;
        }
        write_at(&alpha, "safe.lua", "safe replacement").unwrap();
        assert!(link_file(
            &f.path("beta", "beta.lua"),
            &f.path("alpha", ".collision.tmp")
        ));
        let target = file_path(&alpha, "new.lua", false).unwrap();
        assert!(target.write_temporary("outside", ".collision.tmp").is_err());
        assert!(!f.path("alpha", "new.lua").exists());
        assert!(fs::symlink_metadata(f.path("alpha", ".collision.tmp"))
            .unwrap()
            .file_type()
            .is_symlink());
        f.assert_beta_unchanged();
    }

    #[test]
    fn hardlinked_files_and_temp_traps_do_not_change_external_inodes() {
        let f = Fixture::new();
        let alpha = f.alpha();
        fs::hard_link(
            f.path("beta", "beta.lua"),
            f.path("alpha", "safe.gizmoth-tmp"),
        )
        .unwrap();
        fs::hard_link(
            f.path("beta", "beta.lua"),
            f.path("alpha", ".collision.tmp"),
        )
        .unwrap();
        fs::hard_link(f.path("beta", "beta.lua"), f.path("alpha", "shared.lua")).unwrap();
        write_at(&alpha, "safe.lua", "new file").unwrap();
        let target = file_path(&alpha, "new.lua", false).unwrap();
        assert!(target.write_temporary("outside", ".collision.tmp").is_err());
        write_at(&alpha, "shared.lua", "new inode").unwrap();
        assert_eq!(
            file_path(&alpha, "shared.lua", false)
                .unwrap()
                .read(MAX_PROJECT_BYTES)
                .unwrap(),
            "new inode"
        );
        fs::hard_link(f.path("beta", "beta.lua"), f.path("alpha", "renamed.lua")).unwrap();
        rename_at(&alpha, "renamed.lua", "alpha/renamed.lua").unwrap();
        remove_at(&alpha, "safe.gizmoth-tmp").unwrap();
        f.assert_beta_unchanged();
    }

    #[cfg(unix)]
    #[test]
    fn replacing_a_resolved_parent_with_a_link_cannot_redirect_operations() {
        let f = Fixture::new();
        let alpha = f.alpha();
        write_at(&alpha, "assets/beta.lua", "original project").unwrap();
        let file = file_path(&alpha, "assets/beta.lua", false).unwrap();
        fs::rename(f.path("alpha", "assets"), f.path("alpha", "held")).unwrap();
        assert!(link_dir(&f.path("beta", ""), &f.path("alpha", "assets")));
        assert_eq!(file.read(MAX_PROJECT_BYTES).unwrap(), "original project");
        file.write("still original project").unwrap();
        assert_eq!(
            fs::read_to_string(f.path("alpha", "held/beta.lua")).unwrap(),
            "still original project"
        );
        file.remove().unwrap();
        assert!(!f.path("alpha", "held/beta.lua").exists());
        assert!(file_path(&alpha, "assets/beta.lua", false).is_err());
        f.assert_beta_unchanged();
    }

    #[cfg(unix)]
    #[test]
    fn replacing_a_project_root_does_not_redirect_an_open_project() {
        let f = Fixture::new();
        let alpha = f.alpha();
        fs::rename(f.path("alpha", ""), f.path("held", "")).unwrap();
        assert!(link_dir(&f.path("beta", ""), &f.path("alpha", "")));
        assert!(project_dir_at(&f.projects, "alpha").is_err());
        write_at(&alpha, "new.lua", "original project handle").unwrap();
        assert_eq!(
            fs::read_to_string(f.path("held", "new.lua")).unwrap(),
            "original project handle"
        );
        f.assert_beta_unchanged();
    }

    #[test]
    fn recursive_project_deletion_never_follows_sibling_links() {
        let f = Fixture::new();
        let alpha = f.alpha();
        if !link_dir(&f.path("beta", ""), &f.path("alpha", "assets")) {
            return;
        }
        alpha.remove_open_dir_all().unwrap();
        assert!(!f.path("alpha", "").exists());
        f.assert_beta_unchanged();
    }
}
