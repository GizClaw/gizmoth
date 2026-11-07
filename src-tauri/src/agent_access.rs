//! What the coding assistant may do beyond its own project, set per project by
//! the user and enforced here rather than in the webview's tool code:
//! files under the user's home folder (none / read / read-write), installing
//! to devices (ask / allow / deny, applied by the tool) and web access.
//!
//! Home access is confined with cap-std: every component is opened relative to
//! a pinned home directory without following symlinks, so a link inside the
//! home folder can never lead outside it. Credentials, this app's own data and
//! (for writes) startup and system configuration are always refused.

use crate::workspace::{child_dir, ProjectFile};
use cap_std::fs::Dir;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};
use tauri::Manager;

const MAX_HOME_READ: u64 = 1024 * 1024;
const MAX_HOME_WRITE: usize = 1024 * 1024;
const MAX_LIST: usize = 500;
const MAX_FETCH_BYTES: usize = 2 * 1024 * 1024;
const SEARCH_SERVICE: &str = "com.gizclaw.gizmoth.search";

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum FileAccess {
    None,
    #[default]
    Read,
    Write,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum RunAccess {
    #[default]
    Ask,
    Allow,
    Deny,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum WebAccess {
    #[default]
    Allow,
    Deny,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(default)]
pub struct Permissions {
    pub files: FileAccess,
    pub run: RunAccess,
    pub web: WebAccess,
}

pub struct AccessStore(pub Mutex<()>);

fn data_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|_| "无法定位应用数据目录")?;
    fs::create_dir_all(&dir).map_err(|_| "无法创建应用数据目录")?;
    Ok(dir)
}

