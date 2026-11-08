use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

fn label(id: &str) -> Result<String, String> {
    let uuid = uuid::Uuid::parse_str(id).map_err(|_| "模拟器 ID 无效。")?;
    Ok(format!("simulator-{}", uuid.simple()))
}

fn storage_directory(app: &AppHandle, id: &str) -> Result<std::path::PathBuf, String> {
    let uuid = uuid::Uuid::parse_str(id).map_err(|_| "模拟器 ID 无效。")?;
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("simulators")
        .join(uuid.to_string()))
}

static OPENING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
struct Opening(String);
impl Drop for Opening {
    fn drop(&mut self) {
        if let Ok(mut opening) = OPENING.lock() {
            opening.remove(&self.0);
        }
    }
}
static DELETING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
static PENDING_DELETIONS: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
struct Deleting(String, bool);
impl Deleting {
    fn defer(mut self) {
        self.1 = false;
    }
}
impl Drop for Deleting {
    fn drop(&mut self) {
        if self.1 {
            if let Ok(mut deleting) = DELETING.lock() {
                deleting.remove(&self.0);
            }
        }
    }
}

pub fn on_window_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    if !matches!(event, tauri::WindowEvent::Destroyed) {
        return;
    }
    let id = PENDING_DELETIONS
        .lock()
        .ok()
        .and_then(|mut pending| pending.remove(window.label()));
    let Some(id) = id else {
        return;
    };
    let app = window.app_handle().clone();
    tauri::async_runtime::spawn(async move {
        let guard = Deleting(id.clone(), true);
        if let Err(error) = finish_delete(app.clone(), id, guard).await {
            let _ = app.emit_to("main", "simulator-delete-failed", error);
        }
    });
}

#[cfg(target_os = "macos")]
fn require_private_storage() -> Result<(), String> {
    let version = std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .map_err(|e| e.to_string())?;
    let major = String::from_utf8_lossy(&version.stdout)
        .trim()
        .split('.')
        .next()
        .and_then(|value| value.parse::<u32>().ok());
    if !version.status.success() || major.is_none_or(|major| major < 14) {
        return Err("独立模拟器存储需要 macOS 14 或更高版本。".into());
    }
    Ok(())
}
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

// Only simulator windows admit the provider-verified Web firmware's Blob document,
// workers and WebAssembly. The main application's stricter policy is unchanged.
pub(crate) const SIMULATOR_CSP: &str = "default-src 'self'; script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval' blob:; worker-src 'self' blob:; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self'; connect-src 'self' ipc: http://ipc.localhost https://static-volc.gizclaw.com https://ap.dev.gizclaw.com https://ap.gizclaw.com https://ap.trial.gizclaw.com blob: data:; frame-src 'self' blob:; media-src 'self' blob:; object-src 'none'; base-uri 'none'";

