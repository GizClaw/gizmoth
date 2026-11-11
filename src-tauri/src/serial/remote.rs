//! Imported nodes: other GizMoth serial services whose ports this service
//! shows beside its own, over gizmoth.gateway.v1 (LAN or a Tailscale network).
//!
//! - Each node has one connection task: handshake (`GetNode`, which also
//!   verifies the node identity), then a `WatchTopology` stream. Disconnects
//!   keep the last ports, marked stale, and reconnect with backoff.
//! - Device commands open an exclusive `Session` on the owning node on demand.
//!   The owning node arbitrates; an idle session is released after a minute.
//! - Imported ports are addressed locally as `<node id>--<port id>`. Only this
//!   node's own ports are exported, so a cascade has one level.
//!
//! `remotes.json` (mode 0600) stores addresses, API keys and pinned
//! certificate fingerprints; it is written only by the daemon.
use super::{
    daemon::JobKind,
    gateway,
    grpc::{self, pb, NodeIdentity},
    now_ms,
};
use hyper::{StatusCode, Uri};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_rustls::rustls;
use tokio_stream::wrappers::ReceiverStream;

type Reply = Result<(StatusCode, Value), (StatusCode, String)>;
type Failure = (StatusCode, String);

/// Forwarded jobs may upload a firmware package before replying.
pub const JOB_TIMEOUT: Duration = Duration::from_secs(300);
const SEPARATOR: &str = "--";
const MAX_NODES: usize = 16;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
const GRANT_WAIT: Duration = Duration::from_secs(6);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const SESSION_IDLE: Duration = Duration::from_secs(60);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const MAX_PACKAGE: u64 = 256 * 1024 * 1024;
const UPLOAD_CHUNK: usize = 256 * 1024;

/// `<node id>--<port id>` for an imported port; `None` for local ports.
pub fn split(id: &str) -> Option<(&str, &str)> {
    let (node, port) = id.split_once(SEPARATOR)?;
    (uuid::Uuid::parse_str(node).is_ok() && valid_port_id(port)).then_some((node, port))
}

pub fn local_id(node: &str, port: &str) -> String {
    format!("{node}{SEPARATOR}{port}")
}

fn valid_port_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.contains(SEPARATOR)
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Whether an HTTP path starts a job on an imported port.
pub fn is_job_request(path: &str) -> bool {
    let parts: Vec<_> = path.trim_matches('/').split('/').collect();
    matches!(parts.as_slice(), ["v1", "ports", id, "jobs"] if split(id).is_some())
}

fn bad(message: impl Into<String>) -> Failure {
    (StatusCode::BAD_REQUEST, message.into())
}
fn offline() -> Failure {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "远端 GizMoth 当前未连接".into(),
    )
}
fn not_found() -> Failure {
    (StatusCode::NOT_FOUND, "串口不存在".into())
}

// ------------------------------------------------------------- addresses

#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    tls: bool,
    host: String,
    port: u16,
}

impl Target {
    fn url(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!(
            "{}://{host}:{}",
            if self.tls { "https" } else { "http" },
            self.port
        )
    }
}

fn loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Accepts `host`, `host:port` or a URL. Plaintext is allowed on loopback
/// only, matching the serving side; the default port is the Gateway's.
fn parse_target(input: &str) -> Result<Target, String> {
    let input = input.trim();
    if input.is_empty() || input.len() > 512 || input.chars().any(char::is_control) {
        return Err("请输入远端 GizMoth 的地址".into());
    }
    let explicit = input.contains("://");
    let url = url::Url::parse(&if explicit {
        input.to_string()
    } else {
        format!("https://{input}")
    })
    .map_err(|_| "地址格式无效".to_string())?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err("地址只能包含主机和端口".into());
    }
    let host = match url.host() {
        Some(url::Host::Domain(domain)) => domain.to_string(),
        Some(url::Host::Ipv4(ip)) => ip.to_string(),
        Some(url::Host::Ipv6(ip)) => ip.to_string(),
        None => return Err("地址缺少主机".into()),
    };
    let tls = match url.scheme() {
        "https" if explicit || !loopback(&host) => true,
        "https" => false,
        "http" if loopback(&host) => false,
        "http" => return Err("局域网和 Tailscale 连接必须使用 HTTPS".into()),
        _ => return Err("地址只支持 https://".into()),
    };
    Ok(Target {
        tls,
        host,
        port: url.port().unwrap_or(gateway::DEFAULT_PORT),
    })
}

/// Lowercase hex SHA-256, with or without `:` separators.
fn parse_pin(pin: Option<&str>) -> Result<Option<String>, String> {
    let Some(pin) = pin.map(str::trim).filter(|pin| !pin.is_empty()) else {
        return Ok(None);
    };
    let hex: String = pin
        .chars()
        .filter(|c| *c != ':')
        .collect::<String>()
        .to_ascii_lowercase();
    if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(Some(hex))
    } else {
        Err("证书指纹必须是 SHA-256".into())
    }
}

