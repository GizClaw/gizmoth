mod agent_access;
mod agents;
mod ble;
mod controls;
mod credentials;
mod desktop_state;
mod devenv;
mod device_api;
#[cfg(feature = "native-e2e")]
mod e2e;
#[cfg(feature = "native-e2e")]
mod e2e_firmware;
mod formatter;
pub mod mcp;

#[cfg(all(feature = "native-e2e", not(debug_assertions)))]
compile_error!("The native-e2e test driver must never be included in a release build.");
mod project;
mod providers;
pub mod serial;
mod simulator;
mod simulator_runtime;
#[cfg(target_os = "windows")]
mod simulator_windows;
mod workspace;

use credentials::Device;
use serde_json::Value;
use std::sync::Mutex;
use tauri::AppHandle;
use zeroize::Zeroizing;

type R<T> = Result<T, String>;

#[derive(Debug, PartialEq)]
enum NativeMode {
    Daemon,
    Mcp,
    ListTools,
    Tool(String, Value),
}

#[derive(Debug, PartialEq)]
struct NativeArgs {
    mode: NativeMode,
    data_dir: Option<std::path::PathBuf>,
}

fn parse_native_args(args: &[String]) -> R<Option<NativeArgs>> {
    if args.len() <= 1 {
        return Ok(None);
    }
    // Older macOS launch services can supply this GUI-only process argument.
    #[cfg(target_os = "macos")]
    if args.len() == 2 && args[1].starts_with("-psn_") {
        return Ok(None);
    }
    let mut mode = None;
    let mut data_dir = None;
    let mut input = args[1..].iter().peekable();
    while let Some(arg) = input.next() {
        match arg.as_str() {
            "--data-dir" if data_dir.is_none() => {
                let value = input
                    .next()
                    .filter(|value| !value.is_empty() && !value.starts_with("--"))
                    .ok_or("--data-dir requires a directory")?;
                data_dir = Some(std::path::PathBuf::from(value));
            }
            serial::DAEMON_FLAG | mcp::FLAG | "--list-tools" | "--tool" if mode.is_none() => {
                mode = Some(match arg.as_str() {
                    serial::DAEMON_FLAG => NativeMode::Daemon,
                    mcp::FLAG => NativeMode::Mcp,
                    "--list-tools" => NativeMode::ListTools,
                    _ => {
                        let name = input
                            .next()
                            .filter(|name| !name.is_empty() && !name.starts_with('-'))
                            .ok_or("--tool requires a tool name")?
                            .clone();
                        let value = if input.peek().is_some_and(|next| !next.starts_with("--")) {
                            let raw = input.next().unwrap();
                            let value: Value = serde_json::from_str(raw)
                                .map_err(|error| format!("Invalid tool JSON: {error}"))?;
                            if !value.is_object() {
                                return Err("Tool arguments must be a JSON object".into());
                            }
                            value
                        } else {
                            serde_json::json!({})
                        };
                        NativeMode::Tool(name, value)
                    }
                });
            }
            _ => return Err(format!("Unexpected or duplicate argument: {arg}")),
        }
    }
    let mode =
        mode.ok_or("Expected --tool <name> [JSON-object], --list-tools, --mcp or --serial-daemon")?;
    Ok(Some(NativeArgs { mode, data_dir }))
}

#[cfg(windows)]
fn attach_cli_console() {
    // Release builds use the Windows GUI subsystem. Attach CLI modes to their
    // invoking terminal, but retain any inherited pipes used by scripts/MCP.
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process: u32) -> i32;
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn SetStdHandle(which: u32, handle: *mut c_void) -> i32;
    }
    let kinds = [(-10i32) as u32, (-11i32) as u32, (-12i32) as u32];
    let valid = |handle: *mut c_void| !handle.is_null() && handle as isize != -1;
    // SAFETY: Win32 standard-handle constants and the parent-process sentinel
    // are passed unchanged; borrowed handles are never closed here.
    unsafe {
        let inherited = kinds.map(|kind| GetStdHandle(kind));
        if (!valid(inherited[1]) || !valid(inherited[2])) && AttachConsole(u32::MAX) != 0 {
            for (kind, handle) in kinds.into_iter().zip(inherited) {
                if valid(handle) {
                    SetStdHandle(kind, handle);
                }
            }
        }
    }
}

