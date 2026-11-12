//! GizMoth's Gateway: the resident service's single HTTP/WebSocket server,
//! offered to agents on this computer or on the local network.
//!
//! The desktop window authenticates on loopback with the per-launch token in
//! `daemon.json`. Everyone else uses the Gateway API key in `gateway.json`
//! (mode 0600, written only by the daemon). HTTP/1.1, HTTP/2 prior knowledge
//! (h2c), and WebSockets share the same plaintext port on every listener.
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    net::{Ipv4Addr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
};

pub const DEFAULT_PORT: u16 = 7420;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// 127.0.0.1 only: agents on this computer.
    Local,
    /// Eligible LAN and Tailscale interfaces, alongside loopback.
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

/// Intended scope, for diagnostics. Actual LAN binds always name an interface
/// so a socket's identity can fence the desktop token independently of peer IP.
pub fn address(config: &Config) -> SocketAddr {
    match (config.enabled, config.scope) {
        (false, _) => (Ipv4Addr::LOCALHOST, 0).into(),
        (true, Scope::Local) => (Ipv4Addr::LOCALHOST, config.port).into(),
        (true, Scope::Lan) => (Ipv4Addr::UNSPECIFIED, config.port).into(),
    }
}

fn bind_local(config: &Config) -> io::Result<TcpListener> {
    TcpListener::bind((
        Ipv4Addr::LOCALHOST,
        if config.enabled { config.port } else { 0 },
    ))
}

fn lan_address(ip: Ipv4Addr, port: u16) -> io::Result<SocketAddr> {
    if ip.is_loopback() || ip.is_unspecified() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "LAN listeners require a specific non-loopback address",
        ));
    }
    Ok((ip, port).into())
}

/// A complete listener transaction, kept until replacement succeeds. Cloned
/// sockets allow local/LAN switches without rebinding an occupied local port.
pub struct Listeners {
    local: TcpListener,
    lan: Vec<TcpListener>,
    // Retain the intended scope while every LAN interface is disconnected.
    lan_enabled: bool,
    network_error: Option<String>,
    retry_port: Option<u16>,
}

impl Listeners {
    /// Startup may keep private control alive while a configured port is busy.
    pub fn start(config: &Config) -> io::Result<Self> {
        let observed = if config.enabled && config.scope == Scope::Lan {
            try_lan_ips()
        } else {
            Ok(Vec::new())
        };
        let mut listeners = Self::start_for_ips(config, observed.as_deref().unwrap_or_default())?;
        if let Err(error) = observed {
            listeners.network_error = Some(format!(
                "LAN interface query failed: {error}; loopback service remains available"
            ));
        }
        Ok(listeners)
    }

    pub(super) fn start_for_ips(config: &Config, ips: &[Ipv4Addr]) -> io::Result<Self> {
        match Self::prepare_for_ips(config, None, ips) {
            Ok(listeners) => Ok(listeners),
            Err(error) => {
                let (local, retry_port) = match bind_local(config) {
                    Ok(local) => (local, None),
                    Err(_) => (
                        bind_local(&Config {
                            enabled: false,
                            ..config.clone()
                        })?,
                        config.enabled.then_some(config.port),
                    ),
                };
                Ok(Self {
                    local,
                    lan: Vec::new(),
                    lan_enabled: config.enabled && config.scope == Scope::Lan,
                    retry_port,
                    network_error: Some(format!(
                        "Service listener unavailable: {error}; loopback service remains available"
                    )),
                })
            }
        }
    }

    pub fn needs_port_retry(&self) -> bool {
        self.retry_port.is_some()
    }

    pub fn prepare(config: &Config, current: Option<&Self>) -> io::Result<Self> {
        let observed = if config.enabled && config.scope == Scope::Lan {
            try_lan_ips()
        } else {
            Ok(Vec::new())
        };
        let ips = observed
            .as_ref()
            .cloned()
            .unwrap_or_else(|_| current.map(|old| old.lan_addresses()).unwrap_or_default());
        let mut listeners = Self::prepare_for_ips(config, current, &ips)?;
        if let Err(error) = observed {
            listeners.network_error = Some(format!(
                "LAN interface query failed: {error}; loopback service remains available"
            ));
        }
        Ok(listeners)
    }