/// Only the main application may open an internal, bounded simulator window.
#[tauri::command]
pub async fn open_simulator(
    app: AppHandle,
    window: tauri::WebviewWindow,
    profile_id: String,
    token: String,
) -> Result<String, String> {
    if window.label() != "main" {
        return Err("只能从主窗口打开模拟器。".into());
    }
    if profile_id.is_empty()
        || profile_id.len() > 256
        || token.len() != 36
        || !token.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
    {
        return Err("模拟器配置参数无效。".into());
    }
    let _opening = {
        let _guard = STORE.lock().map_err(|e| e.to_string())?;
        if !load(&app)?.iter().any(|profile| profile.id == profile_id) {
            return Err("此模拟器已删除。".into());
        }
        if DELETING
            .lock()
            .map_err(|e| e.to_string())?
            .contains(&profile_id)
        {
            return Err("模拟器正在删除。".into());
        }
        if !OPENING
            .lock()
            .map_err(|e| e.to_string())?
            .insert(profile_id.clone())
        {
            return Err("模拟器正在打开，请稍后再试。".into());
        }
        Opening(profile_id.clone())
    };
    // Building/focusing a native window can wait on the UI thread. Its profile
    // reads must remain free to acquire STORE while this operation is in flight.
    #[cfg(target_os = "macos")]
    require_private_storage()?;
    let label = label(&profile_id)?;
    if let Some(existing) = app.get_webview_window(&label) {
        let mut current_url = existing.url().map_err(|e| e.to_string())?;
        let matches_profile = current_url
            .query_pairs()
            .any(|(key, value)| key == "simulator" && value == profile_id);
        let matches_channel = current_url
            .query_pairs()
            .any(|(key, value)| key == "channel" && value == token);
        // A reloaded main webview has a new transport token. Reconnect the child,
        // while leaving an already-connected runtime untouched on normal reuse.
        if !matches_profile || !matches_channel {
            current_url.set_query(None);
            current_url
                .query_pairs_mut()
                .append_pair("simulator", &profile_id)
                .append_pair("channel", &token);
            existing.navigate(current_url).map_err(|e| e.to_string())?;
        }
        existing.show().map_err(|e| e.to_string())?;
        existing.unminimize().map_err(|e| e.to_string())?;
        existing.set_focus().map_err(|e| e.to_string())?;
        return Ok(label);
    }
    let base = if tauri::is_dev() {
        "http://internal".to_string()
    } else {
        super::simulator_runtime::origin(&app)?
    };
    let mut url = tauri::Url::parse(&format!("{base}/index.html")).map_err(|e| e.to_string())?;
    url.query_pairs_mut()
        .append_pair("simulator", &profile_id)
        .append_pair("channel", &token);
    let path = format!("index.html?{}", url.query().unwrap_or_default());
    let target = if tauri::is_dev() {
        WebviewUrl::App(path.into())
    } else {
        WebviewUrl::External(url.clone())
    };
    let allowed_origin = if tauri::is_dev() {
        app.config().build.dev_url.as_ref().map(|u| u.origin())
    } else {
        Some(url.origin())
    };
    let (theme, effective) = super::desktop_state::simulator_theme(&app);
    let builder = WebviewWindowBuilder::new(&app, label.clone(), target)
        .theme(theme.native())
        .visible(false)
        .background_color(super::desktop_state::background(effective))
        .title("GizMoth 模拟器")
        .inner_size(560.0, 860.0)
        .min_inner_size(340.0, 600.0)
        .resizable(true)
        .on_navigation(move |next| {
            allowed_origin
                .as_ref()
                .is_some_and(|origin| next.origin() == *origin)
                || next.scheme() == "blob"
        })
        .center();
    #[cfg(not(target_os = "macos"))]
    let builder = builder.data_directory(storage_directory(&app, &profile_id)?.join("webview"));
    #[cfg(target_os = "macos")]
    let builder = builder
        // This device must keep its firmware and command timers alive beside the editor.
        .background_throttling(tauri::utils::config::BackgroundThrottlingPolicy::Disabled)
        .data_store_identifier(
            *uuid::Uuid::parse_str(&profile_id)
                .map_err(|e| e.to_string())?
                .as_bytes(),
        )
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true)
        .traffic_light_position(tauri::LogicalPosition::new(16.0, 28.0));
    #[cfg(feature = "native-e2e")]
    let builder = if let Some(script) = super::e2e_firmware::initialization_script(&app)? {
        builder.initialization_script(script)
    } else {
        builder
    };
    let window = builder.build().map_err(|e| e.to_string())?;
    super::desktop_state::show_themed_window(&window)?;
    window.set_focus().map_err(|e| e.to_string())?;
    Ok(label)
}

/// Let simulator close handlers flush firmware before a normal application quit.
pub fn prepare_exit(app: &AppHandle, api: &tauri::ExitRequestApi) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WAITING: AtomicBool = AtomicBool::new(false);
    let windows: Vec<_> = app
        .webview_windows()
        .into_iter()
        .filter(|(label, _)| label.starts_with("simulator-"))
        .map(|(_, window)| window)
        .collect();
    if windows.is_empty() && DELETING.lock().is_ok_and(|deleting| deleting.is_empty()) {
        return;
    }
    api.prevent_exit();
    if WAITING.swap(true, Ordering::AcqRel) {
        return;
    }
    for window in windows {
        if window.close().is_err() {
            WAITING.store(false, Ordering::Release);
            return;
        }
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        for _ in 0..120 {
            if !app
                .webview_windows()
                .keys()
                .any(|label| label.starts_with("simulator-"))
                && DELETING.lock().is_ok_and(|deleting| deleting.is_empty())
            {
                WAITING.store(false, Ordering::Release);
                app.exit(0);
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        // A failed flush leaves its window open with an error; do not claim it was saved.
        WAITING.store(false, Ordering::Release);
    });
}