fn read_all(app: &tauri::AppHandle) -> Result<BTreeMap<String, Permissions>, String> {
    match fs::read(data_dir(app)?.join("agent-permissions.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| "助手权限配置损坏".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(_) => Err("无法读取助手权限配置".into()),
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

pub fn permissions(app: &tauri::AppHandle, id: &str) -> Result<Permissions, String> {
    if !valid_id(id) {
        return Err("项目 ID 无效".into());
    }
    Ok(read_all(app)?.get(id).copied().unwrap_or_default())
}

pub fn set_permissions(app: &tauri::AppHandle, id: &str, value: Permissions) -> Result<(), String> {
    if !valid_id(id) {
        return Err("项目 ID 无效".into());
    }
    let store = app.state::<AccessStore>();
    let _guard = store.0.lock().map_err(|_| "助手权限配置被占用")?;
    let mut all = read_all(app)?;
    all.insert(id.into(), value);
    let path = data_dir(app)?.join("agent-permissions.json");
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let bytes = serde_json::to_vec_pretty(&all).map_err(|_| "无法保存助手权限")?;
    fs::write(&temporary, bytes).map_err(|_| "无法保存助手权限")?;
    fs::rename(&temporary, path).map_err(|_| "无法保存助手权限".into())
}

// Home folder ---------------------------------------------------------------

fn home_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    #[cfg(feature = "native-e2e")]
    return crate::e2e::home_override();
    #[cfg(not(feature = "native-e2e"))]
    app.path().home_dir().map_err(|_| "无法定位用户目录".into())
}

/// Turn "~/a/b", "/Users/me/a/b" or "a/b" into components relative to home.
pub fn home_components(home: &Path, input: &str) -> Result<Vec<String>, String> {
    let raw = input.trim();
    if raw.is_empty() || raw.len() > 1024 || raw.chars().any(|c| c.is_control()) {
        return Err("路径无效".into());
    }
    #[cfg(windows)]
    let raw = raw.replace('\\', "/");
    #[cfg(windows)]
    let raw = raw.as_str();
    let home_text = home.to_string_lossy().replace('\\', "/");
    let relative = if raw == "~" {
        ""
    } else if let Some(rest) = raw.strip_prefix("~/") {
        rest
    } else if let Some(rest) = raw.strip_prefix(home_text.trim_end_matches('/')) {
        if !(rest.is_empty() || rest.starts_with('/')) {
            return Err("只能访问用户目录内的路径".into());
        }
        rest
    } else if raw.starts_with('/') || raw.contains(':') {
        return Err("只能访问用户目录内的路径".into());
    } else {
        raw
    };
    let mut parts = vec![];
    for part in relative.split('/') {
        match part {
            "" | "." => continue,
            ".." => return Err("路径不能包含 ..".into()),
            _ => parts.push(part.to_owned()),
        }
    }
    Ok(parts)
}

/// Locations that are refused whatever the setting: credentials and secrets.
const SECRET_PREFIXES: &[&[&str]] = &[
    &[".ssh"],
    &[".gnupg"],
    &[".aws"],
    &[".azure"],
    &[".kube"],
    &[".docker"],
    &[".netrc"],
    &[".password-store"],
    &[".config", "gh"],
    &[".config", "gcloud"],
    &["Library", "Keychains"],
    &["Library", "Cookies"],
];

fn starts_with(parts: &[String], prefix: &[&str]) -> bool {
    parts.len() >= prefix.len()
        && parts
            .iter()
            .zip(prefix)
            .all(|(part, want)| part.eq_ignore_ascii_case(want))
}

/// Why a home path is refused, or None when the policy allows it.
pub fn refusal(parts: &[String], app_data: &[String], write: bool) -> Option<&'static str> {
    if SECRET_PREFIXES
        .iter()
        .any(|prefix| starts_with(parts, prefix))
    {
        return Some("这个位置保存凭据，助手不能访问");
    }
    if !app_data.is_empty()
        && parts.len() >= app_data.len()
        && parts
            .iter()
            .zip(app_data)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    {
        return Some("GizMoth 自己的数据只能通过项目工具访问");
    }
    if write {
        if parts.is_empty() {
            return Some("不能写入用户目录本身");
        }
        // Shell startup files, dotfolders and ~/Library run code or hold system configuration.
        if parts[0].starts_with('.') || parts[0].eq_ignore_ascii_case("Library") {
            return Some("隐藏的配置文件和资源库（Library）只读，助手不能写入");
        }
    }
    None
}

struct Home {
    dir: Dir,
    app_data: Vec<String>,
}

fn open_home(app: &tauri::AppHandle) -> Result<Home, String> {
    let home = home_path(app)?;
    let app_data = app
        .path()
        .app_data_dir()
        .ok()
        .and_then(|dir| dir.strip_prefix(&home).ok().map(Path::to_path_buf))
        .map(|rel| {
            rel.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    let dir = Dir::open_ambient_dir(&home, cap_std::ambient_authority())
        .map_err(|_| "无法打开用户目录")?;
    Ok(Home { dir, app_data })
}

fn require(app: &tauri::AppHandle, id: &str, write: bool) -> Result<(), String> {
    match (permissions(app, id)?.files, write) {
        (FileAccess::None, _) => Err(
            "这个项目没有允许助手访问项目外的文件。可以在「项目信息 › 助手权限」里打开。".into(),
        ),
        (FileAccess::Read, true) => {
            Err("项目外的文件设为只读，不能写入。可以在「项目信息 › 助手权限」里改为读写。".into())
        }
        _ => Ok(()),
    }
}

fn open_dir(home: &Home, parts: &[String], create: bool) -> Result<Dir, String> {
    let mut dir = home.dir.try_clone().map_err(|_| "无法打开用户目录")?;
    for part in parts {
        dir = child_dir(&dir, part, create)
            .map_err(|_| format!("无法打开目录 {part}（不存在或是符号链接）"))?;
    }
    Ok(dir)
}

fn home_file(home: &Home, parts: &[String], create: bool) -> Result<ProjectFile, String> {
    let (name, folders) = parts.split_last().ok_or("需要文件路径")?;
    Ok(ProjectFile {
        parent: open_dir(home, folders, create)?,
        name: name.clone(),
        ancestors: vec![],
    })
}

#[derive(Serialize)]
pub struct HomeEntry {
    name: String,
    kind: &'static str,
    size: u64,
}

pub fn list(app: &tauri::AppHandle, id: &str, path: &str) -> Result<Vec<HomeEntry>, String> {
    require(app, id, false)?;
    let home = open_home(app)?;
    let parts = home_components(&home_path(app)?, path)?;
    if let Some(reason) = refusal(&parts, &home.app_data, false) {
        return Err(reason.into());
    }
    let dir = open_dir(&home, &parts, false)?;
    let mut out = vec![];
    for entry in dir.entries().map_err(|_| "无法读取目录")? {
        let entry = entry.map_err(|_| "无法读取目录")?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let meta = dir.symlink_metadata(&name).map_err(|_| "无法读取目录")?;
        let kind = if meta.is_dir() {
            "dir"
        } else if meta.is_file() {
            "file"
        } else if meta.file_type().is_symlink() {
            "link"
        } else {
            continue;
        };
        out.push(HomeEntry {
            name,
            kind,
            size: if meta.is_file() { meta.len() } else { 0 },
        });
        if out.len() >= MAX_LIST {
            break;
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

pub fn read(app: &tauri::AppHandle, id: &str, path: &str) -> Result<String, String> {
    require(app, id, false)?;
    let home = open_home(app)?;
    let parts = home_components(&home_path(app)?, path)?;
    if let Some(reason) = refusal(&parts, &home.app_data, false) {
        return Err(reason.into());
    }
    home_file(&home, &parts, false)?.read(MAX_HOME_READ)
}

pub fn write(app: &tauri::AppHandle, id: &str, path: &str, content: &str) -> Result<(), String> {
    require(app, id, true)?;
    if content.len() > MAX_HOME_WRITE {
        return Err("内容超过 1 MiB".into());
    }
    let home = open_home(app)?;
    let parts = home_components(&home_path(app)?, path)?;
    if let Some(reason) = refusal(&parts, &home.app_data, true) {
        return Err(reason.into());
    }
    home_file(&home, &parts, true)?.write(content)
}

// Web -----------------------------------------------------------------------

fn require_web(app: &tauri::AppHandle, id: &str) -> Result<(), String> {
    match permissions(app, id)?.web {
        WebAccess::Allow => Ok(()),
        WebAccess::Deny => {
            Err("这个项目没有允许助手联网。可以在「项目信息 › 助手权限」里打开。".into())
        }
    }
}

/// Only public internet addresses: never this machine, the LAN or link-local devices.
pub fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64 // CGNAT, Tailscale
                || v4.octets()[0] == 0)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return public_address(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00 // unique local
                || (first & 0xffc0) == 0xfe80) // link local
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Fetched {
    status: u16,
    url: String,
    content_type: String,
    location: Option<String>,
    body: String,
    truncated: bool,
}

pub async fn fetch(app: &tauri::AppHandle, id: &str, url: &str) -> Result<Fetched, String> {
    require_web(app, id)?;
    let parsed = url::Url::parse(url.trim()).map_err(|_| "网址无效")?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err("只能打开不含账号信息的 http/https 网址".into());
    }
    let host = parsed.host_str().ok_or("网址缺少主机名")?.to_owned();
    let port = parsed.port_or_known_default().ok_or("网址端口无效")?;
    let addresses: Vec<_> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|_| format!("无法解析 {host}"))?
        .collect();
    if addresses.is_empty() || !addresses.iter().all(|a| public_address(a.ip())) {
        return Err("只能访问公网地址，不能访问本机、局域网或设备地址".into());
    }
    // Pin the checked address so a second DNS answer cannot point somewhere private.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .resolve(&host, addresses[0])
        .user_agent("GizMoth/0.1 (+coding assistant)")
        .build()
        .map_err(|_| "无法创建网络请求")?;
    let mut response = client
        .get(parsed.clone())
        .send()
        .await
        .map_err(|e| format!("请求失败：{e}"))?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    // Redirects are returned, not followed: the next hop gets the same address check.
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| parsed.join(v).ok())
        .map(|u| u.to_string());
    let mut bytes = Vec::new();
    let mut truncated = false;
    while let Some(chunk) = response.chunk().await.map_err(|_| "读取网页失败")? {
        if bytes.len() + chunk.len() > MAX_FETCH_BYTES {
            bytes.extend_from_slice(&chunk[..MAX_FETCH_BYTES - bytes.len()]);
            truncated = true;
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    let textual = content_type.is_empty()
        || content_type.starts_with("text/")
        || ["json", "xml", "javascript", "html"]
            .iter()
            .any(|t| content_type.contains(t));
    let body = if textual {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        format!("(非文本内容：{content_type}，{} 字节)", bytes.len())
    };
    Ok(Fetched {
        status,
        url: parsed.to_string(),
        content_type,
        location,
        body,
        truncated,
    })
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum SearchProvider {
    Brave,
    Tavily,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchSettings {
    provider: Option<SearchProvider>,
    has_key: bool,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

fn search_provider(app: &tauri::AppHandle) -> Result<Option<SearchProvider>, String> {
    match fs::read(data_dir(app)?.join("search.json")) {
        Ok(bytes) => Ok(serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|v| serde_json::from_value(v["provider"].clone()).ok())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("无法读取联网搜索设置".into()),
    }
}

fn search_key_entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(SEARCH_SERVICE, "api-key").map_err(|_| "系统凭据存储不可用".into())
}

pub fn search_settings(app: &tauri::AppHandle) -> Result<SearchSettings, String> {
    let provider = search_provider(app)?;
    let has_key = provider.is_some()
        && match search_key_entry()?.get_password() {
            Ok(key) => !key.is_empty(),
            Err(keyring::Error::NoEntry) => false,
            Err(_) => return Err("无法读取联网搜索密钥".into()),
        };
    Ok(SearchSettings { provider, has_key })
}

pub fn save_search(
    app: &tauri::AppHandle,
    provider: Option<SearchProvider>,
    key: Option<String>,
) -> Result<SearchSettings, String> {
    let path = data_dir(app)?.join("search.json");
    match provider {
        None => {
            let _ = fs::remove_file(&path);
            match search_key_entry()?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(_) => return Err("无法删除联网搜索密钥".into()),
            }
        }
        Some(provider) => {
            if let Some(key) = key.map(|k| k.trim().to_owned()).filter(|k| !k.is_empty()) {
                if key.len() > 512 || key.chars().any(|c| c.is_control() || c.is_whitespace()) {
                    return Err("API Key 格式无效".into());
                }
                search_key_entry()?
                    .set_password(&key)
                    .map_err(|_| "无法保存联网搜索密钥")?;
            }
            let bytes = serde_json::to_vec(&serde_json::json!({ "provider": provider }))
                .map_err(|_| "无法保存设置")?;
            fs::write(&path, bytes).map_err(|_| "无法保存联网搜索设置")?;
        }
    }
    search_settings(app)
}

pub fn parse_brave(body: &serde_json::Value) -> Vec<SearchResult> {
    body["web"]["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            Some(SearchResult {
                title: r["title"].as_str()?.to_owned(),
                url: r["url"].as_str()?.to_owned(),
                snippet: r["description"].as_str().unwrap_or("").to_owned(),
            })
        })
        .collect()
}

pub fn parse_tavily(body: &serde_json::Value) -> Vec<SearchResult> {
    body["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            Some(SearchResult {
                title: r["title"].as_str()?.to_owned(),
                url: r["url"].as_str()?.to_owned(),
                snippet: r["content"].as_str().unwrap_or("").to_owned(),
            })
        })
        .collect()
}