fn valid_key(key: &str) -> Result<(), String> {
    if key.is_empty() || key.len() > 256 || !key.chars().all(|c| c.is_ascii_graphic()) {
        Err("请输入远端 GizMoth 的 API Key".into())
    } else {
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ----------------------------------------------------------------- TLS

#[derive(Default)]
struct Verdict {
    fingerprint: Option<String>,
    failure: Option<String>,
}

/// System-trusted certificates verify normally. A pinned fingerprint instead
/// accepts exactly that end-entity certificate (self-signed LAN certificates).
/// The presented fingerprint is recorded so the user can choose to pin it.
struct Verifier {
    roots: Arc<rustls::client::WebPkiServerVerifier>,
    pin: Option<String>,
    verdict: Arc<Mutex<Verdict>>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl std::fmt::Debug for Verifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Verifier").field("pin", &self.pin).finish()
    }
}

impl rustls::client::danger::ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        server_name: &rustls::pki_types::ServerName<'_>,
        ocsp: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let fingerprint = hex(&Sha256::digest(end_entity.as_ref()));
        let mut verdict = self.verdict.lock().unwrap();
        verdict.fingerprint = Some(fingerprint.clone());
        let result = match &self.pin {
            Some(pin) if *pin == fingerprint => {
                Ok(rustls::client::danger::ServerCertVerified::assertion())
            }
            Some(_) => Err(rustls::Error::General(
                "远端证书与已信任的指纹不一致".into(),
            )),
            None => {
                self.roots
                    .verify_server_cert(end_entity, intermediates, server_name, ocsp, now)
            }
        };
        verdict.failure = result.as_ref().err().map(|error| error.to_string());
        result
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn tls_config(
    pin: Option<String>,
    verdict: Arc<Mutex<Verdict>>,
) -> Result<Arc<rustls::ClientConfig>, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let roots = rustls::client::WebPkiServerVerifier::builder_with_provider(
        Arc::new(roots),
        provider.clone(),
    )
    .build()
    .map_err(|e| e.to_string())?;
    let mut config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Verifier {
            roots,
            pin,
            verdict,
            provider,
        }))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec()];
    Ok(Arc::new(config))
}

#[derive(Clone)]
struct Connector {
    target: Target,
    tls: Option<Arc<rustls::ClientConfig>>,
}

impl tonic::codegen::Service<Uri> for Connector {
    type Response = hyper_util::rt::TokioIo<Box<dyn gateway::Transport>>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, io::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: Uri) -> Self::Future {
        let (target, tls) = (self.target.clone(), self.tls.clone());
        Box::pin(async move {
            let timed_out = || io::Error::new(io::ErrorKind::TimedOut, "连接超时");
            let tcp = tokio::time::timeout(
                CONNECT_TIMEOUT,
                tokio::net::TcpStream::connect((target.host.as_str(), target.port)),
            )
            .await
            .map_err(|_| timed_out())??;
            let _ = tcp.set_nodelay(true);
            let stream: Box<dyn gateway::Transport> = match tls {
                Some(config) => {
                    let name = rustls::pki_types::ServerName::try_from(target.host.clone())
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
                    Box::new(
                        tokio::time::timeout(
                            CONNECT_TIMEOUT,
                            tokio_rustls::TlsConnector::from(config).connect(name, tcp),
                        )
                        .await
                        .map_err(|_| timed_out())??,
                    )
                }
                None => Box::new(tcp),
            };
            Ok(hyper_util::rt::TokioIo::new(stream))
        })
    }
}

#[derive(Clone)]
struct Auth(tonic::metadata::MetadataValue<tonic::metadata::Ascii>);

impl tonic::service::Interceptor for Auth {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> Result<tonic::Request<()>, tonic::Status> {
        request
            .metadata_mut()
            .insert("authorization", self.0.clone());
        Ok(request)
    }
}

type Client = pb::gateway_client::GatewayClient<
    tonic::service::interceptor::InterceptedService<tonic::transport::Channel, Auth>,
>;

fn describe(status: &tonic::Status) -> String {
    match status.code() {
        tonic::Code::Unauthenticated => "API Key 无效，或远端未向局域网开放服务".into(),
        tonic::Code::Unimplemented => "远端 GizMoth 版本过旧，不支持级联".into(),
        _ if status.message().is_empty() => format!("远端返回 {}", status.code()),
        _ => status.message().to_string(),
    }
}

fn connect_error(error: &tonic::transport::Error) -> String {
    use std::error::Error;
    let mut cause: Option<&dyn Error> = Some(error);
    let mut text = error.to_string();
    while let Some(current) = cause {
        text = current.to_string();
        cause = current.source();
    }
    format!("无法连接：{text}")
}