    pub(super) fn prepare_for_ips(
        config: &Config,
        current: Option<&Self>,
        ips: &[Ipv4Addr],
    ) -> io::Result<Self> {
        let wanted = SocketAddr::from((Ipv4Addr::LOCALHOST, config.port));
        let local = match current {
            Some(old) if !config.enabled || old.local.local_addr()? == wanted => {
                old.local.try_clone()?
            }
            _ => bind_local(config)?,
        };
        let mut lan = Vec::new();
        let lan_enabled = config.enabled && config.scope == Scope::Lan;
        if lan_enabled {
            for &ip in ips {
                let address = lan_address(ip, local.local_addr()?.port())?;
                let old = current.and_then(|old| {
                    old.lan
                        .iter()
                        .find(|l| l.local_addr().ok() == Some(address))
                });
                lan.push(match old {
                    Some(old) => old.try_clone()?,
                    None => TcpListener::bind(address)?,
                });
            }
        }
        let network_error = (lan_enabled && lan.is_empty())
            .then(|| "LAN is offline; loopback service remains available".into());
        Ok(Self {
            local,
            lan,
            lan_enabled,
            network_error,
            retry_port: None,
        })
    }

    pub fn lan_enabled(&self) -> bool {
        self.lan_enabled
    }

    pub fn network_error(&self) -> Option<String> {
        self.network_error.clone()
    }

    pub fn lan_addresses(&self) -> Vec<Ipv4Addr> {
        let mut ips = self
            .lan
            .iter()
            .filter_map(|listener| match listener.local_addr().ok()?.ip() {
                std::net::IpAddr::V4(ip) => Some(ip),
                _ => None,
            })
            .collect::<Vec<_>>();
        ips.sort_unstable();
        ips
    }

    /// Individual bind failures retain other interfaces and are retried by the
    /// daemon because the bound set still differs from the observed addresses.
    pub fn refresh_lan(&self, ips: &[Ipv4Addr]) -> io::Result<Self> {
        if !self.lan_enabled && !ips.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "LAN is not enabled",
            ));
        }
        let local = match self.retry_port {
            Some(port) => TcpListener::bind((Ipv4Addr::LOCALHOST, port))?,
            None => self.local.try_clone()?,
        };
        let port = local.local_addr()?.port();
        let mut lan = Vec::new();
        let mut errors = Vec::new();
        for &ip in ips {
            let result = lan_address(ip, port).and_then(|address| {
                match self
                    .lan
                    .iter()
                    .find(|l| l.local_addr().ok() == Some(address))
                {
                    Some(old) => old.try_clone(),
                    None => TcpListener::bind(address),
                }
            });
            match result {
                Ok(listener) => lan.push(listener),
                Err(error) => errors.push(format!("LAN address {ip}:{port} unavailable: {error}")),
            }
        }
        if ips.is_empty() && self.lan_enabled {
            errors.push("LAN is offline; loopback service remains available".into());
        }
        Ok(Self {
            local,
            lan,
            lan_enabled: self.lan_enabled,
            network_error: (!errors.is_empty()).then(|| errors.join("; ")),
            retry_port: None,
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.local.local_addr()
    }

    pub fn urls(&self) -> Vec<String> {
        std::iter::once(&self.local)
            .chain(&self.lan)
            .filter_map(|l| l.local_addr().ok())
            .map(|a| format!("http://{a}"))
            .collect()
    }

    pub fn adopt(&self) -> io::Result<AsyncListeners> {
        fn adopt(listener: &TcpListener) -> io::Result<tokio::net::TcpListener> {
            let listener = listener.try_clone()?;
            listener.set_nonblocking(true)?;
            tokio::net::TcpListener::from_std(listener)
        }
        Ok(AsyncListeners {
            local: adopt(&self.local)?,
            lan: self.lan.iter().map(adopt).collect::<io::Result<Vec<_>>>()?,
        })
    }
}

pub struct AsyncListeners {
    local: tokio::net::TcpListener,
    lan: Vec<tokio::net::TcpListener>,
}

pub struct Accepted {
    pub peer: SocketAddr,
    /// The accepting listener's identity, independent of the caller's IP.
    pub lan: bool,
    pub stream: tokio::net::TcpStream,
}