pub async fn search(
    app: &tauri::AppHandle,
    id: &str,
    query: &str,
) -> Result<Vec<SearchResult>, String> {
    require_web(app, id)?;
    let query = query.trim();
    if query.is_empty() || query.len() > 400 {
        return Err("搜索词为空或太长".into());
    }
    let provider = search_provider(app)?
        .ok_or("还没有配置联网搜索。请在「设置 › 联网搜索」里选择服务并填写 API Key。")?;
    let key = match search_key_entry()?.get_password() {
        Ok(key) if !key.is_empty() => key,
        _ => return Err("联网搜索缺少 API Key，请在「设置 › 联网搜索」里填写。".into()),
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| "无法创建网络请求")?;
    let response = match provider {
        SearchProvider::Brave => {
            client
                .get("https://api.search.brave.com/res/v1/web/search")
                .query(&[("q", query), ("count", "8")])
                .header("X-Subscription-Token", key)
                .header("Accept", "application/json")
                .send()
                .await
        }
        SearchProvider::Tavily => {
            client
                .post("https://api.tavily.com/search")
                .bearer_auth(key)
                .json(&serde_json::json!({ "query": query, "max_results": 8 }))
                .send()
                .await
        }
    }
    .map_err(|e| format!("搜索请求失败：{e}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            401 | 403 => "搜索服务拒绝了 API Key，请在设置里检查。".into(),
            429 => "搜索次数已达上限，请稍后再试。".into(),
            code => format!("搜索服务返回 {code}"),
        });
    }
    let body: serde_json::Value = response.json().await.map_err(|_| "搜索结果格式无效")?;
    Ok(match provider {
        SearchProvider::Brave => parse_brave(&body),
        SearchProvider::Tavily => parse_tavily(&body),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn parts(path: &str) -> Vec<String> {
        home_components(Path::new("/Users/me"), path).unwrap()
    }

    #[test]
    fn home_paths_resolve_relative_to_home_and_never_escape() {
        assert_eq!(parts("~/Repos/app/main.lua"), ["Repos", "app", "main.lua"]);
        assert_eq!(parts("/Users/me/Documents/a.txt"), ["Documents", "a.txt"]);
        assert_eq!(parts("Documents/./a.txt"), ["Documents", "a.txt"]);
        assert!(parts("~").is_empty());
        let home = Path::new("/Users/me");
        assert!(home_components(home, "~/../other").is_err());
        assert!(home_components(home, "/etc/passwd").is_err());
        assert!(home_components(home, "/Users/meow/secret").is_err());
        assert!(home_components(home, "C:/Windows").is_err());
        assert!(home_components(home, "a\u{0}b").is_err());
    }

    #[test]
    fn secrets_app_data_and_startup_files_are_refused() {
        let app = parts("~/Library/Application Support/com.gizclaw.gizmoth");
        assert!(refusal(&parts("~/.ssh/id_ed25519"), &app, false).is_some());
        assert!(refusal(&parts("~/.SSH/config"), &app, false).is_some());
        assert!(refusal(&parts("~/.config/gh/hosts.yml"), &app, false).is_some());
        assert!(refusal(&parts("~/Library/Keychains/login.keychain-db"), &app, false).is_some());
        assert!(refusal(
            &parts("~/Library/Application Support/com.gizclaw.gizmoth/chats/a.json"),
            &app,
            false
        )
        .is_some());
        assert!(
            refusal(&parts("~/.zshrc"), &app, false).is_none(),
            "reading a dotfile is allowed"
        );
        assert!(
            refusal(&parts("~/.zshrc"), &app, true).is_some(),
            "writing it is not"
        );
        assert!(refusal(&parts("~/Library/LaunchAgents/x.plist"), &app, true).is_some());
        assert!(refusal(&parts("~"), &app, true).is_some());
        assert!(refusal(&parts("~/Documents/notes.md"), &app, true).is_none());
        assert!(refusal(&parts("~/Repos/app/.env.example"), &app, true).is_none());
    }

    #[test]
    fn only_public_addresses_are_fetched() {
        for ip in [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1)),
            IpAddr::V4(Ipv4Addr::new(100, 104, 126, 85)),
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            IpAddr::V6("fe80::1".parse().unwrap()),
            IpAddr::V6("fd00::1".parse().unwrap()),
            IpAddr::V6("::ffff:192.168.0.1".parse().unwrap()),
        ] {
            assert!(!public_address(ip), "{ip} must be refused");
        }
        assert!(public_address(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))));
        assert!(public_address(IpAddr::V6(
            "2606:4700::1111".parse().unwrap()
        )));
    }

    #[test]
    fn permissions_default_to_read_ask_allow_and_parse_partially() {
        assert_eq!(
            Permissions::default(),
            Permissions {
                files: FileAccess::Read,
                run: RunAccess::Ask,
                web: WebAccess::Allow
            }
        );
        let partial: Permissions = serde_json::from_str(r#"{"run":"deny"}"#).unwrap();
        assert_eq!(partial.run, RunAccess::Deny);
        assert_eq!(partial.files, FileAccess::Read);
        assert!(serde_json::from_str::<Permissions>(r#"{"files":"everything"}"#).is_err());
    }

    #[test]
    fn search_results_parse_for_both_providers() {
        let brave = serde_json::json!({"web":{"results":[{"title":"A","url":"https://a.example","description":"x"},{"url":"https://broken"}]}});
        assert_eq!(
            parse_brave(&brave),
            vec![SearchResult {
                title: "A".into(),
                url: "https://a.example".into(),
                snippet: "x".into()
            }]
        );
        let tavily =
            serde_json::json!({"results":[{"title":"B","url":"https://b.example","content":"y"}]});
        assert_eq!(parse_tavily(&tavily)[0].snippet, "y");
        assert!(parse_brave(&serde_json::json!({})).is_empty());
    }
}
