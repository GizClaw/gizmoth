//! GizMoth's Gateway: the resident service's single HTTP/WebSocket server,
//! offered to agents on this computer or on the local network.
//!
//! The desktop window always authenticates with the per-launch token in
//! `daemon.json`. Everyone else uses the Gateway API key in `gateway.json`
//! (mode 0600, written only by the daemon).
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, BufReader, Cursor},
    net::{Ipv4Addr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio_rustls::{rustls, server::TlsStream, TlsAcceptor};
use zeroize::Zeroizing;

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
    /// PEM certificate chain and private key paths, never the key contents.
    #[serde(default)]
    pub tls_certificate_chain: Option<PathBuf>,
    #[serde(default)]
    pub tls_private_key: Option<PathBuf>,
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
            tls_certificate_chain: None,
            tls_private_key: None,
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

/// The legacy daemon serves plaintext on this listener. Never bind it publicly,
/// even when TLS material exists: only `bind_lan_tls` can expose LAN traffic.
pub fn bind(config: &Config) -> io::Result<TcpListener> {
    if config.enabled && config.scope == Scope::Lan {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "LAN requires a TLS listener",
        ));
    }
    TcpListener::bind(address(config))
}

fn tls_config(config: &Config) -> io::Result<Arc<rustls::ServerConfig>> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid or unavailable LAN TLS certificate or private key",
        )
    };
    let certificate = config.tls_certificate_chain.as_ref().ok_or_else(invalid)?;
    let private_key = config.tls_private_key.as_ref().ok_or_else(invalid)?;
    if !certificate.is_absolute() || !private_key.is_absolute() {
        return Err(invalid());
    }
    let certificate = fs::read(certificate).map_err(|_| invalid())?;
    let key_bytes = Zeroizing::new(fs::read(private_key).map_err(|_| invalid())?);
    let certificates = rustls_pemfile::certs(&mut BufReader::new(Cursor::new(&certificate)))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid())?;
    let key = rustls_pemfile::private_key(&mut Cursor::new(&*key_bytes))
        .map_err(|_| invalid())?
        .ok_or_else(invalid)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| invalid())?
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .map_err(|_| invalid())?;
    Ok(Arc::new(tls))
}

/// A LAN socket paired with its mandatory TLS configuration. Binding a specific
/// interface leaves 127.0.0.1 available for the daemon's existing HTTP/MCP.
/// Rebuild listeners on certificate changes; never pass their sockets to HTTP.
pub struct LanListener {
    listener: TcpListener,
    tls: TlsAcceptor,
}

pub fn bind_lan_tls(config: &Config, ip: Ipv4Addr) -> io::Result<LanListener> {
    if !config.enabled || config.scope != Scope::Lan || ip.is_loopback() || ip.is_unspecified() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "LAN TLS requires an enabled LAN configuration and a specific non-loopback address",
        ));
    }
    // Validate the complete key pair before exposing a socket.
    let tls = TlsAcceptor::from(tls_config(config)?);
    let listener = TcpListener::bind((ip, config.port))?;
    Ok(LanListener { listener, tls })
}

impl LanListener {
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    pub fn into_async(self) -> io::Result<LanAcceptor> {
        self.listener.set_nonblocking(true)?;
        Ok(LanAcceptor {
            listener: tokio::net::TcpListener::from_std(self.listener)?,
            tls: self.tls,
        })
    }
}

pub struct LanAcceptor {
    listener: tokio::net::TcpListener,
    tls: TlsAcceptor,
}

impl LanAcceptor {
    /// Accept only established TLS streams. The timeout bounds stalled handshakes;
    /// callers may share this acceptor across tasks for concurrent handshakes.
    pub async fn accept(&self) -> io::Result<(TlsStream<tokio::net::TcpStream>, SocketAddr)> {
        let (stream, peer) = self.listener.accept().await?;
        let stream = tokio::time::timeout(Duration::from_secs(5), self.tls.accept(stream))
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "LAN TLS handshake timed out")
            })??;
        Ok((stream, peer))
    }
}

/// A complete listener transaction, kept until replacement succeeds. Cloned
/// sockets allow TLS renewal and local/LAN switches without rebinding the port.
pub struct Listeners {
    local: TcpListener,
    lan: Vec<LanListener>,
}