impl AsyncListeners {
    /// Socket acceptance is cancel-safe. Each accepted stream keeps its entry
    /// point identity, including when this machine calls its own LAN address.
    pub async fn accept(&self) -> io::Result<Accepted> {
        let lan = async {
            let pending = self
                .lan
                .iter()
                .map(|l| Box::pin(l.accept()))
                .collect::<Vec<_>>();
            let (accepted, _, _) = futures_util::future::select_all(pending).await;
            let (stream, peer) = accepted?;
            Ok(Accepted {
                peer,
                lan: true,
                stream,
            })
        };
        tokio::select! {
            local = self.local.accept() => {
                let (stream, peer) = local?;
                Ok(Accepted { peer, lan: false, stream })
            }
            accepted = lan, if !self.lan.is_empty() => accepted,
        }
    }
}

/// Addresses other machines on the local network can use: IPv4 on
/// broadcast-capable interfaces and Tailscale, skipping loopback, link-local,
/// other VPN tunnels (point-to-point) and virtual-machine bridges.
pub fn lan_ips() -> Vec<Ipv4Addr> {
    try_lan_ips().unwrap_or_default()
}

/// An enumeration failure is distinct from a successful offline snapshot.
pub fn try_lan_ips() -> io::Result<Vec<Ipv4Addr>> {
    let mut ips = interfaces()?;
    ips.sort_by_key(|ip| (!ip.is_private(), *ip));
    ips.dedup();
    Ok(ips)
}

#[cfg(unix)]
fn interfaces() -> io::Result<Vec<Ipv4Addr>> {
    let mut found = Vec::new();
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs allocates a list that freeifaddrs releases below;
    // every pointer is checked before it is read.
    unsafe {
        if libc::getifaddrs(&mut list) != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut cursor = list;
        while !cursor.is_null() {
            let entry = &*cursor;
            cursor = entry.ifa_next;
            let flags = entry.ifa_flags;
            if flags & libc::IFF_UP as u32 == 0
                || flags & libc::IFF_LOOPBACK as u32 != 0
                || entry.ifa_addr.is_null()
                || entry.ifa_name.is_null()
            {
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
            // Tailscale tunnels are point-to-point without broadcast; other
            // VPN tunnels remain excluded.
            let lan = flags & libc::IFF_BROADCAST as u32 != 0
                && flags & libc::IFF_POINTOPOINT as u32 == 0;
            if (lan || tailscale(ip))
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_unspecified()
            {
                found.push(ip);
            }
        }
        libc::freeifaddrs(list);
    }
    Ok(found)
}

/// Tailscale assigns node addresses from the 100.64.0.0/10 CGNAT range.
pub fn tailscale(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == 100 && (64..128).contains(&b)
}

#[cfg(not(unix))]
use std::net::UdpSocket;

/// The local address of the route to `destination`. Connecting a UDP socket
/// only selects a route; no packet is sent.
#[cfg(not(unix))]
fn route_source(destination: Ipv4Addr) -> io::Result<Option<Ipv4Addr>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    if let Err(error) = socket.connect((destination, 9)) {
        return if matches!(
            error.kind(),
            io::ErrorKind::NetworkUnreachable | io::ErrorKind::HostUnreachable
        ) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    Ok(match socket.local_addr()?.ip() {
        std::net::IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip),
        _ => None,
    })
}

