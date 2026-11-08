use serde::{Deserialize, Serialize};
use std::{
    fs,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};

const TRAY_ID: &str = "gizmoth";
const TRAY_EVENT: &str = "desktop-tray-action";

pub struct DesktopState {
    keep_in_tray: AtomicBool,
    pending: Mutex<Option<TrayAction>>,
    preferences: Mutex<Preferences>,
}
#[derive(Clone, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum TrayAction {
    Navigate { page: String },
    OpenSimulator { profile_id: String },
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePreference {
    pub(crate) fn native(self) -> Option<tauri::Theme> {
        match self {
            Self::System => None,
            Self::Light => Some(tauri::Theme::Light),
            Self::Dark => Some(tauri::Theme::Dark),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LanguagePreference {
    #[default]
    #[serde(rename = "system")]
    System,
    #[serde(rename = "zh-CN")]
    SimplifiedChinese,
    #[serde(rename = "en")]
    English,
}

impl LanguagePreference {
    fn chinese(self) -> bool {
        match self {
            Self::System => sys_locale::get_locale().is_some_and(|locale| {
                locale.eq_ignore_ascii_case("zh")
                    || locale.to_ascii_lowercase().starts_with("zh-")
                    || locale.to_ascii_lowercase().starts_with("zh_")
            }),
            Self::SimplifiedChinese => true,
            Self::English => false,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    pub keep_in_tray: bool,
    #[serde(default)]
    pub theme: ThemePreference,
    #[serde(default)]
    pub language: LanguagePreference,
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, serde_json::Value>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            keep_in_tray: true,
            theme: ThemePreference::System,
            language: LanguagePreference::System,
            extra: Default::default(),
        }
    }
}

pub(crate) fn background(theme: tauri::Theme) -> tauri::utils::config::Color {
    match theme {
        tauri::Theme::Dark => tauri::utils::config::Color(24, 24, 27, 255),
        _ => tauri::utils::config::Color(255, 255, 255, 255),
    }
}

fn update_background(window: &tauri::WebviewWindow, theme: tauri::Theme) {
    let _ = window.set_background_color(Some(background(theme)));
}

fn apply_theme(app: &tauri::AppHandle, preference: ThemePreference) {
    app.set_theme(preference.native());
    for window in app.webview_windows().values() {
        let _ = window.set_theme(preference.native());
        if let Ok(theme) = window.theme() {
            update_background(window, theme);
        }
    }
}

pub(crate) fn simulator_theme(app: &tauri::AppHandle) -> (ThemePreference, tauri::Theme) {
    let preference = app
        .state::<DesktopState>()
        .preferences
        .lock()
        .map(|p| p.theme)
        .unwrap_or_default();
    let effective = preference
        .native()
        .or_else(|| app.get_webview_window("main").and_then(|w| w.theme().ok()))
        .unwrap_or(tauri::Theme::Light);
    (preference, effective)
}

pub(crate) fn show_themed_window(window: &tauri::WebviewWindow) -> Result<(), String> {
    let state = window.state::<DesktopState>();
    let theme = state.preferences.lock().map_err(|_| "无法读取设置")?.theme;
    // Native window calls can synchronously wait on the UI thread, which also
    // serves a new webview's preference reads. Do not retain the settings lock.
    window
        .set_theme(theme.native())
        .map_err(|e| e.to_string())?;
    if let Ok(theme) = window.theme() {
        update_background(window, theme);
    }
    window.show().map_err(|e| e.to_string())
}

fn persist_update(
    directory: &std::path::Path,
    saved: &mut Preferences,
    update: impl FnOnce(&mut Preferences),
) -> Result<(), String> {
    let mut next = saved.clone();
    update(&mut next);
    let value = serde_json::to_vec(&next).map_err(|_| "无法保存设置")?;
    fs::write(directory.join("desktop.tmp"), value).map_err(|_| "无法保存设置")?;
    fs::rename(
        directory.join("desktop.tmp"),
        directory.join("desktop.json"),
    )
    .map_err(|_| "无法保存设置")?;
    *saved = next;
    Ok(())
}

pub(crate) fn show(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn short_name(name: &str) -> String {
    let single_line = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut label: String = single_line.chars().take(18).collect();
    if single_line.chars().count() > 18 {
        label.push('…');
    }
    // Windows native menus interpret ampersands as mnemonic markers.
    label.replace('&', "&&")
}
fn model_name(model: &str, chinese: bool) -> &str {
    match model {
        "h106-tiga" => "Tiga",
        "h106-zero" => "Zero",
        "h106-orb" => "Orb",
        "h106-zeta" => "Zeta",
        _ => {
            if chinese {
                "未知型号"
            } else {
                "Unknown model"
            }
        }
    }
}
fn build_menu(app: &tauri::AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let chinese = app
        .state::<DesktopState>()
        .preferences
        .lock()
        .map(|p| p.language.chinese())
        .unwrap_or(false);
    let text = |zh, en| if chinese { zh } else { en };
    let menu = Menu::new(app)?;
    menu.append(&MenuItem::with_id(
        app,
        "tray-version",
        format!("GizMoth {}", app.package_info().version),
        false,
        None::<&str>,
    )?)?;
    menu.append(&MenuItem::with_id(
        app,
        "open",
        text("打开 GizMoth", "Open GizMoth"),
        true,
        None::<&str>,
    )?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    let devices = match super::credentials::list(app) {
        Ok(all) => {
            if chinese {
                format!("设备 · {} 台已配对", all.len())
            } else {
                format!("Devices · {} paired", all.len())
            }
        }
        Err(_) => text("设备 · 列表暂不可用", "Devices · unavailable").into(),
    };
    menu.append(&MenuItem::with_id(
        app,
        "devices",
        devices,
        true,
        None::<&str>,
    )?)?;
    let profiles = super::simulator::simulator_profiles_list(app.clone());
    let title = match &profiles {
        Ok(all) => format!("{} · {}", text("模拟器", "Simulators"), all.len()),
        Err(_) => text("模拟器", "Simulators").into(),
    };
    let simulators = Submenu::new(app, title, true)?;
    match profiles {
        Ok(all) if !all.is_empty() => {
            for profile in all {
                simulators.append(&MenuItem::with_id(
                    app,
                    format!("simulator:{}", profile.id),
                    format!(
                        "{} · {}",
                        short_name(&profile.name),
                        model_name(&profile.model, chinese)
                    ),
                    true,
                    None::<&str>,
                )?)?;
            }
        }
        result => {
            let text = if result.is_err() {
                text("无法读取模拟器列表", "Simulators unavailable")
            } else {
                text("还没有模拟器", "No simulators yet")
            };
            simulators.append(&MenuItem::with_id(
                app,
                "simulator-empty",
                text,
                false,
                None::<&str>,
            )?)?;
        }
    }
    simulators.append(&PredefinedMenuItem::separator(app)?)?;
    simulators.append(&MenuItem::with_id(
        app,
        "simulators",
        text("管理模拟器…", "Manage simulators…"),
        true,
        None::<&str>,
    )?)?;
    menu.append(&simulators)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(
        app,
        "settings",
        text("设置…", "Settings…"),
        true,
        None::<&str>,
    )?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(
        app,
        "quit",
        text("退出 GizMoth", "Quit GizMoth"),
        true,
        None::<&str>,
    )?)?;
    Ok(menu)
}

/// Refresh after persisted device/profile changes, outside either store's lock.
/// A menu failure must not turn an already successful save into an error.
pub(crate) fn refresh(app: &tauri::AppHandle) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        if let Ok(menu) = build_menu(app) {
            let _ = tray.set_menu(Some(menu));
        }
    }
}
fn queue(app: &tauri::AppHandle, action: TrayAction) {
    let state = app.state::<DesktopState>();
    if let Ok(mut pending) = state.pending.lock() {
        *pending = Some(action);
    }
    // Keep the action until the main webview consumes it. Early clicks during
    // startup are handled once its listener is ready, and hidden windows work too.
    let _ = app.emit_to("main", TRAY_EVENT, ());
}

pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let directory = app.path().app_data_dir()?;
    fs::create_dir_all(&directory)?;
    let preferences = match fs::read(directory.join("desktop.json")) {
        Ok(bytes) => serde_json::from_slice::<Preferences>(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Preferences::default(),
        Err(error) => return Err(error.into()),
    };
    let theme = preferences.theme;
    app.manage(DesktopState {
        keep_in_tray: AtomicBool::new(preferences.keep_in_tray),
        pending: Mutex::new(None),
        preferences: Mutex::new(preferences),
    });
    app.set_theme(theme.native());
    if let Some(config) = app.config().app.windows.iter().find(|w| w.label == "main") {
        let builder = tauri::WebviewWindowBuilder::from_config(app, config)?
            .visible(false)
            .theme(theme.native());
        let builder = if let Some(native) = theme.native() {
            builder.background_color(background(native))
        } else {
            builder
        };
        let main = builder.build()?;
        apply_theme(app.handle(), theme);
        if config.visible {
            main.show()?;
        }
    }
    let menu = build_menu(app.handle())?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(tauri::include_image!("icons/tray.png"))
        .icon_as_template(true)
        .tooltip("GizMoth")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show(app),
            "quit" => app.exit(0),
            page @ ("devices" | "simulators" | "settings") => {
                show(app);
                queue(app, TrayAction::Navigate { page: page.into() });
            }
            id => {
                if let Some(profile_id) = id.strip_prefix("simulator:") {
                    if uuid::Uuid::parse_str(profile_id).is_ok() {
                        queue(
                            app,
                            TrayAction::OpenSimulator {
                                profile_id: profile_id.into(),
                            },
                        );
                    }
                }
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick { .. } = event {
                show(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

#[tauri::command]
pub fn desktop_take_tray_action(
    state: tauri::State<'_, DesktopState>,
) -> Result<Option<TrayAction>, String> {
    state
        .pending
        .lock()
        .map(|mut action| action.take())
        .map_err(|_| "无法读取托盘操作".into())
}
#[tauri::command]
pub fn desktop_preferences(state: tauri::State<'_, DesktopState>) -> Result<Preferences, String> {
    state
        .preferences
        .lock()
        .map(|p| p.clone())
        .map_err(|_| "无法读取设置".into())
}

#[tauri::command]
pub fn theme_preference(state: tauri::State<'_, DesktopState>) -> Result<ThemePreference, String> {
    state
        .preferences
        .lock()
        .map(|p| p.theme)
        .map_err(|_| "无法读取设置".into())
}

#[tauri::command]
pub fn set_theme_preference(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, DesktopState>,
    preference: ThemePreference,
) -> Result<ThemePreference, String> {
    if window.label() != "main" {
        return Err("此操作只能从主窗口执行。".into());
    }
    let mut saved = state.preferences.lock().map_err(|_| "无法保存设置")?;
    let directory = app
        .path()
        .app_data_dir()
        .map_err(|_| "无法定位应用数据目录")?;
    persist_update(&directory, &mut saved, |p| p.theme = preference)?;
    // Serialize saves, native updates and notifications in the same order.
    apply_theme(&app, preference);
    app.emit("desktop-theme-changed", preference)
        .map_err(|e| e.to_string())?;
    Ok(preference)
}
#[tauri::command]
pub fn language_preference(
    state: tauri::State<'_, DesktopState>,
) -> Result<LanguagePreference, String> {
    state
        .preferences
        .lock()
        .map(|p| p.language)
        .map_err(|_| "无法读取设置".into())
}

#[tauri::command]
pub fn set_language_preference(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, DesktopState>,
    preference: LanguagePreference,
) -> Result<LanguagePreference, String> {
    if window.label() != "main" {
        return Err("此操作只能从主窗口执行。".into());
    }
    let mut saved = state.preferences.lock().map_err(|_| "无法保存设置")?;
    let directory = app
        .path()
        .app_data_dir()
        .map_err(|_| "无法定位应用数据目录")?;
    persist_update(&directory, &mut saved, |p| p.language = preference)?;
    app.emit("desktop-language-changed", preference)
        .map_err(|e| e.to_string())?;
    drop(saved);
    refresh(&app);
    Ok(preference)
}

#[tauri::command]
pub fn set_desktop_preferences(
    app: tauri::AppHandle,
    state: tauri::State<'_, DesktopState>,
    keep_in_tray: bool,
) -> Result<(), String> {
    let mut saved = state.preferences.lock().map_err(|_| "无法保存设置")?;
    let directory = app
        .path()
        .app_data_dir()
        .map_err(|_| "无法定位应用数据目录")?;
    persist_update(&directory, &mut saved, |p| p.keep_in_tray = keep_in_tray)?;
    state.keep_in_tray.store(keep_in_tray, Ordering::Relaxed);
    Ok(())
}

pub fn on_window_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    if let tauri::WindowEvent::ThemeChanged(theme) = event {
        if let Some(webview) = window.app_handle().get_webview_window(window.label()) {
            update_background(&webview, *theme);
        }
    }
    if window.label() != "main" {
        return;
    }
    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
        if window
            .state::<DesktopState>()
            .keep_in_tray
            .load(Ordering::Relaxed)
            && window.hide().is_ok()
        {
            api.prevent_close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_preferences_default_to_system_theme_and_language() {
        let p: Preferences = serde_json::from_str(r#"{"keepInTray":false}"#).unwrap();
        assert!(!p.keep_in_tray);
        assert_eq!(p.theme, ThemePreference::System);
        assert_eq!(p.language, LanguagePreference::System);
        assert!(Preferences::default().keep_in_tray);
    }

    #[test]
    fn theme_and_language_wire_values_are_strict() {
        for (preference, value) in [
            (ThemePreference::System, "system"),
            (ThemePreference::Light, "light"),
            (ThemePreference::Dark, "dark"),
        ] {
            assert_eq!(serde_json::to_value(preference).unwrap(), value);
            assert_eq!(
                serde_json::from_value::<ThemePreference>(value.into()).unwrap(),
                preference
            );
        }
        for (preference, value) in [
            (LanguagePreference::System, "system"),
            (LanguagePreference::SimplifiedChinese, "zh-CN"),
            (LanguagePreference::English, "en"),
        ] {
            assert_eq!(serde_json::to_value(preference).unwrap(), value);
            assert_eq!(
                serde_json::from_value::<LanguagePreference>(value.into()).unwrap(),
                preference
            );
        }
        for invalid in [
            serde_json::json!("auto"),
            serde_json::json!("Dark"),
            serde_json::Value::Null,
            serde_json::json!(0),
        ] {
            assert!(serde_json::from_value::<ThemePreference>(invalid).is_err());
        }
        for invalid in ["zh", "en-US", "ZH-CN", "auto"] {
            assert!(serde_json::from_value::<LanguagePreference>(invalid.into()).is_err());
        }
        assert_eq!(ThemePreference::System.native(), None);
    }

    #[test]
    fn persisted_updates_preserve_other_preferences_and_unknown_fields() {
        let directory = tempfile::tempdir().unwrap();
        let mut saved: Preferences =
            serde_json::from_str(r#"{"keepInTray":true,"future":{"value":7}}"#).unwrap();
        persist_update(directory.path(), &mut saved, |p| {
            p.theme = ThemePreference::Dark
        })
        .unwrap();
        persist_update(directory.path(), &mut saved, |p| {
            p.language = LanguagePreference::English
        })
        .unwrap();
        persist_update(directory.path(), &mut saved, |p| p.keep_in_tray = false).unwrap();
        let disk: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.path().join("desktop.json")).unwrap())
                .unwrap();
        assert_eq!(
            disk,
            serde_json::json!({"keepInTray":false,"theme":"dark","language":"en","future":{"value":7}})
        );
        persist_update(directory.path(), &mut saved, |p| {
            p.theme = ThemePreference::System
        })
        .unwrap();
        assert!(!saved.keep_in_tray);
        assert_eq!(saved.language, LanguagePreference::English);
        assert!(
            persist_update(&directory.path().join("missing"), &mut saved, |p| p
                .language =
                LanguagePreference::SimplifiedChinese)
            .is_err()
        );
        assert_eq!(saved.language, LanguagePreference::English);
    }

    #[test]
    fn native_menu_labels_are_bounded_single_line_and_keep_mnemonics_literal() {
        assert_eq!(short_name("  Desk\n Tiga\t test "), "Desk Tiga test");
        assert_eq!(short_name("A&B"), "A&&B");
        assert_eq!(
            short_name(&"界".repeat(30)),
            format!("{}…", "界".repeat(18))
        );
    }
    #[test]
    fn simulator_actions_keep_the_profile_id_and_use_the_shared_transport_contract() {
        let action = TrayAction::OpenSimulator {
            profile_id: "example-id".into(),
        };
        assert_eq!(
            serde_json::to_value(action).unwrap(),
            serde_json::json!({"kind":"open-simulator","profileId":"example-id"})
        );
    }
}