impl Listeners {
    pub fn prepare(config: &Config, current: Option<&Self>) -> io::Result<Self> {
        let local_config = Config {
            scope: Scope::Local,
            ..config.clone()
        };
        let wanted = address(&local_config);
        let local = match current {
            Some(old) if !config.enabled || old.local.local_addr()? == wanted => {
                old.local.try_clone()?
            }
            _ => bind(&local_config)?,
        };
        let mut lan = Vec::new();
        if config.enabled && config.scope == Scope::Lan {
            let tls = TlsAcceptor::from(tls_config(config)?);
            let ips = lan_ips();
            if ips.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrNotAvailable,
                    "no LAN interface available",
                ));
            }
            for ip in ips {
                let address = SocketAddr::from((ip, local.local_addr()?.port()));
                let old = current.and_then(|old| {
                    old.lan
                        .iter()
                        .find(|l| l.local_addr().ok() == Some(address))
                });
                let listener = match old {
                    Some(old) => old.listener.try_clone()?,
                    None => TcpListener::bind(address)?,
                };
                lan.push(LanListener {
                    listener,
                    tls: tls.clone(),
                });
            }
        }
        Ok(Self { local, lan })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.local.local_addr()
    }

    pub fn urls(&self) -> Vec<String> {
        let mut urls = self
            .local
            .local_addr()
            .ok()
            .map(|a| format!("http://{a}"))
            .into_iter()
            .collect::<Vec<_>>();
        urls.extend(
            self.lan
                .iter()
                .filter_map(|l| l.local_addr().ok())
                .map(|a| format!("https://{a}")),
        );
        urls
    }

    pub fn adopt(&self) -> io::Result<AsyncListeners> {
        let local = self.local.try_clone()?;
        local.set_nonblocking(true)?;
        let lan = self
            .lan
            .iter()
            .map(|l| {
                LanListener {
                    listener: l.listener.try_clone()?,
                    tls: l.tls.clone(),
                }
                .into_async()
            })
            .collect::<io::Result<Vec<_>>>()?;
        Ok(AsyncListeners {
            local: tokio::net::TcpListener::from_std(local)?,
            lan,
        })
    }
}

pub struct AsyncListeners {
    local: tokio::net::TcpListener,
    lan: Vec<LanAcceptor>,
}

pub struct Accepted {
    pub peer: SocketAddr,
    pub tls: bool,
    stream: tokio::net::TcpStream,
    acceptor: Option<TlsAcceptor>,
}

pub trait Transport: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Transport for T {}

impl Accepted {
    /// Call after acquiring a remote connection permit, before any HTTP parsing.
    pub async fn handshake(self) -> io::Result<Box<dyn Transport>> {
        match self.acceptor {
            Some(tls) => {
                let stream = tokio::time::timeout(Duration::from_secs(5), tls.accept(self.stream))
                    .await
                    .map_err(|_| {
                        io::Error::new(io::ErrorKind::TimedOut, "LAN TLS handshake timed out")
                    })??;
                Ok(Box::new(stream))
            }
            None => Ok(Box::new(self.stream)),
        }
    }
}

impl AsyncListeners {
    /// Socket acceptance is cancel-safe; handshakes run separately so a slow
    /// remote peer cannot block loopback or other TLS connections.
    pub async fn accept(&self) -> io::Result<Accepted> {
        let lan = async {
            let pending = self
                .lan
                .iter()
                .map(|l| Box::pin(l.listener.accept()))
                .collect::<Vec<_>>();
            let (accepted, index, _) = futures_util::future::select_all(pending).await;
            let (stream, peer) = accepted?;
            Ok(Accepted {
                peer,
                tls: true,
                stream,
                acceptor: Some(self.lan[index].tls.clone()),
            })
        };
        tokio::select! {
            local = self.local.accept() => {
                let (stream, peer) = local?;
                Ok(Accepted { peer, tls: false, stream, acceptor: None })
            }
            accepted = lan, if !self.lan.is_empty() => accepted,
        }
    }
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
    fn plaintext_lan_and_legacy_config_fail_closed() {
        let config = Config {
            scope: Scope::Lan,
            ..Config::default()
        };
        assert_eq!(
            bind(&config).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(bind_lan_tls(&config, Ipv4Addr::new(192, 0, 2, 1)).is_err());
        assert!(bind_lan_tls(&config, Ipv4Addr::LOCALHOST).is_err());
        let json = serde_json::json!({"enabled": true, "scope": "lan", "port": 7420, "apiKey": "example-key"});
        let old: Config = serde_json::from_value(json).unwrap();
        assert!(old.tls_certificate_chain.is_none() && old.tls_private_key.is_none());
    }

    #[tokio::test]
    async fn tls_requires_trusted_certificate_and_matching_name() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio_rustls::TlsConnector;

        let generated = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let other = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let cert_path = temp.path().join("chain.pem");
        let key_path = temp.path().join("key.pem");
        fs::write(&cert_path, generated.cert.pem()).unwrap();
        fs::write(&key_path, generated.key_pair.serialize_pem()).unwrap();
        let config = Config {
            scope: Scope::Lan,
            tls_certificate_chain: Some(cert_path),
            tls_private_key: Some(key_path.clone()),
            ..Config::default()
        };
        let server_config = tls_config(&config).unwrap();
        fs::write(&key_path, other.key_pair.serialize_pem()).unwrap();
        assert!(
            tls_config(&config).is_err(),
            "mismatched key must fail before binding"
        );

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = LanListener {
            listener,
            tls: TlsAcceptor::from(server_config),
        }
        .into_async()
        .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(generated.cert.der().clone()).unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let client = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(client));