use serde::{Deserialize, Serialize};
use std::{fs, sync::Mutex};
static STORE: Mutex<()> = Mutex::new(());
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulatorProfile {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) model: String,
    firmware_id: String,
    view: String,
}
fn valid(p: &SimulatorProfile) -> bool {
    uuid::Uuid::parse_str(&p.id).is_ok()
        && !p.name.trim().is_empty()
        && p.name.chars().count() <= 80
        && matches!(
            p.model.as_str(),
            "h106-tiga" | "h106-zero" | "h106-orb" | "h106-zeta"
        )
        && matches!(p.view.as_str(), "model" | "screen")
        && p.firmware_id.len() <= 256
}
fn path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("simulators.json"))
}
fn load(app: &AppHandle) -> Result<Vec<SimulatorProfile>, String> {
    match fs::read(path(app)?) {
        Ok(data) => serde_json::from_slice(&data).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(e.to_string()),
    }
}
fn save(app: &AppHandle, profiles: &[SimulatorProfile]) -> Result<(), String> {
    let target = path(app)?;
    let temp = target.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let data = serde_json::to_vec(profiles).map_err(|e| e.to_string())?;
    fs::write(&temp, data).map_err(|e| e.to_string())?;
    fs::rename(&temp, &target).map_err(|e| {
        let _ = fs::remove_file(&temp);
        e.to_string()
    })
}
#[tauri::command]
pub fn simulator_profiles_list(app: AppHandle) -> Result<Vec<SimulatorProfile>, String> {
    let _guard = STORE.lock().map_err(|e| e.to_string())?;
    load(&app)
}
#[tauri::command]
pub fn simulator_profile_create(
    app: AppHandle,
    profile: SimulatorProfile,
) -> Result<SimulatorProfile, String> {
    let _guard = STORE.lock().map_err(|e| e.to_string())?;
    if !valid(&profile) {
        return Err("模拟器配置无效。".into());
    }
    let mut all = load(&app)?;
    if all.iter().any(|p| p.id == profile.id) {
        return Err("模拟器已存在。".into());
    }
    all.push(profile.clone());
    save(&app, &all)?;
    drop(_guard);
    super::desktop_state::refresh(&app);
    Ok(profile)
}
#[tauri::command]
pub fn simulator_profile_update(
    app: AppHandle,
    window: tauri::WebviewWindow,
    id: String,
    patch: serde_json::Value,
) -> Result<SimulatorProfile, String> {
    if window.label() != "main" && window.label() != label(&id)? {
        return Err("只能修改当前模拟器的偏好。".into());
    }
    let _guard = STORE.lock().map_err(|e| e.to_string())?;
    if DELETING.lock().map_err(|e| e.to_string())?.contains(&id) {
        return Err("模拟器正在删除。".into());
    }
    let mut all = load(&app)?;
    let index = all
        .iter()
        .position(|p| p.id == id)
        .ok_or("此模拟器已删除。")?;
    let mut value = serde_json::to_value(&all[index]).map_err(|e| e.to_string())?;
    let fields = patch.as_object().ok_or("配置无效。")?;
    for (key, value_patch) in fields {
        if key == "model" {
            return Err("模拟器机型创建后不可更改，请新建模拟器。".into());
        }
        if !matches!(key.as_str(), "name" | "firmwareId" | "view") {
            return Err("配置字段无效。".into());
        }
        value[key] = value_patch.clone();
    }
    let profile: SimulatorProfile = serde_json::from_value(value).map_err(|e| e.to_string())?;
    if !valid(&profile) {
        return Err("模拟器配置无效。".into());
    }
    all[index] = profile.clone();
    save(&app, &all)?;
    drop(_guard);
    super::desktop_state::refresh(&app);
    Ok(profile)
}
#[tauri::command]
pub async fn simulator_profile_delete(
    app: AppHandle,
    window: tauri::WebviewWindow,
    id: String,
) -> Result<(), String> {
    let owner = label(&id)?;
    if window.label() != "main" && window.label() != owner {
        return Err("只能删除当前模拟器。".into());
    }
    let deleting_self = window.label() == owner;
    drop(window);
    let deletion = {
        let _guard = STORE.lock().map_err(|e| e.to_string())?;
        if !load(&app)?.iter().any(|profile| profile.id == id) {
            return Err("此模拟器已删除。".into());
        }
        if OPENING.lock().map_err(|e| e.to_string())?.contains(&id) {
            return Err("模拟器正在打开，请稍后再试。".into());
        }
        if !DELETING
            .lock()
            .map_err(|e| e.to_string())?
            .insert(id.clone())
        {
            return Err("模拟器正在删除。".into());
        }
        Deleting(id.clone(), true)
    };
    if deleting_self {
        // Return the originating IPC call before closing this WebView. Its
        // Destroyed event performs cleanup without retaining the deleted store.
        PENDING_DELETIONS
            .lock()
            .map_err(|e| e.to_string())?
            .insert(owner, id);
        deletion.defer();
        return Ok(());
    }
    // Deletion deliberately discards this machine. Normal Close uses the UI's
    // shutdown barrier instead; destroying here also prevents new file writes.
    if let Some(window) = app.get_webview_window(&owner) {
        window.destroy().map_err(|e| e.to_string())?;
    }
    finish_delete(app, id, deletion).await
}