/// All native modes dispatch before Tauri or any window is created. Invalid
/// arguments exit with an error instead of falling through to the desktop.
pub fn dispatch(args: &[String]) -> Option<i32> {
    #[cfg(windows)]
    if args.len() > 1 {
        attach_cli_console();
    }
    let options = match parse_native_args(args) {
        Ok(Some(options)) => options,
        Ok(None) => return None,
        Err(error) => {
            eprintln!("{error}");
            return Some(2);
        }
    };
    if options.mode == NativeMode::ListTools {
        return Some(mcp::cli_output(
            Ok(serde_json::json!({ "tools": mcp::tools() })),
            std::io::stdout().lock(),
            std::io::stderr().lock(),
        ));
    }
    let Some(dir) = options.data_dir.or_else(serial::default_data_dir) else {
        eprintln!("Unable to resolve GizMoth's data directory; use --data-dir <directory>");
        return Some(2);
    };
    Some(match options.mode {
        NativeMode::Mcp => mcp::run(&dir),
        NativeMode::Daemon => serial::daemon::run(&dir),
        NativeMode::Tool(name, value) => mcp::cli_output(
            mcp::cli_call(&dir, &name, &value),
            std::io::stdout().lock(),
            std::io::stderr().lock(),
        ),
        NativeMode::ListTools => unreachable!(),
    })
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    fn parse(args: &[&str]) -> R<Option<NativeArgs>> {
        let args = std::iter::once("gizmoth")
            .chain(args.iter().copied())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        parse_native_args(&args)
    }

    #[test]
    fn cli_parses_json_and_data_directory_before_or_after_mode() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().to_str().unwrap();
        for args in [
            vec![
                "--data-dir",
                dir,
                "--tool",
                "serial_status",
                r#"{"port":"exact id"}"#,
            ],
            vec![
                "--tool",
                "serial_status",
                r#"{"port":"exact id"}"#,
                "--data-dir",
                dir,
            ],
        ] {
            assert_eq!(
                parse(&args).unwrap(),
                Some(NativeArgs {
                    mode: NativeMode::Tool(
                        "serial_status".into(),
                        serde_json::json!({"port":"exact id"})
                    ),
                    data_dir: Some(root.path().to_path_buf()),
                })
            );
        }
        assert_eq!(
            parse(&["--tool", "gizmoth_status"]).unwrap().unwrap().mode,
            NativeMode::Tool("gizmoth_status".into(), serde_json::json!({}))
        );
        assert_eq!(
            parse(&["--list-tools"]).unwrap().unwrap().mode,
            NativeMode::ListTools
        );
        assert_eq!(
            parse(&["--mcp", "--data-dir", dir]).unwrap().unwrap().mode,
            NativeMode::Mcp
        );
        assert_eq!(
            parse(&["--serial-daemon", "--data-dir", dir])
                .unwrap()
                .unwrap()
                .mode,
            NativeMode::Daemon
        );
        assert!(parse(&[]).unwrap().is_none());
    }

    #[test]
    fn cli_rejects_malformed_json_nonobjects_and_unexpected_arguments() {
        for args in [
            vec!["--tool"],
            vec!["--tool", "--list-tools"],
            vec!["--tool", ""],
            vec!["--tool", "gizmoth_status", "{"],
            vec!["--tool", "gizmoth_status", "[]"],
            vec!["--tool", "gizmoth_status", "null"],
            vec!["--tool", "gizmoth_status", "42"],
            vec!["--tool", "gizmoth_status", "true"],
            vec!["--tool", "gizmoth_status", r#""text""#],
            vec!["--tool", "gizmoth_status", "{}", "{}"],
            vec!["--list-tools", "unexpected"],
            vec!["--list-tools", "--mcp"],
            vec!["--data-dir"],
            vec!["--data-dir", ""],
            vec!["--data-dir", "a", "--data-dir", "b", "--list-tools"],
            vec!["--data-dir", "a"],
            vec!["--unknown"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }
}

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
    blocking(move || {
        let device = credentials::import(&app, credential)?;
        desktop_state::refresh(&app);
        Ok(device)
    })
    .await
}
#[tauri::command]
async fn rename_device(app: AppHandle, id: String, name: String) -> R<Device> {
    blocking(move || {
        let device = credentials::rename(&app, &id, &name)?;
        desktop_state::refresh(&app);
        Ok(device)
    })
    .await
}
#[tauri::command]
async fn remove_device(app: AppHandle, id: String) -> R<()> {
    blocking(move || {
        credentials::remove(&app, &id)?;
        desktop_state::refresh(&app);
        Ok(())
    })
    .await
}
fn require_main(window: &tauri::WebviewWindow) -> R<()> {
    if window.label() == "main" {
        Ok(())
    } else {
        Err("只能从主窗口管理设备密钥。".into())
    }
}
#[tauri::command]
async fn device_key_status(
    app: AppHandle,
    window: tauri::WebviewWindow,
    id: String,
) -> R<credentials::KeyStatus> {
    require_main(&window)?;
    blocking(move || credentials::status(&app, &id)).await
}
#[tauri::command]
async fn device_key(app: AppHandle, window: tauri::WebviewWindow, id: String) -> R<Option<String>> {
    require_main(&window)?;
    blocking(move || credentials::key(&app, &id)).await
}
#[tauri::command]
async fn device_save_key(
    app: AppHandle,
    window: tauri::WebviewWindow,
    id: String,
    api_key: String,
) -> R<()> {
    let key = Zeroizing::new(api_key);
    require_main(&window)?;
    blocking(move || credentials::save_key(&app, &id, key)).await
}
#[tauri::command]
async fn device_migrate_key(
    app: AppHandle,
    window: tauri::WebviewWindow,
    id: String,
) -> R<credentials::MigrationResult> {
    require_main(&window)?;
    blocking(move || credentials::migrate(&app, &id)).await
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
    let device = ble::bind(&app, &state, &id).await?;
    desktop_state::refresh(&app);
    Ok(device)
}
#[tauri::command]
fn cancel_pairing(state: tauri::State<'_, ble::BleState>) {
    ble::cancel(&state);
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
#[tauri::command]
async fn uninstall_app(app: AppHandle, device_id: String, app_id: String) -> R<Value> {
    device_api::uninstall(&app, &device_id, &app_id).await
}

#[tauri::command]
async fn format_lua_code(window: tauri::WebviewWindow, code: String) -> R<String> {
    require_main(&window)?;
    blocking(move || formatter::lua(&code)).await
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

// Global instructions for external coding agents (main window only).
#[tauri::command]
async fn agents_status(app: AppHandle, window: tauri::WebviewWindow) -> R<agents::Status> {
    require_main(&window)?;
    blocking(move || agents::status(&app)).await
}

#[tauri::command]
async fn agents_set(
    app: AppHandle,
    window: tauri::WebviewWindow,
    id: String,
    installed: bool,
) -> R<agents::Status> {
    require_main(&window)?;
    blocking(move || agents::set(&app, &id, installed)).await
}

// Assistant access beyond the project
#[tauri::command]
fn agent_permissions_get(app: AppHandle, id: String) -> R<agent_access::Permissions> {
    agent_access::permissions(&app, &id)
}
#[tauri::command]
fn agent_permissions_set(
    app: AppHandle,
    id: String,
    permissions: agent_access::Permissions,
) -> R<()> {
    agent_access::set_permissions(&app, &id, permissions)
}
#[tauri::command]
async fn home_list(app: AppHandle, id: String, path: String) -> R<Vec<agent_access::HomeEntry>> {
    blocking(move || agent_access::list(&app, &id, &path)).await
}
#[tauri::command]
async fn home_read(app: AppHandle, id: String, path: String) -> R<String> {
    blocking(move || agent_access::read(&app, &id, &path)).await
}
#[tauri::command]
async fn home_write(app: AppHandle, id: String, path: String, content: String) -> R<()> {
    blocking(move || agent_access::write(&app, &id, &path, &content)).await
}
#[tauri::command]
async fn web_fetch(app: AppHandle, id: String, url: String) -> R<agent_access::Fetched> {
    agent_access::fetch(&app, &id, &url).await
}
#[tauri::command]
async fn web_search(
    app: AppHandle,
    id: String,
    query: String,
) -> R<Vec<agent_access::SearchResult>> {
    agent_access::search(&app, &id, &query).await
}
#[tauri::command]
async fn search_settings_get(app: AppHandle) -> R<agent_access::SearchSettings> {
    blocking(move || agent_access::search_settings(&app)).await
}
#[tauri::command]
async fn search_settings_save(
    app: AppHandle,
    provider: Option<agent_access::SearchProvider>,
    api_key: Option<String>,
) -> R<agent_access::SearchSettings> {
    blocking(move || agent_access::save_search(&app, provider, api_key)).await
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
    let context = tauri::generate_context!();
    #[cfg(feature = "native-e2e")]
    let context = e2e::context(context);
    // Build main during setup, after loading its persisted native appearance.
    let mut context = context;
    for window in &mut context.config_mut().app.windows {
        if window.label == "main" {
            window.create = false;
        }
    }
    let builder = tauri::Builder::default();
    #[cfg(feature = "native-e2e")]
    let builder = builder.plugin(tauri_plugin_wdio_webdriver::init());
    // Each native test owns a temporary store and driver port; concurrent worktrees must not share a singleton.
    #[cfg(not(feature = "native-e2e"))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        desktop_state::show(app);
    }));
    let app = builder
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(
            tauri_plugin_opener::Builder::default()
                .open_js_links_on_click(false)
                .build(),
        )
        .manage(credentials::DeviceStore(Mutex::new(())))
        .manage(providers::ProviderStore(Mutex::new(())))
        .manage(agent_access::AccessStore(Mutex::new(())))
        .manage(ble::BleState::default())
        .manage(devenv::DevEnv::default())
        .setup(|app| {
            #[cfg(not(feature = "native-e2e"))]
            serial::commands::autostart(app);
            desktop_state::setup(app)
        })
        .on_window_event(|window, event| {
            desktop_state::on_window_event(window, event);
            simulator::on_window_event(window, event);
        })
        .invoke_handler(|invoke| {
            #[cfg(feature = "native-e2e")]
            if !e2e::allow_command(invoke.message.command()) {
                let command = invoke.message.command();
                invoke.resolver.reject(format!(
                    "This operation is disabled in native integration tests: {command}"
                ));
                return true;
            }
            if invoke.message.webview_ref().label() != "main"
                && !matches!(
                    invoke.message.command(),
                    "simulator_profiles_list"
                        | "simulator_profile_update"
                        | "simulator_profile_delete"
                        | "simulator_window_destroy"
                        | "simulator_package_put"
                        | "simulator_package_remove"
                        | "theme_preference"
                        | "language_preference"
                )
            {
                invoke.resolver.reject("此操作只能从主窗口执行。");
                return true;
            }
            let handler: fn(tauri::ipc::Invoke) -> bool = tauri::generate_handler![
                simulator::open_simulator,
                simulator::simulator_prepare_update,
                simulator::simulator_cancel_update,
                simulator_runtime::simulator_package_put,
                simulator_runtime::simulator_package_remove,
                simulator::simulator_profiles_list,
                simulator::simulator_profile_create,
                simulator::simulator_profile_update,
                simulator::simulator_profile_delete,
                simulator::simulator_window_destroy,
                list_devices,
                import_device,
                remove_device,
                rename_device,
                device_key_status,
                device_key,
                device_save_key,
                device_migrate_key,
                scan_devices,
                bind_device,
                cancel_pairing,
                device_overview,
                set_device_level,
                list_apps,
                run_app,
                uninstall_app,
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
                format_lua_code,
                project_export,
                project_install,
                chat_load,
                chat_save,
                agents_status,
                agents_set,
                devenv::devenv_state,
                devenv::devenv_refresh,
                devenv::devenv_install,
                devenv::devenv_reinstall,
                devenv::devenv_open_folder,
                devenv::devenv_cancel,
                devenv::devenv_remove_preview,
                devenv::devenv_remove,
                devenv::devenv_add_local,
                agent_permissions_get,
                agent_permissions_set,
                home_list,
                home_read,
                home_write,
                web_fetch,
                web_search,
                search_settings_get,
                search_settings_save,
                providers_list,
                provider_save,
                provider_remove,
                provider_key,
                desktop_state::desktop_preferences,
                desktop_state::desktop_take_tray_action,
                desktop_state::set_desktop_preferences,
                desktop_state::theme_preference,
                desktop_state::set_theme_preference,
                desktop_state::language_preference,
                desktop_state::set_language_preference,
                serial::commands::serial_overview,
                serial::commands::serial_watch,
                serial::commands::serial_system_ports,
                serial::commands::serial_add,
                serial::commands::serial_update,
                serial::commands::serial_remove,
                serial::commands::serial_logs,
                serial::commands::serial_clear_logs,
                serial::commands::serial_send,
                serial::commands::serial_choose_package,
                serial::commands::serial_inspect,
                serial::commands::serial_job,
                serial::commands::serial_cancel_job,
                serial::commands::serial_answer_request,
                serial::commands::serial_release,
                serial::commands::serial_stop_service,
                serial::commands::serial_export_logs,
                serial::commands::serial_remote_probe,
                serial::commands::serial_remote_add,
                serial::commands::serial_remote_remove,
                serial::commands::gateway_status,
                serial::commands::gateway_update
            ];
            handler(invoke)
        })
        .build(context)
        .expect("Unable to start Gizmoth");
    #[cfg(all(feature = "native-e2e", target_os = "macos"))]
    let app = {
        let mut app = app;
        if e2e::background() {
            // Configure before the event loop starts: setup runs after macOS's
            // initial application activation and would allow a brief focus steal.
            app.set_activation_policy(tauri::ActivationPolicy::Prohibited);
        }
        app
    };
    app.run(|app, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            simulator::prepare_exit(app, &api);
        }
    });
}