/// Connects and performs the handshake. Errors are user-facing text.
async fn connect(
    target: &Target,
    key: &str,
    pin: Option<String>,
    verdict: Arc<Mutex<Verdict>>,
) -> Result<(Client, pb::Node), String> {
    let tls = if target.tls {
        Some(tls_config(pin, verdict)?)
    } else {
        None
    };
    // The connector owns addressing and TLS; tonic only sees an HTTP/2 stream.
    let endpoint = tonic::transport::Endpoint::from_shared(format!(
        "http://{}",
        target.url().split_once("://").unwrap().1
    ))
    .map_err(|e| e.to_string())?
    .connect_timeout(CONNECT_TIMEOUT)
    .http2_keep_alive_interval(Duration::from_secs(15))
    .keep_alive_timeout(Duration::from_secs(10))
    .keep_alive_while_idle(true);
    let channel = endpoint
        .connect_with_connector(Connector {
            target: target.clone(),
            tls,
        })
        .await
        .map_err(|e| connect_error(&e))?;
    let value = format!("Bearer {key}")
        .parse()
        .map_err(|_| "API Key 格式无效".to_string())?;
    let mut client = pb::gateway_client::GatewayClient::with_interceptor(channel, Auth(value))
        .max_decoding_message_size(16 * 1024 * 1024);
    let node = tokio::time::timeout(CALL_TIMEOUT, client.get_node(pb::GetNodeRequest {}))
        .await
        .map_err(|_| "远端未响应".to_string())?
        .map_err(|status| describe(&status))?
        .into_inner();
    if uuid::Uuid::parse_str(&node.node_id).is_err() {
        return Err("远端返回了无效的节点标识".into());
    }
    Ok((client, node))
}

// --------------------------------------------------------------- state

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteConfig {
    node_id: String,
    name: String,
    url: String,
    api_key: String,
    #[serde(default)]
    certificate_sha256: Option<String>,
    created_at: u64,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    version: u32,
    nodes: Vec<RemoteConfig>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Connecting,
    Online,
    Offline,
    /// Needs the user: authentication, identity or certificate problems.
    Error,
}

struct NodeState {
    status: Status,
    error: Option<String>,
    info: Option<pb::Node>,
    ports: Vec<pb::Port>,
    last_seen: Option<u64>,
    client: Option<Client>,
}

#[derive(Clone, Debug, PartialEq)]
enum Control {
    Opening,
    Queued(u32, String),
    Granted,
    Ended(String),
}

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<pb::session_output::CommandResult>>>>;

/// This node's exclusive session on one imported port.
struct Session {
    input: mpsc::Sender<pb::SessionInput>,
    control: watch::Receiver<Control>,
    pending: Pending,
    jobs: Mutex<HashSet<String>>,
    last_used: Mutex<Instant>,
    reader: tokio::task::AbortHandle,
}

impl Drop for Session {
    fn drop(&mut self) {
        // Ends the request stream; the owning node releases the port.
        self.reader.abort();
    }
}

struct RemoteNode {
    config: RemoteConfig,
    target: Target,
    state: Mutex<NodeState>,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    stop: watch::Sender<bool>,
}

pub struct Remotes {
    dir: PathBuf,
    node: NodeIdentity,
    dirty: Arc<AtomicBool>,
    write: fn(&Path, &[u8]) -> io::Result<()>,
    nodes: Mutex<Vec<Arc<RemoteNode>>>,
    started: AtomicBool,
}

impl Remotes {
    pub fn load(
        dir: &Path,
        node: NodeIdentity,
        dirty: Arc<AtomicBool>,
        write: fn(&Path, &[u8]) -> io::Result<()>,
    ) -> Arc<Self> {
        let saved = fs::read(dir.join("remotes.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).ok())
            .map(|saved| saved.nodes)
            .unwrap_or_default();
        let nodes = saved
            .into_iter()
            .filter_map(|config| {
                let target = parse_target(&config.url).ok()?;
                uuid::Uuid::parse_str(&config.node_id).ok()?;
                Some(Arc::new(RemoteNode::new(config, target)))
            })
            .take(MAX_NODES)
            .collect();
        Arc::new(Self {
            dir: dir.to_path_buf(),
            node,
            dirty,
            write,
            nodes: Mutex::new(nodes),
            started: AtomicBool::new(false),
        })
    }

    pub fn has_nodes(&self) -> bool {
        !self.nodes.lock().unwrap().is_empty()
    }

    fn save(&self, nodes: &[Arc<RemoteNode>]) -> io::Result<()> {
        let saved = Saved {
            version: 1,
            nodes: nodes.iter().map(|node| node.config.clone()).collect(),
        };
        (self.write)(
            &self.dir.join("remotes.json"),
            &serde_json::to_vec_pretty(&saved)?,
        )
    }

    fn touch(&self) {
        self.dirty.store(true, Ordering::SeqCst);
    }

    /// Starts connection tasks and the idle-session reaper. Call inside the
    /// daemon's runtime.
    pub fn start(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        for node in self.nodes.lock().unwrap().iter() {
            tokio::spawn(self.clone().run_node(node.clone()));
        }
        let this = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            loop {
                interval.tick().await;
                let Some(this) = this.upgrade() else { return };
                this.reap();
            }
        });
    }

    fn node(&self, id: &str) -> Option<Arc<RemoteNode>> {
        self.nodes
            .lock()
            .unwrap()
            .iter()
            .find(|node| node.config.node_id == id)
            .cloned()
    }

    fn client(node: &RemoteNode) -> Result<Client, Failure> {
        node.state
            .lock()
            .unwrap()
            .client
            .clone()
            .ok_or_else(offline)
    }

    // ------------------------------------------------------------ views

    fn control_json(node: &RemoteNode, port: &str) -> Value {
        let sessions = node.sessions.lock().unwrap();
        let Some(session) = sessions.get(port) else {
            return json!({ "state": "none" });
        };
        let control = session.control.borrow().clone();
        match control {
            Control::Opening => json!({ "state": "opening" }),
            Control::Queued(position, holder) => {
                json!({ "state": "queued", "position": position, "holder": holder })
            }
            Control::Granted => json!({ "state": "granted" }),
            Control::Ended(reason) => json!({ "state": "none", "reason": reason }),
        }
    }

