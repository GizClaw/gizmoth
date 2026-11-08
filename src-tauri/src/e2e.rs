//! Native integration-test setup. This module is never part of a normal build.
use std::path::PathBuf;
use tauri::utils::config::AppDirectoriesOverride;

pub fn context(mut context: tauri::Context<tauri::Wry>) -> tauri::Context<tauri::Wry> {
    assert_eq!(
        context.config().identifier,
        "com.gizclaw.gizmoth.e2e",
        "native-e2e requires tauri.e2e.conf.json; refusing to use normal app data"
    );
    let root = PathBuf::from(
        std::env::var_os("GIZMOTH_E2E_DATA_DIR")
            .expect("native-e2e requires a fresh GIZMOTH_E2E_DATA_DIR"),
    );
    assert!(root.is_absolute(), "E2E app data must be an absolute path");
    let root = root
        .canonicalize()
        .expect("E2E app data must already exist");
    assert!(
        root.join(".gizmoth-native-e2e").is_file(),
        "E2E app data must contain the test harness ownership marker"
    );
    context.config_mut().app.app_directories_override = Some(AppDirectoriesOverride::Root(root));
    for window in &mut context.config_mut().app.windows {
        // WKWebView does not use appDirectoriesOverride for website storage.
        window.incognito = true;
    }
    context
}

/// Home-folder tools in tests see only a harness-owned fake home, never the real one.
pub fn home_override() -> Result<PathBuf, String> {
    let home = PathBuf::from(
        std::env::var_os("GIZMOTH_E2E_HOME").ok_or("native-e2e requires GIZMOTH_E2E_HOME")?,
    );
    let home = home.canonicalize().map_err(|_| "E2E home must exist")?;
    if !home.join(".gizmoth-native-e2e-home").is_file() {
        return Err("E2E home must contain the harness marker".into());
    }
    Ok(home)
}

/// No test can accidentally pair a device or touch an OS credential store.
/// These are real application handlers operating only on the isolated fixture.
pub fn allow_command(command: &str) -> bool {
    matches!(
        command,
        "list_devices"
            | "projects_list"
            | "project_create"
            | "project_delete"
            | "project_files"
            | "project_read"
            | "project_write"
            | "project_remove_file"
            | "project_rename_file"
            | "project_check"
            | "format_lua_code"
            | "chat_load"
            | "chat_save"
            | "agent_permissions_get"
            | "agent_permissions_set"
            | "home_list"
            | "home_read"
            | "home_write"
            | "providers_list"
            | "desktop_preferences"
            | "desktop_take_tray_action"
            | "set_desktop_preferences"
            | "theme_preference"
            | "set_theme_preference"
            | "language_preference"
            | "set_language_preference"
            | "open_simulator"
            | "simulator_profiles_list"
            | "simulator_profile_create"
            | "simulator_profile_update"
            | "simulator_profile_delete"
            | "simulator_window_destroy"
            | "simulator_package_put"
            | "simulator_package_remove"
    )
}
