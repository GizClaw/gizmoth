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
//! `remotes.json` (mode 0600) stores addresses and API keys; it is written
//! only by the daemon. Connections use HTTP/2 prior knowledge (h2c).
use super::{
    daemon::JobKind,
    gateway,
    grpc::{self, pb, NodeIdentity},
    now_ms,
};
use hyper::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot, watch};
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
        format!("http://{host}:{}", self.port)
    }
}

/// Accepts `host`, `host:port` or `http://host:port` for LAN or Tailscale.
/// The default port is the Gateway's; no certificate setup is required.
fn parse_target(input: &str) -> Result<Target, String> {
    let input = input.trim();
    if input.is_empty() || input.len() > 512 || input.chars().any(char::is_control) {
        return Err("请输入远端 GizMoth 的地址".into());
    }
    let explicit = input.contains("://");
    let address = if explicit {
        input.to_string()
    } else {
        format!("http://{input}")
    };
    let url = url::Url::parse(&address).map_err(|_| "地址格式无效".to_string())?;
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
    if url.scheme() != "http" {
        return Err("请使用主机地址或 http:// 地址".into());
    }
    // Url normalizes an explicit HTTP port 80 away. Read the port from the
    // validated original authority instead, including bracketed IPv6 hosts.
    let authority = address.split_once("://").unwrap().1.trim_end_matches('/');
    let port = if let Some(ipv6) = authority.strip_prefix('[') {
        ipv6.split_once(']')
            .and_then(|(_, suffix)| suffix.strip_prefix(':'))
    } else {
        authority.split_once(':').map(|(_, port)| port)
    };
    Ok(Target {
        host,
        port: port
            .and_then(|port| port.parse().ok())
            .unwrap_or(gateway::DEFAULT_PORT),
    })
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
async fn connect(target: &Target, key: &str) -> Result<(Client, pb::Node), String> {
    let channel = tonic::transport::Endpoint::from_shared(target.url())
        .map_err(|e| e.to_string())?
        .connect_timeout(CONNECT_TIMEOUT)
        .http2_keep_alive_interval(Duration::from_secs(15))
        .keep_alive_timeout(Duration::from_secs(10))
        .keep_alive_while_idle(true)
        .connect()
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
    /// Needs the user: authentication or identity problems.
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
    Queued(u64, u32, String),
    Granted(u64),
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

// If both mutexes are needed, acquire state before sessions. In particular,
// session creation must obtain its client before locking the session registry.
struct RemoteNode {
    config: RemoteConfig,
    target: Target,
    state: Mutex<NodeState>,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    stop: watch::Sender<bool>,
}

/// Covers forwarded preparation and request handling before a session exists.
/// The daemon reserves this while holding its retirement-admission mutex.
pub(super) struct ForwardedWork {
    active: Arc<AtomicUsize>,
    dirty: Arc<AtomicBool>,
}

impl Drop for ForwardedWork {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
        self.dirty.store(true, Ordering::SeqCst);
    }
}

pub struct Remotes {
    dir: PathBuf,
    node: NodeIdentity,
    dirty: Arc<AtomicBool>,
    write: fn(&Path, &[u8]) -> io::Result<()>,
    nodes: Mutex<Vec<Arc<RemoteNode>>>,
    started: AtomicBool,
    active_requests: Arc<AtomicUsize>,
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
            .filter_map(|mut config| {
                // Migrate saved endpoints from the earlier TLS-only design.
                let url = config
                    .url
                    .strip_prefix("https://")
                    .map(|host| format!("http://{host}"))
                    .unwrap_or_else(|| config.url.clone());
                let target = parse_target(&url).ok()?;
                config.url = target.url();
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
            active_requests: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub fn has_nodes(&self) -> bool {
        !self.nodes.lock().unwrap().is_empty()
    }

    pub(super) fn reserve_work(&self) -> ForwardedWork {
        self.active_requests.fetch_add(1, Ordering::SeqCst);
        self.touch();
        ForwardedWork {
            active: self.active_requests.clone(),
            dirty: self.dirty.clone(),
        }
    }

    /// An open owned/queued session can carry device work even before a fresh
    /// topology or job event arrives. Protect it like a local external lease;
    /// unrelated jobs merely observed on a remote node do not block retirement.
    pub fn has_work(&self) -> bool {
        if self.active_requests.load(Ordering::SeqCst) != 0 {
            return true;
        }
        let nodes = self.nodes.lock().unwrap().clone();
        nodes.iter().any(|node| {
            node.sessions
                .lock()
                .unwrap()
                .values()
                .any(|session| !matches!(*session.control.borrow(), Control::Ended(_)))
        })
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
            Control::Queued(connection, position, holder) => {
                json!({ "state": "queued", "connectionId": connection, "position": position, "holder": holder })
            }
            Control::Granted(connection) => {
                json!({ "state": "granted", "connectionId": connection })
            }
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

    /// Connects once without saving, returning node information and port count.
    pub async fn probe(&self, url: &str, key: &str) -> Reply {
        let target = parse_target(url).map_err(bad)?;
        valid_key(key).map_err(bad)?;
        let (mut client, node) = connect(&target, key)
            .await
            .map_err(|error| (StatusCode::BAD_GATEWAY, error))?;
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
        Ok((
            StatusCode::OK,
            json!({
                "url": target.url(),
                "node": {"id": node.node_id, "name": node.name, "version": node.version},
                "self": node.node_id == self.node.id,
                "ports": ports,
            }),
        ))
    }

    pub async fn add(self: &Arc<Self>, url: &str, key: &str) -> Reply {
        let target = parse_target(url).map_err(bad)?;
        valid_key(key).map_err(bad)?;
        let (_, info) = connect(&target, key)
            .await
            .map_err(|error| (StatusCode::BAD_GATEWAY, error))?;
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
        let (mut client, info) = match connect(&node.target, &node.config.api_key).await {
            Ok(connected) => connected,
            Err(error) => {
                let fatal = error.starts_with("API Key");
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

    fn open_session(&self, mut client: Client, port: &str) -> Result<Arc<Session>, Failure> {
        let (input, requests) = mpsc::channel(16);
        input
            .try_send(pb::SessionInput {
                request_id: String::new(),
                kind: Some(pb::session_input::Kind::Open(pb::session_input::Open {
                    port_id: port.to_string(),
                    client: format!("GizMoth · {}", self.node.name),
                    title: "通过级联操作串口".into(),
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
                            control.send_replace(Control::Queued(
                                queued.connection_id,
                                queued.position,
                                queued.holder,
                            ));
                            dirty.store(true, Ordering::SeqCst);
                        }
                        Some(Kind::Granted(granted)) => {
                            control.send_replace(Control::Granted(granted.connection_id));
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

    fn session(&self, node: &RemoteNode, port: &str) -> Result<Arc<Session>, Failure> {
        let client = Self::client(node)?;
        let mut sessions = node.sessions.lock().unwrap();
        match sessions.get(port) {
            Some(session) if !matches!(*session.control.borrow(), Control::Ended(_)) => {
                Ok(session.clone())
            }
            _ => {
                let session = self.open_session(client, port)?;
                sessions.insert(port.to_string(), session.clone());
                Ok(session)
            }
        }
    }

    async fn granted(&self, node: &RemoteNode, port: &str) -> Result<Arc<Session>, Failure> {
        let session = self.session(node, port)?;
        *session.last_used.lock().unwrap() = Instant::now();
        let mut control = session.control.clone();
        let _ = tokio::time::timeout(
            GRANT_WAIT,
            control.wait_for(|c| matches!(c, Control::Granted(_) | Control::Ended(_))),
        )
        .await;
        let state = control.borrow().clone();
        match state {
            Control::Granted(_) => Ok(session),
            Control::Ended(reason) => {
                node.sessions.lock().unwrap().remove(port);
                self.touch();
                Err((StatusCode::CONFLICT, format!("远端未授予通信权：{reason}")))
            }
            Control::Queued(_, position, holder) => Err((
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

    pub fn release(&self, id: &str, connection: u64) -> Reply {
        let (node_id, port) = split(id).ok_or_else(not_found)?;
        let node = self.node(node_id).ok_or_else(not_found)?;
        {
            let mut sessions = node.sessions.lock().unwrap();
            let matches = sessions.get(port).is_some_and(|session| {
                matches!(*session.control.borrow(), Control::Queued(id, _, _) | Control::Granted(id) if id == connection)
            });
            if !matches {
                return Err((StatusCode::NOT_FOUND, "连接不存在或已结束".into()));
            }
            sessions.remove(port);
        }
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
    fn addresses_use_h2c_for_lan_tailscale_and_loopback() {
        for (input, expected) in [
            ("192.168.1.20", "http://192.168.1.20:7420"),
            (
                "studio-mac.tail1234.ts.net:8443",
                "http://studio-mac.tail1234.ts.net:8443",
            ),
            ("100.101.102.103", "http://100.101.102.103:7420"),
            ("127.0.0.1:7420", "http://127.0.0.1:7420"),
            ("http://localhost:9", "http://localhost:9"),
            ("http://[::1]:7420", "http://[::1]:7420"),
            ("http://192.168.1.20", "http://192.168.1.20:7420"),
            ("192.168.1.20:80", "http://192.168.1.20:80"),
            ("http://192.168.1.20:80", "http://192.168.1.20:80"),
            (
                "studio-mac.tail1234.ts.net:80",
                "http://studio-mac.tail1234.ts.net:80",
            ),
            ("http://[::1]:80/", "http://[::1]:80"),
            ("[::1]:80", "http://[::1]:80"),
            ("http://localhost:8080/", "http://localhost:8080"),
        ] {
            assert_eq!(parse_target(input).unwrap().url(), expected);
        }
        for input in [
            "https://host",
            "http://host/path",
            "http://user:secret@host",
            "http://host?token=secret",
            "ftp://host",
            "",
        ] {
            assert!(parse_target(input).is_err(), "{input}");
        }
    }

    #[test]
    fn snapshots_and_session_opening_share_a_consistent_lock_order() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        let channel = tonic::transport::Endpoint::from_static("http://127.0.0.1:1").connect_lazy();
        let client = pb::gateway_client::GatewayClient::with_interceptor(
            channel,
            Auth("Bearer test".parse().unwrap()),
        );
        let node_id = uuid::Uuid::new_v4().to_string();
        let node = Arc::new(RemoteNode::new(
            RemoteConfig {
                node_id: node_id.clone(),
                name: "Remote".into(),
                url: "http://127.0.0.1:1".into(),
                api_key: "test".into(),
                created_at: 1,
            },
            parse_target("127.0.0.1:1").unwrap(),
        ));
        node.update(|state| {
            state.client = Some(client);
            state.ports.push(pb::Port {
                port_id: "port".into(),
                name: "Serial".into(),
                ..Default::default()
            });
        });
        let directory = tempfile::tempdir().unwrap();
        let remotes = Remotes::load(
            directory.path(),
            NodeIdentity {
                id: uuid::Uuid::new_v4().to_string(),
                name: "Local".into(),
            },
            Arc::new(AtomicBool::new(false)),
            |path, bytes| fs::write(path, bytes),
        );
        remotes.nodes.lock().unwrap().push(node.clone());
        let id = local_id(&node_id, "port");
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let (done, completed) = std::sync::mpsc::channel();
        let snapshots = {
            let (remotes, id, barrier, done) =
                (remotes.clone(), id.clone(), barrier.clone(), done.clone());
            std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..500 {
                    assert!(remotes.nodes_json().is_array());
                    assert!(remotes.port_view(&id).is_some());
                }
                done.send(()).unwrap();
            })
        };
        let commands = {
            let handle = runtime.handle().clone();
            std::thread::spawn(move || {
                let _entered = handle.enter();
                barrier.wait();
                for _ in 0..500 {
                    // No network grant is needed to reproduce the lock inversion:
                    // opening a new session used to lock sessions then state.
                    drop(remotes.session(&node, "port").unwrap());
                    node.sessions.lock().unwrap().remove("port");
                }
                done.send(()).unwrap();
            })
        };
        for _ in 0..2 {
            completed
                .recv_timeout(Duration::from_secs(5))
                .expect("concurrent snapshots and new sessions must not deadlock");
        }
        snapshots.join().unwrap();
        commands.join().unwrap();
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