    fn port_json(node: &RemoteNode, state: &NodeState, port: &pb::Port) -> Value {
        let mut value = grpc::port_json(port, &local_id(&node.config.node_id, &port.port_id));
        value["node"] = json!(node.config.node_id);
        value["remote"] = json!({
            "portId": port.port_id,
            "stale": state.status != Status::Online,
            "control": Self::control_json(node, &port.port_id),
        });
        value
    }

    /// Imported nodes for the daemon snapshot, each with its ports.
    pub fn nodes_json(&self) -> Value {
        let nodes = self.nodes.lock().unwrap().clone();
        json!(nodes
            .iter()
            .map(|node| {
                let state = node.state.lock().unwrap();
                json!({
                    "id": node.config.node_id,
                    "name": state.info.as_ref().map(|info| info.name.clone()).unwrap_or_else(|| node.config.name.clone()),
                    "url": node.config.url,
                    "status": match state.status {
                        Status::Connecting => "connecting",
                        Status::Online => "online",
                        Status::Offline => "offline",
                        Status::Error => "error",
                    },
                    "error": state.error,
                    "bootId": state.info.as_ref().map(|info| info.boot_id.clone()),
                    "version": state.info.as_ref().map(|info| info.version.clone()),
                    "lastSeenAt": state.last_seen,
                    "pinned": node.config.certificate_sha256.is_some(),
                    "ports": state.ports.iter().map(|port| Self::port_json(node, &state, port)).collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>())
    }

    pub fn port_view(&self, id: &str) -> Option<Value> {
        let (node_id, port_id) = split(id)?;
        let node = self.node(node_id)?;
        let state = node.state.lock().unwrap();
        let port = state.ports.iter().find(|port| port.port_id == port_id)?;
        Some(Self::port_json(&node, &state, port))
    }

    // ------------------------------------------------------- management

    /// Connects once without saving. An untrusted certificate is not an
    /// error: its fingerprint is returned so the user can decide to pin it.
    pub async fn probe(&self, url: &str, key: &str, pin: Option<&str>) -> Reply {
        let target = parse_target(url).map_err(bad)?;
        valid_key(key).map_err(bad)?;
        let pin = parse_pin(pin).map_err(bad)?;
        let verdict = Arc::new(Mutex::new(Verdict::default()));
        match connect(&target, key, pin.clone(), verdict.clone()).await {
            Ok((mut client, node)) => {
                let ports = tokio::time::timeout(CALL_TIMEOUT, async {
                    let mut stream = client
                        .watch_topology(pb::WatchTopologyRequest {})
                        .await
                        .ok()?
                        .into_inner();
                    stream.message().await.ok().flatten()
                })
                .await
                .ok()
                .flatten()
                .map(|topology| topology.ports.len());
                let fingerprint = verdict.lock().unwrap().fingerprint.clone();
                Ok((
                    StatusCode::OK,
                    json!({
                        "url": target.url(),
                        "node": { "id": node.node_id, "name": node.name, "version": node.version },
                        "self": node.node_id == self.node.id,
                        "ports": ports,
                        "certificate": fingerprint.map(|sha256| json!({
                            "sha256": sha256,
                            "trusted": true,
                            "pinned": pin.is_some(),
                        })),
                    }),
                ))
            }
            Err(error) => {
                let verdict = verdict.lock().unwrap();
                match (&verdict.fingerprint, &verdict.failure) {
                    (Some(sha256), Some(reason)) => Ok((
                        StatusCode::OK,
                        json!({
                            "url": target.url(),
                            "certificate": { "sha256": sha256, "trusted": false, "reason": reason },
                        }),
                    )),
                    _ => Err((StatusCode::BAD_GATEWAY, error)),
                }
            }
        }
    }

    pub async fn add(self: &Arc<Self>, url: &str, key: &str, pin: Option<&str>) -> Reply {
        let target = parse_target(url).map_err(bad)?;
        valid_key(key).map_err(bad)?;
        let pin = parse_pin(pin).map_err(bad)?;
        let verdict = Arc::new(Mutex::new(Verdict::default()));
        let (_, info) = connect(&target, key, pin.clone(), verdict.clone())
            .await
            .map_err(|error| {
                let untrusted = verdict.lock().unwrap().failure.is_some();
                if untrusted {
                    (
                        StatusCode::CONFLICT,
                        "远端证书未受信任，请先确认证书指纹".to_string(),
                    )
                } else {
                    (StatusCode::BAD_GATEWAY, error)
                }
            })?;
        if info.node_id == self.node.id {
            return Err(bad("这是本机的 GizMoth，无需添加"));
        }
        let node = {
            let mut nodes = self.nodes.lock().unwrap();
            if nodes.iter().any(|node| node.config.node_id == info.node_id) {
                return Err((StatusCode::CONFLICT, "这台 GizMoth 已在列表中".into()));
            }
            if nodes.len() >= MAX_NODES {
                return Err((StatusCode::CONFLICT, "远端 GizMoth 数量已达上限".into()));
            }
            let config = RemoteConfig {
                node_id: info.node_id.clone(),
                name: info.name.clone(),
                url: target.url(),
                api_key: key.to_string(),
                certificate_sha256: pin,
                created_at: now_ms(),
            };
            let node = Arc::new(RemoteNode::new(config, target));
            let mut next = nodes.clone();
            next.push(node.clone());
            self.save(&next).map_err(|e| bad(e.to_string()))?;
            *nodes = next;
            node
        };
        if self.started.load(Ordering::SeqCst) {
            tokio::spawn(self.clone().run_node(node.clone()));
        }
        self.touch();
        Ok((
            StatusCode::CREATED,
            json!({ "id": info.node_id, "name": info.name, "url": node.config.url }),
        ))
    }

    pub fn remove(&self, id: &str) -> Reply {
        let mut nodes = self.nodes.lock().unwrap();
        let index = nodes
            .iter()
            .position(|node| node.config.node_id == id)
            .ok_or((StatusCode::NOT_FOUND, "远端 GizMoth 不存在".to_string()))?;
        let mut next = nodes.clone();
        let node = next.remove(index);
        self.save(&next).map_err(|e| bad(e.to_string()))?;
        *nodes = next;
        drop(nodes);
        node.stop.send_replace(true);
        node.sessions.lock().unwrap().clear();
        self.touch();
        Ok((StatusCode::OK, json!({ "removed": id })))
    }

    // -------------------------------------------------------- connection

    async fn run_node(self: Arc<Self>, node: Arc<RemoteNode>) {
        let mut stop = node.stop.subscribe();
        let mut backoff = Duration::from_secs(1);
        loop {
            if *stop.borrow() {
                return;
            }
            node.update(|state| {
                if state.status != Status::Online {
                    state.status = Status::Connecting;
                }
            });
            self.touch();
            let mut reached = false;
            let outcome = tokio::select! {
                outcome = self.serve_node(&node, &mut reached) => outcome,
                _ = stop.wait_for(|stop| *stop) => return,
            };
            node.sessions.lock().unwrap().clear();
            node.update(|state| {
                state.client = None;
                let (status, error) = outcome;
                state.status = status;
                state.error = Some(error);
            });
            self.touch();
            if reached {
                backoff = Duration::from_secs(1);
            }
            tokio::select! {
                _ = tokio::time::sleep(backoff) => {}
                _ = stop.wait_for(|stop| *stop) => return,
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    /// Returns when the connection ends, with the status to show.
    async fn serve_node(&self, node: &RemoteNode, reached: &mut bool) -> (Status, String) {
        let verdict = Arc::new(Mutex::new(Verdict::default()));
        let (mut client, info) = match connect(
            &node.target,
            &node.config.api_key,
            node.config.certificate_sha256.clone(),
            verdict.clone(),
        )
        .await
        {
            Ok(connected) => connected,
            Err(error) => {
                let fatal =
                    verdict.lock().unwrap().failure.is_some() || error.starts_with("API Key");
                return (
                    if fatal {
                        Status::Error
                    } else {
                        Status::Offline
                    },
                    error,
                );
            }
        };
        if info.node_id != node.config.node_id {
            return (
                Status::Error,
                format!(
                    "该地址现在是另一台 GizMoth（{}），请移除后重新添加",
                    info.name
                ),
            );
        }
        let mut stream = match client.watch_topology(pb::WatchTopologyRequest {}).await {
            Ok(response) => response.into_inner(),
            Err(status) => return (Status::Offline, describe(&status)),
        };
        node.update(|state| {
            state.client = Some(client);
            state.info = Some(info);
            state.status = Status::Online;
            state.error = None;
        });
        self.touch();
        *reached = true;
        loop {
            match stream.message().await {
                Ok(Some(topology)) => {
                    node.update(|state| {
                        state.ports = topology
                            .ports
                            .into_iter()
                            .filter(|port| valid_port_id(&port.port_id))
                            .collect();
                        state.last_seen = Some(now_ms());
                        if let Some(info) = topology.node {
                            state.info = Some(info);
                        }
                    });
                    self.touch();
                }
                Ok(None) => return (Status::Offline, "远端结束了连接".into()),
                Err(status) if status.code() == tonic::Code::Unauthenticated => {
                    return (Status::Error, describe(&status))
                }
                Err(status) => return (Status::Offline, describe(&status)),
            }
        }
    }

    /// Releases sessions nobody used for a while, unless our job still runs.
    fn reap(&self) {
        let nodes = self.nodes.lock().unwrap().clone();
        let mut changed = false;
        for node in nodes {
            let running: HashMap<String, String> = node
                .state
                .lock()
                .unwrap()
                .ports
                .iter()
                .filter_map(|port| Some((port.port_id.clone(), port.job.as_ref()?.job_id.clone())))
                .collect();
            node.sessions.lock().unwrap().retain(|port, session| {
                let ours = running
                    .get(port)
                    .is_some_and(|job| session.jobs.lock().unwrap().contains(job));
                if ours {
                    *session.last_used.lock().unwrap() = Instant::now();
                }
                let ended = matches!(*session.control.borrow(), Control::Ended(_));
                let keep = !ended && session.last_used.lock().unwrap().elapsed() < SESSION_IDLE;
                changed |= !keep;
                keep
            });
        }
        if changed {
            self.touch();
        }
    }

    // ---------------------------------------------------------- sessions

    fn open_session(&self, node: &RemoteNode, port: &str) -> Result<Arc<Session>, Failure> {
        let mut client = Self::client(node)?;
        let (input, requests) = mpsc::channel(16);
        input
            .try_send(pb::SessionInput {
                request_id: String::new(),
                kind: Some(pb::session_input::Kind::Open(pb::session_input::Open {
                    port_id: port.to_string(),
                    client: format!("GizMoth · {}", self.node.name),
                    reason: "通过级联在另一台 GizMoth 上操作此串口".into(),
                    device_data: false,
                })),
            })
            .map_err(|_| offline())?;
        let (control, control_rx) = watch::channel(Control::Opening);
        let pending: Pending = Arc::default();
        let dirty = self.dirty.clone();
        let reader_pending = pending.clone();
        let reader = tokio::spawn(async move {
            use pb::session_output::Kind;
            let end = |reason: String| {
                control.send_replace(Control::Ended(reason));
                reader_pending.lock().unwrap().clear();
                dirty.store(true, Ordering::SeqCst);
            };
            let mut stream = match client.session(ReceiverStream::new(requests)).await {
                Ok(response) => response.into_inner(),
                Err(status) => return end(describe(&status)),
            };
            loop {
                match stream.message().await {
                    Ok(Some(output)) => match output.kind {
                        Some(Kind::Queued(queued)) => {
                            control.send_replace(Control::Queued(queued.position, queued.holder));
                            dirty.store(true, Ordering::SeqCst);
                        }
                        Some(Kind::Granted(_)) => {
                            control.send_replace(Control::Granted);
                            dirty.store(true, Ordering::SeqCst);
                        }
                        Some(Kind::Ended(ended)) => return end(ended.reason),
                        Some(Kind::Result(result)) => {
                            if let Some(waiter) =
                                reader_pending.lock().unwrap().remove(&result.request_id)
                            {
                                let _ = waiter.send(result);
                            }
                        }
                        Some(Kind::Job(_)) | Some(Kind::DeviceData(_)) | None => {}
                    },
                    Ok(None) => return end("远端结束了会话".into()),
                    Err(status) => return end(describe(&status)),
                }
            }
        });
        self.touch();
        Ok(Arc::new(Session {
            input,
            control: control_rx,
            pending,
            jobs: Mutex::default(),
            last_used: Mutex::new(Instant::now()),
            reader: reader.abort_handle(),
        }))
    }

    async fn granted(&self, node: &RemoteNode, port: &str) -> Result<Arc<Session>, Failure> {
        let session = {
            let mut sessions = node.sessions.lock().unwrap();
            match sessions.get(port) {
                Some(session) if !matches!(*session.control.borrow(), Control::Ended(_)) => {
                    session.clone()
                }
                _ => {
                    let session = self.open_session(node, port)?;
                    sessions.insert(port.to_string(), session.clone());
                    session
                }
            }
        };
        *session.last_used.lock().unwrap() = Instant::now();
        let mut control = session.control.clone();
        let _ = tokio::time::timeout(
            GRANT_WAIT,
            control.wait_for(|c| matches!(c, Control::Granted | Control::Ended(_))),
        )
        .await;
        let state = control.borrow().clone();
        match state {
            Control::Granted => Ok(session),
            Control::Ended(reason) => {
                node.sessions.lock().unwrap().remove(port);
                self.touch();
                Err((StatusCode::CONFLICT, format!("远端未授予通信权：{reason}")))
            }
            Control::Queued(position, holder) => Err((
                StatusCode::CONFLICT,
                if holder.is_empty() {
                    format!("已在远端排队（第 {position} 位），获得通信权后可继续操作")
                } else {
                    format!(
                        "已在远端排队（第 {position} 位，当前由 {holder} 使用），获得通信权后可继续操作"
                    )
                },
            )),
            Control::Opening => Err((StatusCode::GATEWAY_TIMEOUT, "远端未响应独占请求".into())),
        }
    }

    async fn command(
        &self,
        id: &str,
        kind: pb::session_input::Kind,
    ) -> Result<pb::session_output::CommandResult, Failure> {
        let (node_id, port) = split(id).ok_or_else(not_found)?;
        let node = self.node(node_id).ok_or_else(not_found)?;
        {
            let state = node.state.lock().unwrap();
            if state.client.is_none() {
                return Err(offline());
            }
            if !state.ports.iter().any(|known| known.port_id == port) {
                return Err(not_found());
            }
        }
        let session = self.granted(&node, port).await?;
        let request_id = uuid::Uuid::new_v4().to_string();
        let (waiter, result) = oneshot::channel();
        session
            .pending
            .lock()
            .unwrap()
            .insert(request_id.clone(), waiter);
        let sent = tokio::time::timeout(
            CALL_TIMEOUT,
            session.input.send(pb::SessionInput {
                request_id: request_id.clone(),
                kind: Some(kind),
            }),
        )
        .await;
        if !matches!(sent, Ok(Ok(()))) {
            session.pending.lock().unwrap().remove(&request_id);
            return Err((StatusCode::BAD_GATEWAY, "远端会话已中断".into()));
        }
        let result = match tokio::time::timeout(COMMAND_TIMEOUT, result).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                return Err((
                    StatusCode::BAD_GATEWAY,
                    "远端会话已中断，请查看串口状态".into(),
                ))
            }
            Err(_) => {
                session.pending.lock().unwrap().remove(&request_id);
                return Err((
                    StatusCode::GATEWAY_TIMEOUT,
                    "远端未在时限内响应，请查看任务状态后再重试".into(),
                ));
            }
        };
        *session.last_used.lock().unwrap() = Instant::now();
        if !result.ok {
            let status = tonic::Status::new(tonic::Code::from_i32(result.code), result.message);
            return Err(grpc::http_error(&status));
        }
        if let Some(job) = &result.job {
            session.jobs.lock().unwrap().insert(job.job_id.clone());
        }
        self.touch();
        Ok(result)
    }

    pub async fn send(&self, id: &str, text: String, ending: Option<&str>) -> Reply {
        if text.len() > 4096 {
            return Err(bad("单次发送不超过 4096 字节"));
        }
        let line_ending = match ending {
            None | Some("lf") => pb::LineEnding::Lf,
            Some("crlf") => pb::LineEnding::Crlf,
            Some("cr") => pb::LineEnding::Cr,
            Some("none") => pb::LineEnding::None,
            _ => return Err(bad("换行设置无效")),
        };
        self.command(
            id,
            pb::session_input::Kind::Send(pb::session_input::Send {
                text,
                line_ending: line_ending.into(),
            }),
        )
        .await?;
        Ok((StatusCode::ACCEPTED, json!({ "queued": true })))
    }

    pub async fn job(
        &self,
        id: &str,
        kind: JobKind,
        path: Option<String>,
        job_id: Option<String>,
    ) -> Reply {
        use pb::session_input::{Install, Kind, Reboot, Refresh};
        let job_id = match job_id {
            Some(job_id) => uuid::Uuid::parse_str(&job_id)
                .map_err(|_| bad("无效的任务 ID"))?
                .to_string(),
            None => uuid::Uuid::new_v4().to_string(),
        };
        let command = match kind {
            JobKind::Refresh => Kind::Refresh(Refresh { job_id }),
            JobKind::RebootApp | JobKind::RebootLoader => Kind::Reboot(Reboot {
                target: if kind == JobKind::RebootApp {
                    pb::RebootTarget::App
                } else {
                    pb::RebootTarget::Loader
                }
                .into(),
                job_id,
            }),
            JobKind::Stage | JobKind::Install => {
                let (node_id, _) = split(id).ok_or_else(not_found)?;
                let node = self.node(node_id).ok_or_else(not_found)?;
                let path = path.ok_or_else(|| bad("请先选择固件包"))?;
                let package = upload(Self::client(&node)?, PathBuf::from(path)).await?;
                Kind::Install(Install {
                    package_id: package.package_id,
                    stage_only: kind == JobKind::Stage,
                    job_id,
                })
            }
            JobKind::RebootUpgrade => {
                return Err((
                    StatusCode::CONFLICT,
                    "远端串口请通过安装固件完成升级".into(),
                ))
            }
        };
        let result = self.command(id, command).await?;
        Ok((
            StatusCode::ACCEPTED,
            result
                .job
                .as_ref()
                .map(grpc::job_json)
                .unwrap_or_else(|| json!({ "accepted": true })),
        ))
    }

    pub async fn cancel_job(&self, job_id: &str) -> Reply {
        let nodes = self.nodes.lock().unwrap().clone();
        for node in nodes {
            let port = node
                .sessions
                .lock()
                .unwrap()
                .iter()
                .find(|(_, session)| session.jobs.lock().unwrap().contains(job_id))
                .map(|(port, _)| port.clone());
            if let Some(port) = port {
                let id = local_id(&node.config.node_id, &port);
                self.command(
                    &id,
                    pb::session_input::Kind::Cancel(pb::session_input::Cancel {
                        job_id: job_id.to_string(),
                    }),
                )
                .await?;
                return Ok((StatusCode::ACCEPTED, json!({ "cancelling": job_id })));
            }
            let foreign = node
                .state
                .lock()
                .unwrap()
                .ports
                .iter()
                .any(|port| port.job.as_ref().is_some_and(|job| job.job_id == job_id));
            if foreign {
                return Err((
                    StatusCode::CONFLICT,
                    "此任务由远端的其他连接发起，只能在那里取消".into(),
                ));
            }
        }
        Err((StatusCode::NOT_FOUND, "任务不存在或已结束".into()))
    }

    pub fn release(&self, id: &str) -> Reply {
        let (node_id, port) = split(id).ok_or_else(not_found)?;
        let node = self.node(node_id).ok_or_else(not_found)?;
        node.sessions.lock().unwrap().remove(port);
        self.touch();
        self.port_view(id)
            .map(|view| (StatusCode::OK, view))
            .ok_or_else(not_found)
    }

    pub async fn logs(&self, id: &str, after: Option<u64>, limit: u64) -> Reply {
        let (node_id, port) = split(id).ok_or_else(not_found)?;
        let node = self.node(node_id).ok_or_else(not_found)?;
        let mut client = Self::client(&node)?;
        let page = tokio::time::timeout(
            CALL_TIMEOUT,
            client.read_logs(pb::ReadLogsRequest {
                port_id: port.to_string(),
                after,
                limit: limit.clamp(1, 2000) as u32,
            }),
        )
        .await
        .map_err(|_| (StatusCode::GATEWAY_TIMEOUT, "远端未响应".to_string()))?
        .map_err(|status| grpc::http_error(&status))?
        .into_inner();
        Ok((StatusCode::OK, grpc::log_page_json(&page)))
    }
}

impl RemoteNode {
    fn new(config: RemoteConfig, target: Target) -> Self {
        Self {
            config,
            target,
            state: Mutex::new(NodeState {
                status: Status::Connecting,
                error: None,
                info: None,
                ports: Vec::new(),
                last_seen: None,
                client: None,
            }),
            sessions: Mutex::default(),
            stop: watch::channel(false).0,
        }
    }

    fn update(&self, f: impl FnOnce(&mut NodeState)) {
        f(&mut self.state.lock().unwrap());
    }
}

/// Streams a local firmware package to the owning node.
async fn upload(mut client: Client, path: PathBuf) -> Result<pb::PackageRef, Failure> {
    use pb::package_chunk::{Header, Kind};
    use tokio::io::AsyncReadExt;
    if !path.is_absolute() || !path.is_file() {
        return Err(bad("请选择本机固件包文件"));
    }
    let size = tokio::fs::metadata(&path)
        .await
        .map_err(|e| bad(e.to_string()))?
        .len();
    if size == 0 || size > MAX_PACKAGE {
        return Err(bad("固件包大小无效"));
    }
    let hashed = path.clone();
    let sha256 = tokio::task::spawn_blocking(move || -> io::Result<String> {
        let mut file = fs::File::open(hashed)?;
        let mut hasher = Sha256::new();
        io::copy(&mut file, &mut hasher)?;
        Ok(hex(&hasher.finalize()))
    })
    .await
    .map_err(|_| bad("内部错误"))?
    .map_err(|e| bad(e.to_string()))?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| bad(e.to_string()))?;
    let (chunks, stream) = mpsc::channel(4);
    let _ = chunks
        .send(pb::PackageChunk {
            kind: Some(Kind::Header(Header { name, size, sha256 })),
        })
        .await;
    tokio::spawn(async move {
        let mut buffer = vec![0u8; UPLOAD_CHUNK];
        loop {
            match file.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    let chunk = pb::PackageChunk {
                        kind: Some(Kind::Data(buffer[..count].to_vec())),
                    };
                    if chunks.send(chunk).await.is_err() {
                        break;
                    }
                }
            }
        }
    });
    client
        .upload_package(ReceiverStream::new(stream))
        .await
        .map(|response| response.into_inner())
        .map_err(|status| grpc::http_error(&status))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_default_to_tls_and_the_gateway_port() {
        let target = parse_target("192.168.1.20").unwrap();
        assert_eq!(target.url(), "https://192.168.1.20:7420");
        assert_eq!(
            parse_target("studio-mac.tail1234.ts.net:8443")
                .unwrap()
                .url(),
            "https://studio-mac.tail1234.ts.net:8443"
        );
        assert_eq!(
            parse_target("100.101.102.103").unwrap().url(),
            "https://100.101.102.103:7420"
        );
        assert_eq!(
            parse_target("127.0.0.1:7420").unwrap().url(),
            "http://127.0.0.1:7420"
        );
        assert_eq!(
            parse_target("http://localhost:9").unwrap().url(),
            "http://localhost:9"
        );
        assert_eq!(
            parse_target("https://[::1]:7420").unwrap().url(),
            "https://[::1]:7420"
        );
        assert!(
            parse_target("http://192.168.1.20").is_err(),
            "plaintext LAN"
        );
        assert!(parse_target("https://host/path").is_err());
        assert!(parse_target("https://user:secret@host").is_err());
        assert!(parse_target("ftp://host").is_err());
        assert!(parse_target("").is_err());
    }

    #[test]
    fn pins_accept_colon_separated_fingerprints() {
        let hex = "AB".repeat(32);
        let colons = (0..32).map(|_| "ab").collect::<Vec<_>>().join(":");
        assert_eq!(parse_pin(Some(&hex)).unwrap(), Some("ab".repeat(32)));
        assert_eq!(parse_pin(Some(&colons)).unwrap(), Some("ab".repeat(32)));
        assert_eq!(parse_pin(Some(" ")).unwrap(), None);
        assert!(parse_pin(Some("abc")).is_err());
    }

    #[test]
    fn imported_ids_never_collide_with_local_ids() {
        let node = uuid::Uuid::new_v4().to_string();
        let port = uuid::Uuid::new_v4().to_string();
        let id = local_id(&node, &port);
        assert_eq!(split(&id), Some((node.as_str(), port.as_str())));
        assert!(split(&port).is_none());
        assert!(split("a").is_none());
        assert!(split(&format!("not-a-node--{port}")).is_none());
        assert!(split(&format!("{node}--bad/port")).is_none());
        assert!(is_job_request(&format!("/v1/ports/{id}/jobs")));
        assert!(!is_job_request(&format!("/v1/ports/{port}/jobs")));
    }
}
