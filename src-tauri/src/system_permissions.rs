//! Main-window permission IPC. Reading status never initializes Bluetooth,
//! requests notification authorization, or tests network connectivity.
use serde::{Deserialize, Serialize};
use tauri_plugin_opener::OpenerExt;

#[cfg(target_os = "macos")]
mod macos;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Granted,
    Denied,
    #[cfg_attr(
        windows,
        allow(
            dead_code,
            reason = "Windows does not prompt for toast access; retain the shared IPC state."
        )
    )]
    Prompt,
    Unknown,
    #[cfg_attr(
        windows,
        allow(
            dead_code,
            reason = "Windows toast APIs report errors instead of an unavailable state; retain the shared IPC state."
        )
    )]
    Unavailable,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Permission {
    Notifications,
    Bluetooth,
    LocalNetwork,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestPermission {
    Notifications,
    Bluetooth,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Permissions {
    notifications: State,
    bluetooth: State,
    local_network: State,
}

async fn notification_state(app: &tauri::AppHandle) -> Result<State, String> {
    #[cfg(target_os = "macos")]
    {
        let _ = app;
        macos::notification_state().await
    }
    #[cfg(windows)]
    {
        windows::notification_state(app)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = app;
        Ok(State::Unavailable)
    }
}
fn bluetooth_state() -> State {
    #[cfg(target_os = "macos")]
    {
        macos::bluetooth_state()
    }
    #[cfg(not(target_os = "macos"))]
    {
        State::Unknown
    }
}
#[tauri::command]
pub async fn system_permissions(app: tauri::AppHandle) -> Permissions {
    Permissions {
        // A query failure is unknown, never a presumed grant or denial.
        notifications: notification_state(&app).await.unwrap_or(State::Unknown),
        bluetooth: bluetooth_state(),
        // There is no supported per-app local-network authorization query on
        // macOS or Windows. Connectivity is not authorization.
        local_network: State::Unknown,
    }
}

#[tauri::command]
pub async fn system_permission_request(
    app: tauri::AppHandle,
    permission: RequestPermission,
) -> Result<State, String> {
    #[cfg(feature = "native-e2e")]
    {
        let _ = (app, permission);
        return Err("原生集成测试不会请求系统权限。".into());
    }
    #[cfg(not(feature = "native-e2e"))]
    match permission {
        RequestPermission::Notifications => {
            #[cfg(target_os = "macos")]
            {
                macos::request_notifications().await
            }
            #[cfg(not(target_os = "macos"))]
            {
                notification_state(&app).await
            }
        }
        RequestPermission::Bluetooth => {
            use tauri::Manager;
            let state = bluetooth_state();
            if matches!(state, State::Denied | State::Granted | State::Unavailable) {
                return Ok(state);
            }
            // Initializes only the same manager/adapter used by Pair Device.
            // No scan, pairing, physical port or device access is performed.
            let initialized = tokio::time::timeout(
                std::time::Duration::from_secs(60),
                crate::ble::request_permission(&app.state::<crate::ble::BleState>()),
            )
            .await;
            let state = bluetooth_state();
            if matches!(state, State::Denied | State::Granted) {
                return Ok(state);
            }
            match initialized {
                Ok(Ok(())) => Ok(state),
                Ok(Err(error)) => Err(error),
                Err(_) => Err("等待蓝牙授权超时，请在系统设置中检查蓝牙权限。".into()),
            }
        }
    }
}

fn settings_url(permission: Permission) -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    {
        Some(match permission {
        Permission::Notifications => "x-apple.systempreferences:com.apple.preference.notifications?id=com.gizclaw.gizmoth",
        Permission::Bluetooth => "x-apple.systempreferences:com.apple.preference.security?Privacy_Bluetooth",
        Permission::LocalNetwork => "x-apple.systempreferences:com.apple.preference.security?Privacy_LocalNetwork",
    })
    }
    #[cfg(windows)]
    {
        Some(match permission {
            Permission::Notifications => "ms-settings:notifications",
            Permission::Bluetooth => "ms-settings:privacy-radios",
            Permission::LocalNetwork => "windowsdefender://network",
        })
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = permission;
        None
    }
}
#[tauri::command]
pub fn system_permission_settings(
    app: tauri::AppHandle,
    permission: Permission,
) -> Result<(), String> {
    let url = settings_url(permission).ok_or("此系统不支持打开对应的权限设置。")?;
    // Only these compile-time allowlisted OS URLs are accepted. The frontend
    // cannot supply paths, applications, URL schemes or arbitrary arguments.
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| format!("无法打开系统权限设置：{e}"))
}