/// Windows: the default route's address, plus the Tailscale address when
/// Tailscale routes its service address (100.100.100.100).
#[cfg(not(unix))]
fn interfaces() -> io::Result<Vec<Ipv4Addr>> {
    let mut found: Vec<_> = route_source(Ipv4Addr::new(192, 0, 2, 1))?
        .into_iter()
        .collect();
    if let Ok(Some(ip)) = route_source(Ipv4Addr::new(100, 100, 100, 100)) {
        if tailscale(ip) {
            found.push(ip);
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_lan_config_ignores_certificate_paths_and_needs_no_files() {
        let json = serde_json::json!({
            "enabled": true, "scope": "lan", "port": 0, "apiKey": "example-key",
            "tlsCertificateChain": "/missing/chain.pem", "tlsPrivateKey": "/missing/key.pem"
        });
        let config: Config = serde_json::from_value(json).unwrap();
        let listeners = Listeners::prepare_for_ips(&config, None, &[]).unwrap();
        assert!(listeners.lan_enabled());
        assert_eq!(listeners.urls().len(), 1);
        let saved = serde_json::to_value(&config).unwrap();
        assert!(saved.get("tlsCertificateChain").is_none());
        assert!(saved.get("tlsPrivateKey").is_none());
        assert!(lan_address(Ipv4Addr::LOCALHOST, 7420).is_err());
        assert!(lan_address(Ipv4Addr::UNSPECIFIED, 7420).is_err());
    }

    #[tokio::test]
    async fn listener_transaction_preserves_loopback_and_lan_identity_on_the_same_port() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let Some(ip) = lan_ips().first().copied() else {
            return;
        };
        let mut config = Config {
            port: 0,
            ..Config::default()
        };
        let local = Listeners::prepare_for_ips(&config, None, &[]).unwrap();
        config.port = local.local_addr().unwrap().port();
        config.scope = Scope::Lan;
        let lan = Listeners::prepare_for_ips(&config, Some(&local), &[ip]).unwrap();
        assert_eq!(lan.local_addr().unwrap(), local.local_addr().unwrap());
        assert_eq!(
            lan.urls(),
            vec![
                format!("http://127.0.0.1:{}", config.port),
                format!("http://{ip}:{}", config.port)
            ]
        );
        let active = lan.adopt().unwrap();
        for (address, is_lan) in [
            (local.local_addr().unwrap(), false),
            ((ip, config.port).into(), true),
        ] {
            let client = async {
                let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
                stream.write_all(b"hello").await.unwrap();
            };
            let server = async {
                let accepted = active.accept().await.unwrap();
                assert_eq!(accepted.lan, is_lan);
                let mut bytes = [0; 5];
                accepted
                    .stream
                    .take(5)
                    .read_exact(&mut bytes)
                    .await
                    .unwrap();
                assert_eq!(&bytes, b"hello");
            };
            tokio::join!(client, server);
        }
        let next = Listeners::prepare_for_ips(&config, Some(&lan), &[ip]).unwrap();
        assert_eq!(
            next.urls(),
            lan.urls(),
            "unchanged listeners reuse their sockets"
        );
        // An invalid replacement must not disturb either active listener.
        assert!(
            Listeners::prepare_for_ips(&config, Some(&next), &[ip, Ipv4Addr::UNSPECIFIED]).is_err()
        );
        assert_eq!(next.urls(), lan.urls());
        config.scope = Scope::Local;
        let narrowed = Listeners::prepare_for_ips(&config, Some(&next), &[ip]).unwrap();
        assert_eq!(narrowed.urls(), local.urls());
        assert!(!narrowed.lan_enabled());
        config.enabled = false;
        let disabled = Listeners::prepare_for_ips(&config, Some(&narrowed), &[ip]).unwrap();
        assert!(disabled.lan.is_empty());
        assert!(disabled.local_addr().unwrap().ip().is_loopback());
    }

    #[test]
    fn offline_lan_retains_scope_and_recovers_without_certificate_files() {
        let config = Config {
            scope: Scope::Lan,
            port: 0,
            ..Config::default()
        };
        let offline = Listeners::prepare_for_ips(&config, None, &[]).unwrap();
        assert!(offline.lan_enabled());
        assert!(offline.network_error().is_some());
        assert_eq!(offline.urls().len(), 1);
        let again = offline.refresh_lan(&[]).unwrap();
        assert!(again.lan_enabled());
        assert_eq!(again.local_addr().unwrap(), offline.local_addr().unwrap());
        let failed = again
            .refresh_lan(&[Ipv4Addr::LOCALHOST, Ipv4Addr::UNSPECIFIED])
            .unwrap();
        assert!(failed.network_error().is_some());
        assert_eq!(failed.urls(), offline.urls());
        let Some(ip) = lan_ips().first().copied() else {
            return;
        };
        let online = again.refresh_lan(&[ip]).unwrap();
        assert_eq!(online.lan_addresses(), vec![ip]);
        assert!(online.network_error().is_none());
        let disconnected = online.refresh_lan(&[]).unwrap();
        drop(online);
        assert!(disconnected.lan_enabled());
        let recovered = disconnected.refresh_lan(&[ip]).unwrap();
        assert_eq!(recovered.lan_addresses(), vec![ip]);
        assert_eq!(
            recovered.local_addr().unwrap(),
            offline.local_addr().unwrap()
        );
    }

    #[test]
    fn tailscale_addresses_are_the_cgnat_range() {
        assert!(tailscale(Ipv4Addr::new(100, 64, 0, 1)));
        assert!(tailscale(Ipv4Addr::new(100, 127, 255, 254)));
        assert!(!tailscale(Ipv4Addr::new(100, 128, 0, 1)));
        assert!(!tailscale(Ipv4Addr::new(192, 168, 1, 2)));
    }

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
