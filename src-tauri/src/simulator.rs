use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    LazyLock,
};

static UPDATING: AtomicBool = AtomicBool::new(false);

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
struct PendingDeletion {
    id: String,
    complete: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
}
static PENDING_DELETIONS: LazyLock<Mutex<HashMap<String, PendingDeletion>>> =
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
    let pending = PENDING_DELETIONS
        .lock()
        .ok()
        .and_then(|mut pending| pending.remove(window.label()));
    let Some(pending) = pending else {
        return;
    };
    let app = window.app_handle().clone();
    tauri::async_runtime::spawn(async move {
        let guard = Deleting(pending.id.clone(), true);
        let result = finish_delete(app.clone(), pending.id, guard).await;
        if let Some(complete) = pending.complete {
            let _ = complete.send(result);
        } else if let Err(error) = result {
            let _ = app.emit_to("main", "simulator-delete-failed", error);
        }
    });
}

#[cfg(target_os = "macos")]
fn private_storage_supported() -> Result<bool, String> {
    let version = std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .map_err(|e| e.to_string())?;
    storage_support_from_probe(
        version.status.success(),
        &String::from_utf8_lossy(&version.stdout),
    )
}

#[cfg(any(target_os = "macos", test))]
fn storage_support_from_probe(success: bool, version: &str) -> Result<bool, String> {
    if !success {
        return Err("无法确认系统存储支持，请稍后重试。".into());
    }
    let major = version
        .trim()
        .split('.')
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or("无法确认系统存储支持，请稍后重试。")?;
    Ok(major >= 14)
}

#[cfg(target_os = "macos")]
fn require_private_storage() -> Result<(), String> {
    if !private_storage_supported()? {
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
        if UPDATING.load(Ordering::Acquire) {
            return Err("正在准备应用更新，请稍后再打开模拟器。".into());
        }
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
    let builder = builder.initialization_script(super::e2e::SIMULATOR_BOOT_TRACE);
    #[cfg(feature = "native-e2e")]
    let builder = if let Some(script) = super::e2e_firmware::initialization_script(&app)? {
        builder.initialization_script(script)
    } else {
        builder
    };
    let window = builder.build().map_err(|e| e.to_string())?;
    #[cfg(target_os = "windows")]
    if let Err(error) = super::simulator_windows::track(&window, false).await {
        // Preserve a live view for retry if the UI callback never retained its
        // environment. Otherwise cleanup can re-probe after window destruction.
        if error.environment_retained {
            let _ = destroy_window(&window);
            super::simulator_windows::monitor_exit(&app, &label);
        }
        return Err(error.message);
    }
    super::desktop_state::show_themed_window(&window)?;
    window.set_focus().map_err(|e| e.to_string())?;
    Ok(label)
}

fn destroy_window(window: &tauri::WebviewWindow) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        // Window destruction alone does not clear the webview manager's event
        // listeners and IPC queues. Release those references before the HWND.
        let webview: &tauri::Webview = window.as_ref();
        webview.close().map_err(|e| e.to_string())?;
    }
    window.destroy().map_err(|e| e.to_string())
}

/// Destroy only the caller after its close handler has flushed the firmware.
/// No target label is accepted from JavaScript.
#[tauri::command]
pub async fn simulator_window_destroy(window: tauri::WebviewWindow) -> Result<(), String> {
    if !window.label().starts_with("simulator-") {
        return Err("只能关闭当前模拟器。".into());
    }
    let result = async {
        #[cfg(target_os = "windows")]
        super::simulator_windows::track(&window, true)
            .await
            .map_err(|error| error.message)?;
        destroy_window(&window)
    }
    .await;
    if result.is_err() {
        // A failed close must not strand a self-deletion reservation.
        if let Some(pending) = PENDING_DELETIONS
            .lock()
            .map_err(|e| e.to_string())?
            .remove(window.label())
        {
            if let Ok(mut deleting) = DELETING.lock() {
                deleting.remove(&pending.id);
            }
            if let Some(complete) = pending.complete {
                let _ = complete.send(result.clone());
            }
        }
    }
    result
}