pub(crate) async fn send(app: &tauri::AppHandle, title: &str, body: &str) -> Result<(), String> {
    #[cfg(feature = "native-e2e")]
    {
        let _ = (app, title, body);
        return Err("原生集成测试不会发送系统通知。".into());
    }
    #[cfg(not(feature = "native-e2e"))]
    {
        if notification_state(app).await? != State::Granted {
            return Err("通知权限尚未授予，请在设置中允许 GizMoth 发送通知。".into());
        }
        #[cfg(target_os = "macos")]
        {
            macos::send(title, body).await
        }
        #[cfg(windows)]
        {
            windows::send(app, title, body)
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            let _ = (title, body);
            Err("此系统暂不支持原生通知。".into())
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::State;
    use windows::{
        Data::Xml::Dom::XmlDocument,
        UI::Notifications::{
            NotificationSetting, ToastNotification, ToastNotificationManager, ToastNotifier,
        },
    };
    use windows_core::HSTRING;
    fn notifier(app: &tauri::AppHandle) -> Result<ToastNotifier, String> {
        // NSIS registers this exact AppUserModelID for the installed app.
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(
            &app.config().identifier,
        ))
        .map_err(|e| format!("无法连接 Windows 通知服务：{e}"))
    }
    pub fn notification_state(app: &tauri::AppHandle) -> Result<State, String> {
        let state = notifier(app)?.Setting().map_err(|e| e.to_string())?;
        Ok(match state {
            NotificationSetting::Enabled => State::Granted,
            NotificationSetting::DisabledForApplication
            | NotificationSetting::DisabledForUser
            | NotificationSetting::DisabledByGroupPolicy
            | NotificationSetting::DisabledByManifest => State::Denied,
            _ => State::Unknown,
        })
    }
    pub fn send(app: &tauri::AppHandle, title: &str, body: &str) -> Result<(), String> {
        let xml = XmlDocument::new().map_err(|e| e.to_string())?;
        let escape = |value: &str| {
            value
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
                .replace('\'', "&apos;")
        };
        xml.LoadXml(&HSTRING::from(format!("<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>", escape(title), escape(body))))
            .map_err(|e| format!("无法创建系统通知：{e}"))?;
        let toast = ToastNotification::CreateToastNotification(&xml).map_err(|e| e.to_string())?;
        notifier(app)?
            .Show(&toast)
            .map_err(|e| format!("系统通知发送失败：{e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ipc_states_and_permission_names_match_contract() {
        for (state, value) in [
            (State::Granted, "granted"),
            (State::Denied, "denied"),
            (State::Prompt, "prompt"),
            (State::Unknown, "unknown"),
            (State::Unavailable, "unavailable"),
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), value);
        }
        assert!(serde_json::from_value::<Permission>("localNetwork".into()).is_ok());
        assert!(serde_json::from_value::<RequestPermission>("notifications".into()).is_ok());
        assert!(serde_json::from_value::<RequestPermission>("bluetooth".into()).is_ok());
        assert!(serde_json::from_value::<RequestPermission>("localNetwork".into()).is_err());
        for invalid in [
            "file:///tmp",
            "notifications?url=other",
            "local-network",
            "camera",
        ] {
            assert!(serde_json::from_value::<Permission>(invalid.into()).is_err());
        }
    }
    #[test]
    fn settings_targets_are_fixed_os_schemes() {
        for permission in [
            Permission::Notifications,
            Permission::Bluetooth,
            Permission::LocalNetwork,
        ] {
            if let Some(url) = settings_url(permission) {
                assert!(
                    url.starts_with("x-apple.systempreferences:")
                        || url.starts_with("ms-settings:")
                        || url.starts_with("windowsdefender:")
                );
            }
        }
    }
}
