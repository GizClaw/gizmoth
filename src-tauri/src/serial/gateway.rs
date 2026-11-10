//! GizMoth's Gateway: the resident service's single HTTP/WebSocket server,
//! offered to agents on this computer or on the local network.
//!
//! The desktop window always authenticates with the per-launch token in
//! `daemon.json`. Everyone else uses the Gateway API key in `gateway.json`
//! (mode 0600, written only by the daemon).
use serde::{Deserialize, Serialize};
use std::{
    fs,
    net::{Ipv4Addr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
};

pub const DEFAULT_PORT: u16 = 7420;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// 127.0.0.1 only: agents on this computer.
    Local,
    /// All interfaces: agents on the local network.
    Lan,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub enabled: bool,
    pub scope: Scope,
    pub port: u16,
    pub api_key: String,
}

pub fn new_key() -> String {
    format!(
        "gmk_{}{}",
        uuid::Uuid::new_v4().simple(),
        &uuid::Uuid::new_v4().simple().to_string()[..16]
    )
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            scope: Scope::Local,
            port: DEFAULT_PORT,
            api_key: new_key(),
        }
    }
}

/// `<app data>/gateway.json`, beside the `serial/` state directory.
pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.parent().unwrap_or(state_dir).join("gateway.json")
}

/// Reads the configuration without creating it (for the desktop window).
pub fn read(state_dir: &Path) -> Option<Config> {
    serde_json::from_slice(&fs::read(path(state_dir)).ok()?).ok()
}

/// Daemon start-up: the stored configuration, or a new default with a key.
pub fn load_or_create(
    state_dir: &Path,
    write: impl Fn(&Path, &[u8]) -> std::io::Result<()>,
) -> Config {
    if let Some(config) = read(state_dir).filter(|c| c.api_key.len() >= 20) {
        return config;
    }
    let config = Config::default();
    let _ = write(
        &path(state_dir),
        &serde_json::to_vec_pretty(&config).unwrap_or_default(),
    );
    config
}

pub fn address(config: &Config) -> SocketAddr {
    match (config.enabled, config.scope) {
        (false, _) => (Ipv4Addr::LOCALHOST, 0).into(),
        (true, Scope::Local) => (Ipv4Addr::LOCALHOST, config.port).into(),
        (true, Scope::Lan) => (Ipv4Addr::UNSPECIFIED, config.port).into(),
    }
}

pub fn bind(config: &Config) -> std::io::Result<TcpListener> {
    TcpListener::bind(address(config))
}

/// Addresses other machines on the local network can use: IPv4 on
/// broadcast-capable interfaces, skipping loopback, link-local, VPN tunnels
/// (point-to-point) and virtual-machine bridges.
pub fn lan_ips() -> Vec<Ipv4Addr> {
    let mut ips = interfaces();
    ips.sort_by_key(|ip| !ip.is_private());
    ips.dedup();
    ips
}

#[cfg(unix)]
fn interfaces() -> Vec<Ipv4Addr> {
    let mut found = Vec::new();
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs allocates a list that freeifaddrs releases below;
    // every pointer is checked before it is read.
    unsafe {
        if libc::getifaddrs(&mut list) != 0 {
            return found;
        }
        let mut cursor = list;
        while !cursor.is_null() {
            let entry = &*cursor;
            cursor = entry.ifa_next;
            let flags = entry.ifa_flags;
            let wanted = flags & libc::IFF_UP as u32 != 0
                && flags & libc::IFF_BROADCAST as u32 != 0
                && flags & (libc::IFF_LOOPBACK | libc::IFF_POINTOPOINT) as u32 == 0;
            if !wanted || entry.ifa_addr.is_null() || entry.ifa_name.is_null() {
                continue;
            }
            let name = std::ffi::CStr::from_ptr(entry.ifa_name).to_string_lossy();
            if ["bridge", "docker", "veth", "vmnet", "vboxnet", "virbr"]
                .iter()
                .any(|p| name.starts_with(p))
            {
                continue;
            }
            if (*entry.ifa_addr).sa_family as i32 != libc::AF_INET {
                continue;
            }
            let addr = &*(entry.ifa_addr as *const libc::sockaddr_in);
            let ip = Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr));
            if !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified() {
                found.push(ip);
            }
        }
        libc::freeifaddrs(list);
    }
    found
}

#[cfg(not(unix))]
use std::net::UdpSocket;

/// Windows: the address of the default route. Connecting a UDP socket only
/// selects a route; no packet is sent.
#[cfg(not(unix))]
fn interfaces() -> Vec<Ipv4Addr> {
    (|| {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
        socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
        match socket.local_addr().ok()?.ip() {
            std::net::IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip),
            _ => None,
        }
    })()
    .into_iter()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_local_with_a_key_and_disabled_binds_loopback() {
        let config = Config::default();
        assert!(config.enabled && config.scope == Scope::Local);
        assert!(config.api_key.starts_with("gmk_") && config.api_key.len() == 52);
        assert_eq!(address(&config).to_string(), "127.0.0.1:7420");
        let lan = Config {
            scope: Scope::Lan,
            ..config.clone()
        };
        assert_eq!(address(&lan).to_string(), "0.0.0.0:7420");
        let off = Config {
            enabled: false,
            ..config
        };
        assert_eq!(address(&off).port(), 0);
        assert!(address(&off).ip().is_loopback());
    }
}