        for (name, succeeds) in [("localhost", true), ("wrong.example", false)] {
            let client = async {
                let tcp = tokio::net::TcpStream::connect(address).await.unwrap();
                let name = rustls::pki_types::ServerName::try_from(name.to_owned()).unwrap();
                let result = connector.connect(name, tcp).await;
                assert_eq!(result.is_ok(), succeeds);
                if let Ok(mut stream) = result {
                    stream.write_all(b"hello").await.unwrap();
                }
            };
            let server = async {
                let result = server.accept().await;
                if succeeds {
                    let (mut stream, _) = result.unwrap();
                    let mut bytes = [0; 5];
                    stream.read_exact(&mut bytes).await.unwrap();
                    assert_eq!(&bytes, b"hello");
                } else {
                    assert!(result.is_err());
                }
            };
            tokio::join!(client, server);
        }
        let client = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
        let untrusted = async {
            let tcp = tokio::net::TcpStream::connect(address).await.unwrap();
            assert!(TlsConnector::from(Arc::new(client))
                .connect(
                    rustls::pki_types::ServerName::try_from("localhost").unwrap(),
                    tcp
                )
                .await
                .is_err());
        };
        let reject = async {
            assert!(server.accept().await.is_err());
        };
        tokio::join!(untrusted, reject);
        // A raw HTTP request never reaches the HTTP handler.
        let raw = async {
            let mut tcp = tokio::net::TcpStream::connect(address).await.unwrap();
            tcp.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
        };
        let reject = async {
            assert!(server.accept().await.is_err());
        };
        tokio::join!(raw, reject);
    }

    #[tokio::test]
    async fn listener_transaction_preserves_loopback_and_renews_tls_on_the_same_port() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let Some(ip) = lan_ips().first().copied() else {
            return;
        };
        let generated = rcgen::generate_simple_self_signed(vec![ip.to_string()]).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let certificate = temp.path().join("chain.pem");
        let key = temp.path().join("key.pem");
        fs::write(&certificate, generated.cert.pem()).unwrap();
        fs::write(&key, generated.key_pair.serialize_pem()).unwrap();
        let mut config = Config {
            port: 0,
            ..Config::default()
        };
        let local = Listeners::prepare(&config, None).unwrap();
        config.port = local.local_addr().unwrap().port();
        config.scope = Scope::Lan;
        config.tls_certificate_chain = Some(certificate.clone());
        config.tls_private_key = Some(key.clone());
        let lan = Listeners::prepare(&config, Some(&local)).unwrap();
        assert_eq!(lan.local_addr().unwrap(), local.local_addr().unwrap());
        assert!(lan
            .urls()
            .iter()
            .skip(1)
            .all(|url| url.starts_with("https://")));
        let active = lan.adopt().unwrap();
        // Both protocols operate on the same port with distinct interface binds.
        for (address, tls) in [
            (local.local_addr().unwrap(), false),
            ((ip, config.port).into(), true),
        ] {
            let client = async {
                let tcp = tokio::net::TcpStream::connect(address).await.unwrap();
                let mut stream: Box<dyn Transport> = if tls {
                    let mut roots = rustls::RootCertStore::empty();
                    roots.add(generated.cert.der().clone()).unwrap();
                    let client = rustls::ClientConfig::builder_with_provider(Arc::new(
                        rustls::crypto::ring::default_provider(),
                    ))
                    .with_safe_default_protocol_versions()
                    .unwrap()
                    .with_root_certificates(roots)
                    .with_no_client_auth();
                    let name = rustls::pki_types::ServerName::try_from(ip.to_string()).unwrap();
                    Box::new(
                        tokio_rustls::TlsConnector::from(Arc::new(client))
                            .connect(name, tcp)
                            .await
                            .unwrap(),
                    )
                } else {
                    Box::new(tcp)
                };
                stream.write_all(b"hello").await.unwrap();
            };
            let server = async {
                let accepted = active.accept().await.unwrap();
                assert_eq!(accepted.tls, tls);
                let mut stream = accepted.handshake().await.unwrap();
                let mut bytes = [0; 5];
                stream.read_exact(&mut bytes).await.unwrap();
                assert_eq!(&bytes, b"hello");
            };
            tokio::join!(client, server);
        }
        let renewed = rcgen::generate_simple_self_signed(vec![ip.to_string()]).unwrap();
        fs::write(&certificate, renewed.cert.pem()).unwrap();
        fs::write(&key, renewed.key_pair.serialize_pem()).unwrap();
        let next = Listeners::prepare(&config, Some(&lan)).unwrap();
        assert_eq!(
            next.urls(),
            lan.urls(),
            "renewal must not rebind an occupied port"
        );
        fs::write(&key, "PRIVATE MATERIAL MUST NOT APPEAR IN ERRORS").unwrap();
        let error = Listeners::prepare(&config, Some(&next))
            .err()
            .unwrap()
            .to_string();
        assert!(!error.contains("PRIVATE MATERIAL"));
        assert!(!error.contains(&key.to_string_lossy().to_string()));
        config.scope = Scope::Local;
        let narrowed = Listeners::prepare(&config, Some(&next)).unwrap();
        assert_eq!(narrowed.urls(), local.urls());
        config.enabled = false;
        let disabled = Listeners::prepare(&config, Some(&narrowed)).unwrap();
        assert!(disabled.lan.is_empty());
        assert!(disabled.local_addr().unwrap().ip().is_loopback());
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