async fn finish_delete(app: AppHandle, id: String, _deletion: Deleting) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    if require_private_storage().is_ok() {
        let identifier = *uuid::Uuid::parse_str(&id)
            .map_err(|e| e.to_string())?
            .as_bytes();
        // WKWebView releases its website store asynchronously after destruction.
        // The removal operation is idempotent; retain the profile if it never succeeds.
        for attempt in 0..50 {
            match app.remove_data_store(identifier).await {
                Ok(()) => break,
                Err(error) if attempt == 49 => return Err(error.to_string()),
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
            }
        }
    }
    let directory = storage_directory(&app, &id)?;
    match fs::remove_dir_all(directory) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    // Keep the profile available for a retry if native storage removal failed.
    let _guard = STORE.lock().map_err(|e| e.to_string())?;
    let mut all = load(&app)?;
    all.retain(|profile| profile.id != id);
    save(&app, &all)?;
    drop(_guard);
    let _ = app.emit("simulator-profiles-changed", ());
    super::desktop_state::refresh(&app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_project_links_are_ignored_and_removed_on_serialization() {
        let id = uuid::Uuid::new_v4().to_string();
        let old = serde_json::json!({ "id": id, "name": "Desk", "model": "h106-zero", "firmwareId": "zero-trial", "view": "screen", "projectId": "legacy-project" });
        let profile: SimulatorProfile = serde_json::from_value(old).unwrap();
        assert!(valid(&profile));
        let saved = serde_json::to_value(&profile).unwrap();
        assert_eq!(saved["id"], id);
        assert_eq!(saved["firmwareId"], "zero-trial");
        assert_eq!(saved["view"], "screen");
        assert!(saved.get("projectId").is_none());
        assert_eq!(saved.as_object().unwrap().len(), 5);
    }
    #[test]
    fn profiles_accept_only_bounded_supported_preferences() {
        let mut p = SimulatorProfile {
            id: uuid::Uuid::new_v4().to_string(),
            name: "桌面".into(),
            model: "h106-tiga".into(),
            firmware_id: String::new(),
            view: "model".into(),
        };
        assert!(valid(&p));
        p.model = "unknown".into();
        assert!(!valid(&p));
        p.model = "h106-zero".into();
        p.name = " ".into();
        assert!(!valid(&p));
        p.name = "x".repeat(81);
        assert!(!valid(&p));
        p.name = "ok".into();
        p.firmware_id = "x".repeat(257);
        assert!(!valid(&p));
        p.firmware_id.clear();
        p.view = "running".into();
        assert!(!valid(&p));
    }
}