fn storage_idle() -> bool {
    let idle = DELETING.lock().is_ok_and(|deleting| deleting.is_empty())
        && OPENING.lock().is_ok_and(|opening| opening.is_empty());
    #[cfg(target_os = "windows")]
    {
        idle && super::simulator_windows::idle()
    }
    #[cfg(not(target_os = "windows"))]
    {
        idle
    }
}

async fn close_all(app: &AppHandle, attempts: usize) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    super::simulator_windows::retry_closed(app);
    for _ in 0..attempts {
        let windows = app.webview_windows();
        if !windows.keys().any(|label| label.starts_with("simulator-")) && storage_idle() {
            return Ok(());
        }
        // Include windows that finish opening while shutdown is in progress.
        for (label, window) in windows {
            if label.starts_with("simulator-") {
                window.close().map_err(|e| e.to_string())?;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err("模拟器尚未完成保存或清理，请检查模拟器窗口后重试。".into())
}

/// Updater installation/relaunch can bypass prevent_exit. Finish the barrier
/// first, and keep new simulator opens blocked until restart or cancellation.
#[tauri::command]
pub async fn simulator_prepare_update(
    app: AppHandle,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("此操作只能从主窗口执行。".into());
    }
    {
        let _guard = STORE.lock().map_err(|e| e.to_string())?;
        UPDATING.store(true, Ordering::Release);
    }
    let result = close_all(&app, 450).await;
    if result.is_err() {
        UPDATING.store(false, Ordering::Release);
    }
    result
}

#[tauri::command]
pub fn simulator_cancel_update(window: tauri::WebviewWindow) -> Result<(), String> {
    if window.label() != "main" {
        return Err("此操作只能从主窗口执行。".into());
    }
    UPDATING.store(false, Ordering::Release);
    Ok(())
}

/// Let simulator close handlers flush firmware before a normal application quit.
pub fn prepare_exit(app: &AppHandle, api: &tauri::ExitRequestApi) {
    static WAITING: AtomicBool = AtomicBool::new(false);
    if !app
        .webview_windows()
        .keys()
        .any(|label| label.starts_with("simulator-"))
        && storage_idle()
    {
        return;
    }
    api.prevent_exit();
    if WAITING.swap(true, Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let attempts = if cfg!(target_os = "windows") {
            450
        } else {
            120
        };
        let result = close_all(&app, attempts).await;
        WAITING.store(false, Ordering::Release);
        if result.is_ok() {
            app.exit(0);
        } else {
            // Make cancellation explicit; a prevented quit must not disappear silently.
            let _ = app.emit_to(
                "main",
                "simulator-close-failed",
                "退出未完成：模拟器尚未保存或清理，请检查模拟器窗口后重试。",
            );
            super::desktop_state::show(&app);
        }
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
        let mut pending = PENDING_DELETIONS.lock().map_err(|e| e.to_string())?;
        // Serialize registration with the Destroyed handler. A queued IPC from
        // an already-closed view must release its guard instead of deferring forever.
        if app.get_webview_window(&owner).is_none() {
            return Err("模拟器窗口已关闭，请在管理页重试删除。".into());
        }
        pending.insert(owner, PendingDeletion { id, complete: None });
        deletion.defer();
        return Ok(());
    }
    // Manager deletion also waits for Destroyed before touching the store. Only
    // the main caller is retained; no handle to the deleted WebView crosses await.
    if let Some(window) = app.get_webview_window(&owner) {
        #[cfg(target_os = "windows")]
        super::simulator_windows::track(&window, true)
            .await
            .map_err(|error| error.message)?;
        let (complete, completed) = tokio::sync::oneshot::channel();
        let registered = {
            let mut pending = PENDING_DELETIONS.lock().map_err(|e| e.to_string())?;
            // Normal Close may finish during the lookup/Windows tracking above.
            // Serialize this second liveness check with the Destroyed handler.
            if app.get_webview_window(&owner).is_some() {
                pending.insert(
                    owner.clone(),
                    PendingDeletion {
                        id: id.clone(),
                        complete: Some(complete),
                    },
                );
                true
            } else {
                false
            }
        };
        if !registered {
            drop(window);
            return finish_delete(app, id, deletion).await;
        }
        if let Err(error) = destroy_window(&window) {
            let canceled = PENDING_DELETIONS
                .lock()
                .map_err(|e| e.to_string())?
                .remove(&owner);
            if canceled.is_some() {
                return Err(error.to_string());
            }
            // Destroyed already took responsibility. Await that cleanup instead
            // of releasing DELETING while it is still touching this store.
        }
        drop(window);
        deletion.defer();
        return completed
            .await
            .map_err(|_| "无法完成模拟器删除。".to_string())?;
    }
    finish_delete(app, id, deletion).await
}

fn windows_storage_contention(error: &std::io::Error) -> bool {
    // Sharing/lock violations need their Win32 codes: Rust's ErrorKind mapping
    // is not guaranteed to classify them as PermissionDenied.
    matches!(error.raw_os_error(), Some(5 | 32 | 33 | 145))
        || matches!(
            error.kind(),
            std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::DirectoryNotEmpty
        )
}

async fn remove_storage_directory(directory: &std::path::Path) -> Result<(), String> {
    let mut retries = 0;
    loop {
        match fs::remove_dir_all(directory) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            // WebView2's browser process can retain file handles after Destroyed.
            // Retry only Windows teardown contention, and keep the profile on failure.
            Err(error)
                if cfg!(target_os = "windows")
                    && windows_storage_contention(&error)
                    && retries < 99 =>
            {
                retries += 1;
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

async fn finish_delete(app: AppHandle, id: String, _deletion: Deleting) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    super::simulator_windows::wait_for_exit(&app, &label(&id)?).await?;
    #[cfg(target_os = "macos")]
    if private_storage_supported()? {
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
    remove_storage_directory(&directory).await?;
    // Keep the profile available for a retry if native storage removal failed.
    let _guard = STORE.lock().map_err(|e| e.to_string())?;
    let mut all = load(&app)?;
    all.retain(|profile| profile.id != id);
    save(&app, &all)?;
    drop(_guard);
    #[cfg(target_os = "windows")]
    super::simulator_windows::forget(&app, &label(&id)?);
    let _ = app.emit("simulator-profiles-changed", ());
    super::desktop_state::refresh(&app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_storage_probe_is_never_treated_as_an_unsupported_system() {
        assert!(!storage_support_from_probe(true, "13.6\n").unwrap());
        assert!(storage_support_from_probe(true, "14.0\n").unwrap());
        assert!(storage_support_from_probe(false, "26.0").is_err());
        assert!(storage_support_from_probe(true, "").is_err());
        assert!(storage_support_from_probe(true, "unknown").is_err());
    }

    #[test]
    fn recognizes_windows_teardown_codes_without_relying_on_error_kind() {
        for code in [5, 32, 33, 145] {
            assert!(windows_storage_contention(
                &std::io::Error::from_raw_os_error(code)
            ));
        }
        assert!(!windows_storage_contention(&std::io::Error::from(
            std::io::ErrorKind::InvalidInput
        )));
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn deletion_waits_for_windows_storage_handles_to_close() {
        use std::os::windows::fs::OpenOptionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("webview");
        fs::create_dir(&directory).unwrap();
        let file = directory.join("storage.db");
        fs::write(&file, b"retained WebView2 data").unwrap();
        let locked = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(file)
            .unwrap();
        let released = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            drop(locked);
        });
        remove_storage_directory(&directory).await.unwrap();
        released.join().unwrap();
        assert!(!directory.exists());
    }

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
