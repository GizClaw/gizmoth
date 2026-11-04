//! Model providers for the embedded coding agent. Non-secret configuration is
//! stored as JSON in the app data directory; API keys live in the OS
//! credential store and are only handed to the agent at request time.

use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf, sync::Mutex};
use tauri::Manager;
use url::Url;
use zeroize::Zeroizing;

const SERVICE: &str = "com.gizclaw.gizmoth.providers";
const APIS: &[&str] = &[
    "openai-completions",
    "openai-responses",
    "anthropic-messages",
    "google-generative-ai",
];

pub struct ProviderStore(pub Mutex<()>);

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModel {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub images: bool,
    #[serde(default = "default_context")]
    pub context_window: u32,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}
fn default_context() -> u32 {
    128_000
}
fn default_max_tokens() -> u32 {
    16_384
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub api: String,
    pub base_url: String,
    pub models: Vec<ProviderModel>,
    #[serde(default)]
    pub has_key: bool,
}

fn registry(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|_| "无法定位应用数据目录")?;
    fs::create_dir_all(&dir).map_err(|_| "无法创建应用数据目录")?;
    Ok(dir.join("providers.json"))
}

pub fn list(app: &tauri::AppHandle) -> Result<Vec<Provider>, String> {
    match fs::read(registry(app)?) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| "Provider 配置损坏".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(_) => Err("无法读取 Provider 配置".into()),
    }
}

fn write(app: &tauri::AppHandle, providers: &[Provider]) -> Result<(), String> {
    let path = registry(app)?;
    let temporary = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(providers).map_err(|_| "无法保存 Provider")?;
    fs::write(&temporary, bytes).map_err(|_| "无法保存 Provider")?;
    fs::rename(&temporary, path).map_err(|_| "无法保存 Provider".into())
}

fn entry(id: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, id).map_err(|_| "系统凭据存储不可用".into())
}

pub fn validate(provider: &Provider) -> Result<(), String> {
    let id_ok = !provider.id.is_empty()
        && provider.id.len() <= 48
        && provider
            .id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-');
    if !id_ok {
        return Err("Provider ID 无效".into());
    }
    if provider.name.trim().is_empty() || provider.name.len() > 80 {
        return Err("Provider 名称需为 1–80 字节".into());
    }
    if !APIS.contains(&provider.api.as_str()) {
        return Err("不支持的接口类型".into());
    }
    let url = Url::parse(&provider.base_url).map_err(|_| "Base URL 无效")?;
    let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    if !(url.scheme() == "https" || (url.scheme() == "http" && local))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("Base URL 需为 HTTPS，本机地址可用 HTTP，且不能包含账号或片段".into());
    }
    if provider.models.len() > 200 {
        return Err("模型数量过多".into());
    }
    for model in &provider.models {
        if model.id.is_empty() || model.id.len() > 200 || model.id.chars().any(char::is_control) {
            return Err("模型 ID 无效".into());
        }
    }
    Ok(())
}

pub fn save(
    app: &tauri::AppHandle,
    mut provider: Provider,
    api_key: Option<String>,
) -> Result<Provider, String> {
    validate(&provider)?;
    let state = app.state::<ProviderStore>();
    let _guard = state.0.lock().map_err(|_| "Provider 存储正忙")?;
    let mut providers = list(app)?;
    let previous = providers
        .iter()
        .find(|p| p.id == provider.id)
        .map(|p| p.has_key)
        .unwrap_or(false);
    provider.has_key = previous;
    if let Some(key) = api_key.map(Zeroizing::new) {
        let key = key.trim();
        if key.is_empty() {
            match entry(&provider.id)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => (),
                Err(_) => return Err("无法删除 API key".into()),
            }
            provider.has_key = false;
        } else {
            if key.len() > 4096 || key.chars().any(char::is_control) {
                return Err("API key 格式无效".into());
            }
            entry(&provider.id)?
                .set_password(key)
                .map_err(|_| "无法写入系统凭据存储，请允许钥匙串访问。")?;
            provider.has_key = true;
        }
    }
    providers.retain(|p| p.id != provider.id);
    providers.push(provider.clone());
    write(app, &providers)?;
    Ok(provider)
}

pub fn remove(app: &tauri::AppHandle, id: &str) -> Result<(), String> {
    let state = app.state::<ProviderStore>();
    let _guard = state.0.lock().map_err(|_| "Provider 存储正忙")?;
    let mut providers = list(app)?;
    match entry(id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => (),
        Err(_) => return Err("无法删除 API key".into()),
    }
    providers.retain(|p| p.id != id);
    write(app, &providers)
}

/// Returns the key to the agent for a single request. Never logged.
pub fn key(app: &tauri::AppHandle, id: &str) -> Result<Option<String>, String> {
    if !list(app)?.iter().any(|p| p.id == id && p.has_key) {
        return Ok(None);
    }
    match entry(id)?.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(_) => Err("无法读取 API key，请允许钥匙串访问。".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn provider(url: &str) -> Provider {
        Provider {
            id: "magpie".into(),
            kind: "magpie".into(),
            name: "magpie".into(),
            api: "openai-completions".into(),
            base_url: url.into(),
            models: vec![],
            has_key: false,
        }
    }
    #[test]
    fn only_https_or_local_http() {
        assert!(validate(&provider("http://127.0.0.1:3425/v1")).is_ok());
        assert!(validate(&provider("https://api.openai.com/v1")).is_ok());
        assert!(validate(&provider("http://example.com/v1")).is_err());
        assert!(validate(&provider("https://u:p@example.com")).is_err());
        let mut p = provider("https://x.dev");
        p.api = "bash".into();
        assert!(validate(&p).is_err());
    }
}
