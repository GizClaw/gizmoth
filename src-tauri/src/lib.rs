mod ble;
mod controls;
mod credentials;
mod desktop_state;
mod device_api;
mod project;
mod providers;
mod workspace;

use credentials::Device;
use serde_json::Value;
use std::sync::Mutex;
use tauri::AppHandle;
use zeroize::Zeroizing;

type R<T> = Result<T, String>;

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> R<T> + Send + 'static) -> R<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|_| "后台任务失败".to_owned())?
}

// Devices
#[tauri::command]
fn list_devices(app: AppHandle) -> R<Vec<Device>> {
    credentials::list(&app)
}
#[tauri::command]
async fn import_device(app: AppHandle, url: String) -> R<Device> {
    let raw = Zeroizing::new(url);
    let credential = credentials::parse(&raw)?;
    blocking(move || credentials::import(&app, credential)).await
}
#[tauri::command]
async fn remove_device(app: AppHandle, id: String) -> R<()> {
    blocking(move || credentials::remove(&app, &id)).await
}
#[tauri::command]
async fn scan_devices(state: tauri::State<'_, ble::BleState>) -> R<Vec<ble::Nearby>> {
    ble::scan(&state).await
}
#[tauri::command]
async fn bind_device(
    app: AppHandle,
    state: tauri::State<'_, ble::BleState>,
    id: String,
) -> R<Device> {
    ble::bind(&app, &state, &id).await
}
#[tauri::command]
async fn device_overview(app: AppHandle, device_id: String) -> R<Value> {
    controls::overview(&app, &device_id).await
}
#[tauri::command]
async fn set_device_level(
    app: AppHandle,
    device_id: String,
    instance: controls::Hardware,
    level: u8,
) -> R<Value> {
    controls::set_level(&app, &device_id, instance, level).await
}
#[tauri::command]
async fn list_apps(app: AppHandle, device_id: String) -> R<Value> {
    device_api::list(&app, &device_id).await
}
#[tauri::command]
async fn run_app(app: AppHandle, device_id: String, app_id: String) -> R<Value> {
    device_api::run(&app, &device_id, &app_id).await
}

// Projects
#[tauri::command]
fn projects_list(app: AppHandle) -> R<Vec<workspace::ProjectSummary>> {
    workspace::list(&app)
}
#[tauri::command]
fn project_create(app: AppHandle, app_id: String, name: String) -> R<workspace::ProjectSummary> {
    workspace::create(&app, &app_id, &name)
}
#[tauri::command]
fn project_delete(app: AppHandle, id: String) -> R<()> {
    workspace::delete(&app, &id)
}
#[tauri::command]
fn project_files(app: AppHandle, id: String) -> R<Vec<workspace::FileEntry>> {
    workspace::files(&app, &id)
}
#[tauri::command]
fn project_read(app: AppHandle, id: String, path: String) -> R<String> {
    workspace::read(&app, &id, &path)
}
#[tauri::command]
fn project_write(app: AppHandle, id: String, path: String, content: String) -> R<()> {
    workspace::write(&app, &id, &path, &content)
}
#[tauri::command]
fn project_remove_file(app: AppHandle, id: String, path: String) -> R<()> {
    workspace::remove(&app, &id, &path)
}
#[tauri::command]
fn project_rename_file(app: AppHandle, id: String, from: String, to: String) -> R<()> {
    workspace::rename(&app, &id, &from, &to)
}
#[tauri::command]
fn project_dir(app: AppHandle, id: String) -> R<String> {
    workspace::reveal_dir(&app, &id)
}
#[tauri::command]
fn project_check(app: AppHandle, id: String) -> R<project::PackageInfo> {
    project::inspect(&workspace::load(&app, &id)?)
}
#[tauri::command]
async fn project_export(app: AppHandle, id: String) -> R<Option<String>> {
    let snapshot = workspace::load(&app, &id)?;
    blocking(move || project::export(snapshot)).await
}
#[tauri::command]
async fn project_install(app: AppHandle, id: String, device_id: String) -> R<Value> {
    let snapshot = workspace::load(&app, &id)?;
    device_api::install(&app, &device_id, snapshot).await
}
#[tauri::command]
fn chat_load(app: AppHandle, id: String) -> R<Option<String>> {
    workspace::chat_load(&app, &id)
}
#[tauri::command]
fn chat_save(app: AppHandle, id: String, json: String) -> R<()> {
    workspace::chat_save(&app, &id, &json)
}

// Providers
#[tauri::command]
fn providers_list(app: AppHandle) -> R<Vec<providers::Provider>> {
    providers::list(&app)
}
#[tauri::command]
async fn provider_save(
    app: AppHandle,
    provider: providers::Provider,
    api_key: Option<String>,
) -> R<providers::Provider> {
    blocking(move || providers::save(&app, provider, api_key)).await
}
#[tauri::command]
async fn provider_remove(app: AppHandle, id: String) -> R<()> {
    blocking(move || providers::remove(&app, &id)).await
}
#[tauri::command]
async fn provider_key(app: AppHandle, id: String) -> R<Option<String>> {
    blocking(move || providers::key(&app, &id)).await
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_http::init())
        .manage(credentials::DeviceStore(Mutex::new(())))
        .manage(providers::ProviderStore(Mutex::new(())))
        .manage(ble::BleState::default())
        .setup(desktop_state::setup)
        .on_window_event(desktop_state::on_window_event)
        .invoke_handler(tauri::generate_handler![
            list_devices,
            import_device,
            remove_device,
            scan_devices,
            bind_device,
            device_overview,
            set_device_level,
            list_apps,
            run_app,
            projects_list,
            project_create,
            project_delete,
            project_files,
            project_read,
            project_write,
            project_remove_file,
            project_rename_file,
            project_dir,
            project_check,
            project_export,
            project_install,
            chat_load,
            chat_save,
            providers_list,
            provider_save,
            provider_remove,
            provider_key,
            desktop_state::desktop_preferences,
            desktop_state::set_desktop_preferences
        ])
        .run(tauri::generate_context!())
        .expect("Unable to start Gizmoth");
}
