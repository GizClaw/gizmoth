//! Resident serial service. One `--serial-daemon` process owns every serial
//! port; the desktop window and MCP clients are clients of the same state.
pub mod client;
pub mod commands;
pub mod daemon;
pub mod gateway;
mod h2loader;
mod logs;

use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const API_VERSION: u32 = 1;
pub const BUILD_ID: &str = env!("GIZMOTH_SERIAL_BUILD");
pub const DAEMON_FLAG: &str = "--serial-daemon";
const IDENTIFIER: &str = "com.gizclaw.gizmoth";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// Probe H2Loader first, then fall back to a plain serial monitor.
    Auto,
    H2loader,
    Serial,
}

/// Who controls a connected port.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// GizMoth controls the port: status, jobs and sends. External pipes are refused.
    #[default]
    Takeover,
    /// GizMoth only records traffic and relays one external WebSocket holder.
    Piping,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsbIdentity {
    pub vid: u16,
    pub pid: u16,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub product: Option<String>,
    #[serde(default)]
    pub manufacturer: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortConfig {
    pub id: String,
    pub name: String,
    /// Local endpoint, e.g. /dev/cu.usbmodem1101 or COM3.
    pub path: String,
    pub protocol: Protocol,
    pub baud: u32,
    pub enabled: bool,
    #[serde(default)]
    pub mode: Mode,
    /// An Auto port has answered as H2Loader; piping then uses 460800 baud.
    #[serde(default)]
    pub h2loader_seen: bool,
    /// Piping only: grant queued requests in order without asking.
    #[serde(default = "default_true")]
    pub auto_grant: bool,
    #[serde(default)]
    pub usb: Option<UsbIdentity>,
    pub created_at: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemPort {
    pub path: String,
    pub name: String,
    pub usb: Option<UsbIdentity>,
}

fn default_true() -> bool {
    true
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Mirrors Tauri's app_data_dir so the daemon and MCP entry need no AppHandle.
pub fn default_data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let base =
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"));
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    base.map(|b| b.join(IDENTIFIER))
}

pub fn state_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("serial")
}

/// Ports the operating system exposes now. Enumeration never opens a port.
pub fn system_ports() -> Vec<SystemPort> {
    let Ok(ports) = serialport::available_ports() else {
        return Vec::new();
    };
    ports
        .into_iter()
        // Darwin lists callout (cu.) and dial-in (tty.) aliases; keep callout.
        .filter(|p| !cfg!(target_os = "macos") || p.port_name.starts_with("/dev/cu."))
        .map(|p| match p.port_type {
            serialport::SerialPortType::UsbPort(info) => SystemPort {
                name: info
                    .product
                    .clone()
                    .or(info.manufacturer.clone())
                    .unwrap_or_else(|| "USB Serial".into()),
                path: p.port_name,
                usb: Some(UsbIdentity {
                    vid: info.vid,
                    pid: info.pid,
                    serial: info.serial_number.filter(|s| !s.is_empty()),
                    product: info.product,
                    manufacturer: info.manufacturer,
                }),
            },
            _ => SystemPort {
                name: "Serial".into(),
                path: p.port_name,
                usb: None,
            },
        })
        .collect()
}

pub fn h2loader_available() -> bool {
    h2loader::AVAILABLE
}
