use serde::{Deserialize, Serialize};
use std::{
    fs,
    sync::atomic::{AtomicBool, Ordering},
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{TrayIconBuilder, TrayIconEvent},
    Manager,
};

pub struct DesktopState(pub AtomicBool);
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    pub keep_in_tray: bool,
}

fn show(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let directory = app.path().app_data_dir()?;
    fs::create_dir_all(&directory)?;
    let preferences = match fs::read(directory.join("desktop.json")) {
        Ok(bytes) => serde_json::from_slice::<Preferences>(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Preferences { keep_in_tray: true }
        }
        Err(error) => return Err(error.into()),
    };
    app.manage(DesktopState(AtomicBool::new(preferences.keep_in_tray)));
    let open = MenuItem::with_id(app, "open", "打开 GizMoth", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 GizMoth", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    TrayIconBuilder::new()
        .icon(
            app.default_window_icon()
                .ok_or("Missing application icon")?
                .clone(),
        )
        .tooltip("GizMoth")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show(app),
            "quit" => app.exit(0),
            _ => (),
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
pub fn desktop_preferences(state: tauri::State<'_, DesktopState>) -> Preferences {
    Preferences {
        keep_in_tray: state.0.load(Ordering::Relaxed),
    }
}
#[tauri::command]
pub fn set_desktop_preferences(
    app: tauri::AppHandle,
    state: tauri::State<'_, DesktopState>,
    keep_in_tray: bool,
) -> Result<(), String> {
    let directory = app
        .path()
        .app_data_dir()
        .map_err(|_| "无法定位应用数据目录")?;
    let value = serde_json::to_vec(&Preferences { keep_in_tray }).map_err(|_| "无法保存设置")?;
    fs::write(directory.join("desktop.tmp"), value).map_err(|_| "无法保存设置")?;
    fs::rename(
        directory.join("desktop.tmp"),
        directory.join("desktop.json"),
    )
    .map_err(|_| "无法保存设置")?;
    state.0.store(keep_in_tray, Ordering::Relaxed);
    Ok(())
}

pub fn on_window_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
        if window.state::<DesktopState>().0.load(Ordering::Relaxed) && window.hide().is_ok() {
            api.prevent_close();
        }
    }
}
